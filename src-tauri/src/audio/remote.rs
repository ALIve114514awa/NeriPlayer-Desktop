use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE, REFERER, USER_AGENT};
use reqwest::StatusCode;
use rodio::source::SeekError as RodioSeekError;
use rodio::Source;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use symphonia::core::audio::{AudioBufferRef, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{self, Time};
use symphonia::default::{get_codecs, get_probe};

use crate::error::{AppError, AppResult};

const REMOTE_INITIAL_BLOCK_BYTES: u64 = 1024 * 1024;
const REMOTE_FETCH_BLOCK_BYTES: u64 = 4 * 1024 * 1024;
const REMOTE_DECODER_BUFFER_BYTES: usize = 256 * 1024;
const REMOTE_MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_DECODE_RETRIES: usize = 3;
const PLAYBACK_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Clone)]
pub struct RemoteAudioCache {
    data_path: PathBuf,
    part_path: PathBuf,
    ready_path: PathBuf,
}

#[derive(Clone)]
pub struct RemoteAudioSource {
    inner: Arc<RemoteAudioInner>,
    pos: u64,
}

struct RemoteAudioInner {
    client: reqwest::Client,
    url: String,
    referer: String,
    total_len: u64,
    cache: Mutex<Vec<CachedSegment>>,
    disk_ranges: Mutex<Vec<CachedRange>>,
    in_flight: Mutex<HashSet<u64>>,
    last_read_pos: Mutex<u64>,
    disk_cache: Option<RemoteAudioCache>,
}

struct CachedSegment {
    start: u64,
    data: Vec<u8>,
}

#[derive(Clone, Copy)]
struct CachedRange {
    start: u64,
    end: u64,
}

impl RemoteAudioSource {
    pub async fn open(
        client: reqwest::Client,
        url: String,
        referer: String,
        disk_cache: Option<RemoteAudioCache>,
    ) -> AppResult<Self> {
        let (total_len, initial_segment) = match probe_range_len(&client, &url, &referer).await {
            Ok((len, data)) => (Some(len), Some(CachedSegment { start: 0, data })),
            Err(range_error) => {
                eprintln!("[remote-audio] range probe failed: {}", range_error);
                (probe_head_len(&client, &url, &referer).await?, None)
            }
        };

        let total_len = total_len.ok_or_else(|| {
            AppError::Audio("Remote source does not expose a seekable byte length".into())
        })?;

        if let Some(cache) = &disk_cache {
            cache.prepare(total_len)?;
        }

        let prefetch_start = initial_segment
            .as_ref()
            .map(|segment| segment.start + segment.data.len() as u64)
            .unwrap_or(0);
        let initial_disk_segment = initial_segment
            .as_ref()
            .map(|segment| (segment.start, segment.data.clone()));
        let cache = initial_segment.into_iter().collect();
        let inner = Arc::new(RemoteAudioInner {
            client,
            url,
            referer,
            total_len,
            cache: Mutex::new(cache),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            last_read_pos: Mutex::new(0),
            disk_cache,
        });
        if let Some((start, data)) = initial_disk_segment {
            write_disk_range(&inner, start, &data);
        }
        spawn_prefetch_sequence(inner.clone(), prefetch_start, true);

        Ok(Self {
            inner,
            pos: 0,
        })
    }

    pub fn byte_len(&self) -> u64 {
        self.inner.total_len
    }

    fn read_cached(&self, out: &mut [u8]) -> usize {
        let cache = match self.inner.cache.lock() {
            Ok(cache) => cache,
            Err(_) => return 0,
        };

        for segment in cache.iter() {
            let segment_end = segment.start + segment.data.len() as u64;
            if self.pos < segment.start || self.pos >= segment_end {
                continue;
            }

            let offset = (self.pos - segment.start) as usize;
            let available = segment.data.len().saturating_sub(offset);
            let len = available.min(out.len());
            out[..len].copy_from_slice(&segment.data[offset..offset + len]);
            return len;
        }

        0
    }

    fn read_disk_cached(&self, out: &mut [u8]) -> usize {
        let Some(cache) = &self.inner.disk_cache else {
            return 0;
        };
        let Some(len) = disk_cached_len(&self.inner, self.pos, out.len()) else {
            return 0;
        };
        cache.read_cached(self.pos, &mut out[..len]).unwrap_or(0)
    }

    fn has_cached_position(&self) -> bool {
        let cache = match self.inner.cache.lock() {
            Ok(cache) => cache,
            Err(_) => return false,
        };

        cache.iter().any(|segment| {
            let segment_end = segment.start + segment.data.len() as u64;
            self.pos >= segment.start && self.pos < segment_end
        })
    }

    fn fetch_from_current_position(&self, wanted_len: usize) -> io::Result<()> {
        let start = self.pos;
        if start >= self.inner.total_len {
            return Ok(());
        }

        let wanted = wanted_len.max(REMOTE_FETCH_BLOCK_BYTES as usize) as u64;
        let end = start
            .saturating_add(wanted)
            .saturating_sub(1)
            .min(self.inner.total_len.saturating_sub(1));
        let data = fetch_range_block(&self.inner, start, end)?;

        if data.is_empty() {
            return Ok(());
        }

        let mut cache = self
            .inner
            .cache
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "remote cache lock poisoned"))?;
        cache.push(CachedSegment { start, data });
        cache.sort_by_key(|segment| segment.start);
        trim_cache(&mut cache, cache_center(&self.inner, self.pos));
        let next_start = start
            .saturating_add(REMOTE_FETCH_BLOCK_BYTES)
            .min(self.inner.total_len);
        drop(cache);
        spawn_prefetch_sequence(self.inner.clone(), next_start, false);
        Ok(())
    }
}

impl Read for RemoteAudioSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.inner.total_len {
            return Ok(0);
        }

        let mut total_read = 0usize;
        while total_read < out.len() && self.pos < self.inner.total_len {
            let read = self.read_cached(&mut out[total_read..]);
            if read > 0 {
                self.pos += read as u64;
                total_read += read;
                remember_read_pos(&self.inner, self.pos);
                continue;
            }
            let read = self.read_disk_cached(&mut out[total_read..]);
            if read > 0 {
                self.pos += read as u64;
                total_read += read;
                remember_read_pos(&self.inner, self.pos);
                continue;
            }
            self.fetch_from_current_position(out.len() - total_read)?;
            if !self.has_cached_position() {
                break;
            }
        }

        Ok(total_read)
    }
}

impl Seek for RemoteAudioSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
            SeekFrom::End(offset) => self.inner.total_len as i128 + offset as i128,
        };

        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek before start",
            ));
        }

        self.pos = (next as u64).min(self.inner.total_len);
        spawn_prefetch_sequence(self.inner.clone(), self.pos, false);
        Ok(self.pos)
    }
}

impl RemoteAudioCache {
    pub fn new(root: PathBuf, cache_key: &str) -> AppResult<Self> {
        let digest = hex::encode(Sha256::digest(cache_key.as_bytes()));
        let shard = digest.get(0..2).unwrap_or("00");
        let dir = root.join(shard);
        std::fs::create_dir_all(&dir).map_err(|err| AppError::Other(err.to_string()))?;
        Ok(Self {
            data_path: dir.join(format!("{}.audio", digest)),
            part_path: dir.join(format!("{}.part", digest)),
            ready_path: dir.join(format!("{}.ready", digest)),
        })
    }

    pub fn ready_path(&self) -> Option<PathBuf> {
        (self.ready_path.is_file() && self.data_path.is_file()).then(|| self.data_path.clone())
    }

    fn prepare(&self, total_len: u64) -> io::Result<()> {
        if self.ready_path().is_some() {
            return Ok(());
        }
        if let Some(parent) = self.part_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&self.part_path)?;
        file.set_len(total_len)?;
        Ok(())
    }

    fn write_range(&self, start: u64, data: &[u8]) -> io::Result<()> {
        if data.is_empty() || self.ready_path().is_some() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&self.part_path)?;
        file.seek(SeekFrom::Start(start))?;
        file.write_all(data)?;
        Ok(())
    }

    fn read_cached(&self, start: u64, out: &mut [u8]) -> io::Result<usize> {
        let path = if self.ready_path().is_some() {
            &self.data_path
        } else {
            &self.part_path
        };
        let mut file = std::fs::OpenOptions::new().read(true).open(path)?;
        file.seek(SeekFrom::Start(start))?;
        file.read(out)
    }

    fn mark_ready(&self) -> io::Result<()> {
        if self.ready_path().is_some() {
            return Ok(());
        }
        if self.data_path.exists() {
            let _ = std::fs::remove_file(&self.data_path);
        }
        std::fs::rename(&self.part_path, &self.data_path)?;
        std::fs::write(&self.ready_path, b"ready")?;
        Ok(())
    }
}

impl MediaSource for RemoteAudioSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.inner.total_len)
    }
}

pub struct SymphoniaAudioDecoder {
    decoder: Box<dyn Decoder>,
    current_frame_offset: usize,
    format: Box<dyn FormatReader>,
    track_id: u32,
    total_duration: Option<Time>,
    buffer: SampleBuffer<i16>,
    spec: SignalSpec,
}

impl SymphoniaAudioDecoder {
    pub fn new(source: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Self, String> {
        let options = MediaSourceStreamOptions {
            buffer_len: REMOTE_DECODER_BUFFER_BYTES,
        };
        let mss = MediaSourceStream::new(source, options);
        Self::init(mss, extension).map_err(|err| format!("Decode error: {}", err))
    }

    fn init(
        mss: MediaSourceStream,
        extension: Option<&str>,
    ) -> symphonia::core::errors::Result<Self> {
        let mut hint = Hint::new();
        if let Some(ext) = extension {
            hint.with_extension(ext);
        }

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts: MetadataOptions = Default::default();
        let mut probed = get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

        let track_id = probed
            .format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(SymphoniaError::Unsupported("No track with supported codec"))?
            .id;

        let track = probed
            .format
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .ok_or(SymphoniaError::Unsupported("Selected track disappeared"))?;

        let mut decoder = get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
        let total_duration = track
            .codec_params
            .time_base
            .zip(track.codec_params.n_frames)
            .map(|(base, frames)| base.calc_time(frames));

        let mut decode_errors = 0usize;
        let decoded = loop {
            let packet = probed.format.next_packet()?;
            if packet.track_id() != track_id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(decoded) => break decoded,
                Err(SymphoniaError::DecodeError(_)) if decode_errors < MAX_DECODE_RETRIES => {
                    decode_errors += 1;
                }
                Err(err) => return Err(err),
            }
        };
        let spec = decoded.spec().to_owned();
        let buffer = Self::copy_buffer(decoded, &spec);

        Ok(Self {
            decoder,
            current_frame_offset: 0,
            format: probed.format,
            track_id,
            total_duration,
            buffer,
            spec,
        })
    }

    fn copy_buffer(decoded: AudioBufferRef<'_>, spec: &SignalSpec) -> SampleBuffer<i16> {
        let duration = units::Duration::from(decoded.capacity() as u64);
        let mut buffer = SampleBuffer::<i16>::new(duration, *spec);
        buffer.copy_interleaved_ref(decoded);
        buffer
    }

    fn refine_position(&mut self, seek_res: SeekedTo) -> Result<(), RodioSeekError> {
        let mut samples_to_pass = seek_res.required_ts.saturating_sub(seek_res.actual_ts);
        let packet = loop {
            let candidate = self
                .format
                .next_packet()
                .map_err(|err| seek_error(format!("Could not refine seek: {}", err)))?;
            if candidate.dur() > samples_to_pass {
                break candidate;
            }
            samples_to_pass = samples_to_pass.saturating_sub(candidate.dur());
        };

        let mut decoded = self.decoder.decode(&packet);
        for _ in 0..MAX_DECODE_RETRIES {
            if decoded.is_ok() {
                break;
            }
            let packet = self
                .format
                .next_packet()
                .map_err(|err| seek_error(format!("Could not retry after seek: {}", err)))?;
            decoded = self.decoder.decode(&packet);
        }
        let decoded =
            decoded.map_err(|err| seek_error(format!("Could not decode after seek: {}", err)))?;
        decoded.spec().clone_into(&mut self.spec);
        self.buffer = Self::copy_buffer(decoded, &self.spec);
        self.current_frame_offset = samples_to_pass as usize * self.channels() as usize;
        Ok(())
    }
}

impl Iterator for SymphoniaAudioDecoder {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.current_frame_offset >= self.buffer.len() {
            let mut decode_errors = 0usize;
            let decoded = loop {
                let packet = self.format.next_packet().ok()?;
                if packet.track_id() != self.track_id {
                    continue;
                }

                match self.decoder.decode(&packet) {
                    Ok(decoded) => break decoded,
                    Err(SymphoniaError::DecodeError(_)) if decode_errors < MAX_DECODE_RETRIES => {
                        decode_errors += 1;
                    }
                    Err(_) => return None,
                }
            };
            decoded.spec().clone_into(&mut self.spec);
            self.buffer = Self::copy_buffer(decoded, &self.spec);
            self.current_frame_offset = 0;
        }

        let sample = *self.buffer.samples().get(self.current_frame_offset)?;
        self.current_frame_offset += 1;
        Some(sample)
    }
}

impl Source for SymphoniaAudioDecoder {
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.buffer.len().saturating_sub(self.current_frame_offset))
    }

    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }

    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration.map(time_to_duration)
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
        let time = match self.total_duration {
            Some(duration) if time_to_duration(duration).saturating_sub(pos).as_millis() < 1 => {
                skip_back_tiny_bit(duration)
            }
            _ => pos.as_secs_f64().into(),
        };

        let channel_offset = self.current_frame_offset % self.channels() as usize;
        let seek_res = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: None,
                },
            )
            .map_err(|err| seek_error(format!("Format seek failed: {}", err)))?;
        self.decoder.reset();
        self.refine_position(seek_res)?;
        self.current_frame_offset += channel_offset;
        Ok(())
    }
}

async fn probe_head_len(
    client: &reqwest::Client,
    url: &str,
    referer: &str,
) -> AppResult<Option<u64>> {
    let response = match client
        .head(url)
        .header(REFERER, referer)
        .header(USER_AGENT, PLAYBACK_USER_AGENT)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };

    if !response.status().is_success() {
        return Ok(None);
    }

    let accepts_ranges = response
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("bytes"))
        .unwrap_or(false);
    if !accepts_ranges {
        return Ok(None);
    }

    Ok(response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|len| *len > 0))
}

async fn probe_range_len(
    client: &reqwest::Client,
    url: &str,
    referer: &str,
) -> AppResult<(u64, Vec<u8>)> {
    let range_end = REMOTE_INITIAL_BLOCK_BYTES.saturating_sub(1);
    let response = client
        .get(url)
        .header(REFERER, referer)
        .header(USER_AGENT, PLAYBACK_USER_AGENT)
        .header(RANGE, format!("bytes=0-{}", range_end))
        .send()
        .await
        .map_err(AppError::Network)?;

    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(AppError::Audio(format!(
            "Remote source does not support HTTP Range: {}",
            response.status()
        )));
    }

    let total_len = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(parse_content_range_total)
        .ok_or_else(|| {
            AppError::Audio("Remote Range response did not include total length".into())
        })?;
    let data = response
        .bytes()
        .await
        .map_err(AppError::Network)?
        .to_vec();

    Ok((total_len, data))
}

fn fetch_range_block(inner: &RemoteAudioInner, start: u64, end: u64) -> io::Result<Vec<u8>> {
    tauri::async_runtime::block_on(fetch_range_block_async(inner, start, end))
}

async fn fetch_range_block_async(
    inner: &RemoteAudioInner,
    start: u64,
    end: u64,
) -> io::Result<Vec<u8>> {
    let result = async {
        let response = inner
            .client
            .get(&inner.url)
            .header(REFERER, inner.referer.as_str())
            .header(USER_AGENT, PLAYBACK_USER_AGENT)
            .header(RANGE, format!("bytes={}-{}", start, end))
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        Ok::<_, reqwest::Error>((status, bytes.to_vec()))
    }
    .await
    .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;

    let (status, data) = result;
    if status == StatusCode::PARTIAL_CONTENT {
        write_disk_range(inner, start, &data);
        return Ok(data);
    }

    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok(Vec::new());
    }

    Err(io::Error::new(
        io::ErrorKind::Other,
        format!("unexpected remote range status: {}", status),
    ))
}

fn spawn_prefetch_sequence(inner: Arc<RemoteAudioInner>, start: u64, mark_complete: bool) {
    tauri::async_runtime::spawn(async move {
        let mut next_start = start.min(inner.total_len);
        let mut reached_end = next_start >= inner.total_len;
        while next_start < inner.total_len {
            if let Some(cached_end) = cached_segment_end(&inner, next_start) {
                next_start = cached_end;
                continue;
            }

            if !claim_prefetch_start(&inner, next_start) {
                break;
            }

            let fetch_start = next_start;
            let end = fetch_start
                .saturating_add(REMOTE_FETCH_BLOCK_BYTES)
                .saturating_sub(1)
                .min(inner.total_len.saturating_sub(1));
            let result = fetch_range_block_async(&inner, fetch_start, end).await;
            release_prefetch_start(&inner, fetch_start);

            match result {
                Ok(data) if data.is_empty() => break,
                Ok(data) => {
                    let fetched_end = fetch_start.saturating_add(data.len() as u64);
                    if let Ok(mut cache) = inner.cache.lock() {
                        if !cache_contains_position(&cache, fetch_start) {
                            cache.push(CachedSegment {
                                start: fetch_start,
                                data,
                            });
                            cache.sort_by_key(|segment| segment.start);
                            trim_cache(&mut cache, cache_center(&inner, fetch_start));
                        }
                    }
                    next_start = fetched_end.max(fetch_start.saturating_add(1));
                    reached_end = next_start >= inner.total_len;
                }
                Err(err) => {
                    eprintln!(
                        "[remote-audio] prefetch stopped at {}: {}",
                        fetch_start, err
                    );
                    break;
                }
            }
        }
        if mark_complete && reached_end {
            if let Some(cache) = &inner.disk_cache {
                if let Err(err) = cache.mark_ready() {
                    eprintln!("[remote-audio] cache finalize failed: {}", err);
                }
            }
        }
    });
}

fn cached_segment_end(inner: &RemoteAudioInner, pos: u64) -> Option<u64> {
    let cache = inner.cache.lock().ok()?;
    cache.iter().find_map(|segment| {
        let segment_end = segment.start + segment.data.len() as u64;
        (pos >= segment.start && pos < segment_end).then_some(segment_end)
    })
}

fn write_disk_range(inner: &RemoteAudioInner, start: u64, data: &[u8]) {
    let Some(cache) = &inner.disk_cache else {
        return;
    };
    if cache.write_range(start, data).is_ok() {
        record_disk_range(inner, start, data.len() as u64);
    }
}

fn record_disk_range(inner: &RemoteAudioInner, start: u64, len: u64) {
    if len == 0 {
        return;
    }
    let mut next = CachedRange {
        start,
        end: start.saturating_add(len).min(inner.total_len),
    };
    let Ok(mut ranges) = inner.disk_ranges.lock() else {
        return;
    };
    ranges.push(next);
    ranges.sort_by_key(|range| range.start);

    let mut merged: Vec<CachedRange> = Vec::with_capacity(ranges.len());
    for range in ranges.iter().copied() {
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        next = range;
        merged.push(next);
    }
    *ranges = merged;
}

fn disk_cached_len(inner: &RemoteAudioInner, pos: u64, wanted_len: usize) -> Option<usize> {
    let ranges = inner.disk_ranges.lock().ok()?;
    ranges.iter().find_map(|range| {
        if pos < range.start || pos >= range.end {
            return None;
        }
        Some((range.end - pos).min(wanted_len as u64) as usize)
    })
}

fn remember_read_pos(inner: &RemoteAudioInner, pos: u64) {
    if let Ok(mut last_read_pos) = inner.last_read_pos.lock() {
        *last_read_pos = pos;
    }
}

fn cache_center(inner: &RemoteAudioInner, fallback: u64) -> u64 {
    inner.last_read_pos.lock().map(|pos| *pos).unwrap_or(fallback)
}

fn cache_contains_position(cache: &[CachedSegment], pos: u64) -> bool {
    cache.iter().any(|segment| {
        let segment_end = segment.start + segment.data.len() as u64;
        pos >= segment.start && pos < segment_end
    })
}

fn claim_prefetch_start(inner: &RemoteAudioInner, start: u64) -> bool {
    if cached_segment_end(inner, start).is_some() {
        return false;
    }
    let Ok(mut in_flight) = inner.in_flight.lock() else {
        return false;
    };
    in_flight.insert(start)
}

fn release_prefetch_start(inner: &RemoteAudioInner, start: u64) {
    if let Ok(mut in_flight) = inner.in_flight.lock() {
        in_flight.remove(&start);
    }
}

fn parse_content_range_total(value: &reqwest::header::HeaderValue) -> Option<u64> {
    let text = value.to_str().ok()?;
    let total = text.rsplit('/').next()?;
    if total == "*" {
        return None;
    }
    total.parse::<u64>().ok()
}

fn trim_cache(cache: &mut Vec<CachedSegment>, current_pos: u64) {
    let mut total: usize = cache.iter().map(|segment| segment.data.len()).sum();
    if total <= REMOTE_MAX_CACHE_BYTES {
        return;
    }

    cache.sort_by_key(|segment| segment_distance(segment, current_pos));
    while total > REMOTE_MAX_CACHE_BYTES {
        let Some(segment) = cache.pop() else {
            break;
        };
        total = total.saturating_sub(segment.data.len());
    }
    cache.sort_by_key(|segment| segment.start);
}

fn segment_distance(segment: &CachedSegment, current_pos: u64) -> u64 {
    let segment_end = segment.start + segment.data.len() as u64;
    if current_pos < segment.start {
        segment.start - current_pos
    } else if current_pos > segment_end {
        current_pos - segment_end
    } else {
        0
    }
}

fn time_to_duration(time: Time) -> Duration {
    Duration::from_secs(time.seconds)
        + Duration::from_nanos((time.frac * 1_000_000_000.0).max(0.0) as u64)
}

fn skip_back_tiny_bit(mut time: Time) -> Time {
    time.frac -= 0.0001;
    if time.frac < 0.0 {
        time.seconds = time.seconds.saturating_sub(1);
        time.frac += 1.0;
    }
    time
}

fn seek_error(message: String) -> RodioSeekError {
    RodioSeekError::Other(Box::new(io::Error::new(io::ErrorKind::Other, message)))
}
