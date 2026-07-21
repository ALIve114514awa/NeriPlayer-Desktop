use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::audio::analyzer::{AudioAnalyzer, SharedAudioLevel};
use crate::audio::buffered::PcmRing;
use crate::audio::effects::{AudioEffectsParams, EqualizerSource, LoudnessSource};
use crate::audio::growing::GrowingAudioReader;
use crate::audio::pcm::PcmSource;
use crate::audio::remote::{
    RemoteAudioSource, RemoteReadCancellation, SymphoniaAudioDecoder,
};
use crate::error::{AppError, AppResult};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
// 远程/超长媒体首帧准备超时：超时必须取消读，否则音频线程卡死会堵住后续 fallback
const REMOTE_PREPARE_TIMEOUT: Duration = Duration::from_secs(12);
// 超长远程 seek 重建 demuxer 可能需要拉尾部 moov，单独放宽
const REMOTE_SEEK_TIMEOUT: Duration = Duration::from_secs(45);
const LOCAL_PREBUFFER: Duration = Duration::from_millis(80);
const GROWING_PREBUFFER: Duration = Duration::from_millis(800);
const REMOTE_PREBUFFER: Duration = Duration::from_millis(400);
const REBUFFER_TARGET: Duration = Duration::from_millis(1_500);
// 音效/倍速切换后的短恢复目标：避免 underrun 后卡 1.5s
const EFFECTS_RECOVER_TARGET: Duration = Duration::from_millis(120);
const PCM_CAPACITY: Duration = Duration::from_secs(4);
const DECODE_IDLE_SLEEP: Duration = Duration::from_millis(2);
const READY_POLL: Duration = Duration::from_millis(20);
const FADE_STEP: Duration = Duration::from_millis(10);
const ANALYSIS_FRAME_SIZE: usize = 2_048;
const PLAYBACK_SUPERSEDED: &str = "Playback request superseded";
const SEEK_SUPERSEDED: &str = "Seek request superseded";

#[derive(Clone)]
struct GenerationToken {
    generation: Arc<AtomicU64>,
    expected: u64,
}

impl GenerationToken {
    fn is_current(&self) -> bool {
        self.generation.load(Ordering::Acquire) == self.expected
    }
}

#[derive(Clone)]
enum AudioSource {
    Bytes(Arc<[u8]>, u64),
    File(String, u64),
    Growing(GrowingAudioReader, u64),
    Remote(RemoteAudioSource, u64),
}

impl AudioSource {
    fn duration_hint_ms(&self) -> u64 {
        match self {
            Self::Bytes(_, hint)
            | Self::File(_, hint)
            | Self::Growing(_, hint)
            | Self::Remote(_, hint) => *hint,
        }
    }

    fn prebuffer_duration(&self) -> Duration {
        match self {
            Self::Growing(_, _) => GROWING_PREBUFFER,
            Self::Remote(_, _) => REMOTE_PREBUFFER,
            Self::Bytes(_, _) | Self::File(_, _) => LOCAL_PREBUFFER,
        }
    }

    fn abort_if_stream(&self) {
        if let Self::Growing(reader, _) = self {
            reader.abort();
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Bytes(_, _) => "bytes",
            Self::File(_, _) => "file",
            Self::Growing(_, _) => "growing",
            Self::Remote(_, _) => "remote",
        }
    }

    fn decoder_source_for_position(
        &self,
        position_ms: u64,
        read_cancellation: RemoteReadCancellation,
    ) -> Self {
        match self {
            Self::Remote(reader, hint) => {
                let reader = if position_ms == 0 {
                    reader.clone()
                } else {
                    reader.seekable_clone()
                };
                Self::Remote(reader.with_read_cancellation(read_cancellation), *hint)
            }
            _ => self.clone(),
        }
    }

    fn prefers_remote_virtual_body_seek(&self) -> bool {
        matches!(self, Self::Remote(reader, _) if reader.prefers_virtual_body_seek())
    }
}

#[derive(Clone, Copy)]
enum PlayTransition {
    Replace,
    Crossfade { fade_out_ms: u32, fade_in_ms: u32 },
}

enum AudioCmd {
    Play {
        source: AudioSource,
        start_position_ms: u64,
        transition: PlayTransition,
        playback_generation: u64,
        transition_generation: u64,
        /// 外部等待超时后置位，打断 remote make_decoder / prebuffer
        prepare_cancel: Arc<AtomicBool>,
        reply: mpsc::Sender<Result<PlaybackStarted, String>>,
    },
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
    SetSpeed(f32),
    Seek {
        position_ms: u64,
        playback_generation: u64,
        seek_generation: u64,
        reply: mpsc::Sender<Result<(), String>>,
    },
    QueryEmpty { reply: mpsc::Sender<bool> },
    /// 丢掉 ring 中过期的已处理样本，使 EQ/响度/倍速近似实时
    InvalidateProcessedBuffer,
    FadeOutPause {
        duration_ms: u32,
        transition_generation: u64,
        reply: mpsc::Sender<Result<(), String>>,
    },
    FadeInResume {
        duration_ms: u32,
        transition_generation: u64,
        reply: mpsc::Sender<Result<(), String>>,
    },
}

/// 参数变更后保留的已缓冲时长：够连续播放，又足够实时
// 保留极短已处理尾部，目标 <0.5s 内听到新音效/倍速
const EFFECTS_KEEP_BUFFER: Duration = Duration::from_millis(40);

#[derive(Clone)]
pub struct PlaybackStarted {
    pub duration_ms: u64,
    pub clock: Arc<PlaybackClock>,
}

pub struct PlaybackClock {
    position_us: AtomicU64,
}

impl PlaybackClock {
    fn new(position_ms: u64) -> Self {
        Self {
            position_us: AtomicU64::new(position_ms.saturating_mul(1_000)),
        }
    }

    fn position_ms(&self) -> u64 {
        self.position_us.load(Ordering::Acquire) / 1_000
    }

    fn store_ms(&self, position_ms: u64) {
        self.position_us
            .store(position_ms.saturating_mul(1_000), Ordering::Release);
    }
}

struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Acquire))
    }

    fn store(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Release);
    }
}

struct PlaybackShared {
    ring: Arc<PcmRing>,
    channels: usize,
    sample_rate: u32,
    paused: AtomicBool,
    buffering: AtomicBool,
    finished: AtomicBool,
    cancelled: Arc<AtomicBool>,
    volume: AtomicF32,
    fade_gain: AtomicF32,
    speed: AtomicF32,
    buffer_target_frames: AtomicUsize,
    clock: Arc<PlaybackClock>,
    wake_lock: Mutex<()>,
    wake: Condvar,
}

impl PlaybackShared {
    fn buffered_frames(&self) -> usize {
        self.ring.readable_samples() / self.channels.max(1)
    }

    fn rebuffer_frames(&self) -> usize {
        duration_to_frames(REBUFFER_TARGET, self.sample_rate)
    }

    fn soft_recover_frames(&self) -> usize {
        duration_to_frames(EFFECTS_RECOVER_TARGET, self.sample_rate).max(1)
    }

    fn update_buffering_state(&self) {
        if !self.buffering.load(Ordering::Acquire) {
            return;
        }
        let target = self.buffer_target_frames.load(Ordering::Acquire);
        if self.buffered_frames() >= target || self.finished.load(Ordering::Acquire) {
            self.buffering.store(false, Ordering::Release);
            // 软恢复结束后恢复正常 underrun 目标，避免后续网络卡顿只缓冲 120ms
            let soft = self.soft_recover_frames();
            if target > 0 && target <= soft.saturating_mul(3) {
                self.buffer_target_frames
                    .store(self.rebuffer_frames(), Ordering::Release);
            }
            self.wake.notify_all();
        }
    }

    fn begin_rebuffering(&self) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        // 音效/倍速刚切过（目标已是软恢复量级）时，underrun 不要拉回 1.5s
        let soft = self.soft_recover_frames();
        let current = self.buffer_target_frames.load(Ordering::Acquire);
        let target = if current > 0 && current <= soft.saturating_mul(3) {
            soft
        } else {
            self.rebuffer_frames()
        };
        self.buffer_target_frames
            .store(target, Ordering::Release);
        self.buffering.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    /// 丢弃 ring 中过旧的已处理 PCM，只留极短尾部；用于 EQ/响度/倍速即时生效
    fn invalidate_processed_buffer(&self, clock_speed: f32) {
        let keep_frames = duration_to_frames(EFFECTS_KEEP_BUFFER, self.sample_rate);
        let keep_samples = keep_frames.saturating_mul(self.channels);
        let before = self.ring.readable_samples();
        self.ring.discard_keeping_tail(keep_samples);
        let after = self.ring.readable_samples();
        let dropped = before.saturating_sub(after);
        if dropped > 0 {
            let dropped_frames = dropped / self.channels.max(1);
            let speed = clock_speed.clamp(0.25, 3.0);
            let advance_us = ((dropped_frames as f64 * 1_000_000.0 / f64::from(self.sample_rate))
                * f64::from(speed)) as u64;
            self.clock
                .position_us
                .fetch_add(advance_us, Ordering::AcqRel);
        }
        // 软恢复目标：若 underrun 只补 ~120ms，不主动静音
        let recover = self.soft_recover_frames().max(keep_frames).max(1);
        self.buffer_target_frames
            .store(recover, Ordering::Release);
        self.buffering.store(false, Ordering::Release);
        self.wake.notify_all();
    }
}

struct PlaybackSession {
    source: AudioSource,
    stream: Stream,
    shared: Arc<PlaybackShared>,
    worker: Option<JoinHandle<()>>,
    duration_ms: u64,
    playback_generation: u64,
}

struct OutputDeviceProfile {
    device: Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    name: String,
}

impl OutputDeviceProfile {
    fn open_default() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No default audio output device".to_string())?;
        let name = device
            .name()
            .unwrap_or_else(|_| "default output".to_string());
        let supported = device
            .default_output_config()
            .map_err(|error| format!("Could not query output format: {error}"))?;
        let sample_format = supported.sample_format();
        let config = supported.into();
        Ok(Self {
            device,
            config,
            sample_format,
            name,
        })
    }
}

impl PlaybackSession {
    fn play(&self) -> Result<(), String> {
        self.shared.paused.store(false, Ordering::Release);
        self.stream
            .play()
            .map_err(|error| format!("Could not start audio output: {error}"))
    }

    fn pause(&self) {
        self.shared.paused.store(true, Ordering::Release);
        if let Err(error) = self.stream.pause() {
            log::warn!(target: "cpal-output", "pause failed: {error}");
        }
    }

    fn stop(&mut self) {
        self.source.abort_if_stream();
        self.shared.cancelled.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        let _ = self.stream.pause();
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
            && self.shared.ring.readable_samples() < self.shared.channels
    }
}

impl Drop for PlaybackSession {
    fn drop(&mut self) {
        self.stop();
    }
}

struct FrameResampler {
    source: Box<dyn PcmSource>,
    source_rate: u32,
    current: Vec<f32>,
    next: Vec<f32>,
    phase: f64,
    initialized: bool,
    source_ended: bool,
    finished: bool,
}

impl FrameResampler {
    fn new(source: Box<dyn PcmSource>) -> Self {
        let source_channels = usize::from(source.channels().max(1));
        let source_rate = source.sample_rate().max(1);
        Self {
            source,
            source_rate,
            current: vec![0.0; source_channels],
            next: vec![0.0; source_channels],
            phase: 0.0,
            initialized: false,
            source_ended: false,
            finished: false,
        }
    }

    fn read_source_frame(source: &mut dyn PcmSource, frame: &mut [f32]) -> bool {
        for sample in frame {
            let Some(value) = source.next() else {
                return false;
            };
            *sample = value;
        }
        true
    }

    fn initialize(&mut self) -> bool {
        if self.initialized {
            return !self.finished;
        }
        if !Self::read_source_frame(self.source.as_mut(), &mut self.current) {
            self.finished = true;
            return false;
        }
        if !Self::read_source_frame(self.source.as_mut(), &mut self.next) {
            self.next.copy_from_slice(&self.current);
            self.source_ended = true;
        }
        self.initialized = true;
        true
    }

    fn next_frame(&mut self, output_rate: u32, speed: f32, output: &mut [f32]) -> bool {
        if self.finished || !self.initialize() {
            return false;
        }

        let phase = self.phase as f32;
        let output_channels = output.len();
        for (channel, sample) in output.iter_mut().enumerate() {
            let current = channel_sample(&self.current, channel, output_channels);
            let next = channel_sample(&self.next, channel, output_channels);
            *sample = current + (next - current) * phase;
        }

        self.phase += f64::from(self.source_rate) * f64::from(speed.clamp(0.25, 3.0))
            / f64::from(output_rate.max(1));
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            if self.source_ended {
                self.finished = true;
                break;
            }
            self.current.copy_from_slice(&self.next);
            if !Self::read_source_frame(self.source.as_mut(), &mut self.next) {
                self.next.copy_from_slice(&self.current);
                self.source_ended = true;
            }
        }
        true
    }
}

fn channel_sample(frame: &[f32], output_channel: usize, output_channels: usize) -> f32 {
    match (frame.len(), output_channels) {
        (1, _) => frame[0],
        (_, 1) => frame.iter().copied().sum::<f32>() / frame.len() as f32,
        _ => frame.get(output_channel).copied().unwrap_or(0.0),
    }
}

/// 播放请求的中间状态，持有 reply 接收端，可在锁外等待
pub struct PlayRequest {
    reply_rx: mpsc::Receiver<Result<PlaybackStarted, String>>,
    pub expected_generation: u64,
    pub current_path: String,
    fade_ms: u32,
    /// 与音频线程 prepare 共享；等待超时后置位以释放音频线程
    prepare_cancel: Arc<AtomicBool>,
    is_remote: bool,
}

pub struct PlayerEngine {
    cmd_tx: mpsc::Sender<AudioCmd>,
    thread_alive: Arc<AtomicBool>,
    pub is_playing: bool,
    pub volume: f32,
    pub speed: f32,
    pub current_path: Option<String>,
    pub duration_ms: u64,
    pub shared_audio_level: Arc<Mutex<SharedAudioLevel>>,
    pub effects_params: Arc<Mutex<AudioEffectsParams>>,
    playback_generation: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
    transition_generation: Arc<AtomicU64>,
    loaded_generation: Option<u64>,
    clock: Option<Arc<PlaybackClock>>,
}

impl Default for PlayerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerEngine {
    pub fn new() -> Self {
        Self::with_playback_generation(Arc::new(AtomicU64::new(0)))
    }

    pub fn with_playback_generation(playback_generation: Arc<AtomicU64>) -> Self {
        let shared_audio_level = SharedAudioLevel::new();
        let effects_params = AudioEffectsParams::new_shared();
        let seek_generation = Arc::new(AtomicU64::new(0));
        let transition_generation = Arc::new(AtomicU64::new(0));
        let (cmd_tx, thread_alive) = spawn_audio_thread(
            Arc::clone(&shared_audio_level),
            Arc::clone(&effects_params),
            Arc::clone(&playback_generation),
            Arc::clone(&seek_generation),
            Arc::clone(&transition_generation),
        );
        Self {
            cmd_tx,
            thread_alive,
            is_playing: false,
            volume: 1.0,
            speed: 1.0,
            current_path: None,
            duration_ms: 0,
            shared_audio_level,
            effects_params,
            playback_generation,
            seek_generation,
            transition_generation,
            loaded_generation: None,
            clock: None,
        }
    }

    fn ensure_alive(&mut self) {
        if self.thread_alive.load(Ordering::Acquire) {
            return;
        }
        log::warn!(target: "cpal-output", "audio thread stopped, restarting");
        let (cmd_tx, thread_alive) = spawn_audio_thread(
            Arc::clone(&self.shared_audio_level),
            Arc::clone(&self.effects_params),
            Arc::clone(&self.playback_generation),
            Arc::clone(&self.seek_generation),
            Arc::clone(&self.transition_generation),
        );
        self.cmd_tx = cmd_tx;
        self.thread_alive = thread_alive;
        let _ = self.cmd_tx.send(AudioCmd::SetVolume(self.volume));
        let _ = self.cmd_tx.send(AudioCmd::SetSpeed(self.speed));
    }

    fn ensure_request_current(&self, expected: u64) -> AppResult<()> {
        if self.playback_generation.load(Ordering::Acquire) == expected {
            Ok(())
        } else {
            Err(AppError::Audio(PLAYBACK_SUPERSEDED.into()))
        }
    }

    /// 发送播放命令到音频线程，不等待结果。返回 PlayRequest 供锁外等待。
    fn request_start_source(
        &mut self,
        source: AudioSource,
        start_position_ms: u64,
        transition: PlayTransition,
        expected_generation: u64,
        current_path: String,
    ) -> AppResult<PlayRequest> {
        self.ensure_alive();
        self.ensure_request_current(expected_generation)?;
        let transition_generation =
            self.transition_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let prepare_cancel = Arc::new(AtomicBool::new(false));
        let is_remote = matches!(source, AudioSource::Remote(_, _) | AudioSource::Growing(_, _));
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::Play {
                source,
                start_position_ms,
                transition,
                playback_generation: expected_generation,
                transition_generation,
                prepare_cancel: Arc::clone(&prepare_cancel),
                reply: reply_tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let fade_ms = match transition {
            PlayTransition::Replace => 0,
            PlayTransition::Crossfade {
                fade_out_ms,
                fade_in_ms,
            } => fade_out_ms.max(fade_in_ms),
        };
        Ok(PlayRequest {
            reply_rx,
            expected_generation,
            current_path,
            fade_ms,
            prepare_cancel,
            is_remote,
        })
    }

    /// 用音频线程返回的结果更新播放器状态
    pub fn complete_start(&mut self, result: PlaybackStarted, expected_generation: u64, current_path: String) -> AppResult<u64> {
        self.ensure_request_current(expected_generation)?;
        self.is_playing = true;
        self.current_path = Some(current_path);
        self.duration_ms = result.duration_ms;
        self.loaded_generation = Some(expected_generation);
        self.clock = Some(result.clock);
        Ok(result.duration_ms)
    }

    /// 便捷方法：发送命令并阻塞等待（持锁整个过程，仅限内部无并发要求场景）
    fn start_source(
        &mut self,
        source: AudioSource,
        start_position_ms: u64,
        transition: PlayTransition,
        expected_generation: u64,
        current_path: String,
    ) -> AppResult<u64> {
        let request = self.request_start_source(
            source, start_position_ms, transition, expected_generation, current_path,
        )?;
        let started = wait_for_play_result(&request)?;
        self.complete_start(started, request.expected_generation, request.current_path)
    }

    pub fn play_file(&mut self, path: &str, generation: u64) -> AppResult<u64> {
        self.play_file_at_with_hint(path, 0, 0, generation)
    }

    pub fn play_file_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.play_file_at_with_hint(path, duration_hint_ms, 0, generation)
    }

    pub fn play_file_at(
        &mut self,
        path: &str,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.play_file_at_with_hint(path, 0, start_position_ms, generation)
    }

    pub fn play_file_at_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::File(path.into(), duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            path.into(),
        )
    }

    pub fn play_bytes(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.play_bytes_at(data, duration_hint_ms, 0, generation)
    }

    pub fn play_bytes_at(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::Bytes(Arc::<[u8]>::from(data), duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            "__bytes__".into(),
        )
    }

    pub fn play_stream(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.play_stream_at(reader, duration_hint_ms, 0, generation)
    }

    pub fn play_stream_at(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::Growing(reader, duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            "__growing__".into(),
        )
    }

    pub fn play_remote_at(
        &mut self,
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::Remote(reader, duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            "__remote__".into(),
        )
    }

    pub fn position_ms(&self) -> u64 {
        self.clock
            .as_ref()
            .map(|clock| clock.position_ms())
            .unwrap_or(0)
            .min(self.duration_ms.max(1))
    }

    pub fn pause(&mut self) {
        if self.is_playing {
            self.transition_generation.fetch_add(1, Ordering::AcqRel);
            let _ = self.cmd_tx.send(AudioCmd::Pause);
            self.is_playing = false;
        }
    }

    pub fn resume(&mut self) {
        if !self.is_playing && self.current_path.is_some() {
            self.transition_generation.fetch_add(1, Ordering::AcqRel);
            let _ = self.cmd_tx.send(AudioCmd::Resume);
            self.is_playing = true;
        }
    }

    pub fn stop(&mut self) {
        self.transition_generation.fetch_add(1, Ordering::AcqRel);
        let _ = self.cmd_tx.send(AudioCmd::Stop);
        self.is_playing = false;
        self.current_path = None;
        self.duration_ms = 0;
        self.loaded_generation = None;
        self.clock = None;
        SharedAudioLevel::reset(&self.shared_audio_level);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        let _ = self.cmd_tx.send(AudioCmd::SetVolume(self.volume));
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(0.25, 3.0);
        // SetSpeed 内部按旧速丢 ring 并补时钟，无需再发 Invalidate
        let _ = self.cmd_tx.send(AudioCmd::SetSpeed(self.speed));
    }

    pub fn set_loudness_gain(&self, millibels: i32) {
        if let Ok(mut params) = self.effects_params.lock() {
            params.loudness_gain_mb = millibels.clamp(0, 1_500);
        }
        let _ = self.cmd_tx.send(AudioCmd::InvalidateProcessedBuffer);
    }

    pub fn set_equalizer(&self, enabled: bool, bands: &[i32]) {
        if let Ok(mut params) = self.effects_params.lock() {
            params.eq_enabled = enabled;
            for (index, value) in bands.iter().copied().enumerate().take(5) {
                params.eq_band_levels_mb[index] = value.clamp(-1_500, 1_500);
            }
        }
        let _ = self.cmd_tx.send(AudioCmd::InvalidateProcessedBuffer);
    }

    pub fn reset_effects(&self) {
        if let Ok(mut params) = self.effects_params.lock() {
            params.reset();
        }
        let _ = self.cmd_tx.send(AudioCmd::InvalidateProcessedBuffer);
    }

    pub fn request_seek(
        &mut self,
        position_ms: u64,
        request_generation: u64,
    ) -> AppResult<mpsc::Receiver<Result<(), String>>> {
        self.ensure_alive();
        self.ensure_request_current(request_generation)?;
        if self.loaded_generation != Some(request_generation) {
            return Err(AppError::Audio(PLAYBACK_SUPERSEDED.into()));
        }
        let position_ms = clamp_position(position_ms, self.duration_ms);
        self.transition_generation.fetch_add(1, Ordering::AcqRel);
        let seek_generation = self.seek_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::Seek {
                position_ms,
                playback_generation: request_generation,
                seek_generation,
                reply: reply_tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;
        Ok(reply_rx)
    }

    pub fn loaded_generation(&self) -> Option<u64> {
        self.loaded_generation
    }

    pub fn is_finished(&self) -> bool {
        if !self.is_playing || self.position_ms() < 500 {
            return false;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(AudioCmd::QueryEmpty { reply: reply_tx })
            .is_err()
        {
            return true;
        }
        reply_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or(false)
    }

    pub fn mark_ended(&mut self) {
        self.is_playing = false;
    }

    pub fn pause_with_fade(&mut self, duration_ms: u32) -> AppResult<()> {
        self.ensure_alive();
        let transition_generation =
            self.transition_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::FadeOutPause {
                duration_ms,
                transition_generation,
                reply: reply_tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;
        let result = receive_fade_result(reply_rx, duration_ms);
        if result.is_ok() {
            self.is_playing = false;
        }
        result
    }

    pub fn resume_with_fade(&mut self, duration_ms: u32) -> AppResult<()> {
        self.ensure_alive();
        let transition_generation =
            self.transition_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::FadeInResume {
                duration_ms,
                transition_generation,
                reply: reply_tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;
        let result = receive_fade_result(reply_rx, duration_ms);
        if result.is_ok() {
            self.is_playing = true;
        }
        result
    }

    pub fn crossfade_bytes(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::Bytes(Arc::<[u8]>::from(data), duration_hint_ms),
            0,
            PlayTransition::Crossfade {
                fade_out_ms,
                fade_in_ms,
            },
            generation,
            "__bytes__".into(),
        )
    }

    pub fn crossfade_file(
        &mut self,
        path: &str,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<u64> {
        self.crossfade_file_with_hint(path, 0, fade_out_ms, fade_in_ms, generation)
    }

    pub fn crossfade_file_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::File(path.into(), duration_hint_ms),
            0,
            PlayTransition::Crossfade {
                fade_out_ms,
                fade_in_ms,
            },
            generation,
            path.into(),
        )
    }

    pub fn crossfade_stream(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::Growing(reader, duration_hint_ms),
            0,
            PlayTransition::Crossfade {
                fade_out_ms,
                fade_in_ms,
            },
            generation,
            "__growing__".into(),
        )
    }

    pub fn crossfade_remote(
        &mut self,
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<u64> {
        self.start_source(
            AudioSource::Remote(reader, duration_hint_ms),
            0,
            PlayTransition::Crossfade {
                fade_out_ms,
                fade_in_ms,
            },
            generation,
            "__remote__".into(),
        )
    }

    // request_* 变体：只发命令不等待，供 cmd 层锁外等待
    pub fn request_play_file_at_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::File(path.into(), duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            path.into(),
        )
    }

    pub fn request_play_file_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_play_file_at_with_hint(path, duration_hint_ms, 0, generation)
    }

    pub fn request_play_bytes_at(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::Bytes(Arc::<[u8]>::from(data), duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            "__bytes__".into(),
        )
    }

    pub fn request_play_stream_at(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::Growing(reader, duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            "__growing__".into(),
        )
    }

    pub fn request_play_remote_at(
        &mut self,
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        start_position_ms: u64,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::Remote(reader, duration_hint_ms),
            start_position_ms,
            PlayTransition::Replace,
            generation,
            "__remote__".into(),
        )
    }

    pub fn request_crossfade_file_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::File(path.into(), duration_hint_ms),
            0,
            PlayTransition::Crossfade { fade_out_ms, fade_in_ms },
            generation,
            path.into(),
        )
    }

    pub fn request_crossfade_bytes(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::Bytes(Arc::<[u8]>::from(data), duration_hint_ms),
            0,
            PlayTransition::Crossfade { fade_out_ms, fade_in_ms },
            generation,
            "__bytes__".into(),
        )
    }

    pub fn request_crossfade_stream(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::Growing(reader, duration_hint_ms),
            0,
            PlayTransition::Crossfade { fade_out_ms, fade_in_ms },
            generation,
            "__growing__".into(),
        )
    }

    pub fn request_crossfade_remote(
        &mut self,
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        generation: u64,
    ) -> AppResult<PlayRequest> {
        self.request_start_source(
            AudioSource::Remote(reader, duration_hint_ms),
            0,
            PlayTransition::Crossfade { fade_out_ms, fade_in_ms },
            generation,
            "__remote__".into(),
        )
    }
}

fn spawn_audio_thread(
    shared_level: Arc<Mutex<SharedAudioLevel>>,
    effects_params: Arc<Mutex<AudioEffectsParams>>,
    playback_generation: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
    transition_generation: Arc<AtomicU64>,
) -> (mpsc::Sender<AudioCmd>, Arc<AtomicBool>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_for_thread = Arc::clone(&alive);
    thread::Builder::new()
        .name("cpal-playback-control".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                audio_control_loop(
                    cmd_rx,
                    shared_level,
                    effects_params,
                    playback_generation,
                    seek_generation,
                    transition_generation,
                );
            }));
            if let Err(error) = result {
                log::error!(target: "cpal-output", "control thread panicked: {error:?}");
            }
            alive_for_thread.store(false, Ordering::Release);
        })
        .expect("Could not start CPAL playback control thread");
    (cmd_tx, alive)
}

fn audio_control_loop(
    receiver: mpsc::Receiver<AudioCmd>,
    shared_level: Arc<Mutex<SharedAudioLevel>>,
    effects_params: Arc<Mutex<AudioEffectsParams>>,
    playback_generation: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
    transition_generation: Arc<AtomicU64>,
) {
    let mut current: Option<PlaybackSession> = None;
    let mut volume = 1.0f32;
    let mut speed = 1.0f32;
    let mut deferred = None;
    let mut output_profile = match OutputDeviceProfile::open_default() {
        Ok(profile) => {
            log::info!(
                target: "cpal-output",
                "prewarmed {}: {} Hz, {} ch",
                profile.name,
                profile.config.sample_rate.0,
                profile.config.channels
            );
            Some(profile)
        }
        Err(error) => {
            log::warn!(target: "cpal-output", "device prewarm deferred: {error}");
            None
        }
    };

    loop {
        let command = match deferred.take() {
            Some(command) => command,
            None => match receiver.recv() {
                Ok(command) => command,
                Err(_) => break,
            },
        };
        match command {
            AudioCmd::Play {
                source,
                start_position_ms,
                transition,
                playback_generation: expected,
                transition_generation: expected_transition,
                prepare_cancel,
                reply,
            } => {
                let source_label = source.label();
                let transition_label = match transition {
                    PlayTransition::Replace => "replace",
                    PlayTransition::Crossfade { .. } => "crossfade",
                };
                let command_started = Instant::now();
                log::info!(
                    target: "cpal-output",
                    "play command received source={}, generation={}, start_ms={}, transition={}",
                    source_label,
                    expected,
                    start_position_ms,
                    transition_label,
                );
                let prepared = (|| {
                    if prepare_cancel.load(Ordering::Acquire) {
                        return Err("Timed out waiting for decoded audio".into());
                    }
                    let output = ensure_output_profile(&mut output_profile)?;
                    prepare_session(
                        source,
                        start_position_ms,
                        volume,
                        speed,
                        Arc::clone(&shared_level),
                        Arc::clone(&effects_params),
                        Arc::clone(&playback_generation),
                        expected,
                        None,
                        None,
                        output,
                        Arc::clone(&prepare_cancel),
                    )
                })();
                if prepared
                    .as_ref()
                    .is_err_and(|error| error.starts_with("Could not build audio output"))
                {
                    output_profile = None;
                }
                let next = match prepared {
                    Ok(next) => next,
                    Err(error) => {
                        log::error!(
                            target: "cpal-output",
                            "failed to prepare {} generation={} elapsed_ms={}: {}",
                            source_label,
                            expected,
                            command_started.elapsed().as_millis(),
                            error,
                        );
                        let _ = reply.send(Err(error));
                        continue;
                    }
                };
                if let Err(error) = ensure_generation(&playback_generation, expected)
                    .and_then(|_| {
                        ensure_generation(&transition_generation, expected_transition)
                    })
                {
                    let _ = reply.send(Err(error));
                    continue;
                }

                next.shared.fade_gain.store(match transition {
                    PlayTransition::Crossfade { fade_in_ms, .. } if fade_in_ms > 0 => 0.0,
                    PlayTransition::Replace | PlayTransition::Crossfade { .. } => 1.0,
                });
                if let Err(error) = next.play() {
                    log::error!(
                        target: "cpal-output",
                        "failed to start output source={} generation={} elapsed_ms={}: {}",
                        source_label,
                        expected,
                        command_started.elapsed().as_millis(),
                        error,
                    );
                    let _ = reply.send(Err(error));
                    continue;
                }

                let started = PlaybackStarted {
                    duration_ms: next.duration_ms,
                    clock: Arc::clone(&next.shared.clock),
                };
                let mut previous = current.take();
                current = Some(next);
                log::info!(
                    target: "cpal-output",
                    "play session started source={} generation={} duration_ms={} elapsed_ms={}",
                    source_label,
                    expected,
                    started.duration_ms,
                    command_started.elapsed().as_millis(),
                );

                match transition {
                    PlayTransition::Replace => {
                        if let Some(mut previous) = previous.take() {
                            previous.stop();
                        }
                        let _ = reply.send(Ok(started));
                    }
                    PlayTransition::Crossfade {
                        fade_out_ms,
                        fade_in_ms,
                    } => {
                        // 新会话已可输出，先提交播放结果；淡化不再阻塞首播完成
                        let _ = reply.send(Ok(started));
                        let fade_result = crossfade_sessions(
                            previous.as_ref(),
                            current.as_ref().expect("committed playback session"),
                            fade_out_ms,
                            fade_in_ms,
                            &playback_generation,
                            expected,
                            &transition_generation,
                            expected_transition,
                        );
                        if let Some(session) = current.as_ref() {
                            session.shared.fade_gain.store(1.0);
                        }
                        if let Some(mut previous) = previous.take() {
                            previous.stop();
                        }
                        if let Err(error) = fade_result {
                            log::warn!(target: "cpal-output", "crossfade interrupted: {error}");
                        }
                    }
                }
            }
            AudioCmd::Pause => {
                if let Some(session) = &current {
                    session.pause();
                }
            }
            AudioCmd::Resume => {
                if let Some(session) = &current {
                    let _ = session.play();
                }
            }
            AudioCmd::Stop => {
                if let Some(mut session) = current.take() {
                    session.stop();
                }
                SharedAudioLevel::reset(&shared_level);
            }
            AudioCmd::SetVolume(next_volume) => {
                volume = next_volume.clamp(0.0, 1.0);
                if let Some(session) = &current {
                    session.shared.volume.store(volume);
                }
            }
            AudioCmd::SetSpeed(next_speed) => {
                // 先记下旧速：丢弃 ring 时按旧速补时钟，内容位置才对齐
                let previous_speed = speed;
                speed = next_speed.clamp(0.25, 3.0);
                if let Some(session) = &current {
                    session.shared.speed.store(speed);
                    session
                        .shared
                        .invalidate_processed_buffer(previous_speed);
                }
            }
            AudioCmd::Seek {
                position_ms,
                playback_generation: expected,
                seek_generation: expected_seek,
                reply,
            } => {
                let mut latest = position_ms;
                let mut latest_generation = expected;
                let mut latest_seek_generation = expected_seek;
                let mut latest_reply = reply;
                take_latest_seek(
                    &mut latest,
                    &mut latest_generation,
                    &mut latest_seek_generation,
                    &mut latest_reply,
                    &receiver,
                    &mut deferred,
                );
                let (
                    source,
                    paused,
                    rollback_position_ms,
                    clock,
                ) = {
                    let Some(session) = current.as_ref() else {
                        let _ = latest_reply.send(Err("Nothing is playing".into()));
                        continue;
                    };
                    if session.playback_generation != latest_generation
                        || ensure_generation(&playback_generation, latest_generation).is_err()
                    {
                        let _ = latest_reply.send(Err(PLAYBACK_SUPERSEDED.into()));
                        continue;
                    }
                    let seek_token = GenerationToken {
                        generation: Arc::clone(&seek_generation),
                        expected: latest_seek_generation,
                    };
                    if !seek_token.is_current() {
                        let _ = latest_reply.send(Err(SEEK_SUPERSEDED.into()));
                        continue;
                    }
                    (
                        session.source.clone(),
                        session.shared.paused.load(Ordering::Acquire),
                        session.shared.clock.position_ms(),
                        Arc::clone(&session.shared.clock),
                    )
                };
                // seek_token 需要在块外，重新构造一次
                let seek_token = GenerationToken {
                    generation: Arc::clone(&seek_generation),
                    expected: latest_seek_generation,
                };
                if !seek_token.is_current() {
                    let _ = latest_reply.send(Err(SEEK_SUPERSEDED.into()));
                    continue;
                }
                // 先停旧会话：取消旧 decode worker 的 remote 读，避免和 seekable 重建抢 in_flight
                let mut previous = match current.take() {
                    Some(session) => session,
                    None => {
                        let _ = latest_reply.send(Err("Nothing is playing".into()));
                        continue;
                    }
                };
                previous.stop();
                let prepared = (|| {
                    let output = ensure_output_profile(&mut output_profile)?;
                    prepare_session(
                        source,
                        latest,
                        volume,
                        speed,
                        Arc::clone(&shared_level),
                        Arc::clone(&effects_params),
                        Arc::clone(&playback_generation),
                        latest_generation,
                        Some(seek_token.clone()),
                        Some(Arc::clone(&clock)),
                        output,
                        Arc::new(AtomicBool::new(false)),
                    )
                })();
                if prepared
                    .as_ref()
                    .is_err_and(|error| error.starts_with("Could not build audio output"))
                {
                    output_profile = None;
                }
                match prepared {
                    Ok(next) => {
                        if ensure_generation(&playback_generation, latest_generation).is_err()
                            || !seek_token.is_current()
                        {
                            drop(previous);
                            let _ = latest_reply.send(Err(SEEK_SUPERSEDED.into()));
                            continue;
                        }
                        if !paused {
                            if let Err(error) = next.play() {
                                log::warn!(target: "cpal-output", "seek play failed: {error}");
                                clock.store_ms(rollback_position_ms);
                                if let Ok(output) = ensure_output_profile(&mut output_profile) {
                                    if let Ok(restored) = prepare_session(
                                        previous.source.clone(),
                                        rollback_position_ms,
                                        volume,
                                        speed,
                                        Arc::clone(&shared_level),
                                        Arc::clone(&effects_params),
                                        Arc::clone(&playback_generation),
                                        latest_generation,
                                        None,
                                        Some(Arc::clone(&clock)),
                                        output,
                                        Arc::new(AtomicBool::new(false)),
                                    ) {
                                        let _ = restored.play();
                                        current = Some(restored);
                                    }
                                }
                                drop(previous);
                                drop(next);
                                let _ = latest_reply.send(Err(error));
                                continue;
                            }
                        }
                        drop(previous);
                        current = Some(next);
                        let _ = latest_reply.send(Ok(()));
                    }
                    Err(error) => {
                        let superseded = ensure_generation(
                            &playback_generation,
                            latest_generation,
                        )
                        .is_err()
                            || !seek_token.is_current();
                        log::warn!(target: "cpal-output", "seek failed: {error}");
                        if superseded {
                            drop(previous);
                            let _ = latest_reply.send(Err(SEEK_SUPERSEDED.into()));
                            continue;
                        }
                        // 回滚时钟并重建旧位置
                        clock.store_ms(rollback_position_ms);
                        match (|| {
                            let output = ensure_output_profile(&mut output_profile)?;
                            prepare_session(
                                previous.source.clone(),
                                rollback_position_ms,
                                volume,
                                speed,
                                Arc::clone(&shared_level),
                                Arc::clone(&effects_params),
                                Arc::clone(&playback_generation),
                                latest_generation,
                                None,
                                Some(Arc::clone(&clock)),
                                output,
                                Arc::new(AtomicBool::new(false)),
                            )
                        })() {
                            Ok(restored) => {
                                if !paused {
                                    if let Err(play_error) = restored.play() {
                                        log::warn!(
                                            target: "cpal-output",
                                            "seek rollback play failed: {play_error}"
                                        );
                                    }
                                }
                                current = Some(restored);
                            }
                            Err(restore_error) => {
                                log::warn!(
                                    target: "cpal-output",
                                    "seek rollback failed: {restore_error}"
                                );
                            }
                        }
                        drop(previous);
                        let _ = latest_reply.send(Err(error));
                    }
                }
            }
            AudioCmd::QueryEmpty { reply } => {
                let _ = reply.send(current.as_ref().is_none_or(PlaybackSession::is_empty));
            }
            AudioCmd::InvalidateProcessedBuffer => {
                if let Some(session) = current.as_ref() {
                    // EQ/响度已改 params；用当前倍速补时钟（丢的是输出帧）
                    let clock_speed = session.shared.speed.load();
                    session.shared.invalidate_processed_buffer(clock_speed);
                }
            }
            AudioCmd::FadeOutPause {
                duration_ms,
                transition_generation: expected_transition,
                reply,
            } => {
                let result = if let Some(session) = &current {
                    fade_gain(
                        &session.shared,
                        session.shared.fade_gain.load(),
                        0.0,
                        duration_ms,
                        &playback_generation,
                        session.playback_generation,
                        &transition_generation,
                        expected_transition,
                    )
                    .map(|_| session.pause())
                } else {
                    Err("Nothing is playing".into())
                };
                let _ = reply.send(result);
            }
            AudioCmd::FadeInResume {
                duration_ms,
                transition_generation: expected_transition,
                reply,
            } => {
                let result = if let Some(session) = &current {
                    session.shared.fade_gain.store(0.0);
                    session.play().and_then(|_| {
                        fade_gain(
                            &session.shared,
                            0.0,
                            1.0,
                            duration_ms,
                            &playback_generation,
                            session.playback_generation,
                            &transition_generation,
                            expected_transition,
                        )
                    })
                } else {
                    Err("Nothing is playing".into())
                };
                let _ = reply.send(result);
            }
        }
    }

    if let Some(mut session) = current {
        session.stop();
    }
}

fn ensure_output_profile(
    output_profile: &mut Option<OutputDeviceProfile>,
) -> Result<&OutputDeviceProfile, String> {
    if output_profile.is_none() {
        *output_profile = Some(OutputDeviceProfile::open_default()?);
    }
    output_profile
        .as_ref()
        .ok_or_else(|| "No default audio output device".to_string())
}

fn prepare_session(
    source: AudioSource,
    start_position_ms: u64,
    volume: f32,
    speed: f32,
    shared_level: Arc<Mutex<SharedAudioLevel>>,
    effects_params: Arc<Mutex<AudioEffectsParams>>,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
    operation_generation: Option<GenerationToken>,
    clock: Option<Arc<PlaybackClock>>,
    output: &OutputDeviceProfile,
    prepare_cancel: Arc<AtomicBool>,
) -> Result<PlaybackSession, String> {
    let prepare_started = Instant::now();
    ensure_preparation_current(
        &playback_generation,
        expected_generation,
        operation_generation.as_ref(),
    )?;
    if prepare_cancel.load(Ordering::Acquire) {
        return Err("Timed out waiting for decoded audio".into());
    }
    let decoder_started = Instant::now();
    let session_cancelled = Arc::new(AtomicBool::new(false));
    // 外部 prepare_cancel 挂到读取消链：等待超时后 remote read 可立刻打断
    let read_cancellation = RemoteReadCancellation::new(
        Arc::clone(&session_cancelled),
        operation_generation.as_ref().map(|token| {
            (Arc::clone(&token.generation), token.expected)
        }),
    )
    .with_external_cancel(Arc::clone(&prepare_cancel));
    log::info!(
        target: "cpal-output",
        "decoder begin source={}, generation={}, start_ms={}",
        source.label(),
        expected_generation,
        start_position_ms,
    );
    // 必须先 clone/升级 access_mode，再判断 virtual-body：
    // LongFormProgressive 源在 clone 前也要能选中该路径（prefers 已兼容），
    // 但最终以 decoder_source 上的状态为准，避免误走 format.seek。
    let decoder_source =
        source.decoder_source_for_position(start_position_ms, read_cancellation);
    let use_byte_seek =
        start_position_ms > 0 && decoder_source.prefers_remote_virtual_body_seek();
    if start_position_ms > 0 {
        log::info!(
            target: "cpal-output",
            "decoder path generation={}, start_ms={}, use_byte_seek={}, prefers_virtual={}",
            expected_generation,
            start_position_ms,
            use_byte_seek,
            decoder_source.prefers_remote_virtual_body_seek(),
        );
    }
    let mut decoder = match make_decoder_for_position(&decoder_source, start_position_ms, use_byte_seek)
    {
        Ok(decoder) => decoder,
        Err(error) => {
            session_cancelled.store(true, Ordering::Release);
            return Err(error);
        }
    };
    if prepare_cancel.load(Ordering::Acquire) {
        session_cancelled.store(true, Ordering::Release);
        return Err("Timed out waiting for decoded audio".into());
    }
    let decoder_ms = decoder_started.elapsed().as_millis();
    let duration_ms = decoder
        .total_duration()
        .map(|duration| duration.as_millis() as u64)
        .filter(|duration| *duration > 0)
        .unwrap_or_else(|| source.duration_hint_ms());
    let start_position_ms = clamp_position(start_position_ms, duration_ms);
    // 字节跳转路径已经在目标附近顺序打开，不再走 format.seek（它会扫全文件）
    if start_position_ms > 0 && !use_byte_seek {
        decoder
            .try_seek(Duration::from_millis(start_position_ms))
            .map_err(|error| format!("Could not seek decoder: {error}"))?;
    } else if start_position_ms > 0 && use_byte_seek {
        log::info!(
            target: "cpal-output",
            "virtual-body open ready generation={}, start_ms={}, skip_format_seek=true virtual_body=true",
            expected_generation,
            start_position_ms,
        );
    }

    let config = &output.config;
    let channels = usize::from(config.channels.max(1));
    let sample_rate = config.sample_rate.0.max(1);
    let capacity_samples = duration_to_frames(PCM_CAPACITY, sample_rate)
        .saturating_mul(channels)
        .max(channels * 2);
    let initial_target = duration_to_frames(source.prebuffer_duration(), sample_rate);
    let clock = clock.unwrap_or_else(|| Arc::new(PlaybackClock::new(start_position_ms)));
    clock
        .position_us
        .store(start_position_ms.saturating_mul(1_000), Ordering::Release);
    let shared = Arc::new(PlaybackShared {
        ring: Arc::new(PcmRing::new(capacity_samples)),
        channels,
        sample_rate,
        paused: AtomicBool::new(false),
        buffering: AtomicBool::new(true),
        finished: AtomicBool::new(false),
        cancelled: Arc::clone(&session_cancelled),
        volume: AtomicF32::new(volume),
        fade_gain: AtomicF32::new(1.0),
        speed: AtomicF32::new(speed),
        buffer_target_frames: AtomicUsize::new(initial_target),
        clock,
        wake_lock: Mutex::new(()),
        wake: Condvar::new(),
    });

    let processed: Box<dyn PcmSource> = Box::new(LoudnessSource::new(
        EqualizerSource::new(decoder, Arc::clone(&effects_params)),
        effects_params,
    ));
    let worker = spawn_decode_worker(
        processed,
        Arc::clone(&shared),
        shared_level,
        Arc::clone(&playback_generation),
        expected_generation,
        operation_generation.clone(),
    )?;
    let output_started = Instant::now();
    let stream = match build_output_stream(
        &output.device,
        config,
        output.sample_format,
        Arc::clone(&shared),
    ) {
        Ok(stream) => stream,
        Err(error) => {
            shared.cancelled.store(true, Ordering::Release);
            shared.wake.notify_all();
            return Err(error);
        }
    };
    let output_ms = output_started.elapsed().as_millis();
    let ready_started = Instant::now();
    if let Err(error) = wait_until_ready(
        &shared,
        &playback_generation,
        expected_generation,
        operation_generation.as_ref(),
        &prepare_cancel,
    ) {
        shared.cancelled.store(true, Ordering::Release);
        shared.wake.notify_all();
        let _ = stream.pause();
        return Err(error);
    }
    let ready_wait_ms = ready_started.elapsed().as_millis();

    log::info!(
        target: "cpal-output",
        "prepared {} on {}: {} Hz, {} ch, target={}ms, decoder={}ms, output={}ms, wait={}ms, total={}ms",
        source.label(),
        output.name,
        sample_rate,
        channels,
        source.prebuffer_duration().as_millis(),
        decoder_ms,
        output_ms,
        ready_wait_ms,
        prepare_started.elapsed().as_millis()
    );
    Ok(PlaybackSession {
        source,
        stream,
        shared,
        worker: Some(worker),
        duration_ms,
        playback_generation: expected_generation,
    })
}

fn make_decoder_for_position(
    source: &AudioSource,
    start_position_ms: u64,
    use_byte_seek: bool,
) -> Result<Box<dyn PcmSource>, String> {
    match source {
        AudioSource::Bytes(data, _) => SymphoniaAudioDecoder::new(
            Box::new(Cursor::new(Arc::clone(data))),
            None,
        )
        .map(|decoder| Box::new(decoder) as Box<dyn PcmSource>),
        AudioSource::File(path, _) => SymphoniaAudioDecoder::new_file(Path::new(path))
            .map(|decoder| Box::new(decoder) as Box<dyn PcmSource>),
        AudioSource::Growing(reader, _) => {
            SymphoniaAudioDecoder::new(Box::new(reader.clone()), None)
                .map(|decoder| Box::new(decoder) as Box<dyn PcmSource>)
        }
        AudioSource::Remote(reader, _) => {
            if use_byte_seek && start_position_ms > 0 {
                reader
                    .configure_virtual_body_for_time(start_position_ms)
                    .map_err(|error| format!("Could not prepare remote virtual body: {error}"))?;
                let decoder = SymphoniaAudioDecoder::new_remote_virtual(reader.clone(), true)?;
                Ok(Box::new(decoder) as Box<dyn PcmSource>)
            } else {
                // 普通远程：demuxer open 可隐藏 seekable；无虚拟 body
                reader.clear_virtual_body();
                let decoder = SymphoniaAudioDecoder::new_remote(reader.clone())?;
                Ok(Box::new(decoder) as Box<dyn PcmSource>)
            }
        }
    }
}

fn spawn_decode_worker(
    source: Box<dyn PcmSource>,
    shared: Arc<PlaybackShared>,
    shared_level: Arc<Mutex<SharedAudioLevel>>,
    playback_generation: Arc<AtomicU64>,
    expected_generation: u64,
    operation_generation: Option<GenerationToken>,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("audio-decode".into())
        .spawn(move || {
            let mut converter = FrameResampler::new(source);
            let mut frame = vec![0.0; shared.channels];
            let mut analyzer = AudioAnalyzer::new();
            analyzer.configure(shared.sample_rate, ANALYSIS_FRAME_SIZE);
            let mut analysis = Vec::with_capacity(ANALYSIS_FRAME_SIZE);

            let mut frames_pushed = 0u64;
            let mut exit_reason = "source_eof";
            while !shared.cancelled.load(Ordering::Acquire)
                && playback_generation.load(Ordering::Acquire) == expected_generation
                && operation_generation
                    .as_ref()
                    .is_none_or(GenerationToken::is_current)
            {
                if shared.ring.writable_samples() < shared.channels {
                    thread::sleep(DECODE_IDLE_SLEEP);
                    continue;
                }
                if !converter.next_frame(shared.sample_rate, shared.speed.load(), &mut frame) {
                    exit_reason = "decoder_exhausted";
                    break;
                }
                if !shared.ring.try_push_frame(&frame) {
                    thread::yield_now();
                    continue;
                }
                frames_pushed = frames_pushed.saturating_add(1);

                analysis.push(frame[0]);
                if analysis.len() >= ANALYSIS_FRAME_SIZE {
                    let result = analyzer.analyze_frame(&analysis);
                    SharedAudioLevel::try_update(
                        &shared_level,
                        result.level,
                        result.beat_impulse,
                    );
                    analysis.clear();
                }
                shared.update_buffering_state();
            }
            if shared.cancelled.load(Ordering::Acquire) {
                exit_reason = "cancelled";
            } else if playback_generation.load(Ordering::Acquire) != expected_generation {
                exit_reason = "generation_changed";
            } else if operation_generation
                .as_ref()
                .is_some_and(|token| !token.is_current())
            {
                exit_reason = "operation_superseded";
            }
            log::info!(
                target: "cpal-output",
                "decode worker exit reason={}, frames={}, generation={}",
                exit_reason,
                frames_pushed,
                expected_generation,
            );
            shared.finished.store(true, Ordering::Release);
            shared.update_buffering_state();
            shared.wake.notify_all();
        })
        .map_err(|error| format!("Could not start decoder worker: {error}"))
}

fn wait_until_ready(
    shared: &PlaybackShared,
    playback_generation: &AtomicU64,
    expected_generation: u64,
    operation_generation: Option<&GenerationToken>,
    prepare_cancel: &AtomicBool,
) -> Result<(), String> {
    let started = Instant::now();
    let mut guard = shared
        .wake_lock
        .lock()
        .map_err(|_| "Playback ready lock poisoned".to_string())?;
    while shared.buffering.load(Ordering::Acquire) {
        ensure_preparation_current(
            playback_generation,
            expected_generation,
            operation_generation,
        )?;
        if shared.cancelled.load(Ordering::Acquire)
            || prepare_cancel.load(Ordering::Acquire)
        {
            return Err(if prepare_cancel.load(Ordering::Acquire) {
                "Timed out waiting for decoded audio".into()
            } else {
                PLAYBACK_SUPERSEDED.into()
            });
        }
        if started.elapsed() >= COMMAND_TIMEOUT {
            return Err("Timed out waiting for decoded audio".into());
        }
        guard = shared
            .wake
            .wait_timeout(guard, READY_POLL)
            .map_err(|_| "Playback ready lock poisoned".to_string())?
            .0;
    }
    ensure_preparation_current(
        playback_generation,
        expected_generation,
        operation_generation,
    )?;
    if prepare_cancel.load(Ordering::Acquire) {
        return Err("Timed out waiting for decoded audio".into());
    }
    Ok(())
}

fn build_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    shared: Arc<PlaybackShared>,
) -> Result<Stream, String> {
    match sample_format {
        SampleFormat::I8 => build_typed_stream::<i8>(device, config, shared),
        SampleFormat::I16 => build_typed_stream::<i16>(device, config, shared),
        SampleFormat::I32 => build_typed_stream::<i32>(device, config, shared),
        SampleFormat::I64 => build_typed_stream::<i64>(device, config, shared),
        SampleFormat::U8 => build_typed_stream::<u8>(device, config, shared),
        SampleFormat::U16 => build_typed_stream::<u16>(device, config, shared),
        SampleFormat::U32 => build_typed_stream::<u32>(device, config, shared),
        SampleFormat::U64 => build_typed_stream::<u64>(device, config, shared),
        SampleFormat::F32 => build_typed_stream::<f32>(device, config, shared),
        SampleFormat::F64 => build_typed_stream::<f64>(device, config, shared),
        other => Err(format!("Unsupported output sample format: {other}")),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<PlaybackShared>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let callback_shared = Arc::clone(&shared);
    let channels = shared.channels;
    let mut frame = vec![0.0f32; channels];
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                let silence = T::from_sample(0.0);
                if callback_shared.paused.load(Ordering::Acquire)
                    || callback_shared.buffering.load(Ordering::Acquire)
                {
                    output.fill(silence);
                    return;
                }

                let mut rendered_frames = 0usize;
                let mut underflowed = false;
                let gain = callback_shared.volume.load() * callback_shared.fade_gain.load();
                for output_frame in output.chunks_mut(channels) {
                    if underflowed {
                        output_frame.fill(silence);
                        continue;
                    }
                    if !callback_shared.ring.try_pop_frame(&mut frame) {
                        output_frame.fill(silence);
                        callback_shared.begin_rebuffering();
                        underflowed = true;
                        continue;
                    }

                    for (target, sample) in output_frame.iter_mut().zip(frame.iter().copied()) {
                        *target = T::from_sample((sample * gain).clamp(-1.0, 1.0));
                    }
                    rendered_frames += 1;
                }
                if rendered_frames > 0 {
                    let speed = callback_shared.speed.load().clamp(0.25, 3.0);
                    let elapsed_us = ((rendered_frames as f64 * 1_000_000.0
                        / f64::from(callback_shared.sample_rate))
                        * f64::from(speed)) as u64;
                    callback_shared
                        .clock
                        .position_us
                        .fetch_add(elapsed_us, Ordering::AcqRel);
                }
            },
            move |error| {
                log::error!(target: "cpal-output", "stream error: {error}");
                shared.begin_rebuffering();
            },
            None,
        )
        .map_err(|error| format!("Could not build audio output: {error}"))
}

fn crossfade_sessions(
    previous: Option<&PlaybackSession>,
    next: &PlaybackSession,
    fade_out_ms: u32,
    fade_in_ms: u32,
    playback_generation: &AtomicU64,
    expected_generation: u64,
    transition_generation: &AtomicU64,
    expected_transition: u64,
) -> Result<(), String> {
    let duration_ms = fade_out_ms.max(fade_in_ms);
    if duration_ms == 0 {
        next.shared.fade_gain.store(1.0);
        return Ok(());
    }
    let started = Instant::now();
    loop {
        ensure_generation(playback_generation, expected_generation)?;
        ensure_generation(transition_generation, expected_transition)?;
        let elapsed_ms = started.elapsed().as_secs_f32() * 1_000.0;
        if let Some(previous) = previous {
            let progress = if fade_out_ms == 0 {
                1.0
            } else {
                (elapsed_ms / fade_out_ms as f32).min(1.0)
            };
            previous.shared.fade_gain.store(1.0 - progress);
        }
        let progress = if fade_in_ms == 0 {
            1.0
        } else {
            (elapsed_ms / fade_in_ms as f32).min(1.0)
        };
        next.shared.fade_gain.store(progress);
        if elapsed_ms >= duration_ms as f32 {
            break;
        }
        thread::sleep(FADE_STEP);
    }
    next.shared.fade_gain.store(1.0);
    Ok(())
}

fn fade_gain(
    shared: &PlaybackShared,
    from: f32,
    to: f32,
    duration_ms: u32,
    playback_generation: &AtomicU64,
    expected_generation: u64,
    transition_generation: &AtomicU64,
    expected_transition: u64,
) -> Result<(), String> {
    if duration_ms == 0 {
        shared.fade_gain.store(to);
        return Ok(());
    }
    let started = Instant::now();
    loop {
        ensure_generation(playback_generation, expected_generation)?;
        ensure_generation(transition_generation, expected_transition)?;
        let progress =
            (started.elapsed().as_secs_f32() * 1_000.0 / duration_ms as f32).min(1.0);
        shared.fade_gain.store(from + (to - from) * progress);
        if progress >= 1.0 {
            break;
        }
        thread::sleep(FADE_STEP);
    }
    Ok(())
}

fn receive_fade_result(
    receiver: mpsc::Receiver<Result<(), String>>,
    duration_ms: u32,
) -> AppResult<()> {
    receiver
        .recv_timeout(COMMAND_TIMEOUT + Duration::from_millis(u64::from(duration_ms) + 1_000))
        .map_err(|error| AppError::Audio(format!("Audio thread timeout: {error}")))?
        .map_err(AppError::Audio)
}

fn ensure_generation(generation: &AtomicU64, expected: u64) -> Result<(), String> {
    if generation.load(Ordering::Acquire) == expected {
        Ok(())
    } else {
        Err(PLAYBACK_SUPERSEDED.into())
    }
}

fn ensure_preparation_current(
    playback_generation: &AtomicU64,
    expected_playback_generation: u64,
    operation_generation: Option<&GenerationToken>,
) -> Result<(), String> {
    ensure_generation(playback_generation, expected_playback_generation)?;
    if operation_generation.is_none_or(GenerationToken::is_current) {
        Ok(())
    } else {
        Err(SEEK_SUPERSEDED.into())
    }
}

pub fn wait_for_seek_result(
    receiver: mpsc::Receiver<Result<(), String>>,
) -> AppResult<()> {
    // 超长媒体 seek 可能要等 tail moov + 目标位置数据
    receiver
        .recv_timeout(REMOTE_SEEK_TIMEOUT)
        .map_err(|error| AppError::Audio(format!("Seek command timeout: {error}")))?
        .map_err(AppError::Audio)
}

/// 在锁外等待播放命令的结果
pub fn wait_for_play_result(request: &PlayRequest) -> AppResult<PlaybackStarted> {
    // 远程源用更短超时；超时必须 cancel prepare，否则音频线程卡在 make_decoder 会堵死后续 fallback
    let base = if request.is_remote {
        REMOTE_PREPARE_TIMEOUT
    } else {
        COMMAND_TIMEOUT
    };
    let timeout = base + Duration::from_millis(u64::from(request.fade_ms) + 1_000);
    match request.reply_rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(AppError::Audio),
        Err(_) => {
            request.prepare_cancel.store(true, Ordering::Release);
            // 给音频线程一点时间感知 cancel 并回包，避免泄漏阻塞
            match request.reply_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(result) => result.map_err(AppError::Audio),
                Err(error) => Err(AppError::Audio(format!("Audio thread timeout: {error}"))),
            }
        }
    }
}

fn take_latest_seek(
    position_ms: &mut u64,
    playback_generation: &mut u64,
    seek_generation: &mut u64,
    reply: &mut mpsc::Sender<Result<(), String>>,
    receiver: &mpsc::Receiver<AudioCmd>,
    deferred: &mut Option<AudioCmd>,
) {
    loop {
        match receiver.try_recv() {
            Ok(AudioCmd::Seek {
                position_ms: next,
                playback_generation: next_generation,
                seek_generation: next_seek_generation,
                reply: next_reply,
            }) => {
                let _ = reply.send(Err(SEEK_SUPERSEDED.into()));
                *position_ms = next;
                *playback_generation = next_generation;
                *seek_generation = next_seek_generation;
                *reply = next_reply;
            }
            Ok(command) => {
                *deferred = Some(command);
                break;
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        }
    }
}

fn clamp_position(position_ms: u64, duration_ms: u64) -> u64 {
    if duration_ms > 0 {
        position_ms.min(duration_ms)
    } else {
        position_ms
    }
}

fn duration_to_frames(duration: Duration, sample_rate: u32) -> usize {
    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(sample_rate.max(1)))
        .saturating_add(999_999_999)
        / 1_000_000_000;
    usize::try_from(frames).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        channel_sample, clamp_position, duration_to_frames, ensure_generation,
        ensure_preparation_current, take_latest_seek, AudioCmd, GenerationToken, PlaybackClock,
        SEEK_SUPERSEDED,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn duration_to_frames_matches_android_buffer_thresholds() {
        assert_eq!(duration_to_frames(Duration::from_millis(800), 48_000), 38_400);
        assert_eq!(duration_to_frames(Duration::from_millis(1_500), 48_000), 72_000);
    }

    #[test]
    fn channel_conversion_handles_mono_and_downmix() {
        assert_eq!(channel_sample(&[0.25], 1, 2), 0.25);
        assert!((channel_sample(&[0.25, 0.75], 0, 1) - 0.5).abs() < f32::EPSILON);
        assert_eq!(channel_sample(&[0.25, 0.75], 1, 2), 0.75);
    }

    #[test]
    fn seek_queue_keeps_latest_consecutive_position() {
        let (sender, receiver) = mpsc::channel();
        let (first_reply, first_result) = mpsc::channel();
        let (second_reply, second_result) = mpsc::channel();
        let (third_reply, third_result) = mpsc::channel();
        assert!(sender
            .send(AudioCmd::Seek {
                position_ms: 100,
                playback_generation: 7,
                seek_generation: 1,
                reply: first_reply,
            })
            .is_ok());
        assert!(sender
            .send(AudioCmd::Seek {
                position_ms: 800,
                playback_generation: 7,
                seek_generation: 2,
                reply: second_reply,
            })
            .is_ok());
        assert!(sender
            .send(AudioCmd::Seek {
                position_ms: 1_600,
                playback_generation: 8,
                seek_generation: 3,
                reply: third_reply,
            })
            .is_ok());
        let mut latest = 0;
        let mut generation = 7;
        let mut seek_generation = 0;
        let (initial_reply, initial_result) = mpsc::channel();
        let mut reply = initial_reply;
        let mut deferred = None;

        take_latest_seek(
            &mut latest,
            &mut generation,
            &mut seek_generation,
            &mut reply,
            &receiver,
            &mut deferred,
        );

        assert_eq!(latest, 1_600);
        assert_eq!(generation, 8);
        assert_eq!(seek_generation, 3);
        assert_eq!(
            initial_result.recv().expect("initial result"),
            Err(SEEK_SUPERSEDED.into())
        );
        assert_eq!(
            first_result.recv().expect("first result"),
            Err(SEEK_SUPERSEDED.into())
        );
        assert_eq!(
            second_result.recv().expect("second result"),
            Err(SEEK_SUPERSEDED.into())
        );
        reply.send(Ok(())).expect("latest reply");
        assert_eq!(third_result.recv().expect("third result"), Ok(()));
        assert!(deferred.is_none());
    }

    #[test]
    fn playback_generation_rejects_superseded_work() {
        let generation = AtomicU64::new(7);
        assert!(ensure_generation(&generation, 7).is_ok());
        generation.store(8, Ordering::Release);
        assert!(ensure_generation(&generation, 7).is_err());
    }

    #[test]
    fn newer_seek_cancels_only_the_old_seek_preparation() {
        let playback_generation = AtomicU64::new(7);
        let seek_generation = Arc::new(AtomicU64::new(2));
        let old_seek = GenerationToken {
            generation: Arc::clone(&seek_generation),
            expected: 1,
        };
        let current_seek = GenerationToken {
            generation: seek_generation,
            expected: 2,
        };

        assert!(ensure_preparation_current(
            &playback_generation,
            7,
            Some(&old_seek),
        )
        .is_err());
        assert!(ensure_preparation_current(
            &playback_generation,
            7,
            Some(&current_seek),
        )
        .is_ok());
    }

    #[test]
    fn playback_clock_starts_at_requested_position() {
        let clock = PlaybackClock::new(12_345);
        assert_eq!(clock.position_ms(), 12_345);
    }

    #[test]
    fn known_duration_clamps_seek() {
        assert_eq!(clamp_position(20_000, 10_000), 10_000);
        assert_eq!(clamp_position(20_000, 0), 20_000);
    }
}
