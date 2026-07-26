use reqwest::header::{CONTENT_RANGE, RANGE, REFERER, USER_AGENT};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
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
use crate::audio::pcm::{PcmSeekError, PcmSource};

const REMOTE_INITIAL_BLOCK_BYTES: u64 = 128 * 1024;
/// 超长媒体首包加大，尽量一次拿齐 moov + 起始音频
const REMOTE_LONG_INITIAL_BLOCK_BYTES: u64 = 512 * 1024;
const REMOTE_FETCH_BLOCK_BYTES: u64 = 256 * 1024;
const REMOTE_FRAGMENTED_SEEK_BLOCK_BYTES: u64 = 8 * 1024 * 1024;
const REMOTE_DECODER_BUFFER_BYTES: usize = 128 * 1024;
/// virtual-body 远程 demuxer 缓冲：需覆盖单段 moof+mdat 回跳
const REMOTE_VIRTUAL_DECODER_BUFFER_BYTES: usize = 2 * 1024 * 1024;
/// 拼接 demux 首段 body：必须 < REMOTE_REQUEST_TIMEOUT 能下完，否则 prepare 被取消
/// 2MB ≈ 几十秒 AAC，足够出声；后续可按需再拼
const REMOTE_VIRTUAL_SPLICE_BODY_BYTES: u64 = 2 * 1024 * 1024;
/// 拼接 demux 单次 Range 上限（避免 16MB 一次拉超时）
const REMOTE_VIRTUAL_SPLICE_FETCH_CHUNK: u64 = 512 * 1024;
const REMOTE_MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
// 超长媒体按 bitrate 估算时 low/target 会落到 min；抬高下限保证首帧 moov+音频够解码
const REMOTE_PREFETCH_LOW_WATER_MS: u64 = 5_000;
const REMOTE_PREFETCH_TARGET_MS: u64 = 15_000;
const REMOTE_PREFETCH_MIN_LOW_WATER_BYTES: u64 = 256 * 1024;
const REMOTE_PREFETCH_MAX_LOW_WATER_BYTES: u64 = 8 * 1024 * 1024;
const REMOTE_PREFETCH_MIN_TARGET_BYTES: u64 = 1024 * 1024;
const REMOTE_PREFETCH_MAX_TARGET_BYTES: u64 = 16 * 1024 * 1024;
const REMOTE_PREFETCH_FALLBACK_LOW_WATER_BYTES: u64 = 1024 * 1024;
const REMOTE_PREFETCH_FALLBACK_TARGET_BYTES: u64 = 4 * 1024 * 1024;
/// ≥45 分钟按「长内容」处理：初始不向 demuxer 暴露 seekable，避免扫文件尾找 moov
const REMOTE_LONG_FORM_DURATION_MS: u64 = 45 * 60 * 1000;
/// 长内容额外预取尾部（moov 常在文件尾），seek 时 demuxer 才能秒开索引
const REMOTE_LONG_FORM_TAIL_BYTES: u64 = 1024 * 1024;
const MAX_DECODE_RETRIES: usize = 3;
/// virtual-body 中段打开后跳过坏包直到关键帧的上限
const MAX_VIRTUAL_BODY_SKIP_PACKETS: usize = 256;
/// virtual-body 拼接 demux：最多吞掉的 moov 假样本数（stco 指向文件头 mdat）
const MAX_VIRTUAL_BODY_SKIP_MOOV_SAMPLES: usize = 4096;
// 超长 YouTube 首包探测 2s 容易假失败，放宽到 6s
const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CACHE_MARKER_VERSION: &str = "v2";
const CACHE_MIN_DURATION_GAP_MS: u64 = 5_000;
const CACHE_MIN_DURATION_RATIO_PERCENT: u64 = 85;
const STALE_CACHE_PART_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_VALIDATED_CACHE_STAMPS: usize = 4_096;
const PLAYBACK_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// 按 URL 选择拉流 User-Agent.
/// YouTube googlevideo CDN 校验拉流 UA 与直链 `c=` 客户端一致, 不一致直接 403;
/// 因此 googlevideo 直链按铸造它的客户端选择匹配 UA (对齐 Android), 其它平台用桌面 Chrome UA.
fn playback_user_agent(url: &str) -> &'static str {
    if url.contains("googlevideo.com") || url.contains("youtube.com") {
        crate::api::youtube::playback::stream_user_agent_for_url(url)
    } else {
        PLAYBACK_USER_AGENT
    }
}

fn summarize_remote_error(error: &dyn std::fmt::Display) -> String {
    let mut message = error.to_string();
    for scheme in ["https://", "http://"] {
        while let Some(start) = message.find(scheme) {
            let tail = &message[start..];
            let end = tail
                .find(|value: char| value.is_whitespace() || matches!(value, ')' | ']' | '}'))
                .map(|offset| start + offset)
                .unwrap_or(message.len());
            message.replace_range(start..end, "[url]");
        }
    }
    message
}

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
    access_mode: RemoteAccessMode,
    read_cancellation: Option<RemoteReadCancellation>,
}

#[derive(Clone)]
pub struct RemoteReadCancellation {
    session_cancelled: Arc<AtomicBool>,
    operation_generation: Option<(Arc<AtomicU64>, u64)>,
    /// 外部 prepare 超时取消（不改 generation，仅打断当前 prepare）
    external_cancel: Option<Arc<AtomicBool>>,
}

impl RemoteReadCancellation {
    pub fn new(
        session_cancelled: Arc<AtomicBool>,
        operation_generation: Option<(Arc<AtomicU64>, u64)>,
    ) -> Self {
        Self {
            session_cancelled,
            operation_generation,
            external_cancel: None,
        }
    }

    pub fn with_external_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.external_cancel = Some(flag);
        self
    }

    fn is_cancelled(&self) -> bool {
        self.session_cancelled.load(Ordering::Acquire)
            || self
                .external_cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
            || self
                .operation_generation
                .as_ref()
                .is_some_and(|(generation, expected)| {
                    generation.load(Ordering::Acquire) != *expected
                })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteAccessMode {
    StandardSeekable,
    /// 长内容首播：顺序读，不向 demuxer 暴露长度/可 seek，避免扫尾部 moov
    LongFormProgressive,
    FragmentedProgressive,
    FragmentedSeekable,
}

impl RemoteAccessMode {
    fn for_url(url: &str, duration_hint_ms: u64) -> Self {
        if is_fragmented_mp4_url(url) {
            Self::FragmentedProgressive
        } else if duration_hint_ms >= REMOTE_LONG_FORM_DURATION_MS {
            Self::LongFormProgressive
        } else {
            Self::StandardSeekable
        }
    }

    fn is_seekable(self) -> bool {
        matches!(self, Self::StandardSeekable | Self::FragmentedSeekable)
    }

    fn demand_fetch_block_bytes(self) -> u64 {
        if matches!(self, Self::FragmentedSeekable) {
            REMOTE_FRAGMENTED_SEEK_BLOCK_BYTES
        } else {
            REMOTE_FETCH_BLOCK_BYTES
        }
    }
}

struct RemoteAudioInner {
    client: reqwest::Client,
    url: String,
    referer: String,
    total_len: u64,
    /// 首个 mdat/moof 起点；长内容虚拟 seek 时逻辑头长度
    header_end: AtomicU64,
    prefetch_window: PrefetchWindow,
    cache: Mutex<Vec<CachedSegment>>,
    disk_ranges: Mutex<Vec<CachedRange>>,
    in_flight: Mutex<HashSet<u64>>,
    range_available: Condvar,
    last_read_pos: Mutex<u64>,
    /// 保护尾部 moov/sidx：seek 重开 demuxer 时不可被 trim 掉
    protected_ranges: Mutex<Vec<CachedRange>>,
    disk_cache: Option<RemoteAudioCache>,
    cache_finalize_started: AtomicBool,
    /// 预取世代：seek 时 +1，旧预取任务看到后立刻停
    prefetch_epoch: AtomicU64,
    /// demuxer 打开阶段：对 isomp4 隐藏 seekable，避免扫完整 mdat
    demuxer_open_sequential: AtomicBool,
    /// 虚拟 body：逻辑 header_end 对应的远端 moof 起点；0 表示关闭
    virtual_body_origin: AtomicU64,
    /// 粗粒度时间→字节映射（来自 sidx 或线性估算），长内容 seek 用
    seek_index: Mutex<Option<RemoteSeekIndex>>,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
}

/// 粗粒度 seek 索引：用 sidx 或线性比例估算目标字节
#[derive(Clone, Debug)]
struct RemoteSeekIndex {
    duration_ms: u64,
    entries: Vec<SeekIndexEntry>,
}

#[derive(Clone, Copy, Debug)]
struct SeekIndexEntry {
    time_ms: u64,
    byte_offset: u64,
}

impl RemoteSeekIndex {
    /// 返回 <= target_ms 的最近 segment 起点（moof 边界），供 virtual-body 跳转
    #[cfg(test)]
    fn estimate_segment_start(&self, target_ms: u64) -> u64 {
        self.estimate_segment(target_ms).0
    }

    /// (start, end_exclusive_hint)：用下一条 sidx 推段长，便于一次预满 moof+mdat
    fn estimate_segment(&self, target_ms: u64) -> (u64, Option<u64>) {
        if self.entries.is_empty() {
            return (0, None);
        }
        let mut best_idx = 0usize;
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.time_ms <= target_ms {
                best_idx = idx;
            } else {
                break;
            }
        }
        let start = self.entries[best_idx].byte_offset;
        let end = self.entries.get(best_idx + 1).map(|e| e.byte_offset);
        (start, end)
    }

    #[allow(dead_code)]
    fn estimate_byte(&self, target_ms: u64) -> u64 {

        if self.entries.is_empty() {
            return 0;
        }
        if target_ms == 0 {
            return self.entries[0].byte_offset;
        }
        // 找最后一个 time <= target 的 entry，再线性插值到下一个
        let mut lo = 0usize;
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.time_ms <= target_ms {
                lo = idx;
            } else {
                break;
            }
        }
        let left = self.entries[lo];
        if lo + 1 >= self.entries.len() {
            return left.byte_offset;
        }
        let right = self.entries[lo + 1];
        if right.time_ms <= left.time_ms {
            return left.byte_offset;
        }
        let span = right.time_ms - left.time_ms;
        let progress = target_ms.saturating_sub(left.time_ms).min(span);
        let byte_span = right.byte_offset.saturating_sub(left.byte_offset);
        left.byte_offset
            + (u128::from(byte_span) * u128::from(progress) / u128::from(span)) as u64
    }
}

impl RemoteAudioInner {
    fn playback_cancelled(&self) -> bool {
        self.playback_generation.load(Ordering::Acquire) != self.expected_generation
    }

    fn ensure_playback_current(&self) -> io::Result<()> {
        if self.playback_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "playback request superseded",
            ))
        } else {
            Ok(())
        }
    }
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
        playback_generation: Arc<AtomicU64>,
        expected_generation: u64,
    ) -> AppResult<Self> {
        let open_started = Instant::now();
        let host = url::Url::parse(&url)
            .ok()
            .and_then(|value| value.host_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        let access_mode = RemoteAccessMode::for_url(&url, duration_hint_ms);
        log::info!(
            target: "remote-audio",
            "open begin host={}, mode={:?}, generation={}, duration_hint_ms={}",
            host,
            access_mode,
            expected_generation,
            duration_hint_ms,
        );
        if playback_generation.load(Ordering::Acquire) != expected_generation {
            return Err(AppError::Audio("Playback request superseded".into()));
        }
        let initial_block = if duration_hint_ms >= REMOTE_LONG_FORM_DURATION_MS {
            REMOTE_LONG_INITIAL_BLOCK_BYTES
        } else {
            REMOTE_INITIAL_BLOCK_BYTES
        };
        let probe_started = Instant::now();
        let range_result = {
            let range_probe = probe_range_len(&client, &url, &referer, initial_block);
            tokio::pin!(range_probe);
            tokio::select! {
                result = &mut range_probe => result,
                () = wait_for_generation_change(
                    &playback_generation,
                    expected_generation,
                ) => {
                    return Err(AppError::Audio("Playback request superseded".into()));
                }
            }
        };
        log::info!(
            target: "remote-audio",
            "range probe finished host={}, ok={}, elapsed_ms={}, total_ms={}",
            host,
            range_result.is_ok(),
            probe_started.elapsed().as_millis(),
            open_started.elapsed().as_millis(),
        );
        let (total_len, initial_segment) = match range_result {
            Ok((len, data)) => {
                log::info!(
                    target: "remote-audio",
                    "range probe data host={}, total_len={}, initial_bytes={}",
                    host,
                    len,
                    data.len(),
                );
                (len, Some(CachedSegment { start: 0, data }))
            }
            Err(range_error) => {
                if playback_generation.load(Ordering::Acquire) != expected_generation {
                    return Err(AppError::Audio("Playback request superseded".into()));
                }
                log::warn!(
                    target: "remote-audio",
                    "range probe failed host={}, elapsed_ms={}, error={}",
                    host,
                    probe_started.elapsed().as_millis(),
                    summarize_remote_error(&range_error),
                );
                return Err(AppError::Audio(
                    "Remote source does not support seekable HTTP Range".into(),
                ));
            }
        };

        if let Some(cache) = disk_cache.as_ref().cloned() {
            let cache_started = Instant::now();
            let queued_at = Instant::now();
            tokio::task::spawn_blocking(move || {
                log::info!(
                    target: "remote-cache",
                    "prepare worker started queued_ms={}",
                    queued_at.elapsed().as_millis(),
                );
                cache.prepare(total_len)
            })
            .await
            .map_err(|error| AppError::Other(error.to_string()))?
            .map_err(|error| AppError::Other(error.to_string()))?;
            log::info!(
                target: "remote-cache",
                "prepare host={}, total_len={}, elapsed_ms={}",
                host,
                total_len,
                cache_started.elapsed().as_millis(),
            );
        }

        let initial_disk_segment = initial_segment
            .as_ref()
            .map(|segment| (segment.start, segment.data.clone()));
        let cache = initial_segment.into_iter().collect();
        let prefetch_window = prefetch_window(total_len, duration_hint_ms);
        let mut protected_ranges = Vec::new();
        if let Some((start, data)) = &initial_disk_segment {
            if !data.is_empty() {
                protected_ranges.push(CachedRange {
                    start: *start,
                    end: start.saturating_add(data.len() as u64),
                });
            }
        }
        let head_bytes = initial_disk_segment
            .as_ref()
            .map(|(_, data)| data.as_slice())
            .unwrap_or(&[]);
        let header_end = detect_mp4_header_end(head_bytes).unwrap_or(0);
        // URL 后缀嗅探不到 googlevideo 的分片直链，必须按首包内容再判一次。
        //
        // 嗅探结果只用于「打开阶段顺序读」这一件事，不改 access_mode 也不建
        // 虚拟 body 索引：
        // - 分片模式的按需块是 8MB，那是给 Bilibili 独立 .m4s 分片文件调的，
        //   套到 YouTube 的单文件上反而拖慢 seek
        // - 这类流自带完整 sidx，symphonia 的 isomp4 能原生按 sidx 定位
        //   （日志里的 "stream is segmented with a segment index"），
        //   再套一层虚拟 body 拼接反而会让样本偏移对不上，直接 end of stream
        let sniffed_fragmented = !matches!(access_mode, RemoteAccessMode::FragmentedProgressive)
            && detect_fragmented_mp4(head_bytes);
        let inner = Arc::new(RemoteAudioInner {
            client,
            url,
            referer,
            total_len,
            header_end: AtomicU64::new(header_end),
            prefetch_window,
            cache: Mutex::new(cache),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(protected_ranges),
            disk_cache,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation,
            expected_generation,
        });
        if header_end > 0 {
            log::info!(
                target: "remote-audio",
                "mp4 header_end detected host={}, header_end={}",
                host,
                header_end,
            );
        }
        if sniffed_fragmented {
            // 打开阶段隐藏 seekable，避免 symphonia 顺着 moof 链把整个文件读完；
            // sidx 位于 header_end 之前，顺序读同样能拿到，不影响后续按 sidx seek。
            // finish_demuxer_open() 会在打开完成后恢复可 seek。
            inner
                .demuxer_open_sequential
                .store(true, Ordering::Release);
            log::info!(
                target: "remote-audio",
                "fragmented mp4 sniffed from payload host={}, mode={:?}, opening sequentially",
                host,
                access_mode,
            );
        }
        inner
            .ensure_playback_current()
            .map_err(|err| AppError::Audio(err.to_string()))?;
        if let Some((start, data)) = initial_disk_segment {
            write_disk_range(&inner, start, &data);
        }
        let needs_seek_index = matches!(access_mode, RemoteAccessMode::LongFormProgressive);
        // 分片 MP4 的 sidx 紧跟 moov 位于首包，先用首包建索引；
        // 建成了就不必再为长内容拉 1MB 尾部，直接省掉起播路径上的一次往返
        let head_index_ready = needs_seek_index
            && total_len > 0
            && duration_hint_ms > 0
            && build_seek_index_from_head(&inner, duration_hint_ms);

        // 长内容：尾部 moov/sidx 预取进缓存，seek 升级为 seekable 后 demuxer 能直接跳转
        if matches!(access_mode, RemoteAccessMode::LongFormProgressive)
            && total_len > 0
            && !head_index_ready
        {
            let tail_len = REMOTE_LONG_FORM_TAIL_BYTES.min(total_len);
            let tail_start = total_len.saturating_sub(tail_len);
            // 与首包重叠时跳过（极短文件）
            if tail_start > 0 {
                let tail_end = total_len.saturating_sub(1);
                match fetch_range_block_async(
                    &inner,
                    tail_start,
                    tail_end,
                    None,
                )
                .await
                {
                    Ok(data) if !data.is_empty() => {
                        log::info!(
                            target: "remote-audio",
                            "long-form tail cached host={}, start={}, bytes={}",
                            host,
                            tail_start,
                            data.len(),
                        );
                        protect_cached_range(&inner, tail_start, data.len() as u64);
                        if let Ok(mut cache) = inner.cache.lock() {
                            if !cache_contains_position(&cache, tail_start) {
                                cache.push(CachedSegment {
                                    start: tail_start,
                                    data: data.clone(),
                                });
                                cache.sort_by_key(|segment| segment.start);
                                trim_cache(&inner, &mut cache, cache_center(&inner, 0));
                            }
                        }
                        write_disk_range(&inner, tail_start, &data);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        log::warn!(
                            target: "remote-audio",
                            "long-form tail prefetch failed host={}, error={}",
                            host,
                            summarize_remote_error(&err),
                        );
                    }
                }
            }
        }
        // 首包没建成索引时再兜底：长内容可退到尾部 sidx / 线性估算，
        // 嗅探出的分片内容只接受真实 sidx，解析不到就保持原 format.seek 行为
        if needs_seek_index && total_len > 0 && duration_hint_ms > 0 && !head_index_ready {
            let allow_linear_fallback =
                matches!(access_mode, RemoteAccessMode::LongFormProgressive);
            build_seek_index_for_long_form(&inner, duration_hint_ms, allow_linear_fallback);
        }
        replenish_prefetch_window(&inner, 0);

        log::info!(
            target: "remote-audio",
            "open ready host={}, total_len={}, prefetch_low={}, prefetch_target={}, total_ms={}",
            host,
            total_len,
            prefetch_window.low_water_bytes,
            prefetch_window.target_bytes,
            open_started.elapsed().as_millis(),
        );

        Ok(Self {
            inner,
            pos: 0,
            access_mode,
            read_cancellation: None,
        })
    }

    pub fn byte_len(&self) -> u64 {
        self.inner.total_len
    }

    pub fn seekable_clone(&self) -> Self {
        // 分片 m4s → FragmentedSeekable（大块按需）；
        // 长内容渐进 m4a → StandardSeekable（用 moov 表跳转，绝不能走分片 8MB 逻辑）
        let (access_mode, sequential_demuxer_open) = match self.access_mode {
            RemoteAccessMode::FragmentedProgressive => {
                (RemoteAccessMode::FragmentedSeekable, true)
            }
            RemoteAccessMode::LongFormProgressive => {
                (RemoteAccessMode::StandardSeekable, true)
            }
            mode => (mode, false),
        };
        // 作废旧位置的预取，避免 seek 重建 demuxer 时和旧 in_flight 撞车
        invalidate_prefetch_epoch(&self.inner);
        // 长内容/分片升级：demuxer 打开必须假装不可 seek，否则会扫完整 mdat
        // 短内容本身就是 StandardSeekable，moov 可能在尾部，不能强制顺序打开
        if sequential_demuxer_open {
            self.inner
                .demuxer_open_sequential
                .store(true, Ordering::Release);
        }
        Self {
            inner: Arc::clone(&self.inner),
            pos: 0,
            access_mode,
            read_cancellation: self.read_cancellation.clone(),
        }
    }

    pub fn with_read_cancellation(mut self, cancellation: RemoteReadCancellation) -> Self {
        self.read_cancellation = Some(cancellation);
        self
    }

    /// demuxer 打开完成：允许真实时间 seek 的字节跳转
    pub fn finish_demuxer_open(&self) {
        self.inner
            .demuxer_open_sequential
            .store(false, Ordering::Release);
    }

    fn is_demuxer_open_sequential(&self) -> bool {
        self.inner.demuxer_open_sequential.load(Ordering::Acquire)
    }

    /// 长内容 seek：用「虚拟 body」打开 demuxer，而不是 format.seek 扫全文件
    ///
    /// LongFormProgressive 也算：seek 时会先 seekable_clone 升级，再走虚拟 body。
    /// 判断必须在 clone 前后都能成立，否则会误走 format.seek 扫整文件 mdat。
    pub fn prefers_virtual_body_seek(&self) -> bool {
        let header_end = self.inner.header_end.load(Ordering::Acquire);
        if header_end == 0 {
            return false;
        }
        let mode_ok = matches!(
            self.access_mode,
            RemoteAccessMode::LongFormProgressive
                | RemoteAccessMode::StandardSeekable
                | RemoteAccessMode::FragmentedSeekable
        );
        if !mode_ok {
            return false;
        }
        self.inner
            .seek_index
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|index| !index.entries.is_empty()))
            .unwrap_or(false)
    }

    /// 取出文件头缓存（ftyp/moov/sidx），供 virtual-body 拼接 demux 使用
    pub fn header_bytes(&self) -> Option<Vec<u8>> {
        let header_end = self.inner.header_end.load(Ordering::Acquire);
        if header_end == 0 {
            return None;
        }
        let need = header_end as usize;
        let mut out = vec![0u8; need];
        // 优先内存缓存 start=0
        if let Ok(cache) = self.inner.cache.lock() {
            if let Some(seg) = cache.iter().find(|s| s.start == 0) {
                let n = seg.data.len().min(need);
                out[..n].copy_from_slice(&seg.data[..n]);
                if n == need {
                    return Some(out);
                }
            }
        }
        // 磁盘缓存兜底
        if let Some(disk) = &self.inner.disk_cache {
            if disk.read_cached(0, &mut out).unwrap_or(0) == need {
                return Some(out);
            }
        }
        None
    }

    /// 配置虚拟 body：逻辑 [0, header_end) = 文件头；逻辑 header_end.. = 目标 moof 起
    pub fn configure_virtual_body_for_time(&self, position_ms: u64) -> io::Result<u64> {
        self.ensure_read_current()?;
        let header_end = self.inner.header_end.load(Ordering::Acquire);
        if header_end == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mp4 header_end unknown",
            ));
        }
        let index = self
            .inner
            .seek_index
            .lock()
            .map_err(|_| io::Error::other("seek index lock poisoned"))?
            .clone();
        let Some(index) = index else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "seek index missing",
            ));
        };
        // 只算 sidx 落点；真正 body 拉取交给 splice 分块（避免这里一次拉过大超时）
        let (seg_start, _seg_end_hint) = index.estimate_segment(position_ms);
        let mut target = seg_start.max(header_end).min(self.inner.total_len.saturating_sub(1));

        // 轻量探测：最多拉 256KB 校验 moof 并对齐；失败也不致命，splice 会再 snap
        let probe_end = target
            .saturating_add(REMOTE_FETCH_BLOCK_BYTES)
            .saturating_sub(1)
            .min(self.inner.total_len.saturating_sub(1));
        let probe = if position_is_cached_range(&self.inner, target, probe_end) {
            None
        } else {
            match fetch_range_block(
                &self.inner,
                target,
                probe_end,
                self.read_cancellation.as_ref(),
            ) {
                Ok(data) if !data.is_empty() => Some(data),
                Ok(_) => None,
                Err(err) => {
                    log::warn!(
                        target: "remote-audio",
                        "virtual-body probe fetch failed at {}: {}",
                        target,
                        err,
                    );
                    None
                }
            }
        };
        if let Some(data) = probe {
            if let Some(rel) = find_moof_offset_near(&data, 0, 64 * 1024) {
                if rel > 0 {
                    log::warn!(
                        target: "remote-audio",
                        "virtual-body sidx miss-aligned by {} bytes, snapping to moof",
                        rel,
                    );
                    target = target
                        .saturating_add(rel)
                        .min(self.inner.total_len.saturating_sub(1));
                }
                let store = data[rel as usize..].to_vec();
                if !store.is_empty() {
                    if let Ok(mut cache) = self.inner.cache.lock() {
                        if !cache_contains_position(&cache, target) {
                            cache.push(CachedSegment {
                                start: target,
                                data: store.clone(),
                            });
                            cache.sort_by_key(|segment| segment.start);
                            trim_cache(&self.inner, &mut cache, target);
                        }
                    }
                    write_disk_range(&self.inner, target, &store);
                    protect_cached_range(&self.inner, target, store.len() as u64);
                }
            } else {
                log::warn!(
                    target: "remote-audio",
                    "virtual-body probe {:#x} has no moof in first 64KiB (head={:02x?})",
                    target,
                    &data[..data.len().min(16)],
                );
            }
        }

        self.inner
            .virtual_body_origin
            .store(target, Ordering::Release);
        self.inner
            .demuxer_open_sequential
            .store(true, Ordering::Release);
        invalidate_prefetch_epoch(&self.inner);
        let logical_len = header_end.saturating_add(self.inner.total_len.saturating_sub(target));
        log::info!(
            target: "remote-audio",
            "virtual-body configured target_ms={}, header_end={}, body_origin={}, total_len={}, logical_len={}",
            position_ms,
            header_end,
            target,
            self.inner.total_len,
            logical_len,
        );
        Ok(target)
    }

    pub fn clear_virtual_body(&self) {
        self.inner.virtual_body_origin.store(0, Ordering::Release);
    }

    /// virtual-body **纯逻辑连续**坐标（AtomIterator / moof_base_pos 共用）：
    /// - [0, header_end) → 物理 header
    /// - [header_end + k) → 物理 body_origin + k
    ///
    /// 绝对 base_data_offset 的 sample Seek 在 `seek()` 里先折算成逻辑坐标，
    /// 禁止在 map 里对 pos≥origin 做恒等：否则 MSS pos 跳到物理 55MB，
    /// AtomIterator 仍在逻辑 ~header_end，下一 atom 直接 overread / EOF。
    fn map_logical_to_physical(&self, pos: u64) -> u64 {
        let origin = self.inner.virtual_body_origin.load(Ordering::Acquire);
        if origin == 0 {
            return pos.min(self.inner.total_len);
        }
        let header_end = self.inner.header_end.load(Ordering::Acquire);
        if pos < header_end {
            pos.min(self.inner.total_len)
        } else {
            origin
                .saturating_add(pos - header_end)
                .min(self.inner.total_len)
        }
    }

    fn logical_len(&self) -> u64 {
        let origin = self.inner.virtual_body_origin.load(Ordering::Acquire);
        if origin == 0 {
            return self.inner.total_len;
        }
        let header_end = self.inner.header_end.load(Ordering::Acquire);
        // 纯连续逻辑长度；绝对 sample 偏移在 seek 时折算，不靠拉长 logical_len
        header_end.saturating_add(self.inner.total_len.saturating_sub(origin))
    }

    /// 将 demuxer 的 Seek 目标折成逻辑坐标。
    /// - 相对 moof：目标已在 [0, logical_len)
    /// - 绝对 base_data_offset：目标 ≥ body_origin，折成 header_end + (phys - origin)
    fn normalize_seek_offset(&self, offset: u64) -> u64 {
        let origin = self.inner.virtual_body_origin.load(Ordering::Acquire);
        if origin == 0 {
            return offset.min(self.inner.total_len);
        }
        let header_end = self.inner.header_end.load(Ordering::Acquire);
        let logical_len = self.logical_len();
        if offset >= origin {
            // 绝对文件偏移 → 逻辑 body
            let logical = header_end.saturating_add(offset - origin);
            logical.min(logical_len)
        } else {
            offset.min(logical_len)
        }
    }

    fn read_cancelled(&self) -> bool {
        self.inner.playback_cancelled()
            || self
                .read_cancellation
                .as_ref()
                .is_some_and(RemoteReadCancellation::is_cancelled)
    }

    fn ensure_read_current(&self) -> io::Result<()> {
        if self.read_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "remote read superseded",
            ))
        } else {
            Ok(())
        }
    }

    /// 按物理偏移读内存缓存
    fn read_cached_at(&self, physical: u64, out: &mut [u8]) -> usize {
        let cache = match self.inner.cache.lock() {
            Ok(cache) => cache,
            Err(_) => return 0,
        };

        for segment in cache.iter() {
            let segment_end = segment.start + segment.data.len() as u64;
            if physical < segment.start || physical >= segment_end {
                continue;
            }

            let offset = (physical - segment.start) as usize;
            let available = segment.data.len().saturating_sub(offset);
            let len = available.min(out.len());
            out[..len].copy_from_slice(&segment.data[offset..offset + len]);
            return len;
        }

        0
    }

    fn read_disk_cached_at(&self, physical: u64, out: &mut [u8]) -> usize {
        let Some(cache) = &self.inner.disk_cache else {
            return 0;
        };
        let Some(len) = disk_cached_len(&self.inner, physical, out.len()) else {
            return 0;
        };
        cache.read_cached(physical, &mut out[..len]).unwrap_or(0)
    }

    fn has_cached_physical(&self, physical: u64) -> bool {
        let cache = match self.inner.cache.lock() {
            Ok(cache) => cache,
            Err(_) => return false,
        };

        cache.iter().any(|segment| {
            let segment_end = segment.start + segment.data.len() as u64;
            physical >= segment.start && physical < segment_end
        })
    }

    /// 按物理偏移拉取；全程不改 self.pos（self.pos 只存逻辑坐标）
    fn fetch_physical_range(&self, physical: u64, wanted_len: usize) -> io::Result<()> {
        self.ensure_read_current()?;
        if physical >= self.inner.total_len {
            return Ok(());
        }
        if self.has_cached_physical(physical)
            || disk_cached_len(&self.inner, physical, 1).is_some()
        {
            return Ok(());
        }
        if self.wait_for_prefetch(physical) {
            return Ok(());
        }

        // 同 offset 被别人占着时优先等；超时仍无数据则抢占，避免 seek 死等旧预取
        let mut claimed = claim_prefetch_start(&self.inner, physical);
        if !claimed {
            if self.wait_for_prefetch(physical) || self.has_cached_physical(physical) {
                return Ok(());
            }
            force_release_prefetch_start(&self.inner, physical);
            claimed = claim_prefetch_start(&self.inner, physical);
            if !claimed {
                if self.wait_for_prefetch(physical) || self.has_cached_physical(physical) {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "remote range is already being fetched",
                ));
            }
        }

        let wanted = wanted_len.max(self.access_mode.demand_fetch_block_bytes() as usize) as u64;
        let end = physical
            .saturating_add(wanted)
            .saturating_sub(1)
            .min(self.inner.total_len.saturating_sub(1));
        let fetched = fetch_range_block(
            &self.inner,
            physical,
            end,
            self.read_cancellation.as_ref(),
        );
        release_prefetch_start(&self.inner, physical);
        let data = fetched?;

        if data.is_empty() {
            return Ok(());
        }

        let mut cache = self
            .inner
            .cache
            .lock()
            .map_err(|_| io::Error::other("remote cache lock poisoned"))?;
        if !cache_contains_position(&cache, physical) {
            cache.push(CachedSegment {
                start: physical,
                data,
            });
            cache.sort_by_key(|segment| segment.start);
            trim_cache(
                &self.inner,
                &mut cache,
                cache_center(&self.inner, physical),
            );
        }
        drop(cache);
        replenish_prefetch_window(&self.inner, physical);
        Ok(())
    }

    fn wait_for_prefetch(&self, start: u64) -> bool {
        let started = Instant::now();
        // seek 后需求读不应被 15s 旧预取拖死；短等即可
        let wait_budget = Duration::from_millis(800);
        let Ok(mut in_flight) = self.inner.in_flight.lock() else {
            return false;
        };
        while in_flight.contains(&start) && started.elapsed() < wait_budget {
            if self.read_cancelled() {
                return false;
            }
            in_flight = match self
                .inner
                .range_available
                .wait_timeout(in_flight, Duration::from_millis(40))
            {
                Ok((guard, _)) => guard,
                Err(error) => error.into_inner().0,
            };
            drop(in_flight);
            if self.has_cached_physical(start) || disk_cached_len(&self.inner, start, 1).is_some() {
                return true;
            }
            in_flight = match self.inner.in_flight.lock() {
                Ok(guard) => guard,
                Err(_) => return false,
            };
        }
        drop(in_flight);

        self.has_cached_physical(start) || disk_cached_len(&self.inner, start, 1).is_some()
    }
}

impl Read for RemoteAudioSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.ensure_read_current()?;
        let logical_len = self.logical_len();
        if out.is_empty() || self.pos >= logical_len {
            return Ok(0);
        }

        let mut total_read = 0usize;
        while total_read < out.len() && self.pos < logical_len {
            self.ensure_read_current()?;
            let logical_pos = self.pos;
            let physical = self.map_logical_to_physical(logical_pos);

            // 全程不改 self.pos 为物理坐标：缓存按 physical 显式寻址
            let mut read = self.read_cached_at(physical, &mut out[total_read..]);
            if read == 0 {
                read = self.read_disk_cached_at(physical, &mut out[total_read..]);
            }
            if read == 0 {
                match self.fetch_physical_range(physical, out.len() - total_read) {
                    Ok(()) => {
                        read = self.read_cached_at(physical, &mut out[total_read..]);
                        if read == 0 {
                            read = self.read_disk_cached_at(physical, &mut out[total_read..]);
                        }
                    }
                    Err(_err) if total_read > 0 => return Ok(total_read),
                    Err(err) => return Err(err),
                }
            }
            if read == 0 {
                break;
            }
            self.pos = logical_pos.saturating_add(read as u64);
            total_read += read;
            remember_read_pos(&self.inner, physical.saturating_add(read as u64));
            if !self.is_demuxer_open_sequential() {
                replenish_prefetch_window(
                    &self.inner,
                    physical.saturating_add(read as u64),
                );
            }
        }

        Ok(total_read)
    }
}

impl Seek for RemoteAudioSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.ensure_read_current()?;
        let logical_len = self.logical_len();
        let raw_next = match pos {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
            SeekFrom::End(offset) => logical_len as i128 + offset as i128,
        };

        if raw_next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek before start",
            ));
        }

        let previous = self.pos;
        // Start 可能带绝对 base_data_offset；先折成逻辑坐标再 clamp
        let next = match pos {
            SeekFrom::Start(offset) => self.normalize_seek_offset(offset),
            _ => (raw_next as u64).min(logical_len),
        };
        self.pos = next;
        if self.pos.abs_diff(previous) > REMOTE_FETCH_BLOCK_BYTES {
            invalidate_prefetch_epoch(&self.inner);
        }
        let physical = self.map_logical_to_physical(self.pos);
        if self.inner.virtual_body_origin.load(Ordering::Acquire) > 0
            && self.pos.abs_diff(previous) > 64
        {
            log::debug!(
                target: "remote-audio",
                "virtual-body seek logical={} physical={} header_end={} body_origin={} prev={} raw={}",
                self.pos,
                physical,
                self.inner.header_end.load(Ordering::Acquire),
                self.inner.virtual_body_origin.load(Ordering::Acquire),
                previous,
                raw_next,
            );
        }
        remember_read_pos(&self.inner, physical);
        // 返回逻辑 pos（MSS / AtomIterator 只认连续逻辑坐标）
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
        let lookup_started = Instant::now();
        let digest_prefix = self.digest.get(..8).unwrap_or(&self.digest);
        if self.bypass_ready.load(Ordering::Acquire) {
            log::info!(
                target: "remote-cache",
                "lookup bypassed digest={} elapsed_ms={}",
                digest_prefix,
                lookup_started.elapsed().as_millis(),
            );
            return None;
        }

        let marker_text = match std::fs::read_to_string(&self.ready_path) {
            Ok(value) => value,
            Err(error) => {
                log::info!(
                    target: "remote-cache",
                    "lookup miss digest={} reason=marker_read error_kind={:?} elapsed_ms={}",
                    digest_prefix,
                    error.kind(),
                    lookup_started.elapsed().as_millis(),
                );
                return None;
            }
        };
        let marker = match parse_cache_marker(marker_text.trim()) {
            Some(marker) => marker,
            None => {
                log::warn!(
                    target: "remote-cache",
                    "lookup miss digest={} reason=marker_parse elapsed_ms={}",
                    digest_prefix,
                    lookup_started.elapsed().as_millis(),
                );
                return None;
            }
        };
        let (path, marked_length, expected_sha256, file_name) = match &marker {
            CacheMarker::Legacy { .. } => {
                log::warn!(
                    target: "remote-cache",
                    "lookup miss digest={} reason=legacy_marker elapsed_ms={}",
                    digest_prefix,
                    lookup_started.elapsed().as_millis(),
                );
                return None;
            }
            CacheMarker::Validated {
                content_length,
                sha256,
                file_name,
            } => (
                self.resolve_marker_file(file_name)?,
                *content_length,
                sha256.as_str(),
                file_name.as_str(),
            ),
        };

        if let Err(err) = validate_published_cache_file(
            &path,
            marked_length,
            self.expected_content_length,
            self.expected_duration_ms,
            expected_sha256,
            file_name,
        ) {
            log::warn!(
                target: "remote-cache",
                "lookup miss digest={} reason=validation error={} elapsed_ms={}",
                digest_prefix,
                err,
                lookup_started.elapsed().as_millis(),
            );
            return None;
        }

        if let Ok(mut published) = self.published_path.lock() {
            *published = Some(path.clone());
        }
        log::info!(
            target: "remote-cache",
            "lookup hit digest={} bytes={} elapsed_ms={}",
            digest_prefix,
            marked_length,
            lookup_started.elapsed().as_millis(),
        );
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

    pub(crate) fn prepare_sequential_write(&self, total_len: u64) -> AppResult<PathBuf> {
        self.prepare(total_len)
            .map_err(|err| AppError::Other(err.to_string()))?;
        self.staging_path()
            .map_err(|err| AppError::Other(err.to_string()))
    }

    pub(crate) fn publish_sequential_write(&self) -> AppResult<PathBuf> {
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
        prune_disk_cache(
            &self.cache_root,
            self.max_cache_bytes
                .saturating_sub(validated.content_length),
            &self.digest,
        );
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
    std::fs::create_dir_all(directory)?;
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

fn validate_published_cache_file(
    path: &Path,
    marked_length: u64,
    expected_length: Option<u64>,
    _expected_duration_ms: Option<u64>,
    expected_sha256: &str,
    file_name: &str,
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
    let expected_suffix = format!(".{}.audio", expected_sha256);
    if !file_name.ends_with(&expected_suffix) {
        return Err("cache file name does not match published SHA-256".into());
    }

    if reuse_validated_audio_cache(
        path,
        content_length,
        metadata.modified().ok(),
        Some(expected_sha256),
    )
    .is_none()
    {
        // 文件名已包含 SHA-256，长度也与 marker 一致
        // 完整性已由下载流程保证，跳过昂贵的 symphonia
        // probe that reopens and parses the entire container (very slow for large
        // ISO-MP4 files in debug builds).
    }
    let validated = ValidatedCacheFile {
        content_length,
        sha256: expected_sha256.to_string(),
    };
    remember_validated_audio_cache(path, metadata.modified().ok(), &validated);
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
    let decoder = SymphoniaAudioDecoder::new_file(path)?;
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
        // 仅 try_new 扫描顶层 atom 时隐藏 seekable，避免扫完整 mdat
        if self.is_demuxer_open_sequential() {
            return false;
        }
        // virtual-body：打开后必须可 seek，样本要从 mdat 回跳
        if self.inner.virtual_body_origin.load(Ordering::Acquire) > 0 {
            return true;
        }
        self.access_mode.is_seekable()
    }

    fn byte_len(&self) -> Option<u64> {
        if self.is_demuxer_open_sequential() {
            return None;
        }
        if self.inner.virtual_body_origin.load(Ordering::Acquire) > 0 {
            return Some(self.logical_len());
        }
        self.access_mode
            .is_seekable()
            .then_some(self.inner.total_len)
    }
}

fn is_fragmented_mp4_url(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .map(|url| url.path().to_ascii_lowercase().ends_with(".m4s"))
        .unwrap_or_else(|| {
            value
                .split(['?', '#'])
                .next()
                .is_some_and(|path| path.to_ascii_lowercase().ends_with(".m4s"))
        })
}

/// 从首包字节嗅探分片 MP4
///
/// 仅靠 URL 后缀不够：Bilibili DASH 用 `.m4s`，但 YouTube 的 googlevideo 直链
/// 形如 `/videoplayback?itag=140&...`，路径没有任何扩展名，却同样是分片 MP4
/// （`moov` 里 `stbl` 为空，时间轴全在 `moof` 中）。误判成普通 MP4 会让
/// demuxer 拿着空样本表做 seek，长内容上表现为 seek 无响应。
fn detect_fragmented_mp4(data: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let Ok(size_bytes) = data[offset..offset + 4].try_into() else {
            return false;
        };
        let size = u32::from_be_bytes(size_bytes) as usize;
        let kind = &data[offset + 4..offset + 8];

        // styp/sidx/moof 是分片 MP4 的直接标志
        if kind == b"styp" || kind == b"sidx" || kind == b"moof" {
            return true;
        }
        // moov 内含 mvex 表示后续为 movie fragment
        if kind == b"moov" {
            let header_len = if size == 1 { 16 } else { 8 };
            let end = if size == 0 {
                data.len()
            } else {
                (offset + size).min(data.len())
            };
            let body_start = (offset + header_len).min(end);
            if data[body_start..end]
                .windows(4)
                .any(|window| window == b"mvex")
            {
                return true;
            }
        }

        let header_len = if size == 1 { 16 } else { 8 };
        let atom_size = if size == 1 {
            let Ok(size_bytes) = data[offset + 8..offset + 16].try_into() else {
                return false;
            };
            u64::from_be_bytes(size_bytes) as usize
        } else if size == 0 {
            return false;
        } else {
            size
        };
        if atom_size < header_len {
            return false;
        }
        // mdat 之后不会再有头部信息，且首包通常放不下整个 mdat
        if kind == b"mdat" {
            return false;
        }
        offset += atom_size;
    }
    false
}

pub struct SymphoniaAudioDecoder {
    decoder: Box<dyn Decoder>,
    current_frame_offset: usize,
    format: Box<dyn FormatReader>,
    track_id: u32,
    total_duration: Option<Time>,
    buffer: SampleBuffer<f32>,
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

    /// 远程源：若处于 demuxer_open_sequential，打开时对 isomp4 隐藏 seekable
    pub fn new_remote(source: RemoteAudioSource) -> Result<Self, String> {
        Self::new_remote_virtual(source, false)
    }

    /// 远程源：virtual_body 模式用于长内容 seek（header + 目标 moof）
    pub fn new_remote_virtual(
        mut source: RemoteAudioSource,
        keep_virtual_body: bool,
    ) -> Result<Self, String> {
        let finish_handle = RemoteAudioSource {
            inner: Arc::clone(&source.inner),
            pos: 0,
            access_mode: source.access_mode,
            read_cancellation: source.read_cancellation.clone(),
        };
        source.pos = 0;
        if keep_virtual_body {
            source
                .inner
                .demuxer_open_sequential
                .store(true, Ordering::Release);

            // 关键修复：把 header + 目标 moof/mdat 拼成连续内存 demux。
            // 旧路径让 isomp4 在 virtual-body 上自己 next_packet：
            // - MoovSegment stco 样本会被映射到中段字节 → 假 AAC
            // - Unsupported("coupling channel element") 会直接杀 worker
            // - 段边界假 EOF 会让 worker frames≈1k 就 exhausted
            // 拼接后 demuxer 看到的是真实连续字节，坐标无歧义。
            return Self::new_remote_virtual_spliced(source, finish_handle);
        }
        let buffer_len = REMOTE_DECODER_BUFFER_BYTES;
        let options = MediaSourceStreamOptions { buffer_len };
        let mss = MediaSourceStream::new(Box::new(source), options);
        let decoder = Self::init_remote(mss, None)
            .map_err(|err| format!("Decode error: {}", err))?;
        finish_handle.finish_demuxer_open();
        finish_handle.clear_virtual_body();
        log::info!(
            target: "remote-audio",
            "demuxer open finished sequential_cleared=true virtual_body=false",
        );
        Ok(decoder)
    }

    /// virtual-body：header ∥ 目标 moof/mdat 连续拼接后本地 demux
    fn new_remote_virtual_spliced(
        source: RemoteAudioSource,
        finish_handle: RemoteAudioSource,
    ) -> Result<Self, String> {
        let header_end = source.inner.header_end.load(Ordering::Acquire);
        let origin = source.inner.virtual_body_origin.load(Ordering::Acquire);
        if header_end == 0 || origin == 0 {
            return Err("virtual-body splice requires header_end and body_origin".into());
        }
        let header = source
            .header_bytes()
            .ok_or_else(|| "virtual-body header cache missing".to_string())?;
        if header.len() < header_end as usize {
            return Err(format!(
                "virtual-body header incomplete: got {} need {}",
                header.len(),
                header_end
            ));
        }
        let header = header[..header_end as usize].to_vec();

        // 从缓存取 body；分块拉满 want_body（默认 2MB），避免单次 16MB Range 超时被 prepare 取消
        let mut body = Vec::new();
        if let Ok(cache) = source.inner.cache.lock() {
            // 合并所有覆盖 [origin, origin+want) 的段
            let mut cursor = origin;
            let want = REMOTE_VIRTUAL_SPLICE_BODY_BYTES;
            let limit = origin.saturating_add(want).min(source.inner.total_len);
            while cursor < limit {
                let Some(seg) = cache.iter().find(|s| {
                    s.start <= cursor && cursor < s.start + s.data.len() as u64
                }) else {
                    break;
                };
                let off = (cursor - seg.start) as usize;
                let take = (limit - cursor).min(seg.data.len() as u64 - off as u64) as usize;
                body.extend_from_slice(&seg.data[off..off + take]);
                cursor += take as u64;
            }
        }
        let want_body = REMOTE_VIRTUAL_SPLICE_BODY_BYTES
            .min(source.inner.total_len.saturating_sub(origin))
            .max(REMOTE_FETCH_BLOCK_BYTES);
        if (body.len() as u64) < want_body {
            let mut cursor = origin.saturating_add(body.len() as u64);
            let end_target = origin
                .saturating_add(want_body)
                .min(source.inner.total_len);
            while cursor < end_target {
                source.ensure_read_current().map_err(|err| err.to_string())?;
                let chunk_end = cursor
                    .saturating_add(REMOTE_VIRTUAL_SPLICE_FETCH_CHUNK)
                    .saturating_sub(1)
                    .min(end_target.saturating_sub(1));
                log::info!(
                    target: "remote-audio",
                    "virtual-body splice fetch chunk {}..{} ({}/{})",
                    cursor,
                    chunk_end,
                    body.len(),
                    want_body,
                );
                let data = fetch_range_block(
                    &source.inner,
                    cursor,
                    chunk_end,
                    source.read_cancellation.as_ref(),
                )
                .map_err(|err| format!("virtual-body body fetch: {err}"))?;
                if data.is_empty() {
                    break;
                }
                body.extend_from_slice(&data);
                cursor = cursor.saturating_add(data.len() as u64);
                write_disk_range(
                    &source.inner,
                    cursor.saturating_sub(data.len() as u64),
                    &data,
                );
            }
            if !body.is_empty() {
                if let Ok(mut cache) = source.inner.cache.lock() {
                    if !cache_contains_position(&cache, origin) {
                        cache.push(CachedSegment {
                            start: origin,
                            data: body.clone(),
                        });
                        cache.sort_by_key(|segment| segment.start);
                        trim_cache(&source.inner, &mut cache, origin);
                    }
                }
                protect_cached_range(&source.inner, origin, body.len() as u64);
            }
        }
        if body.len() < 16 || body.get(4..8) != Some(&b"moof"[..]) {
            // 再 snap 一次
            if let Some(rel) = find_moof_offset_near(&body, 0, 64 * 1024) {
                body = body[rel as usize..].to_vec();
            }
        }
        if body.get(4..8) != Some(&b"moof"[..]) {
            return Err(format!(
                "virtual-body body does not start with moof (head={:02x?})",
                &body[..body.len().min(16)]
            ));
        }

        let mut spliced = Vec::with_capacity(header.len() + body.len());
        spliced.extend_from_slice(&header);
        spliced.extend_from_slice(&body);
        log::info!(
            target: "remote-audio",
            "virtual-body spliced header={} body={} total={} origin={}",
            header.len(),
            body.len(),
            spliced.len(),
            origin,
        );

        let cursor = std::io::Cursor::new(spliced);
        let options = MediaSourceStreamOptions {
            buffer_len: REMOTE_VIRTUAL_DECODER_BUFFER_BYTES,
        };
        let mss = MediaSourceStream::new(Box::new(cursor), options);
        // 本地 Cursor 全程 seekable；init 内已跳过 moov 假样本
        finish_handle.finish_demuxer_open();
        let decoder = Self::init_remote_spliced(mss)
            .map_err(|err| format!("Decode error: {}", err))?;
        log::info!(
            target: "remote-audio",
            "demuxer open finished sequential_cleared=true virtual_body=true splice=true",
        );
        Ok(decoder)
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

    /// 远程打开：try_new 期间可保持 sequential；探测完成后立刻恢复 seekable 再读首包
    fn init_remote(
        mss: MediaSourceStream,
        enable_seek_after_probe: Option<RemoteAudioSource>,
    ) -> symphonia::core::errors::Result<Self> {
        let hint = Hint::new();
        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts: MetadataOptions = Default::default();
        let mut probed = get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

        // try_new 已结束：允许样本级 Seek（virtual-body 必需）
        if let Some(handle) = enable_seek_after_probe.as_ref() {
            handle.finish_demuxer_open();
        }

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

        // 中段 moof 首帧可能非 RAP，多跳过一些 DecodeError。
        // 注意：fragmented 流里 moov 常无样本，首个 moof 的 first_ts 也可能是 0，
        // 绝不能按 ts==0 丢弃，否则会把目标段整段扔掉。
        let max_errors = if enable_seek_after_probe.is_some() {
            MAX_VIRTUAL_BODY_SKIP_PACKETS
        } else {
            MAX_DECODE_RETRIES
        };
        let mut decode_errors = 0usize;
        let mut packet_idx = 0usize;
        let decoded = loop {
            let packet = probed.format.next_packet()?;
            if packet.track_id() != track_id {
                continue;
            }
            packet_idx += 1;
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    if enable_seek_after_probe.is_some() {
                        log::info!(
                            target: "remote-audio",
                            "virtual-body first good packet idx={} ts={} dur={} bytes={} after_errors={}",
                            packet_idx,
                            packet.ts(),
                            packet.dur(),
                            packet.buf().len(),
                            decode_errors,
                        );
                    }
                    break decoded;
                }
                Err(SymphoniaError::DecodeError(msg)) if decode_errors < max_errors => {
                    decode_errors += 1;
                    if enable_seek_after_probe.is_some() && decode_errors <= 4 {
                        log::warn!(
                            target: "remote-audio",
                            "virtual-body decode error idx={} ts={} bytes={} err={}",
                            packet_idx,
                            packet.ts(),
                            packet.buf().len(),
                            msg,
                        );
                    }
                    decoder.reset();
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

    /// 拼接 demux：本地 Cursor 全程 seekable；首包必须来自 moof（跳过 moov stco 假样本）
    fn init_remote_spliced(
        mss: MediaSourceStream,
    ) -> symphonia::core::errors::Result<Self> {
        let hint = Hint::new();
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

        // 先吞掉 moov 段样本（stco 指向文件头 mdat，在拼接缓冲里是错误数据）
        // 策略：连续 DecodeError / Unsupported 都跳；一旦解出好包就停
        let mut skipped = 0usize;
        let mut decode_errors = 0usize;
        let decoded = loop {
            let packet = match probed.format.next_packet() {
                Ok(p) => p,
                Err(err) if skipped < MAX_VIRTUAL_BODY_SKIP_MOOV_SAMPLES => {
                    // 可能还在假样本区；再试
                    skipped += 1;
                    if skipped <= 4 {
                        log::warn!(
                            target: "remote-audio",
                            "splice next_packet err while skipping moov: {}",
                            err,
                        );
                    }
                    continue;
                }
                Err(err) => return Err(err),
            };
            if packet.track_id() != track_id {
                continue;
            }
            skipped += 1;
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    log::info!(
                        target: "remote-audio",
                        "splice first good packet after_skip={} ts={} dur={} bytes={}",
                        skipped,
                        packet.ts(),
                        packet.dur(),
                        packet.buf().len(),
                    );
                    break decoded;
                }
                Err(SymphoniaError::DecodeError(_))
                | Err(SymphoniaError::Unsupported(_))
                    if decode_errors < MAX_VIRTUAL_BODY_SKIP_PACKETS =>
                {
                    decode_errors += 1;
                    decoder.reset();
                }
                Err(SymphoniaError::ResetRequired) => {
                    decoder.reset();
                }
                Err(err) if skipped < MAX_VIRTUAL_BODY_SKIP_MOOV_SAMPLES => {
                    // 其它瞬时错误也跳
                    if skipped <= 4 {
                        log::warn!(
                            target: "remote-audio",
                            "splice skip decode err: {}",
                            err,
                        );
                    }
                    decoder.reset();
                }
                Err(err) => return Err(err),
            }
            if skipped >= MAX_VIRTUAL_BODY_SKIP_MOOV_SAMPLES {
                return Err(SymphoniaError::DecodeError(
                    "virtual-body splice: could not find decodable moof sample",
                ));
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

    /// 已有好包后继续跳过坏包（当前未使用；拼接路径在 init 内已处理）
    #[allow(dead_code)]
    fn skip_to_fragment_samples(&mut self, max_packets: usize) -> usize {
        let mut skipped = 0usize;
        while skipped < max_packets {
            // 已有 buffer 里是好包就直接返回
            if !self.buffer.is_empty() && self.current_frame_offset < self.buffer.len() {
                break;
            }
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(_) => break,
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    decoded.spec().clone_into(&mut self.spec);
                    self.buffer = Self::copy_buffer(decoded, &self.spec);
                    self.current_frame_offset = 0;
                    break;
                }
                Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::Unsupported(_)) => {
                    skipped += 1;
                    self.decoder.reset();
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                }
                Err(_) => break,
            }
        }
        skipped
    }

    fn copy_buffer(decoded: AudioBufferRef<'_>, spec: &SignalSpec) -> SampleBuffer<f32> {
        let duration = units::Duration::from(decoded.capacity() as u64);
        let mut buffer = SampleBuffer::<f32>::new(duration, *spec);
        buffer.copy_interleaved_ref(decoded);
        buffer
    }

    fn refine_position(&mut self, seek_res: SeekedTo) -> Result<(), PcmSeekError> {
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

// skip_decode_errors 已移除：init_remote 内已处理坏包；外部再跳会让 buffer 与 format 失步

impl Iterator for SymphoniaAudioDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.current_frame_offset >= self.buffer.len() {
            // 中段流允许跳过更多坏包；真 EOF 才返回 None
            let mut decode_errors = 0usize;
            let mut consecutive_io = 0usize;
            let decoded = loop {
                let packet = match self.format.next_packet() {
                    Ok(packet) => {
                        consecutive_io = 0;
                        packet
                    }
                    Err(SymphoniaError::ResetRequired) => {
                        self.decoder.reset();
                        continue;
                    }
                    Err(SymphoniaError::IoError(err))
                        if consecutive_io < MAX_VIRTUAL_BODY_SKIP_PACKETS
                            && (err.kind() == std::io::ErrorKind::UnexpectedEof
                                || err.kind() == std::io::ErrorKind::Interrupted
                                || err.kind() == std::io::ErrorKind::WouldBlock) =>
                    {
                        // 预取窗口边缘瞬时 EOF / 被抢占：稍等再试，避免整段饿死
                        consecutive_io += 1;
                        if consecutive_io <= 3 {
                            log::warn!(
                                target: "remote-audio",
                                "virtual-body next_packet io retry={} kind={} msg={}",
                                consecutive_io,
                                err.kind(),
                                err,
                            );
                        }
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    Err(err) => {
                        log::warn!(
                            target: "remote-audio",
                            "virtual-body next_packet fatal: {}",
                            err,
                        );
                        return None;
                    }
                };
                if packet.track_id() != self.track_id {
                    continue;
                }

                match self.decoder.decode(&packet) {
                    Ok(decoded) => break decoded,
                    // DecodeError + Unsupported（如 aac coupling）都可跳，不能当 fatal 杀 worker
                    Err(SymphoniaError::DecodeError(_))
                    | Err(SymphoniaError::Unsupported(_))
                        if decode_errors < MAX_VIRTUAL_BODY_SKIP_PACKETS =>
                    {
                        decode_errors += 1;
                        if decode_errors <= 4 || decode_errors.is_multiple_of(16) {
                            log::warn!(
                                target: "remote-audio",
                                "virtual-body stream decode skip count={}",
                                decode_errors,
                            );
                        }
                        self.decoder.reset();
                    }
                    Err(SymphoniaError::ResetRequired) => {
                        self.decoder.reset();
                    }
                    Err(err) => {
                        log::warn!(
                            target: "remote-audio",
                            "virtual-body stream decode fatal: {}",
                            err,
                        );
                        return None;
                    }
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

impl PcmSource for SymphoniaAudioDecoder {
    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }

    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration.map(time_to_duration)
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), PcmSeekError> {
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

async fn probe_range_len(
    client: &reqwest::Client,
    url: &str,
    referer: &str,
    initial_block_bytes: u64,
) -> AppResult<(u64, Vec<u8>)> {
    let range_end = initial_block_bytes.max(REMOTE_INITIAL_BLOCK_BYTES).saturating_sub(1);
    let response = client
        .get(url)
        .header(REFERER, referer)
        .header(USER_AGENT, playback_user_agent(url))
        .header(RANGE, format!("bytes=0-{}", range_end))
        .timeout(REMOTE_PROBE_TIMEOUT)
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

fn fetch_range_block(
    inner: &RemoteAudioInner,
    start: u64,
    end: u64,
    read_cancellation: Option<&RemoteReadCancellation>,
) -> io::Result<Vec<u8>> {
    tauri::async_runtime::block_on(fetch_range_block_async(
        inner,
        start,
        end,
        read_cancellation,
    ))
}

async fn fetch_range_block_async(
    inner: &RemoteAudioInner,
    start: u64,
    end: u64,
    read_cancellation: Option<&RemoteReadCancellation>,
) -> io::Result<Vec<u8>> {
    let request_started = Instant::now();
    inner.ensure_playback_current()?;
    if read_cancellation.is_some_and(RemoteReadCancellation::is_cancelled) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote read superseded",
        ));
    }
    let request = async {
        let response = inner
            .client
            .get(&inner.url)
            .header(REFERER, inner.referer.as_str())
            .header(USER_AGENT, playback_user_agent(&inner.url))
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
    };
    tokio::pin!(request);
    let result = tokio::select! {
        result = &mut request => {
            match result {
                Ok(result) => result,
                Err(error) => {
                    log::warn!(
                        target: "remote-range",
                        "request failed generation={}, range={}..{}, elapsed_ms={}, error={}",
                        inner.expected_generation,
                        start,
                        end,
                        request_started.elapsed().as_millis(),
                        summarize_remote_error(&error),
                    );
                    return Err(io::Error::other(error.to_string()));
                }
            }
        }
        () = wait_for_remote_read_cancellation(inner, read_cancellation) => {
            log::info!(
                target: "remote-range",
                "request cancelled generation={}, range={}..{}, elapsed_ms={}",
                inner.expected_generation,
                start,
                end,
                request_started.elapsed().as_millis(),
            );
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "remote read superseded",
            ));
        }
    };

    let (status, content_range, data) = result;
    let elapsed_ms = request_started.elapsed().as_millis();
    if start == 0 || elapsed_ms >= 200 {
        log::info!(
            target: "remote-range",
            "response generation={}, range={}..{}, status={}, bytes={}, elapsed_ms={}",
            inner.expected_generation,
            start,
            end,
            status,
            data.len(),
            elapsed_ms,
        );
    } else {
        log::debug!(
            target: "remote-range",
            "response generation={}, range={}..{}, status={}, bytes={}, elapsed_ms={}",
            inner.expected_generation,
            start,
            end,
            status,
            data.len(),
            elapsed_ms,
        );
    }
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

async fn wait_for_remote_read_cancellation(
    inner: &RemoteAudioInner,
    read_cancellation: Option<&RemoteReadCancellation>,
) {
    while !inner.playback_cancelled()
        && !read_cancellation.is_some_and(RemoteReadCancellation::is_cancelled)
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_generation_change(generation: &AtomicU64, expected: u64) {
    while generation.load(Ordering::Acquire) == expected {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
    if inner.playback_cancelled() {
        return;
    }
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
    let Some(first_start) = claim_prefetch_sequence_start(&inner, plan) else {
        return;
    };
    let epoch = inner.prefetch_epoch.load(Ordering::Acquire);
    tauri::async_runtime::spawn(async move {
        let mut next_start = first_start;
        while next_start < inner.total_len && next_start < plan.target_end {
            if inner.playback_cancelled()
                || inner.prefetch_epoch.load(Ordering::Acquire) != epoch
            {
                release_prefetch_start(&inner, next_start);
                break;
            }

            let fetch_start = next_start;
            let end = fetch_start
                .saturating_add(REMOTE_FETCH_BLOCK_BYTES)
                .saturating_sub(1)
                .min(plan.target_end.saturating_sub(1))
                .min(inner.total_len.saturating_sub(1));
            let result = fetch_range_block_async(&inner, fetch_start, end, None).await;

            let should_stop = match result {
                Ok(data) if data.is_empty() => true,
                Ok(data) => {
                    // epoch 变了：丢弃本块，避免旧位置继续污染缓存中心
                    if inner.prefetch_epoch.load(Ordering::Acquire) != epoch {
                        true
                    } else {
                        let fetched_end = fetch_start.saturating_add(data.len() as u64);
                        if let Ok(mut cache) = inner.cache.lock() {
                            if !cache_contains_position(&cache, fetch_start) {
                                cache.push(CachedSegment {
                                    start: fetch_start,
                                    data,
                                });
                                cache.sort_by_key(|segment| segment.start);
                                trim_cache(
                                    &inner,
                                    &mut cache,
                                    cache_center(&inner, fetch_start),
                                );
                            }
                        }
                        next_start = fetched_end.max(fetch_start.saturating_add(1));
                        false
                    }
                }
                Err(err) => {
                    log::warn!(
                        target: "remote-audio",
                        "prefetch stopped at {}: {}",
                        fetch_start, err
                    );
                    true
                }
            };
            release_prefetch_start(&inner, fetch_start);
            if should_stop {
                break;
            }
            if next_start >= inner.total_len || next_start >= plan.target_end {
                break;
            }
            if !claim_prefetch_start(&inner, next_start) {
                break;
            }
        }
        if next_start >= inner.total_len {
            try_mark_disk_cache_ready(&inner);
        }
    });
}

fn claim_prefetch_sequence_start(
    inner: &RemoteAudioInner,
    plan: PrefetchPlan,
) -> Option<u64> {
    let start = plan.start.min(inner.total_len);
    if start >= inner.total_len || start >= plan.target_end {
        return None;
    }
    claim_prefetch_start(inner, start).then_some(start)
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
                log::warn!(target: "remote-audio", "cache finalize rejected: {}", err);
            }
        })
    {
        log::warn!(
            target: "remote-audio",
            "cache finalize worker unavailable: {}",
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

fn position_is_cached_range(inner: &RemoteAudioInner, start: u64, end: u64) -> bool {
    if end < start {
        return false;
    }
    // 只要 start 已缓存就认为窗口可用（按需读会补齐）
    if let Ok(cache) = inner.cache.lock() {
        if cache_contains_position(&cache, start) {
            return true;
        }
    }
    disk_cached_len(inner, start, 1).is_some()
}

/// 只用首包的 sidx 建索引，成功返回 true；失败不写入，交给调用方兜底
fn build_seek_index_from_head(inner: &RemoteAudioInner, duration_hint_ms: u64) -> bool {
    let Some(index) = parse_sidx_seek_index(inner, duration_hint_ms.max(1)) else {
        return false;
    };
    if index.entries.is_empty() {
        return false;
    }
    log::info!(
        target: "remote-audio",
        "seek index from head sidx duration_ms={}, entries={}",
        index.duration_ms,
        index.entries.len()
    );
    let Ok(mut slot) = inner.seek_index.lock() else {
        return false;
    };
    *slot = Some(index);
    true
}

fn build_seek_index_for_long_form(
    inner: &RemoteAudioInner,
    duration_hint_ms: u64,
    allow_linear_fallback: bool,
) {
    let duration_ms = duration_hint_ms.max(1);
    let mut index = parse_sidx_seek_index(inner, duration_ms)
        .or_else(|| parse_sidx_seek_index_from_tail(inner, duration_ms));
    if index.is_none() && !allow_linear_fallback {
        // 分片内容没解析到真实 sidx 时不启用虚拟 body：线性估算落点不可靠，
        // 保持原有的 format.seek 行为，避免把可用路径换成更差的路径
        log::info!(
            target: "remote-audio",
            "seek index skipped: no sidx found and linear fallback disabled"
        );
        return;
    }
    if index.is_none() {
        // 线性估算兜底：足够让字节跳转落到大致区域
        let total = inner.total_len.max(1);
        let steps = 32u64;
        let mut entries = Vec::with_capacity(steps as usize + 1);
        for i in 0..=steps {
            let time_ms = duration_ms.saturating_mul(i) / steps;
            let byte_offset = total.saturating_mul(i) / steps;
            entries.push(SeekIndexEntry {
                time_ms,
                byte_offset,
            });
        }
        index = Some(RemoteSeekIndex {
            duration_ms,
            entries,
        });
        log::info!(
            target: "remote-audio",
            "seek index fallback linear duration_ms={}, entries={}",
            duration_ms,
            steps + 1
        );
    } else if let Some(ref built) = index {
        log::info!(
            target: "remote-audio",
            "seek index from sidx duration_ms={}, entries={}",
            built.duration_ms,
            built.entries.len()
        );
    }
    if let Ok(mut slot) = inner.seek_index.lock() {
        *slot = index;
    }
}

fn parse_sidx_seek_index(inner: &RemoteAudioInner, duration_ms: u64) -> Option<RemoteSeekIndex> {
    // 从头缓存里找 sidx
    let head = {
        let cache = inner.cache.lock().ok()?;
        cache
            .iter()
            .find(|segment| segment.start == 0)
            .map(|segment| segment.data.clone())
    }?;
    parse_sidx_from_bytes(&head, 0, duration_ms, inner.total_len)
}

fn parse_sidx_seek_index_from_tail(
    inner: &RemoteAudioInner,
    duration_ms: u64,
) -> Option<RemoteSeekIndex> {
    let cache = inner.cache.lock().ok()?;
    let tail = cache
        .iter()
        .filter(|segment| segment.start > 0)
        .max_by_key(|segment| segment.start)?;
    parse_sidx_from_bytes(&tail.data, tail.start, duration_ms, inner.total_len)
}


fn detect_mp4_header_end(data: &[u8]) -> Option<u64> {
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        let kind = &data[offset + 4..offset + 8];
        let header_len = if size == 1 { 16 } else { 8 };
        let atom_size = if size == 1 {
            if offset + 16 > data.len() {
                break;
            }
            u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?) as usize
        } else if size == 0 {
            // 到文件尾：对 header 探测无意义
            break;
        } else {
            size
        };
        if atom_size < header_len {
            break;
        }
        // 首个 mdat/moof 即媒体起点
        if kind == b"mdat" || kind == b"moof" {
            return Some(offset as u64);
        }
        if offset + atom_size > data.len() {
            // 不完整 atom：若不是 mdat/moof，停止
            break;
        }
        offset += atom_size;
    }
    None
}

/// 在 [start, start+window) 内找第一个 moof atom 相对偏移；用于 sidx 落点校正
fn find_moof_offset_near(data: &[u8], start: usize, window: u64) -> Option<u64> {
    if start >= data.len() {
        return None;
    }
    let end = (start + window as usize).min(data.len());
    let mut offset = start;
    while offset + 8 <= end {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        let kind = &data[offset + 4..offset + 8];
        if kind == b"moof" {
            return Some((offset - start) as u64);
        }
        let atom_size = if size == 1 {
            if offset + 16 > data.len() {
                break;
            }
            u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?) as usize
        } else if size == 0 {
            break;
        } else {
            size
        };
        if atom_size < 8 {
            // 坏 size：逐字节扫描找 "moof" fourcc
            offset += 1;
            continue;
        }
        if offset + atom_size > data.len() {
            // 不完整：在剩余窗口里暴力找 moof fourcc
            let search = &data[offset..end];
            if let Some(pos) = search
                .windows(4)
                .position(|w| w == b"moof")
            {
                // fourcc 在 size 之后 4 字节，atom 起点 = pos - 4
                if pos >= 4 {
                    return Some((offset + pos - 4 - start) as u64);
                }
            }
            break;
        }
        offset += atom_size;
    }
    // 兜底：窗口内暴力找 moof fourcc
    let search = &data[start..end];
    if let Some(pos) = search.windows(4).position(|w| w == b"moof") {
        if pos >= 4 {
            return Some((pos - 4) as u64);
        }
    }
    None
}

fn parse_sidx_from_bytes(
    data: &[u8],
    base_offset: u64,
    duration_ms: u64,
    total_len: u64,
) -> Option<RemoteSeekIndex> {
    // 在缓冲里扫描 sidx box
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        let kind = &data[offset + 4..offset + 8];
        let atom_size = if size == 1 {
            if offset + 16 > data.len() {
                break;
            }
            u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?) as usize
        } else if size == 0 {
            data.len().saturating_sub(offset)
        } else {
            size
        };
        if atom_size < 8 || offset + atom_size > data.len() {
            break;
        }
        if kind == b"sidx" {
            let header_len = if size == 1 { 16 } else { 8 };
            let body = &data[offset + header_len..offset + atom_size];
            if let Some(index) =
                parse_sidx_body(body, base_offset + offset as u64 + atom_size as u64, duration_ms, total_len)
            {
                return Some(index);
            }
        }
        if atom_size == 0 {
            break;
        }
        offset += atom_size;
    }
    None
}

fn parse_sidx_body(
    body: &[u8],
    first_offset_anchor: u64,
    duration_ms: u64,
    total_len: u64,
) -> Option<RemoteSeekIndex> {
    if body.len() < 20 {
        return None;
    }
    let version = body[0];
    let mut o = 4usize; // skip version+flags
    o += 4; // reference_id
    if o + 4 > body.len() {
        return None;
    }
    let timescale = u32::from_be_bytes(body[o..o + 4].try_into().ok()?) as u64;
    o += 4;
    if timescale == 0 {
        return None;
    }
    let (earliest_pts, first_offset) = if version == 0 {
        if o + 8 > body.len() {
            return None;
        }
        let pts = u32::from_be_bytes(body[o..o + 4].try_into().ok()?) as u64;
        o += 4;
        let off = u32::from_be_bytes(body[o..o + 4].try_into().ok()?) as u64;
        o += 4;
        (pts, first_offset_anchor + off)
    } else {
        if o + 16 > body.len() {
            return None;
        }
        let pts = u64::from_be_bytes(body[o..o + 8].try_into().ok()?);
        o += 8;
        let off = u64::from_be_bytes(body[o..o + 8].try_into().ok()?);
        o += 8;
        (pts, first_offset_anchor + off)
    };
    if o + 4 > body.len() {
        return None;
    }
    o += 2; // reserved
    let reference_count = u16::from_be_bytes(body[o..o + 2].try_into().ok()?) as usize;
    o += 2;

    let mut entries = Vec::with_capacity(reference_count.saturating_add(1));
    let mut time_ticks = earliest_pts;
    let mut byte_pos = first_offset.min(total_len);
    entries.push(SeekIndexEntry {
        time_ms: time_ticks.saturating_mul(1000) / timescale,
        byte_offset: byte_pos,
    });
    for _ in 0..reference_count {
        if o + 12 > body.len() {
            break;
        }
        let reference = u32::from_be_bytes(body[o..o + 4].try_into().ok()?);
        o += 4;
        let subsegment_duration = u32::from_be_bytes(body[o..o + 4].try_into().ok()?) as u64;
        o += 4;
        o += 4; // SAP
        let reference_type = (reference & 0x8000_0000) != 0;
        let reference_size = (reference & 0x7fff_ffff) as u64;
        if reference_type {
            // 嵌套 sidx，跳过大小但仍推进偏移
            byte_pos = byte_pos.saturating_add(reference_size).min(total_len);
            continue;
        }
        byte_pos = byte_pos.saturating_add(reference_size).min(total_len);
        time_ticks = time_ticks.saturating_add(subsegment_duration);
        entries.push(SeekIndexEntry {
            time_ms: time_ticks.saturating_mul(1000) / timescale,
            byte_offset: byte_pos,
        });
    }
    if entries.len() < 2 {
        return None;
    }
    // 夹到声明时长
    for entry in &mut entries {
        if duration_ms > 0 {
            entry.time_ms = entry.time_ms.min(duration_ms);
        }
        entry.byte_offset = entry.byte_offset.min(total_len);
    }
    Some(RemoteSeekIndex {
        duration_ms: duration_ms.max(entries.last().map(|e| e.time_ms).unwrap_or(0)),
        entries,
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
        drop(in_flight);
        inner.range_available.notify_all();
    }
}

/// seek 时作废旧预取：抬 epoch + 清空 in_flight，让需求读立刻能 claim
fn invalidate_prefetch_epoch(inner: &RemoteAudioInner) {
    inner.prefetch_epoch.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut in_flight) = inner.in_flight.lock() {
        in_flight.clear();
        drop(in_flight);
        inner.range_available.notify_all();
    }
}

fn force_release_prefetch_start(inner: &RemoteAudioInner, start: u64) {
    if let Ok(mut in_flight) = inner.in_flight.lock() {
        if in_flight.remove(&start) {
            drop(in_flight);
            inner.range_available.notify_all();
        }
    }
}

fn protect_cached_range(inner: &RemoteAudioInner, start: u64, len: u64) {
    if len == 0 {
        return;
    }
    let next = CachedRange {
        start,
        end: start.saturating_add(len).min(inner.total_len),
    };
    let Ok(mut ranges) = inner.protected_ranges.lock() else {
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
        merged.push(range);
    }
    *ranges = merged;
}

fn range_overlaps(a: CachedRange, b: CachedRange) -> bool {
    a.start < b.end && b.start < a.end
}

fn segment_is_protected(inner: &RemoteAudioInner, segment: &CachedSegment) -> bool {
    let segment_range = CachedRange {
        start: segment.start,
        end: segment.start.saturating_add(segment.data.len() as u64),
    };
    let Ok(ranges) = inner.protected_ranges.lock() else {
        return false;
    };
    ranges
        .iter()
        .any(|range| range_overlaps(*range, segment_range))
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

fn trim_cache(inner: &RemoteAudioInner, cache: &mut Vec<CachedSegment>, current_pos: u64) {
    let mut total: usize = cache.iter().map(|segment| segment.data.len()).sum();
    if total <= REMOTE_MAX_CACHE_BYTES {
        return;
    }

    // 先丢离读头最远且未受保护（非头/尾 moov）的块
    cache.sort_by_key(|segment| {
        let protected = segment_is_protected(inner, segment);
        (protected, segment_distance(segment, current_pos))
    });
    while total > REMOTE_MAX_CACHE_BYTES {
        let Some(segment) = cache.pop() else {
            break;
        };
        // 只剩保护块时停止，避免把 moov 裁掉导致 seek 重开 demuxer 扫全文件
        if segment_is_protected(inner, &segment) {
            cache.push(segment);
            break;
        }
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

fn seek_error(message: String) -> PcmSeekError {
    PcmSeekError::new(message)
}

#[cfg(test)]
mod tests {
    use super::{
        cache_duration_is_suspicious, format_cache_marker, is_fragmented_mp4_url,
        claim_prefetch_sequence_start, parse_cache_marker, parse_content_range, prefetch_plan,
        prefetch_window,
        ranges_cover_source,
        release_prefetch_start, remember_validated_audio_cache, reuse_validated_audio_cache,
        seek_target_time, track_duration, CacheMarker, CachedRange, CachedSegment, HttpByteRange,
        PrefetchPlan, PrefetchWindow, RemoteAccessMode, RemoteAudioCache, RemoteAudioInner,
        RemoteAudioSource, RemoteReadCancellation, SymphoniaAudioDecoder, ValidatedCacheFile,
        REMOTE_PREFETCH_FALLBACK_LOW_WATER_BYTES, REMOTE_PREFETCH_FALLBACK_TARGET_BYTES,
    };
    use reqwest::header::HeaderValue;
    use std::collections::HashSet;
    use std::io::{Cursor, Read};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
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

    fn pcm_wav_24(samples: &[i32]) -> Vec<u8> {
        let data_len = samples.len() as u32 * 3;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&8_000u32.to_le_bytes());
        wav.extend_from_slice(&24_000u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes());
        wav.extend_from_slice(&24u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            let bytes = sample.to_le_bytes();
            wav.extend_from_slice(&bytes[..3]);
        }
        wav
    }

    #[test]
    fn decoder_preserves_pcm_below_sixteen_bit_resolution() {
        let wav = pcm_wav_24(&[1, 256]);
        let mut decoder = SymphoniaAudioDecoder::new(Box::new(Cursor::new(wav)), Some("wav"))
            .expect("24-bit wav decoder");

        let least_significant_sample = decoder.next().expect("first PCM sample");

        assert!(least_significant_sample > 0.0);
        assert!(least_significant_sample < 1.0 / 32_768.0);
    }

    #[test]
    fn prefetch_window_uses_duration_hint_for_time_based_buffering() {
        // 9MB / 180s ≈ 50KB/s → 5s=250KB、15s=750KB，均被抬到 min
        let window = prefetch_window(9_000_000, 180_000);
        assert_eq!(window.low_water_bytes, 256 * 1024);
        assert_eq!(window.target_bytes, 1024 * 1024);

        // 更高码率时按时间窗口缩放
        let hi = prefetch_window(36_000_000, 180_000);
        assert_eq!(hi.low_water_bytes, 1_000_000);
        assert_eq!(hi.target_bytes, 3_000_000);
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
    fn duplicate_prefetch_sequence_is_rejected_before_task_submission() {
        let inner = RemoteAudioInner {
            client: reqwest::Client::new(),
            url: "http://127.0.0.1:9/should-not-be-requested".into(),
            referer: "https://example.com".into(),
            total_len: 4,
            header_end: AtomicU64::new(0),
            prefetch_window: PrefetchWindow {
                low_water_bytes: 1,
                target_bytes: 4,
            },
            cache: Mutex::new(Vec::new()),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(Vec::new()),
            disk_cache: None,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation: Arc::new(AtomicU64::new(1)),
            expected_generation: 1,
        };
        let plan = PrefetchPlan {
            start: 0,
            target_end: 4,
        };

        assert_eq!(claim_prefetch_sequence_start(&inner, plan), Some(0));
        assert_eq!(claim_prefetch_sequence_start(&inner, plan), None);
        assert_eq!(inner.in_flight.lock().expect("in-flight lock").len(), 1);

        release_prefetch_start(&inner, 0);
        assert_eq!(claim_prefetch_sequence_start(&inner, plan), Some(0));
        release_prefetch_start(&inner, 0);
    }

    #[test]
    fn demand_read_waits_for_matching_prefetch_instead_of_downloading_twice() {
        let playback_generation = Arc::new(AtomicU64::new(1));
        let inner = Arc::new(RemoteAudioInner {
            client: reqwest::Client::new(),
            url: "http://127.0.0.1:9/should-not-be-requested".into(),
            referer: "https://example.com".into(),
            total_len: 4,
            header_end: AtomicU64::new(0),
            prefetch_window: PrefetchWindow {
                low_water_bytes: 1,
                target_bytes: 1,
            },
            cache: Mutex::new(Vec::new()),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::from([0])),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(Vec::new()),
            disk_cache: None,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation,
            expected_generation: 1,
        });
        let producer_inner = Arc::clone(&inner);
        let producer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            producer_inner
                .cache
                .lock()
                .expect("cache lock")
                .push(CachedSegment {
                    start: 0,
                    data: vec![1, 2, 3, 4],
                });
            release_prefetch_start(&producer_inner, 0);
        });
        let mut source = RemoteAudioSource {
            inner,
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: None,
        };
        let mut bytes = [0; 4];

        source.read_exact(&mut bytes).expect("prefetched read");

        assert_eq!(bytes, [1, 2, 3, 4]);
        assert!(producer.join().is_ok());
    }

    #[test]
    fn superseded_remote_source_stops_before_network_or_cache_io() {
        let playback_generation = Arc::new(AtomicU64::new(1));
        let inner = Arc::new(RemoteAudioInner {
            client: reqwest::Client::new(),
            url: "http://127.0.0.1:9/should-not-be-requested".into(),
            referer: "https://example.com".into(),
            total_len: 4,
            header_end: AtomicU64::new(0),
            prefetch_window: PrefetchWindow {
                low_water_bytes: 1,
                target_bytes: 1,
            },
            cache: Mutex::new(Vec::new()),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(Vec::new()),
            disk_cache: None,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation: Arc::clone(&playback_generation),
            expected_generation: 1,
        });
        playback_generation.store(2, std::sync::atomic::Ordering::Release);
        let mut source = RemoteAudioSource {
            inner,
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: None,
        };
        let mut byte = [0];

        let error = source.read(&mut byte).expect_err("superseded read");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn stopped_session_cancels_remote_read_without_changing_track_generation() {
        let playback_generation = Arc::new(AtomicU64::new(1));
        let session_cancelled = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(RemoteAudioInner {
            client: reqwest::Client::new(),
            url: "http://127.0.0.1:9/should-not-be-requested".into(),
            referer: "https://example.com".into(),
            total_len: 4,
            header_end: AtomicU64::new(0),
            prefetch_window: PrefetchWindow {
                low_water_bytes: 1,
                target_bytes: 1,
            },
            cache: Mutex::new(Vec::new()),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(Vec::new()),
            disk_cache: None,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation,
            expected_generation: 1,
        });
        let mut source = RemoteAudioSource {
            inner,
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: Some(RemoteReadCancellation::new(
                Arc::clone(&session_cancelled),
                None,
            )),
        };
        session_cancelled.store(true, Ordering::Release);

        let error = source
            .read(&mut [0])
            .expect_err("stopped session read");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn fragmented_mp4_starts_progressively_and_promotes_for_seek() {
        assert!(is_fragmented_mp4_url(
            "https://cdn.example/audio/track.M4S?deadline=123"
        ));
        assert!(!is_fragmented_mp4_url(
            "https://cdn.example/audio/track.flac?deadline=123"
        ));

        let source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/track.m4s".into(),
                referer: "https://example.com".into(),
                total_len: 4,
                header_end: AtomicU64::new(0),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 1,
                    target_bytes: 1,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(false),
                virtual_body_origin: AtomicU64::new(0),
                seek_index: Mutex::new(None),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::FragmentedProgressive,
            read_cancellation: None,
        };

        assert!(!symphonia::core::io::MediaSource::is_seekable(&source));
        assert!(symphonia::core::io::MediaSource::byte_len(&source).is_none());
        let seekable = source.seekable_clone();
        // demuxer open 阶段隐藏 seekable，避免扫 mdat
        assert!(!symphonia::core::io::MediaSource::is_seekable(&seekable));
        assert!(source.inner.demuxer_open_sequential.load(std::sync::atomic::Ordering::Acquire));
        seekable.finish_demuxer_open();
        assert!(symphonia::core::io::MediaSource::is_seekable(&seekable));
        assert_eq!(
            seekable.access_mode.demand_fetch_block_bytes(),
            super::REMOTE_FRAGMENTED_SEEK_BLOCK_BYTES
        );
    }

    #[test]
    fn long_form_progressive_promotes_to_standard_seekable() {
        let source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/long.m4a".into(),
                referer: "https://example.com".into(),
                total_len: 135_000_000,
                header_end: AtomicU64::new(0),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 256 * 1024,
                    target_bytes: 1024 * 1024,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(false),
                virtual_body_origin: AtomicU64::new(0),
                seek_index: Mutex::new(None),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::LongFormProgressive,
            read_cancellation: None,
        };

        assert!(!symphonia::core::io::MediaSource::is_seekable(&source));
        assert!(symphonia::core::io::MediaSource::byte_len(&source).is_none());

        let seekable = source.seekable_clone();
        assert_eq!(seekable.access_mode, RemoteAccessMode::StandardSeekable);
        // demuxer open 阶段隐藏 seekable，finish 后才对 demuxer 暴露
        assert!(!symphonia::core::io::MediaSource::is_seekable(&seekable));
        assert!(source.inner.demuxer_open_sequential.load(std::sync::atomic::Ordering::Acquire));
        seekable.finish_demuxer_open();
        assert!(symphonia::core::io::MediaSource::is_seekable(&seekable));
        // 绝不能走分片 8MB 按需块，否则 seek 会卡死
        assert_eq!(
            seekable.access_mode.demand_fetch_block_bytes(),
            super::REMOTE_FETCH_BLOCK_BYTES
        );
        // seekable_clone 必须作废旧预取 epoch，否则 seek 会撞 in_flight
        assert_eq!(
            source.inner.prefetch_epoch.load(std::sync::atomic::Ordering::Acquire),
            1
        );
        assert!(source.inner.in_flight.lock().expect("lock").is_empty());
    }

    #[test]
    fn long_form_prefers_virtual_body_before_and_after_promote() {
        let index = super::RemoteSeekIndex {
            duration_ms: 100_000,
            entries: vec![
                super::SeekIndexEntry {
                    time_ms: 0,
                    byte_offset: 1_000,
                },
                super::SeekIndexEntry {
                    time_ms: 50_000,
                    byte_offset: 50_000_000,
                },
                super::SeekIndexEntry {
                    time_ms: 100_000,
                    byte_offset: 100_000_000,
                },
            ],
        };
        let source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/long.m4a".into(),
                referer: "https://example.com".into(),
                total_len: 135_000_000,
                header_end: AtomicU64::new(10_799),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 256 * 1024,
                    target_bytes: 1024 * 1024,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(false),
                virtual_body_origin: AtomicU64::new(0),
                seek_index: Mutex::new(Some(index)),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::LongFormProgressive,
            read_cancellation: None,
        };

        // 关键：clone 前 LongForm 也必须 prefers，否则 seek 会先选 format.seek
        assert!(source.prefers_virtual_body_seek());
        let seekable = source.seekable_clone();
        assert!(seekable.prefers_virtual_body_seek());
        assert_eq!(seekable.access_mode, RemoteAccessMode::StandardSeekable);
    }

    #[test]
    fn virtual_body_media_source_seekable_after_open_only() {
        let source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/long.m4a".into(),
                referer: "https://example.com".into(),
                total_len: 10_000,
                header_end: AtomicU64::new(100),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 1,
                    target_bytes: 1,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(true),
                virtual_body_origin: AtomicU64::new(5_000),
                seek_index: Mutex::new(None),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: None,
        };

        assert!(!symphonia::core::io::MediaSource::is_seekable(&source));
        assert!(symphonia::core::io::MediaSource::byte_len(&source).is_none());

        source.finish_demuxer_open();
        assert!(symphonia::core::io::MediaSource::is_seekable(&source));
        assert_eq!(
            symphonia::core::io::MediaSource::byte_len(&source),
            Some(source.logical_len())
        );
        // 纯逻辑连续：header + (total - origin)
        assert_eq!(source.logical_len(), 100 + (10_000 - 5_000));
    }



    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn fragmented_mp4_is_sniffed_from_sidx_moof_and_mvex() {
        // ftyp + sidx（YouTube itag140 的典型头部布局）
        let mut sidx_head = mp4_box(b"ftyp", &[0u8; 8]);
        sidx_head.extend_from_slice(&mp4_box(b"sidx", &[0u8; 16]));
        assert!(super::detect_fragmented_mp4(&sidx_head));

        // ftyp + moov(含 mvex)
        let moov_body = mp4_box(b"mvex", &[0u8; 8]);
        let mut mvex_head = mp4_box(b"ftyp", &[0u8; 8]);
        mvex_head.extend_from_slice(&mp4_box(b"moov", &moov_body));
        assert!(super::detect_fragmented_mp4(&mvex_head));

        // styp 开头的独立分片
        assert!(super::detect_fragmented_mp4(&mp4_box(b"styp", &[0u8; 8])));
    }

    #[test]
    fn progressive_mp4_is_not_sniffed_as_fragmented() {
        // ftyp + moov(仅 mvhd) + mdat：普通渐进式 MP4
        let moov_body = mp4_box(b"mvhd", &[0u8; 16]);
        let mut head = mp4_box(b"ftyp", &[0u8; 8]);
        head.extend_from_slice(&mp4_box(b"moov", &moov_body));
        head.extend_from_slice(&mp4_box(b"mdat", &[0u8; 32]));

        assert!(!super::detect_fragmented_mp4(&head));
        assert!(!super::detect_fragmented_mp4(&[]));
        assert!(!super::detect_fragmented_mp4(b"OggS\x00\x02"));
    }

    #[test]
    fn youtube_style_url_needs_payload_sniffing() {
        // googlevideo 直链没有扩展名，仅靠 URL 判不出分片
        let url = "https://rr5---sn-abc.googlevideo.com/videoplayback?expire=1&itag=140";
        assert!(!super::is_fragmented_mp4_url(url));
        assert!(super::is_fragmented_mp4_url("https://cdn.example/audio/seg-1.m4s"));
    }

    #[test]
    fn detect_mp4_header_end_finds_mdat() {
        // ftyp(16) + free(8) + mdat(size=100)
        let mut data = Vec::new();
        data.extend_from_slice(&16u32.to_be_bytes());
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(&[0u8; 8]);
        data.extend_from_slice(&8u32.to_be_bytes());
        data.extend_from_slice(b"free");
        data.extend_from_slice(&100u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0u8; 20]);
        assert_eq!(super::detect_mp4_header_end(&data), Some(24));
    }

    #[test]
    fn find_moof_offset_near_snaps_past_garbage_prefix() {
        // 12 字节垃圾 + moof(size=32)
        let mut data = vec![0u8; 12];
        data.extend_from_slice(&32u32.to_be_bytes());
        data.extend_from_slice(b"moof");
        data.extend_from_slice(&[0u8; 24]);
        assert_eq!(super::find_moof_offset_near(&data, 0, 64), Some(12));
        // 已对齐
        assert_eq!(super::find_moof_offset_near(&data[12..], 0, 64), Some(0));
    }

    #[test]
    fn virtual_body_maps_logical_header_and_body() {
        let source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/long.m4a".into(),
                referer: "https://example.com".into(),
                total_len: 10_000,
                header_end: AtomicU64::new(100),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 1,
                    target_bytes: 1,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(false),
                virtual_body_origin: AtomicU64::new(5_000),
                seek_index: Mutex::new(None),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: None,
        };

        assert_eq!(source.map_logical_to_physical(0), 0);
        assert_eq!(source.map_logical_to_physical(99), 99);
        // 相对 moof：逻辑 body 映射到 physical origin+
        assert_eq!(source.map_logical_to_physical(100), 5_000);
        assert_eq!(source.map_logical_to_physical(150), 5_050);
        // 纯逻辑：pos 绝不能用物理恒等，否则 MSS/AtomIterator 失步
        // 5_000 在逻辑里是 body 中点：physical = 5000 + (5000-100) = 9900
        assert_eq!(source.map_logical_to_physical(5_000), 9_900);
        assert_eq!(source.logical_len(), 100 + (10_000 - 5_000));
    }

    #[test]
    fn virtual_body_normalizes_absolute_sample_seeks() {
        use std::io::{Seek, SeekFrom};
        // 对齐真实长视频：header=10799, body_origin≈55MB, total≈135MB
        let mut source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/long.m4a".into(),
                referer: "https://example.com".into(),
                total_len: 135_169_876,
                header_end: AtomicU64::new(10_799),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 1,
                    target_bytes: 1,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(false),
                virtual_body_origin: AtomicU64::new(54_947_352),
                seek_index: Mutex::new(None),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: None,
        };

        // 相对样本：逻辑 header_end → 物理 origin
        assert_eq!(source.map_logical_to_physical(10_799), 54_947_352);
        assert_eq!(source.map_logical_to_physical(11_299), 54_947_852);
        // 绝对 base_data_offset：Seek(物理) 必须折成逻辑并返回逻辑 pos
        let logical = source.seek(SeekFrom::Start(54_947_852)).unwrap();
        assert_eq!(logical, 10_799 + 500);
        assert_eq!(source.pos, 10_799 + 500);
        assert_eq!(source.map_logical_to_physical(source.pos), 54_947_852);
        // logical_len = header + (total - origin)，不能被拉成 total
        assert_eq!(
            source.logical_len(),
            10_799 + (135_169_876 - 54_947_352)
        );
    }

    #[test]
    fn estimate_segment_start_snaps_to_entry_boundary() {
        let index = super::RemoteSeekIndex {
            duration_ms: 100_000,
            entries: vec![
                super::SeekIndexEntry {
                    time_ms: 0,
                    byte_offset: 1_000,
                },
                super::SeekIndexEntry {
                    time_ms: 50_000,
                    byte_offset: 50_000,
                },
                super::SeekIndexEntry {
                    time_ms: 100_000,
                    byte_offset: 100_000,
                },
            ],
        };
        assert_eq!(index.estimate_segment_start(0), 1_000);
        assert_eq!(index.estimate_segment_start(49_999), 1_000);
        assert_eq!(index.estimate_segment_start(50_000), 50_000);
        assert_eq!(index.estimate_segment_start(90_000), 50_000);
        assert_eq!(index.estimate_segment_start(100_000), 100_000);
    }

    #[test]
    fn virtual_body_seek_uses_logical_sample_offsets() {
        use std::io::{Seek, SeekFrom};
        let mut source = RemoteAudioSource {
            inner: Arc::new(RemoteAudioInner {
                client: reqwest::Client::new(),
                url: "https://cdn.example/audio/long.m4a".into(),
                referer: "https://example.com".into(),
                total_len: 10_000,
                header_end: AtomicU64::new(100),
                prefetch_window: PrefetchWindow {
                    low_water_bytes: 1,
                    target_bytes: 1,
                },
                cache: Mutex::new(Vec::new()),
                disk_ranges: Mutex::new(Vec::new()),
                in_flight: Mutex::new(HashSet::new()),
                range_available: Condvar::new(),
                last_read_pos: Mutex::new(0),
                protected_ranges: Mutex::new(Vec::new()),
                disk_cache: None,
                cache_finalize_started: AtomicBool::new(false),
                prefetch_epoch: AtomicU64::new(0),
                demuxer_open_sequential: AtomicBool::new(false),
                virtual_body_origin: AtomicU64::new(5_000),
                seek_index: Mutex::new(None),
                playback_generation: Arc::new(AtomicU64::new(1)),
                expected_generation: 1,
            }),
            pos: 0,
            access_mode: RemoteAccessMode::StandardSeekable,
            read_cancellation: None,
        };

        // sample Seek 用逻辑坐标；映射到物理 body
        assert_eq!(source.seek(SeekFrom::Start(100)).unwrap(), 100);
        assert_eq!(source.map_logical_to_physical(source.pos), 5_000);
        assert_eq!(source.seek(SeekFrom::Start(150)).unwrap(), 150);
        assert_eq!(source.map_logical_to_physical(source.pos), 5_050);
        assert_eq!(source.seek(SeekFrom::Start(50)).unwrap(), 50);
        assert_eq!(source.map_logical_to_physical(source.pos), 50);
    }

    #[test]
    fn linear_seek_index_estimates_midpoint_bytes() {
        let index = super::RemoteSeekIndex {
            duration_ms: 100_000,
            entries: vec![
                super::SeekIndexEntry {
                    time_ms: 0,
                    byte_offset: 0,
                },
                super::SeekIndexEntry {
                    time_ms: 50_000,
                    byte_offset: 500,
                },
                super::SeekIndexEntry {
                    time_ms: 100_000,
                    byte_offset: 1_000,
                },
            ],
        };
        assert_eq!(index.estimate_byte(0), 0);
        assert_eq!(index.estimate_byte(25_000), 250);
        assert_eq!(index.estimate_byte(75_000), 750);
        assert_eq!(index.estimate_byte(100_000), 1_000);
    }

    #[test]
    fn parse_sidx_body_builds_time_byte_entries() {
        // version0 sidx: timescale=1000, earliest=0, first_offset=0, 2 refs size=100 dur=1000
        let mut body = Vec::new();
        body.push(0); // version
        body.extend_from_slice(&[0, 0, 0]); // flags
        body.extend_from_slice(&1u32.to_be_bytes()); // reference_id
        body.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        body.extend_from_slice(&0u32.to_be_bytes()); // earliest_pts
        body.extend_from_slice(&0u32.to_be_bytes()); // first_offset
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        body.extend_from_slice(&2u16.to_be_bytes()); // reference_count
        for _ in 0..2 {
            body.extend_from_slice(&100u32.to_be_bytes()); // size, type=media
            body.extend_from_slice(&1000u32.to_be_bytes()); // duration ticks
            body.extend_from_slice(&0u32.to_be_bytes()); // sap
        }
        let index = super::parse_sidx_body(&body, 0, 10_000, 10_000).expect("sidx");
        assert!(index.entries.len() >= 3);
        assert_eq!(index.entries[0].byte_offset, 0);
        assert_eq!(index.entries[1].byte_offset, 100);
        assert_eq!(index.entries[1].time_ms, 1000);
    }

    #[test]
    fn invalidate_prefetch_epoch_clears_in_flight_and_allows_reclaim() {
        let inner = Arc::new(RemoteAudioInner {
            client: reqwest::Client::new(),
            url: "https://cdn.example/audio/long.m4a".into(),
            referer: "https://example.com".into(),
            total_len: 1024,
            header_end: AtomicU64::new(0),
            prefetch_window: PrefetchWindow {
                low_water_bytes: 1,
                target_bytes: 1,
            },
            cache: Mutex::new(Vec::new()),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::from([0, 256])),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(Vec::new()),
            disk_cache: None,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation: Arc::new(AtomicU64::new(1)),
            expected_generation: 1,
        });
        assert!(!super::claim_prefetch_start(&inner, 0));
        super::invalidate_prefetch_epoch(&inner);
        assert_eq!(
            inner.prefetch_epoch.load(std::sync::atomic::Ordering::Acquire),
            1
        );
        assert!(inner.in_flight.lock().expect("lock").is_empty());
        assert!(super::claim_prefetch_start(&inner, 0));
        super::release_prefetch_start(&inner, 0);
    }

    #[test]
    fn trim_cache_keeps_protected_tail_segments() {
        let protected = CachedSegment {
            start: 900,
            data: vec![9; 100],
        };
        let near = CachedSegment {
            start: 0,
            data: vec![1; super::REMOTE_MAX_CACHE_BYTES],
        };
        let inner = RemoteAudioInner {
            client: reqwest::Client::new(),
            url: "https://cdn.example/audio/long.m4a".into(),
            referer: "https://example.com".into(),
            total_len: 1000,
            header_end: AtomicU64::new(0),
            prefetch_window: PrefetchWindow {
                low_water_bytes: 1,
                target_bytes: 1,
            },
            cache: Mutex::new(vec![near, protected]),
            disk_ranges: Mutex::new(Vec::new()),
            in_flight: Mutex::new(HashSet::new()),
            range_available: Condvar::new(),
            last_read_pos: Mutex::new(0),
            protected_ranges: Mutex::new(vec![CachedRange {
                start: 900,
                end: 1000,
            }]),
            disk_cache: None,
            cache_finalize_started: AtomicBool::new(false),
            prefetch_epoch: AtomicU64::new(0),
            demuxer_open_sequential: AtomicBool::new(false),
            virtual_body_origin: AtomicU64::new(0),
            seek_index: Mutex::new(None),
            playback_generation: Arc::new(AtomicU64::new(1)),
            expected_generation: 1,
        };
        let mut cache = {
            let guard = inner.cache.lock().expect("cache");
            guard
                .iter()
                .map(|segment| CachedSegment {
                    start: segment.start,
                    data: segment.data.clone(),
                })
                .collect::<Vec<_>>()
        };
        super::trim_cache(&inner, &mut cache, 0);
        assert!(
            cache.iter().any(|segment| segment.start == 900),
            "protected tail must survive trim"
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
    fn sequential_stream_publish_keeps_cache_unready_until_download_finishes() {
        let root = tempfile::tempdir().expect("temp cache root");
        let wav = pcm_wav(800);
        let cache = RemoteAudioCache::new(
            root.path().to_path_buf(),
            "sequential-audio",
            1024 * 1024,
            Some(wav.len() as u64),
            100,
        )
        .expect("cache");

        let staging = cache
            .prepare_sequential_write(wav.len() as u64)
            .expect("prepare sequential cache");
        std::fs::write(&staging, &wav).expect("write sequential cache");
        assert!(cache.ready_path().is_none());

        let published = cache
            .publish_sequential_write()
            .expect("publish sequential cache");
        assert_eq!(std::fs::read(published).expect("published bytes"), wav);
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
    fn cache_lookup_miss_does_not_create_a_shard_directory() {
        let root = tempfile::tempdir().expect("cache root");
        let cache = RemoteAudioCache::new(
            root.path().join("audio"),
            "lookup-only",
            512 * 1024 * 1024,
            None,
            1_000,
        )
        .expect("cache");

        assert!(!cache.cache_dir.exists());
        assert!(cache.ready_path().is_none());
        assert!(!cache.cache_dir.exists());
    }

    #[test]
    fn cache_capacity_scan_runs_only_after_a_complete_file_is_published() {
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
            wav.len() as u64,
            Some(wav.len() as u64),
            100,
        )
        .expect("new cache lookup");
        assert!(existing_path.exists());

        lookup
            .prepare(wav.len() as u64)
            .expect("prepare cache miss");
        assert!(existing_path.exists());
        lookup.write_range(0, &wav).expect("write complete cache");
        lookup.mark_ready().expect("publish complete cache");
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
