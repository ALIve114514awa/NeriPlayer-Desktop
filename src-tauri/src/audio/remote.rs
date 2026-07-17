use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE, REFERER, USER_AGENT};
use reqwest::StatusCode;
use rodio::source::SeekError as RodioSeekError;
use rodio::Source;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{self, Time, TimeBase};
use symphonia::default::{get_codecs, get_probe};

use crate::error::{AppError, AppResult};

const REMOTE_INITIAL_BLOCK_BYTES: u64 = 128 * 1024;
const REMOTE_FETCH_BLOCK_BYTES: u64 = 512 * 1024;
const REMOTE_DECODER_BUFFER_BYTES: usize = 128 * 1024;
const REMOTE_MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const REMOTE_PREFETCH_LOW_WATER_MS: u64 = 5_000;
const REMOTE_PREFETCH_TARGET_MS: u64 = 15_000;
const REMOTE_PREFETCH_MIN_LOW_WATER_BYTES: u64 = 128 * 1024;
const REMOTE_PREFETCH_MAX_LOW_WATER_BYTES: u64 = 8 * 1024 * 1024;
const REMOTE_PREFETCH_MIN_TARGET_BYTES: u64 = 512 * 1024;
const REMOTE_PREFETCH_MAX_TARGET_BYTES: u64 = 16 * 1024 * 1024;
const REMOTE_PREFETCH_FALLBACK_LOW_WATER_BYTES: u64 = 1024 * 1024;
const REMOTE_PREFETCH_FALLBACK_TARGET_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DECODE_RETRIES: usize = 3;
const REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CACHE_MARKER_VERSION: &str = "v2";
const CACHE_MIN_DURATION_GAP_MS: u64 = 5_000;
const CACHE_MIN_DURATION_RATIO_PERCENT: u64 = 85;
const STALE_CACHE_PART_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_VALIDATED_CACHE_STAMPS: usize = 4_096;
const PLAYBACK_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Clone)]
pub struct RemoteAudioCache {
    cache_root: PathBuf,
    cache_dir: PathBuf,
    digest: String,
    legacy_data_path: PathBuf,
    staging: Arc<Mutex<CacheStaging>>,
    ready_path: PathBuf,
    expected_content_length: Option<u64>,
    expected_duration_ms: Option<u64>,
    max_cache_bytes: u64,
    published_path: Arc<Mutex<Option<PathBuf>>>,
    bypass_ready: Arc<AtomicBool>,
}

struct CacheStaging {
    path: Option<PathBuf>,
}

impl Drop for CacheStaging {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
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
    prefetch_window: PrefetchWindow,
    cache: Mutex<Vec<CachedSegment>>,
    disk_ranges: Mutex<Vec<CachedRange>>,
    in_flight: Mutex<HashSet<u64>>,
    last_read_pos: Mutex<u64>,
    disk_cache: Option<RemoteAudioCache>,
    cache_finalize_started: AtomicBool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrefetchWindow {
    low_water_bytes: u64,
    target_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrefetchPlan {
    start: u64,
    target_end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CacheMarker {
    Legacy {
        content_length: u64,
    },
    Validated {
        content_length: u64,
        sha256: String,
        file_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HttpByteRange {
    start: u64,
    end: u64,
    total: u64,
}

#[derive(Clone)]
struct ValidatedCacheFile {
    content_length: u64,
    sha256: String,
}

#[derive(Clone)]
struct ValidatedCacheStamp {
    content_length: u64,
    modified: SystemTime,
    sha256: String,
}

static VALIDATED_AUDIO_CACHE: OnceLock<Mutex<HashMap<PathBuf, ValidatedCacheStamp>>> =
    OnceLock::new();

impl RemoteAudioSource {
    pub async fn open(
        client: reqwest::Client,
        url: String,
        referer: String,
        disk_cache: Option<RemoteAudioCache>,
        duration_hint_ms: u64,
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

        let initial_disk_segment = initial_segment
            .as_ref()
            .map(|segment| (segment.start, segment.data.clone()));
        let cache = initial_segment.into_iter().collect();
        let prefetch_window = prefetch_window(total_len, duration_hint_ms);
        let inner = Arc::new(RemoteAudioInner {
            client,
            url,
            referer,
            total_len,
            prefetch_window,
            cache: Mutex::new(cache),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            last_read_pos: Mutex::new(0),
            disk_cache,
            cache_finalize_started: AtomicBool::new(false),
        });
        if let Some((start, data)) = initial_disk_segment {
            write_disk_range(&inner, start, &data);
        }
        replenish_prefetch_window(&inner, 0);

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
            .map_err(|_| io::Error::other("remote cache lock poisoned"))?;
        cache.push(CachedSegment { start, data });
        cache.sort_by_key(|segment| segment.start);
        trim_cache(&mut cache, cache_center(&self.inner, self.pos));
        drop(cache);
        replenish_prefetch_window(&self.inner, self.pos);
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
                replenish_prefetch_window(&self.inner, self.pos);
                continue;
            }
            let read = self.read_disk_cached(&mut out[total_read..]);
            if read > 0 {
                self.pos += read as u64;
                total_read += read;
                remember_read_pos(&self.inner, self.pos);
                replenish_prefetch_window(&self.inner, self.pos);
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
        replenish_prefetch_window(&self.inner, self.pos);
        Ok(self.pos)
    }
}

impl RemoteAudioCache {
    pub fn new(
        root: PathBuf,
        cache_key: &str,
        max_cache_bytes: u64,
        expected_content_length: Option<u64>,
        expected_duration_ms: u64,
    ) -> AppResult<Self> {
        let digest = hex::encode(Sha256::digest(cache_key.as_bytes()));
        let shard = digest.get(0..2).unwrap_or("00");
        let dir = root.join(shard);
        std::fs::create_dir_all(&dir).map_err(|err| AppError::Other(err.to_string()))?;
        Ok(Self {
            cache_root: root,
            cache_dir: dir.clone(),
            digest: digest.clone(),
            legacy_data_path: dir.join(format!("{}.audio", digest)),
            staging: Arc::new(Mutex::new(CacheStaging { path: None })),
            ready_path: dir.join(format!("{}.ready", digest)),
            expected_content_length: expected_content_length.filter(|length| *length > 0),
            expected_duration_ms: (expected_duration_ms > 0).then_some(expected_duration_ms),
            max_cache_bytes,
            published_path: Arc::new(Mutex::new(None)),
            bypass_ready: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn ready_path(&self) -> Option<PathBuf> {
        if self.bypass_ready.load(Ordering::Acquire) {
            return None;
        }

        let marker_text = std::fs::read_to_string(&self.ready_path).ok()?;
        let marker = parse_cache_marker(marker_text.trim())?;
        let (path, marked_length, expected_sha256, needs_upgrade) = match &marker {
            CacheMarker::Legacy { content_length } => (
                self.legacy_data_path.clone(),
                *content_length,
                None,
                true,
            ),
            CacheMarker::Validated {
                content_length,
                sha256,
                file_name,
            } => (
                self.resolve_marker_file(file_name)?,
                *content_length,
                Some(sha256.as_str()),
                false,
            ),
        };

        let validated = match validate_cache_file(
            &path,
            marked_length,
            self.expected_content_length,
            self.expected_duration_ms,
            expected_sha256,
        ) {
            Ok(validated) => validated,
            Err(err) => {
                eprintln!(
                    "[remote-audio] ignoring invalid cache {}: {}",
                    path.display(),
                    err
                );
                return None;
            }
        };

        if needs_upgrade {
            let file_name = path.file_name().and_then(|value| value.to_str())?;
            let upgraded = format_cache_marker(
                validated.content_length,
                &validated.sha256,
                file_name,
            );
            if let Err(err) = atomic_write_cache_marker(&self.ready_path, &upgraded) {
                eprintln!(
                    "[remote-audio] legacy cache marker upgrade failed for {}: {}",
                    path.display(),
                    err
                );
            }
        }

        if let Ok(mut published) = self.published_path.lock() {
            *published = Some(path.clone());
        }
        Some(path)
    }

    pub fn bypass_ready_for_session(&self) {
        self.bypass_ready.store(true, Ordering::Release);
        if let Ok(mut published) = self.published_path.lock() {
            *published = None;
        }
    }

    pub fn fresh_staging(&self) -> AppResult<Self> {
        Ok(Self {
            cache_root: self.cache_root.clone(),
            cache_dir: self.cache_dir.clone(),
            digest: self.digest.clone(),
            legacy_data_path: self.legacy_data_path.clone(),
            staging: Arc::new(Mutex::new(CacheStaging { path: None })),
            ready_path: self.ready_path.clone(),
            expected_content_length: self.expected_content_length,
            expected_duration_ms: self.expected_duration_ms,
            max_cache_bytes: self.max_cache_bytes,
            published_path: Arc::new(Mutex::new(None)),
            bypass_ready: Arc::new(AtomicBool::new(true)),
        })
    }

    fn staging_path(&self) -> io::Result<PathBuf> {
        let mut staging = self
            .staging
            .lock()
            .map_err(|_| io::Error::other("cache staging lock poisoned"))?;
        if let Some(path) = &staging.path {
            return Ok(path.clone());
        }

        let path = create_cache_staging(&self.cache_dir, &self.digest)?;
        staging.path = Some(path.clone());
        Ok(path)
    }

    pub fn publish_complete_bytes(&self, bytes: &[u8]) -> AppResult<PathBuf> {
        self.prepare(bytes.len() as u64)
            .map_err(|err| AppError::Other(err.to_string()))?;
        self.write_range(0, bytes)
            .map_err(|err| AppError::Other(err.to_string()))?;
        self.mark_ready()
            .map_err(|err| AppError::Other(err.to_string()))?;
        self.published_path()
            .map_err(|err| AppError::Other(err.to_string()))?
            .ok_or_else(|| AppError::Other("cache publish produced no readable file".into()))
    }

    fn prepare(&self, total_len: u64) -> io::Result<()> {
        if self
            .expected_content_length
            .is_some_and(|expected| expected != total_len)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "remote content length {} does not match expected {}",
                    total_len,
                    self.expected_content_length.unwrap_or_default()
                ),
            ));
        }
        if let Some(path) = self.ready_path() {
            if std::fs::metadata(&path)?.len() == total_len {
                return Ok(());
            }
        }
        prune_disk_cache(
            &self.cache_root,
            self.max_cache_bytes.saturating_sub(total_len),
            &self.digest,
        );
        if let Ok(mut published) = self.published_path.lock() {
            *published = None;
        }
        let staging_path = self.staging_path()?;
        if let Some(parent) = staging_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(staging_path)?;
        file.set_len(total_len)?;
        Ok(())
    }

    fn write_range(&self, start: u64, data: &[u8]) -> io::Result<()> {
        if data.is_empty() || self.published_path().ok().flatten().is_some() {
            return Ok(());
        }
        let end = start
            .checked_add(data.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache range overflow"))?;
        let staging_path = self.staging_path()?;
        let staging_len = std::fs::metadata(&staging_path)?.len();
        if end > staging_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cache range {}..{} exceeds {}", start, end, staging_len),
            ));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(staging_path)?;
        file.seek(SeekFrom::Start(start))?;
        file.write_all(data)?;
        Ok(())
    }

    fn read_cached(&self, start: u64, out: &mut [u8]) -> io::Result<usize> {
        let published = self.published_path().ok().flatten();
        let path = match &published {
            Some(path) => path.clone(),
            None => self.staging_path()?,
        };
        let mut file = match std::fs::OpenOptions::new().read(true).open(path) {
            Ok(file) => file,
            Err(staging_error) if published.is_none() => {
                let Some(published_path) = self.published_path().ok().flatten() else {
                    return Err(staging_error);
                };
                std::fs::OpenOptions::new()
                    .read(true)
                    .open(published_path)?
            }
            Err(err) => return Err(err),
        };
        file.seek(SeekFrom::Start(start))?;
        file.read(out)
    }

    fn mark_ready(&self) -> io::Result<()> {
        if self.ready_path().is_some() {
            return Ok(());
        }
        let staging_path = self.staging_path()?;
        let validated = validate_cache_file(
            &staging_path,
            std::fs::metadata(&staging_path)?.len(),
            self.expected_content_length,
            self.expected_duration_ms,
            None,
        )
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        File::open(&staging_path)?.sync_all()?;

        let staging_stem = staging_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(&self.digest);
        let final_file_name = format!("{}.{}.audio", staging_stem, validated.sha256);
        let final_path = self.cache_dir.join(&final_file_name);
        if final_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("cache publish target already exists: {}", final_path.display()),
            ));
        }
        std::fs::rename(&staging_path, &final_path)?;
        sync_parent_directory(&final_path)?;
        let final_modified = std::fs::metadata(&final_path)
            .and_then(|metadata| metadata.modified())
            .ok();
        remember_validated_audio_cache(&final_path, final_modified, &validated);
        if let Ok(mut published) = self.published_path.lock() {
            *published = Some(final_path.clone());
        }

        let marker = format_cache_marker(
            validated.content_length,
            &validated.sha256,
            &final_file_name,
        );
        atomic_write_cache_marker(&self.ready_path, &marker)?;
        sync_parent_directory(&self.ready_path)?;
        Ok(())
    }

    fn published_path(&self) -> io::Result<Option<PathBuf>> {
        self.published_path
            .lock()
            .map(|path| path.clone())
            .map_err(|_| io::Error::other("published cache path lock poisoned"))
    }

    fn resolve_marker_file(&self, file_name: &str) -> Option<PathBuf> {
        let path = Path::new(file_name);
        if path.components().count() != 1
            || path.extension().and_then(|value| value.to_str()) != Some("audio")
            || !file_name.starts_with(&self.digest)
        {
            return None;
        }
        Some(self.cache_dir.join(path))
    }
}

fn create_cache_staging(directory: &Path, digest: &str) -> io::Result<PathBuf> {
    let staging_file = tempfile::Builder::new()
        .prefix(&format!("{}.", digest))
        .suffix(".part")
        .tempfile_in(directory)?;
    let (staging_file, staging_path) = staging_file
        .keep()
        .map_err(|err| err.error)?;
    drop(staging_file);
    Ok(staging_path)
}

fn format_cache_marker(content_length: u64, sha256: &str, file_name: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        CACHE_MARKER_VERSION, content_length, sha256, file_name
    )
}

fn parse_cache_marker(marker: &str) -> Option<CacheMarker> {
    if let Some(content_length) = marker
        .strip_prefix("v1:")
        .and_then(|value| value.parse::<u64>().ok())
    {
        return (content_length > 0).then_some(CacheMarker::Legacy { content_length });
    }

    let mut parts = marker.splitn(4, ':');
    if parts.next()? != CACHE_MARKER_VERSION {
        return None;
    }
    let content_length = parts.next()?.parse::<u64>().ok()?;
    let sha256 = parts.next()?.trim().to_ascii_lowercase();
    let file_name = parts.next()?.trim().to_string();
    if content_length == 0
        || sha256.len() != 64
        || !sha256.bytes().all(|value| value.is_ascii_hexdigit())
        || file_name.is_empty()
    {
        return None;
    }
    Some(CacheMarker::Validated {
        content_length,
        sha256,
        file_name,
    })
}

fn validate_cache_file(
    path: &Path,
    marked_length: u64,
    expected_length: Option<u64>,
    expected_duration_ms: Option<u64>,
    expected_sha256: Option<&str>,
) -> Result<ValidatedCacheFile, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("cache metadata unavailable: {}", err))?;
    let content_length = metadata.len();
    if !metadata.is_file() || content_length == 0 || content_length != marked_length {
        return Err(format!(
            "cache length mismatch: marked={}, actual={}",
            marked_length, content_length
        ));
    }
    if expected_length.is_some_and(|expected| expected != content_length) {
        return Err(format!(
            "cache length does not match source: expected={}, actual={}",
            expected_length.unwrap_or_default(),
            content_length
        ));
    }
    let modified = metadata.modified().ok();
    if let Some(validated) = reuse_validated_audio_cache(
        path,
        content_length,
        modified,
        expected_sha256,
    ) {
        return Ok(validated);
    }

    validate_audio_file(path, expected_duration_ms)?;
    let sha256 = sha256_file(path).map_err(|err| format!("cache hash failed: {}", err))?;
    if expected_sha256.is_some_and(|expected| !sha256.eq_ignore_ascii_case(expected)) {
        return Err("cache SHA-256 mismatch".into());
    }
    let validated = ValidatedCacheFile {
        content_length,
        sha256,
    };
    remember_validated_audio_cache(path, modified, &validated);
    Ok(validated)
}

fn reuse_validated_audio_cache(
    path: &Path,
    content_length: u64,
    modified: Option<SystemTime>,
    expected_sha256: Option<&str>,
) -> Option<ValidatedCacheFile> {
    let modified = modified?;
    let expected_sha256 = expected_sha256?;
    let cache = VALIDATED_AUDIO_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache = cache.lock().ok()?;
    let stamp = cache.get(path)?;
    if stamp.content_length != content_length
        || stamp.modified != modified
        || !stamp.sha256.eq_ignore_ascii_case(expected_sha256)
    {
        return None;
    }
    Some(ValidatedCacheFile {
        content_length,
        sha256: stamp.sha256.clone(),
    })
}

fn remember_validated_audio_cache(
    path: &Path,
    modified: Option<SystemTime>,
    validated: &ValidatedCacheFile,
) {
    let Some(modified) = modified else {
        return;
    };
    let cache = VALIDATED_AUDIO_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut cache) = cache.lock() else {
        return;
    };
    if cache.len() >= MAX_VALIDATED_CACHE_STAMPS && !cache.contains_key(path) {
        cache.clear();
    }
    cache.insert(
        path.to_path_buf(),
        ValidatedCacheStamp {
            content_length: validated.content_length,
            modified,
            sha256: validated.sha256.clone(),
        },
    );
}

fn validate_audio_file(path: &Path, expected_duration_ms: Option<u64>) -> Result<(), String> {
    let mut decoder = SymphoniaAudioDecoder::new_file(path)?;
    let actual_duration_ms = decoder
        .total_duration()
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    if cache_duration_is_suspicious(expected_duration_ms, actual_duration_ms) {
        return Err(format!(
            "cache duration is suspiciously short: expected={}ms, actual={}ms",
            expected_duration_ms.unwrap_or_default(),
            actual_duration_ms
        ));
    }
    if decoder.next().is_none() {
        return Err("cache contains no decodable audio frame".into());
    }
    Ok(())
}

fn cache_duration_is_suspicious(expected_duration_ms: Option<u64>, actual_duration_ms: u64) -> bool {
    let Some(expected_duration_ms) = expected_duration_ms.filter(|duration| *duration > 0) else {
        return false;
    };
    actual_duration_ms > 0
        && actual_duration_ms.saturating_add(CACHE_MIN_DURATION_GAP_MS) < expected_duration_ms
        && u128::from(actual_duration_ms) * 100
            < u128::from(expected_duration_ms) * CACHE_MIN_DURATION_RATIO_PERCENT as u128
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn atomic_write_cache_marker(path: &Path, marker: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache marker has no parent directory"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".ready-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temporary.write_all(marker.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|err| err.error)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache file has no parent directory"))?;
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn prune_disk_cache(root: &PathBuf, max_cache_bytes: u64, keep_digest: &str) {
    if !root.exists() {
        return;
    }

    let mut groups: HashMap<String, Vec<(PathBuf, u64, std::time::SystemTime)>> = HashMap::new();
    collect_disk_cache_files(root, &mut groups, keep_digest);
    let mut total = groups
        .values()
        .flat_map(|files| files.iter())
        .map(|(_, size, _)| *size)
        .sum::<u64>();
    if total <= max_cache_bytes {
        return;
    }

    let mut ordered = groups.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(_, files)| {
        files
            .iter()
            .map(|(_, _, modified)| *modified)
            .min()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    for (_, files) in ordered {
        if total <= max_cache_bytes {
            break;
        }
        for (path, size, _) in files {
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

fn collect_disk_cache_files(
    root: &PathBuf,
    groups: &mut HashMap<String, Vec<(PathBuf, u64, std::time::SystemTime)>>,
    keep_digest: &str,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_disk_cache_files(&path, groups, keep_digest);
            continue;
        }
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !matches!(extension, "audio" | "part" | "ready") {
            continue;
        }
        let metadata = entry.metadata().ok();
        if extension == "part"
            && metadata
                .as_ref()
                .and_then(|value| value.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .is_none_or(|age| age < STALE_CACHE_PART_AGE)
        {
            continue;
        }
        let Some(digest) = cache_file_digest(&path) else {
            continue;
        };
        if digest == keep_digest {
            continue;
        }
        groups.entry(digest.to_string()).or_default().push((
            path,
            metadata.as_ref().map(|value| value.len()).unwrap_or(0),
            metadata
                .and_then(|value| value.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        ));
    }
}

fn cache_file_digest(path: &Path) -> Option<&str> {
    let file_name = path.file_name()?.to_str()?;
    let digest = file_name.get(0..64)?;
    (digest.bytes().all(|value| value.is_ascii_hexdigit())).then_some(digest)
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

    pub fn new_file(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|err| format!("Cannot open file: {}", err))?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !matches!(*value, "audio" | "part"));
        Self::new(Box::new(file), extension)
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
        let total_duration = track_duration(
            track.codec_params.time_base,
            track.codec_params.n_frames,
        );

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
        let time = seek_target_time(self.total_duration, pos);

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
        .timeout(REMOTE_REQUEST_TIMEOUT)
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
        .timeout(REMOTE_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(AppError::Network)?;

    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(AppError::Audio(format!(
            "Remote source does not support HTTP Range: {}",
            response.status()
        )));
    }

    let content_range = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(parse_content_range)
        .ok_or_else(|| {
            AppError::Audio("Remote Range response did not include a valid byte range".into())
        })?;
    let data = response
        .bytes()
        .await
        .map_err(AppError::Network)?
        .to_vec();
    validate_http_range_data(content_range, 0, range_end, &data)
        .map_err(|err| AppError::Audio(err.to_string()))?;

    Ok((content_range.total, data))
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
            .timeout(REMOTE_REQUEST_TIMEOUT)
            .send()
            .await?;
        let status = response.status();
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(parse_content_range);
        let bytes = response.bytes().await?;
        Ok::<_, reqwest::Error>((status, content_range, bytes.to_vec()))
    }
    .await
    .map_err(|err| io::Error::other(err.to_string()))?;

    let (status, content_range, data) = result;
    if status == StatusCode::PARTIAL_CONTENT {
        let content_range = content_range.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "partial response omitted Content-Range",
            )
        })?;
        if content_range.total != inner.total_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "remote length changed from {} to {}",
                    inner.total_len, content_range.total
                ),
            ));
        }
        validate_http_range_data(content_range, start, end, &data)?;
        write_disk_range(inner, start, &data);
        return Ok(data);
    }

    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok(Vec::new());
    }

    Err(io::Error::other(format!(
        "unexpected remote range status: {}",
        status
    )))
}

fn prefetch_window(total_len: u64, duration_hint_ms: u64) -> PrefetchWindow {
    let target_bytes = estimated_buffer_bytes(
        total_len,
        duration_hint_ms,
        REMOTE_PREFETCH_TARGET_MS,
        REMOTE_PREFETCH_FALLBACK_TARGET_BYTES,
        REMOTE_PREFETCH_MIN_TARGET_BYTES,
        REMOTE_PREFETCH_MAX_TARGET_BYTES,
    );
    let low_water_bytes = estimated_buffer_bytes(
        total_len,
        duration_hint_ms,
        REMOTE_PREFETCH_LOW_WATER_MS,
        REMOTE_PREFETCH_FALLBACK_LOW_WATER_BYTES,
        REMOTE_PREFETCH_MIN_LOW_WATER_BYTES,
        REMOTE_PREFETCH_MAX_LOW_WATER_BYTES,
    )
    .min((target_bytes / 2).max(1));

    PrefetchWindow {
        low_water_bytes,
        target_bytes,
    }
}

fn estimated_buffer_bytes(
    total_len: u64,
    duration_hint_ms: u64,
    buffer_duration_ms: u64,
    fallback_bytes: u64,
    min_bytes: u64,
    max_bytes: u64,
) -> u64 {
    if total_len == 0 {
        return 0;
    }
    if duration_hint_ms == 0 {
        return fallback_bytes.clamp(min_bytes, max_bytes).min(total_len);
    }

    let estimated = (u128::from(total_len) * u128::from(buffer_duration_ms))
        .div_ceil(u128::from(duration_hint_ms))
        .min(u128::from(u64::MAX)) as u64;
    estimated.clamp(min_bytes, max_bytes).min(total_len)
}

fn prefetch_plan(
    read_pos: u64,
    cached_end: u64,
    total_len: u64,
    window: PrefetchWindow,
) -> Option<PrefetchPlan> {
    let read_pos = read_pos.min(total_len);
    if read_pos >= total_len {
        return None;
    }

    let cached_end = cached_end.clamp(read_pos, total_len);
    let buffered_bytes = cached_end.saturating_sub(read_pos);
    if buffered_bytes > window.low_water_bytes {
        return None;
    }

    let target_end = read_pos.saturating_add(window.target_bytes).min(total_len);
    (cached_end < target_end).then_some(PrefetchPlan {
        start: cached_end,
        target_end,
    })
}

fn replenish_prefetch_window(inner: &Arc<RemoteAudioInner>, read_pos: u64) {
    let cached_end = contiguous_cached_end(inner, read_pos);
    if cached_end >= inner.total_len {
        try_mark_disk_cache_ready(inner);
        return;
    }

    let Some(plan) = prefetch_plan(read_pos, cached_end, inner.total_len, inner.prefetch_window)
    else {
        return;
    };
    spawn_prefetch_sequence(inner.clone(), plan);
}

fn spawn_prefetch_sequence(inner: Arc<RemoteAudioInner>, plan: PrefetchPlan) {
    tauri::async_runtime::spawn(async move {
        let mut next_start = plan.start.min(inner.total_len);
        while next_start < inner.total_len && next_start < plan.target_end {
            let cached_end = contiguous_cached_end(&inner, next_start);
            if cached_end > next_start {
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
                .min(plan.target_end.saturating_sub(1))
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
        if next_start >= inner.total_len {
            try_mark_disk_cache_ready(&inner);
        }
    });
}

fn contiguous_cached_end(inner: &RemoteAudioInner, pos: u64) -> u64 {
    let mut ranges = Vec::new();
    if let Ok(cache) = inner.cache.lock() {
        ranges.extend(cache.iter().map(|segment| CachedRange {
            start: segment.start,
            end: segment.start.saturating_add(segment.data.len() as u64),
        }));
    }
    if let Ok(disk_ranges) = inner.disk_ranges.lock() {
        ranges.extend(disk_ranges.iter().copied());
    }
    ranges.sort_by_key(|range| range.start);

    let mut end = pos.min(inner.total_len);
    for range in ranges {
        if range.end <= end {
            continue;
        }
        if range.start > end {
            break;
        }
        end = range.end.min(inner.total_len);
    }
    end
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

fn try_mark_disk_cache_ready(inner: &RemoteAudioInner) {
    let Some(cache) = &inner.disk_cache else {
        return;
    };
    let is_complete = inner
        .disk_ranges
        .lock()
        .map(|ranges| ranges_cover_source(&ranges, inner.total_len))
        .unwrap_or(false);
    if !is_complete
        || inner
            .cache_finalize_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }

    let cache = cache.clone();
    if let Err(spawn_error) = std::thread::Builder::new()
        .name("audio-cache-finalize".into())
        .spawn(move || {
            if let Err(err) = cache.mark_ready() {
                eprintln!("[remote-audio] cache finalize rejected: {}", err);
            }
        })
    {
        eprintln!(
            "[remote-audio] cache finalize worker unavailable: {}",
            spawn_error
        );
    }
}

fn ranges_cover_source(ranges: &[CachedRange], total_len: u64) -> bool {
    if total_len == 0 {
        return false;
    }

    let mut covered_end = 0u64;
    for range in ranges {
        if range.start > covered_end {
            return false;
        }
        covered_end = covered_end.max(range.end);
        if covered_end >= total_len {
            return true;
        }
    }
    false
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
    if contiguous_cached_end(inner, start) > start {
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

fn parse_content_range(value: &reqwest::header::HeaderValue) -> Option<HttpByteRange> {
    let text = value.to_str().ok()?;
    let value = text.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    if total == "*" || range == "*" {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    let parsed = HttpByteRange {
        start: start.parse::<u64>().ok()?,
        end: end.parse::<u64>().ok()?,
        total: total.parse::<u64>().ok()?,
    };
    (parsed.start <= parsed.end && parsed.end < parsed.total).then_some(parsed)
}

fn validate_http_range_data(
    actual: HttpByteRange,
    requested_start: u64,
    requested_end: u64,
    data: &[u8],
) -> io::Result<()> {
    if actual.start != requested_start || actual.end > requested_end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unexpected Content-Range {}-{}/{} for request {}-{}",
                actual.start, actual.end, actual.total, requested_start, requested_end
            ),
        ));
    }
    let expected_len = actual
        .end
        .checked_sub(actual.start)
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid byte range"))?;
    if data.len() as u64 != expected_len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "range body length {} does not match Content-Range length {}",
                data.len(),
                expected_len
            ),
        ));
    }
    Ok(())
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
    } else {
        current_pos.saturating_sub(segment_end)
    }
}

fn time_to_duration(time: Time) -> Duration {
    Duration::from_secs(time.seconds)
        + Duration::from_nanos((time.frac * 1_000_000_000.0).max(0.0) as u64)
}

fn track_duration(time_base: Option<TimeBase>, n_frames: Option<u64>) -> Option<Time> {
    let frames = n_frames.filter(|frames| *frames > 0)?;
    time_base.map(|base| base.calc_time(frames))
}

fn seek_target_time(total_duration: Option<Time>, position: Duration) -> Time {
    match total_duration.filter(|duration| !time_to_duration(*duration).is_zero()) {
        Some(duration)
            if time_to_duration(duration)
                .saturating_sub(position)
                .as_millis()
                < 1 =>
        {
            skip_back_tiny_bit(duration)
        }
        _ => position.as_secs_f64().into(),
    }
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
    RodioSeekError::Other(Box::new(io::Error::other(message)))
}

#[cfg(test)]
mod tests {
    use super::{
        cache_duration_is_suspicious, format_cache_marker, parse_cache_marker,
        parse_content_range, prefetch_plan, prefetch_window, ranges_cover_source,
        remember_validated_audio_cache, reuse_validated_audio_cache, seek_target_time,
        track_duration, CacheMarker, CachedRange, HttpByteRange, PrefetchPlan, PrefetchWindow,
        RemoteAudioCache, ValidatedCacheFile,
        REMOTE_PREFETCH_FALLBACK_LOW_WATER_BYTES, REMOTE_PREFETCH_FALLBACK_TARGET_BYTES,
    };
    use reqwest::header::HeaderValue;
    use std::path::PathBuf;
    use std::time::Duration;
    use symphonia::core::units::{Time, TimeBase};

    fn pcm_wav(sample_count: u32) -> Vec<u8> {
        let data_len = sample_count * 2;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8_000u32.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.resize(44 + data_len as usize, 0);
        wav
    }

    #[test]
    fn prefetch_window_uses_duration_hint_for_time_based_buffering() {
        let window = prefetch_window(9_000_000, 180_000);

        assert_eq!(window.low_water_bytes, 250_000);
        assert_eq!(window.target_bytes, 750_000);
    }

    #[test]
    fn prefetch_window_falls_back_when_duration_is_unknown() {
        let window = prefetch_window(10_000_000, 0);

        assert_eq!(
            window,
            PrefetchWindow {
                low_water_bytes: REMOTE_PREFETCH_FALLBACK_LOW_WATER_BYTES,
                target_bytes: REMOTE_PREFETCH_FALLBACK_TARGET_BYTES,
            }
        );
    }

    #[test]
    fn prefetch_plan_refills_only_after_reaching_low_watermark() {
        let window = PrefetchWindow {
            low_water_bytes: 250_000,
            target_bytes: 750_000,
        };

        assert_eq!(prefetch_plan(1_000_000, 1_250_001, 5_000_000, window), None);
        assert_eq!(
            prefetch_plan(1_000_000, 1_250_000, 5_000_000, window),
            Some(PrefetchPlan {
                start: 1_250_000,
                target_end: 1_750_000,
            })
        );
    }

    #[test]
    fn prefetch_plan_never_reads_past_source_end() {
        let window = PrefetchWindow {
            low_water_bytes: 250_000,
            target_bytes: 750_000,
        };

        assert_eq!(
            prefetch_plan(4_800_000, 4_900_000, 5_000_000, window),
            Some(PrefetchPlan {
                start: 4_900_000,
                target_end: 5_000_000,
            })
        );
    }

    #[test]
    fn disk_cache_is_ready_only_when_ranges_cover_entire_source() {
        assert!(!ranges_cover_source(
            &[
                CachedRange { start: 0, end: 512 },
                CachedRange {
                    start: 768,
                    end: 1_024,
                },
            ],
            1_024,
        ));
        assert!(ranges_cover_source(
            &[
                CachedRange { start: 0, end: 512 },
                CachedRange {
                    start: 512,
                    end: 1_024,
                },
            ],
            1_024,
        ));
    }

    #[test]
    fn cache_marker_requires_hash_length_and_file_name() {
        let hash = "ab".repeat(32);
        let marker = format_cache_marker(2_048, &hash, "digest.audio");

        assert_eq!(
            parse_cache_marker(&marker),
            Some(CacheMarker::Validated {
                content_length: 2_048,
                sha256: hash,
                file_name: "digest.audio".into(),
            })
        );
        assert_eq!(
            parse_cache_marker("v1:1024"),
            Some(CacheMarker::Legacy {
                content_length: 1_024,
            })
        );
        assert_eq!(parse_cache_marker("v1:0"), None);
        assert_eq!(parse_cache_marker("v2:1024:bad:digest.audio"), None);
    }

    #[test]
    fn content_range_parser_rejects_wildcards_and_invalid_bounds() {
        assert_eq!(
            parse_content_range(&HeaderValue::from_static("bytes 0-511/1024")),
            Some(HttpByteRange {
                start: 0,
                end: 511,
                total: 1_024,
            })
        );
        assert_eq!(
            parse_content_range(&HeaderValue::from_static("bytes */1024")),
            None
        );
        assert_eq!(
            parse_content_range(&HeaderValue::from_static("bytes 512-511/1024")),
            None
        );
    }

    #[test]
    fn duration_guard_only_rejects_materially_short_media() {
        assert!(!cache_duration_is_suspicious(Some(272_000), 271_530));
        assert!(!cache_duration_is_suspicious(Some(272_000), 0));
        assert!(cache_duration_is_suspicious(Some(272_000), 30_000));
    }

    #[test]
    fn zero_frame_track_duration_is_treated_as_unknown() {
        assert_eq!(
            track_duration(Some(TimeBase::new(1, 96_000)), Some(0)),
            None
        );
    }

    #[test]
    fn unknown_zero_duration_does_not_clamp_seek_to_track_start() {
        let resolved = seek_target_time(
            Some(Time::new(0, 0.0)),
            Duration::from_millis(22_720),
        );

        assert_eq!(resolved.seconds, 22);
        assert!((resolved.frac - 0.72).abs() < 0.000_001);
    }

    #[test]
    fn known_duration_still_clamps_seek_just_before_track_end() {
        let resolved = seek_target_time(
            Some(Time::new(272, 0.0)),
            Duration::from_millis(300_000),
        );

        assert_eq!(resolved.seconds, 271);
        assert!((resolved.frac - 0.9999).abs() < 0.000_001);
    }

    #[test]
    fn cache_publish_requires_decodable_complete_staging_file() {
        let root = tempfile::tempdir().expect("temp cache root");
        let wav = pcm_wav(800);
        let cache = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "valid-audio",
            1024 * 1024,
            Some(wav.len() as u64),
            100,
        )
        .expect("cache");

        cache.prepare(wav.len() as u64).expect("prepare");
        cache.write_range(0, &wav).expect("write staging");
        let staging_path = cache.staging_path().expect("staging path");
        assert!(!cache.ready_path.exists());

        cache.mark_ready().expect("publish validated cache");
        let published = cache.ready_path().expect("ready cache");
        assert_eq!(std::fs::read(published).expect("published bytes"), wav);
        assert!(!staging_path.exists());
    }

    #[test]
    fn invalid_media_never_creates_ready_marker() {
        let root = tempfile::tempdir().expect("temp cache root");
        let invalid = b"<html>not audio</html>";
        let cache = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "invalid-audio",
            1024 * 1024,
            Some(invalid.len() as u64),
            0,
        )
        .expect("cache");

        cache.prepare(invalid.len() as u64).expect("prepare");
        cache.write_range(0, invalid).expect("write staging");

        assert!(cache.mark_ready().is_err());
        assert!(!cache.ready_path.exists());
        assert!(cache.ready_path().is_none());
    }

    #[test]
    fn concurrent_cache_instances_never_share_staging_files() {
        let root = tempfile::tempdir().expect("temp cache root");
        let first = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "same-key",
            1024 * 1024,
            None,
            0,
        )
        .expect("first cache");
        let second = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "same-key",
            1024 * 1024,
            None,
            0,
        )
        .expect("second cache");

        assert_ne!(
            first.staging_path().expect("first staging"),
            second.staging_path().expect("second staging")
        );
    }

    #[test]
    fn complete_fallback_download_publishes_through_fresh_staging() {
        let root = tempfile::tempdir().expect("temp cache root");
        let wav = pcm_wav(800);
        let initial = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "fallback-audio",
            1024 * 1024,
            Some(wav.len() as u64),
            100,
        )
        .expect("initial cache");
        let fallback = initial.fresh_staging().expect("fresh staging");
        let initial_staging = initial.staging_path().expect("initial staging");

        let published = fallback
            .publish_complete_bytes(&wav)
            .expect("publish fallback bytes");
        let fallback_staging = fallback.staging_path().expect("fallback staging");

        assert_ne!(initial_staging, fallback_staging);
        assert_eq!(std::fs::read(published).expect("published bytes"), wav);
        assert!(fallback.ready_path().is_none());
        fallback
            .bypass_ready
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(fallback.ready_path().is_some());
    }

    #[test]
    fn cache_hit_does_not_create_unused_staging_file() {
        let root = tempfile::tempdir().expect("temp cache root");
        let wav = pcm_wav(800);
        let writer = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "cache-hit",
            1024 * 1024,
            Some(wav.len() as u64),
            100,
        )
        .expect("writer cache");
        writer
            .publish_complete_bytes(&wav)
            .expect("publish cached audio");
        drop(writer);

        let hit = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "cache-hit",
            1024 * 1024,
            Some(wav.len() as u64),
            100,
        )
        .expect("cache hit");

        assert!(hit.ready_path().is_some());
        assert!(hit.staging.lock().expect("staging lock").path.is_none());
    }

    #[test]
    fn cache_lookup_defers_capacity_scan_until_a_write_is_prepared() {
        let root = tempfile::tempdir().expect("temp cache root");
        let wav = pcm_wav(800);
        let existing = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "existing-audio",
            1024 * 1024,
            Some(wav.len() as u64),
            100,
        )
        .expect("existing cache");
        let existing_path = existing
            .publish_complete_bytes(&wav)
            .expect("publish existing audio");

        let lookup = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "new-audio",
            1,
            None,
            0,
        )
        .expect("new cache lookup");
        assert!(existing_path.exists());

        lookup.prepare(1).expect("prepare cache miss");
        assert!(!existing_path.exists());
    }

    #[test]
    fn validated_cache_stamp_is_reused_only_for_unchanged_file_metadata() {
        let path = PathBuf::from("validated-cache-stamp-test.audio");
        let modified = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(42);
        let sha256 = "ab".repeat(32);
        let validated = ValidatedCacheFile {
            content_length: 1_024,
            sha256: sha256.clone(),
        };
        remember_validated_audio_cache(&path, Some(modified), &validated);

        assert!(reuse_validated_audio_cache(
            &path,
            1_024,
            Some(modified),
            Some(&sha256),
        )
        .is_some());
        assert!(reuse_validated_audio_cache(
            &path,
            1_025,
            Some(modified),
            Some(&sha256),
        )
        .is_none());
        assert!(reuse_validated_audio_cache(
            &path,
            1_024,
            Some(modified + Duration::from_secs(1)),
            Some(&sha256),
        )
        .is_none());
    }
}
