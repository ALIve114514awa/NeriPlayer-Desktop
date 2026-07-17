// 音频播放引擎 — 专用线程架构
// OutputStream 是 !Send，必须在创建它的线程上操作。
// 所有 Sink 操作通过 channel 发送到专用音频线程执行。

use rodio::source::SeekError;
use rodio::{OutputStream, Sink, Source};
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::analyzer::{AudioAnalyzer, SharedAudioLevel};
use crate::audio::buffered::{
    AsyncPcmSource, DEFAULT_BUFFER_CAPACITY, DEFAULT_PREBUFFER_DURATION,
};
use crate::audio::effects::{AudioEffectsParams, EqualizerSource, LoudnessSource};
use crate::audio::growing::GrowingAudioReader;
use crate::audio::remote::{RemoteAudioSource, SymphoniaAudioDecoder};
use crate::error::{AppError, AppResult};

/// 播放操作 recv 超时（网络音频解码可能慢）
const RECV_TIMEOUT: Duration = Duration::from_secs(30);
/// 分析帧大小（样本数），~46ms@44.1kHz
const ANALYSIS_FRAME_SIZE: usize = 2048;
const LOCAL_PREBUFFER_DURATION: Duration = Duration::from_millis(80);
const LOCAL_BUFFER_CAPACITY: Duration = Duration::from_secs(3);

// 音频来源——seek 时需要重建 decoder
#[derive(Clone)]
enum AudioSource {
    Bytes(Vec<u8>),
    File(String, u64),
    Growing(GrowingAudioReader, u64),
    Remote(RemoteAudioSource, u64),
}

impl AudioSource {
    fn abort_if_stream(&self) {
        if let AudioSource::Growing(reader, _) = self {
            reader.abort();
        }
    }

    fn is_remote(&self) -> bool {
        matches!(self, AudioSource::Remote(_, _))
    }

    fn pcm_buffer_durations(&self) -> (Duration, Duration) {
        if matches!(self, AudioSource::Growing(_, _) | AudioSource::Remote(_, _)) {
            (DEFAULT_PREBUFFER_DURATION, DEFAULT_BUFFER_CAPACITY)
        } else {
            (LOCAL_PREBUFFER_DURATION, LOCAL_BUFFER_CAPACITY)
        }
    }
}

fn stop_prev_transition(
    prev_sink: &mut Option<Arc<Sink>>,
    prev_source: &mut Option<AudioSource>,
    prev_cleanup_deadline: &mut Option<Instant>,
) {
    if let Some(old_prev) = prev_sink.take() {
        old_prev.stop();
    }
    if let Some(old_prev_source) = prev_source.take() {
        old_prev_source.abort_if_stream();
    }
    *prev_cleanup_deadline = None;
}

fn take_latest_seek(
    position_ms: &mut u64,
    rx: &mpsc::Receiver<AudioCmd>,
    deferred_cmd: &mut Option<AudioCmd>,
) {
    loop {
        match rx.try_recv() {
            Ok(AudioCmd::Seek { position_ms: next }) => {
                *position_ms = next;
            }
            Ok(next_cmd) => {
                *deferred_cmd = Some(next_cmd);
                break;
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        }
    }
}

/// Fade 步进间隔
const FADE_STEP_MS: u64 = 20;

fn fade_step_count(duration_ms: u32) -> u64 {
    if duration_ms == 0 {
        return 0;
    }
    u64::from(duration_ms).div_ceil(FADE_STEP_MS)
}

fn fade_progress(step: u64, duration_ms: u32) -> f32 {
    if duration_ms == 0 {
        return 1.0;
    }
    ((step * FADE_STEP_MS) as f32 / duration_ms as f32).min(1.0)
}

fn fade_timeout(duration_ms: u32) -> Duration {
    RECV_TIMEOUT + Duration::from_millis(u64::from(duration_ms) + 1000)
}

fn receive_fade_result(rx: mpsc::Receiver<Result<(), String>>, duration_ms: u32) -> AppResult<()> {
    rx.recv_timeout(fade_timeout(duration_ms))
        .map_err(|error| AppError::Audio(format!("Audio thread timeout: {}", error)))?
        .map_err(AppError::Audio)
}

fn next_transition_generation(generation: &AtomicU64) -> u64 {
    generation.fetch_add(1, Ordering::AcqRel) + 1
}

fn cancel_active_transition(
    transition_generation: &AtomicU64,
    prev_sink: &mut Option<Arc<Sink>>,
    prev_source: &mut Option<AudioSource>,
    prev_cleanup_deadline: &mut Option<Instant>,
) -> u64 {
    let generation = next_transition_generation(transition_generation);
    stop_prev_transition(prev_sink, prev_source, prev_cleanup_deadline);
    generation
}

fn is_transition_current(generation: &AtomicU64, expected: u64) -> bool {
    generation.load(Ordering::Acquire) == expected
}

fn is_crossfade_worker_current(
    transition_generation: &AtomicU64,
    expected_transition: u64,
    playback_generation: &AtomicU64,
    expected_playback: u64,
) -> bool {
    is_transition_current(transition_generation, expected_transition)
        && playback_generation.load(Ordering::Acquire) == expected_playback
}

fn query_empty_result(result: Result<bool, mpsc::RecvTimeoutError>) -> bool {
    match result {
        Ok(empty) => empty,
        Err(mpsc::RecvTimeoutError::Timeout) => false,
        Err(mpsc::RecvTimeoutError::Disconnected) => true,
    }
}

fn ensure_playback_generation(generation: &AtomicU64, expected: u64) -> Result<(), String> {
    if generation.load(Ordering::Acquire) == expected {
        Ok(())
    } else {
        Err("Playback request superseded".to_string())
    }
}

fn duration_from_hint_or_else<F>(duration_hint_ms: u64, probe: F) -> u64
where
    F: FnOnce() -> u64,
{
    if duration_hint_ms > 0 {
        duration_hint_ms
    } else {
        probe()
    }
}

// 音频线程命令
enum AudioCmd {
    PlayBytes {
        data: Vec<u8>,
        duration_hint_ms: u64,
        start_position_ms: u64,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    PlayFile {
        path: String,
        duration_hint_ms: u64,
        start_position_ms: u64,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    PlayStream {
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        start_position_ms: u64,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    PlayRemote {
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        start_position_ms: u64,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
    SetSpeed(f32),
    Seek {
        position_ms: u64,
    },
    QueryEmpty {
        reply: mpsc::Sender<bool>,
    },
    /// 渐出后暂停：在 duration_ms 内将音量降至 0，然后 pause
    FadeOutPause {
        duration_ms: u32,
        reply: mpsc::Sender<Result<(), String>>,
    },
    /// resume 后渐入：先 resume，然后在 duration_ms 内将音量从 0 升至目标音量
    FadeInResume {
        duration_ms: u32,
        reply: mpsc::Sender<Result<(), String>>,
    },
    /// Crossfade 播放新字节流：对当前 sink fade out，新 sink fade in
    CrossfadeBytes {
        data: Vec<u8>,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    /// Crossfade 播放新文件
    CrossfadeFile {
        path: String,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    /// Crossfade 播放边下边读的音频流
    CrossfadeStream {
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    CrossfadeRemote {
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        playback_generation: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
}

struct CrossfadeWorker {
    old_sink: Option<Arc<Sink>>,
    new_sink: Arc<Sink>,
    target_volume: f32,
    fade_out_ms: u32,
    fade_in_ms: u32,
    transition_generation: Arc<AtomicU64>,
    expected_transition_generation: u64,
    playback_generation: Arc<AtomicU64>,
    expected_playback_generation: u64,
}

// ─── AnalyzingSource ─────────────────────────────────────────────────────────
// rodio Source 包装器：透传所有音频数据，同时在 ring buffer 中累积样本，
// 每 ANALYSIS_FRAME_SIZE 个样本调用 AudioAnalyzer 分析一帧。

struct AnalyzingSource<S> {
    inner: S,
    analyzer: AudioAnalyzer,
    shared: Arc<Mutex<SharedAudioLevel>>,
    buffer: Vec<f32>,
    channels: u16,
    sample_rate: u32,
    channel_index: u16,
}

impl<S> AnalyzingSource<S>
where
    S: Source<Item = i16> + Send,
{
    fn new(source: S, shared: Arc<Mutex<SharedAudioLevel>>) -> Self {
        let channels = source.channels();
        let sample_rate = source.sample_rate();
        let mut analyzer = AudioAnalyzer::new();
        analyzer.configure(sample_rate, ANALYSIS_FRAME_SIZE);
        Self {
            inner: source,
            analyzer,
            shared,
            buffer: Vec::with_capacity(ANALYSIS_FRAME_SIZE),
            channels,
            sample_rate,
            channel_index: 0,
        }
    }

    fn flush_buffer(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let result = self.analyzer.analyze_frame(&self.buffer);
        SharedAudioLevel::try_update(&self.shared, result.level, result.beat_impulse);
        self.buffer.clear();
    }
}

impl<S> Iterator for AnalyzingSource<S>
where
    S: Source<Item = i16> + Send,
{
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        let sample = self.inner.next()?;
        if self.channel_index == 0 {
            self.buffer.push(sample as f32 / 32768.0);
        }
        self.channel_index = (self.channel_index + 1) % self.channels.max(1);
        if self.buffer.len() >= ANALYSIS_FRAME_SIZE {
            self.flush_buffer();
        }
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for AnalyzingSource<S>
where
    S: Source<Item = i16> + Send,
{
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let result = self.inner.try_seek(pos);
        if result.is_ok() {
            // seek 成功，重置分析器状态避免残留数据影响
            self.analyzer.reset();
            self.buffer.clear();
            self.channel_index = 0;
        }
        result
    }
}

// ─── PlayerEngine ────────────────────────────────────────────────────────────

pub struct PlayerEngine {
    cmd_tx: mpsc::Sender<AudioCmd>,
    thread_alive: Arc<AtomicBool>,
    pub is_playing: bool,
    pub volume: f32,
    pub speed: f32,
    pub current_path: Option<String>,
    pub duration_ms: u64,
    play_start_time: Option<Instant>,
    accumulated_ms: u64,
    /// 共享音频电平数据，供 main.rs ticker 线程读取
    pub shared_audio_level: Arc<Mutex<SharedAudioLevel>>,
    /// 共享音效参数（响度增益 + 均衡器），音频线程实时读取
    pub effects_params: Arc<std::sync::Mutex<AudioEffectsParams>>,
    playback_generation: Arc<AtomicU64>,
}

unsafe impl Send for PlayerEngine {}

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
        let shared_level = SharedAudioLevel::new();
        let effects_params = AudioEffectsParams::new_shared();
        let (tx, alive) = Self::spawn_audio_thread(
            shared_level.clone(),
            effects_params.clone(),
            playback_generation.clone(),
        );
        Self {
            cmd_tx: tx,
            thread_alive: alive,
            is_playing: false,
            volume: 1.0,
            speed: 1.0,
            current_path: None,
            duration_ms: 0,
            play_start_time: None,
            accumulated_ms: 0,
            shared_audio_level: shared_level,
            effects_params,
            playback_generation,
        }
    }

    /// 启动音频线程，返回 (命令发送端, 存活标记)
    fn spawn_audio_thread(
        shared_level: Arc<Mutex<SharedAudioLevel>>,
        effects_params: Arc<std::sync::Mutex<AudioEffectsParams>>,
        playback_generation: Arc<AtomicU64>,
    ) -> (mpsc::Sender<AudioCmd>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel::<AudioCmd>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_flag = alive.clone();

        std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || {
                if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::audio_thread(rx, shared_level, effects_params, playback_generation);
                })) {
                    eprintln!("[audio-thread] PANIC: {:?}", e);
                }
                alive_flag.store(false, Ordering::SeqCst);
                eprintln!("[audio-thread] thread exited");
            })
            .expect("Failed to spawn audio thread");

        (tx, alive)
    }

    /// 检查并重启已死的音频线程
    fn ensure_alive(&mut self) {
        if !self.thread_alive.load(Ordering::SeqCst) {
            eprintln!("[PlayerEngine] audio thread dead, respawning...");
            let (tx, alive) = Self::spawn_audio_thread(
                self.shared_audio_level.clone(),
                self.effects_params.clone(),
                self.playback_generation.clone(),
            );
            self.cmd_tx = tx;
            self.thread_alive = alive;
            let _ = self.cmd_tx.send(AudioCmd::SetVolume(self.volume));
            if (self.speed - 1.0).abs() > 0.01 {
                let _ = self.cmd_tx.send(AudioCmd::SetSpeed(self.speed));
            }
        }
    }

    fn ensure_playback_request_current(&self, expected_playback_generation: u64) -> AppResult<()> {
        ensure_playback_generation(&self.playback_generation, expected_playback_generation)
            .map_err(AppError::Audio)
    }

    /// 从 source 创建 decoder，成功返回 (decoder_box, duration_ms)
    fn make_decoder(
        source: &AudioSource,
    ) -> Result<(Box<dyn Source<Item = i16> + Send>, u64), String> {
        match source {
            AudioSource::Bytes(data) => {
                let dec = SymphoniaAudioDecoder::new(Box::new(Cursor::new(data.clone())), None)?;
                let dur = dec
                    .total_duration()
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                Ok((Box::new(dec), dur))
            }
            AudioSource::File(path, duration_hint_ms) => {
                let dec = SymphoniaAudioDecoder::new_file(Path::new(path))?;
                let dur = duration_from_hint_or_else(*duration_hint_ms, || {
                    dec.total_duration()
                        .map(|duration| duration.as_millis() as u64)
                        .unwrap_or(0)
                });
                Ok((Box::new(dec), dur))
            }
            AudioSource::Growing(reader, duration_hint_ms) => {
                let dec = SymphoniaAudioDecoder::new(Box::new(reader.clone()), None)?;
                let dur = dec
                    .total_duration()
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(*duration_hint_ms);
                Ok((Box::new(dec), dur))
            }
            AudioSource::Remote(reader, duration_hint_ms) => {
                let dec = SymphoniaAudioDecoder::new(Box::new(reader.clone()), None)?;
                let dur = dec
                    .total_duration()
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(*duration_hint_ms);
                Ok((Box::new(dec), dur))
            }
        }
    }

    fn clamp_start_position(position_ms: u64, duration_ms: u64) -> u64 {
        if duration_ms > 0 {
            position_ms.min(duration_ms)
        } else {
            position_ms
        }
    }

    fn prepare_decoder_position(
        mut source: Box<dyn Source<Item = i16> + Send>,
        position_ms: u64,
    ) -> (Box<dyn Source<Item = i16> + Send>, bool) {
        if position_ms == 0 {
            return (source, true);
        }

        let position = Duration::from_millis(position_ms);
        if source.try_seek(position).is_ok() {
            eprintln!("[audio-thread] decoder seek OK: {}ms", position_ms);
            (source, true)
        } else {
            eprintln!(
                "[audio-thread] decoder seek failed, using skip fallback: {}ms",
                position_ms
            );
            (Box::new(source.skip_duration(position)), false)
        }
    }

    fn buffer_decoder(
        source: &AudioSource,
        decoder: Box<dyn Source<Item = i16> + Send>,
        playback_generation: &AtomicU64,
        expected_playback_generation: u64,
    ) -> Result<Box<dyn Source<Item = i16> + Send>, String> {
        let (prebuffer, capacity) = source.pcm_buffer_durations();
        let buffered = AsyncPcmSource::new_cancellable(
            decoder,
            prebuffer,
            capacity,
            || playback_generation.load(Ordering::Acquire) != expected_playback_generation,
        )
        .ok_or_else(|| "Playback request superseded".to_string())?;
        Ok(Box::new(buffered))
    }

    /// 在音频线程中执行 fade out（阻塞调用线程）
    fn do_fade_out_sink(sink: &Sink, target_volume: f32, duration_ms: u32) {
        if duration_ms == 0 {
            sink.set_volume(0.0);
            return;
        }
        let steps = fade_step_count(duration_ms);
        let start_vol = target_volume;
        for i in 1..=steps {
            let t = fade_progress(i, duration_ms);
            sink.set_volume(start_vol * (1.0 - t));
            std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
        }
        sink.set_volume(0.0);
    }

    /// 在音频线程中执行 fade in（阻塞调用线程）
    fn do_fade_in_sink(sink: &Sink, target_volume: f32, duration_ms: u32) {
        if duration_ms == 0 {
            sink.set_volume(target_volume);
            return;
        }
        let steps = fade_step_count(duration_ms);
        sink.set_volume(0.0);
        for i in 1..=steps {
            let t = fade_progress(i, duration_ms);
            sink.set_volume(target_volume * t);
            std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
        }
        sink.set_volume(target_volume);
    }

    fn spawn_crossfade_worker(worker: CrossfadeWorker) {
        let CrossfadeWorker {
            old_sink,
            new_sink,
            target_volume,
            fade_out_ms,
            fade_in_ms,
            transition_generation,
            expected_transition_generation,
            playback_generation,
            expected_playback_generation,
        } = worker;
        let _ = std::thread::Builder::new()
            .name("audio-crossfade".into())
            .spawn(move || {
                let fade_duration = fade_out_ms.max(fade_in_ms);
                if fade_duration == 0 {
                    if !is_crossfade_worker_current(
                        &transition_generation,
                        expected_transition_generation,
                        &playback_generation,
                        expected_playback_generation,
                    ) {
                        return;
                    }
                    if let Some(ref old) = old_sink {
                        old.stop();
                    }
                    new_sink.set_volume(target_volume);
                    return;
                }

                let steps = fade_step_count(fade_duration);
                for i in 1..=steps {
                    if !is_crossfade_worker_current(
                        &transition_generation,
                        expected_transition_generation,
                        &playback_generation,
                        expected_playback_generation,
                    ) {
                        return;
                    }
                    if let Some(ref old) = old_sink {
                        let out_t = fade_progress(i, fade_out_ms);
                        old.set_volume(target_volume * (1.0 - out_t));
                    }
                    let in_t = fade_progress(i, fade_in_ms);
                    new_sink.set_volume(target_volume * in_t);
                    std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
                }

                if !is_crossfade_worker_current(
                    &transition_generation,
                    expected_transition_generation,
                    &playback_generation,
                    expected_playback_generation,
                ) {
                    return;
                }
                if let Some(ref old) = old_sink {
                    old.stop();
                }
                new_sink.set_volume(target_volume);
            });
    }

    /// 音频线程主循环
    fn audio_thread(
        rx: mpsc::Receiver<AudioCmd>,
        shared_level: Arc<Mutex<SharedAudioLevel>>,
        effects_params: Arc<std::sync::Mutex<AudioEffectsParams>>,
        playback_generation: Arc<AtomicU64>,
    ) {
        let (mut stream, mut handle) = match OutputStream::try_default() {
            Ok((stream, handle)) => (Some(stream), Some(handle)),
            Err(error) => {
                eprintln!("[audio-thread] audio output warmup failed: {}", error);
                (None, None)
            }
        };
        let mut current_sink: Option<Arc<Sink>> = None;
        let mut prev_sink: Option<Arc<Sink>> = None; // crossfade 过渡用
        let mut prev_source: Option<AudioSource> = None;
        let mut prev_cleanup_deadline: Option<Instant> = None;
        let mut current_volume: f32 = 1.0;
        let mut current_speed: f32 = 1.0;
        // 保留当前音频来源，用于 seek 时重建 decoder
        let mut current_source: Option<AudioSource> = None;
        let mut current_duration_ms: u64 = 0;
        let mut deferred_cmd: Option<AudioCmd> = None;
        let transition_generation = Arc::new(AtomicU64::new(0));

        macro_rules! ensure_output {
            () => {
                if handle.is_none() {
                    match OutputStream::try_default() {
                        Ok((s, h)) => {
                            stream = Some(s);
                            handle = Some(h);
                        }
                        Err(e) => {
                            eprintln!("[audio-thread] Failed to open audio output: {}", e);
                        }
                    }
                }
            };
        }

        loop {
            let cmd = if let Some(cmd) = deferred_cmd.take() {
                cmd
            } else {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(cmd) => cmd,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let should_cleanup_prev = prev_cleanup_deadline
                            .map(|deadline| Instant::now() >= deadline)
                            .unwrap_or(false)
                            || prev_sink.as_ref().map(|sink| sink.empty()).unwrap_or(false);
                        if should_cleanup_prev {
                            stop_prev_transition(
                                &mut prev_sink,
                                &mut prev_source,
                                &mut prev_cleanup_deadline,
                            );
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        eprintln!("[audio-thread] channel disconnected, exiting");
                        break;
                    }
                }
            };

            match cmd {
                AudioCmd::PlayBytes {
                    data,
                    duration_hint_ms,
                    start_position_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        let data_len = data.len();
                        eprintln!(
                            "[audio-thread] PlayBytes: {} bytes, hint={}ms, start={}ms",
                            data_len, duration_hint_ms, start_position_ms
                        );

                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let source = AudioSource::Bytes(data);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms = if dur > 0 { dur } else { duration_hint_ms };
                        let start_ms = Self::clamp_start_position(start_position_ms, duration_ms);
                        let (dec, _) = Self::prepare_decoder_position(dec, start_ms);
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;

                        eprintln!("[audio-thread] decoded ok, duration={}ms", duration_ms);
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }
                        // 音效链: Decoder → Equalizer → Loudness → Analyzer
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        sink.set_volume(current_volume);
                        sink.set_speed(current_speed);
                        sink.append(analyzing);

                        current_sink = Some(sink);
                        current_source = Some(source);
                        current_duration_ms = duration_ms;
                        Ok(duration_ms)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::PlayFile {
                    path,
                    duration_hint_ms,
                    start_position_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let source = AudioSource::File(path, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let start_ms = Self::clamp_start_position(start_position_ms, dur);
                        let (dec, _) = Self::prepare_decoder_position(dec, start_ms);
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }
                        // 音效链: Decoder → Equalizer → Loudness → Analyzer
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        sink.set_volume(current_volume);
                        sink.set_speed(current_speed);
                        sink.append(analyzing);

                        current_sink = Some(sink);
                        current_source = Some(source);
                        current_duration_ms = dur;
                        Ok(dur)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::PlayStream {
                    reader,
                    duration_hint_ms,
                    start_position_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        eprintln!(
                            "[audio-thread] PlayStream: hint={}ms, start={}ms",
                            duration_hint_ms, start_position_ms
                        );
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let source = AudioSource::Growing(reader, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms = if dur > 0 { dur } else { duration_hint_ms };
                        let start_ms = Self::clamp_start_position(start_position_ms, duration_ms);
                        let (dec, _) = Self::prepare_decoder_position(dec, start_ms);
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;
                        eprintln!(
                            "[audio-thread] streaming decoder ok, duration={}ms",
                            duration_ms
                        );
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        sink.set_volume(current_volume);
                        sink.set_speed(current_speed);
                        sink.append(analyzing);

                        current_sink = Some(sink);
                        current_source = Some(source);
                        current_duration_ms = duration_ms;
                        Ok(duration_ms)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::PlayRemote {
                    reader,
                    duration_hint_ms,
                    start_position_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        eprintln!(
                            "[audio-thread] PlayRemote: hint={}ms, start={}ms",
                            duration_hint_ms, start_position_ms
                        );
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let source = AudioSource::Remote(reader, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms = if dur > 0 { dur } else { duration_hint_ms };
                        let start_ms = Self::clamp_start_position(start_position_ms, duration_ms);
                        let (dec, used_native_seek) = Self::prepare_decoder_position(dec, start_ms);
                        if start_ms > 0 && !used_native_seek {
                            return Err("Remote source is not seekable".to_string());
                        }
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;
                        eprintln!(
                            "[audio-thread] remote decoder ok, duration={}ms",
                            duration_ms
                        );
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        sink.set_volume(current_volume);
                        sink.set_speed(current_speed);
                        sink.append(analyzing);

                        current_sink = Some(sink);
                        current_source = Some(source);
                        current_duration_ms = duration_ms;
                        Ok(duration_ms)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::Pause => {
                    cancel_active_transition(
                        &transition_generation,
                        &mut prev_sink,
                        &mut prev_source,
                        &mut prev_cleanup_deadline,
                    );
                    if let Some(ref sink) = current_sink {
                        sink.pause();
                    }
                }

                AudioCmd::Resume => {
                    if let Some(ref sink) = current_sink {
                        sink.play();
                    }
                }

                AudioCmd::Stop => {
                    cancel_active_transition(
                        &transition_generation,
                        &mut prev_sink,
                        &mut prev_source,
                        &mut prev_cleanup_deadline,
                    );
                    if let Some(old_source) = current_source.take() {
                        old_source.abort_if_stream();
                    }
                    if let Some(sink) = current_sink.take() {
                        sink.stop();
                    }
                    current_duration_ms = 0;
                    // 重置共享电平
                    SharedAudioLevel::reset(&shared_level);
                }

                AudioCmd::SetVolume(vol) => {
                    current_volume = vol;
                    if let Some(ref sink) = current_sink {
                        sink.set_volume(vol);
                    }
                }

                AudioCmd::SetSpeed(spd) => {
                    current_speed = spd;
                    if let Some(ref sink) = current_sink {
                        sink.set_speed(spd);
                    }
                }

                AudioCmd::Seek { position_ms } => {
                    let mut position_ms = position_ms;
                    take_latest_seek(&mut position_ms, &rx, &mut deferred_cmd);
                    let result = (|| -> Result<(), String> {
                        eprintln!("[audio-thread] Seek to {}ms", position_ms);

                        // 所有来源先尝试原生 seek（symphonia 对 File 和 Cursor<Vec<u8>> 都支持）
                        if let Some(ref sink) = current_sink {
                            if sink.try_seek(Duration::from_millis(position_ms)).is_ok() {
                                eprintln!("[audio-thread] Native seek OK");
                                return Ok(());
                            }
                            eprintln!("[audio-thread] Native seek failed, falling back to rebuild");
                        }

                        // 原生 seek 失败时重建 decoder；远端流必须走真正 seek，不能慢速跳样本
                        let source = current_source
                            .as_ref()
                            .cloned()
                            .ok_or_else(|| "Nothing is playing".to_string())?;
                        let seek_generation = playback_generation.load(Ordering::Acquire);
                        let source_is_remote = source.is_remote();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let was_paused = current_sink
                            .as_ref()
                            .map(|s| s.is_paused())
                            .unwrap_or(false);

                        if source_is_remote {
                            stop_prev_transition(
                                &mut prev_sink,
                                &mut prev_source,
                                &mut prev_cleanup_deadline,
                            );
                            if let Some(old_sink) = current_sink.take() {
                                old_sink.stop();
                            }
                            current_source = None;
                        }

                        let (dec, _) = Self::make_decoder(&source)?;
                        let (dec, used_native_seek) =
                            Self::prepare_decoder_position(dec, position_ms);
                        if source_is_remote && !used_native_seek {
                            return Err("Remote source is not seekable".to_string());
                        }
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            seek_generation,
                        )?;

                        // 音效链: Decoder → Equalizer → Loudness → Analyzer
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        sink.set_volume(current_volume);
                        sink.set_speed(current_speed);
                        sink.append(analyzing);
                        if was_paused {
                            sink.pause();
                        }

                        if !source_is_remote {
                            stop_prev_transition(
                                &mut prev_sink,
                                &mut prev_source,
                                &mut prev_cleanup_deadline,
                            );
                            if let Some(old_sink) = current_sink.take() {
                                old_sink.stop();
                            }
                        }

                        current_sink = Some(sink);
                        current_source = Some(source);
                        eprintln!("[audio-thread] Seek via rebuild OK");
                        Ok(())
                    })();
                    if let Err(error) = result {
                        eprintln!("[audio-thread] Seek failed: {}", error);
                    }

                    // 远端 seek 可能会阻塞在一次 Range 请求上，完成后再次合并期间到达的连续 seek
                    // 让拖动进度条最终落到最新位置，而不是反馈一个已经过期的中间位置
                    if deferred_cmd.is_none() {
                        if let Ok(next_cmd) = rx.try_recv() {
                            match next_cmd {
                                AudioCmd::Seek { position_ms } => {
                                    let mut latest_position_ms = position_ms;
                                    take_latest_seek(
                                        &mut latest_position_ms,
                                        &rx,
                                        &mut deferred_cmd,
                                    );
                                    if deferred_cmd.is_none() {
                                        deferred_cmd = Some(AudioCmd::Seek {
                                            position_ms: latest_position_ms,
                                        });
                                    }
                                }
                                next_cmd => deferred_cmd = Some(next_cmd),
                            }
                        }
                    }
                }

                AudioCmd::QueryEmpty { reply } => {
                    let empty = match &current_sink {
                        Some(sink) => sink.empty(),
                        None => true,
                    };
                    let _ = reply.send(empty);
                }

                AudioCmd::FadeOutPause { duration_ms, reply } => {
                    cancel_active_transition(
                        &transition_generation,
                        &mut prev_sink,
                        &mut prev_source,
                        &mut prev_cleanup_deadline,
                    );
                    let result = match current_sink.as_ref() {
                        Some(sink) => {
                            Self::do_fade_out_sink(sink, current_volume, duration_ms);
                            sink.pause();
                            // 恢复 volume 设置（pause 状态下不影响听感）
                            sink.set_volume(current_volume);
                            Ok(())
                        }
                        None => Err("Nothing is playing".to_string()),
                    };
                    let _ = reply.send(result);
                }

                AudioCmd::FadeInResume { duration_ms, reply } => {
                    let result = match current_sink.as_ref() {
                        Some(sink) => {
                            sink.set_volume(0.0);
                            sink.play();
                            Self::do_fade_in_sink(sink, current_volume, duration_ms);
                            Ok(())
                        }
                        None => Err("Nothing is playing".to_string()),
                    };
                    let _ = reply.send(result);
                }

                AudioCmd::CrossfadeBytes {
                    data,
                    duration_hint_ms,
                    fade_out_ms,
                    fade_in_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        // 创建新 sink
                        let source = AudioSource::Bytes(data);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms_val = if dur > 0 { dur } else { duration_hint_ms };
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        new_sink.set_volume(0.0); // 从 0 开始 fade in
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        ensure_playback_generation(&playback_generation, request_generation)?;
                        let generation = cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        current_sink = Some(new_sink.clone());
                        current_source = Some(source);
                        current_duration_ms = duration_ms_val;

                        if let Some(ref old) = old_sink {
                            prev_sink = Some(old.clone());
                            prev_source = old_source;
                            prev_cleanup_deadline = Some(
                                Instant::now()
                                    + Duration::from_millis(
                                        fade_out_ms.max(fade_in_ms) as u64 + FADE_STEP_MS * 2,
                                    ),
                            );
                        } else if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        Self::spawn_crossfade_worker(CrossfadeWorker {
                            old_sink,
                            new_sink,
                            target_volume: current_volume,
                            fade_out_ms,
                            fade_in_ms,
                            transition_generation: transition_generation.clone(),
                            expected_transition_generation: generation,
                            playback_generation: playback_generation.clone(),
                            expected_playback_generation: request_generation,
                        });

                        Ok(duration_ms_val)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::CrossfadeFile {
                    path,
                    duration_hint_ms,
                    fade_out_ms,
                    fade_in_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        // 创建新 sink
                        let source = AudioSource::File(path, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        new_sink.set_volume(0.0);
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        ensure_playback_generation(&playback_generation, request_generation)?;
                        let generation = cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        current_sink = Some(new_sink.clone());
                        current_source = Some(source);
                        current_duration_ms = dur;

                        if let Some(ref old) = old_sink {
                            prev_sink = Some(old.clone());
                            prev_source = old_source;
                            prev_cleanup_deadline = Some(
                                Instant::now()
                                    + Duration::from_millis(
                                        fade_out_ms.max(fade_in_ms) as u64 + FADE_STEP_MS * 2,
                                    ),
                            );
                        } else if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        Self::spawn_crossfade_worker(CrossfadeWorker {
                            old_sink,
                            new_sink,
                            target_volume: current_volume,
                            fade_out_ms,
                            fade_in_ms,
                            transition_generation: transition_generation.clone(),
                            expected_transition_generation: generation,
                            playback_generation: playback_generation.clone(),
                            expected_playback_generation: request_generation,
                        });

                        Ok(dur)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::CrossfadeStream {
                    reader,
                    duration_hint_ms,
                    fade_out_ms,
                    fade_in_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        eprintln!(
                            "[audio-thread] CrossfadeStream: hint={}ms",
                            duration_hint_ms
                        );
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let source = AudioSource::Growing(reader, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms_val = if dur > 0 { dur } else { duration_hint_ms };
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        new_sink.set_volume(0.0);
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        ensure_playback_generation(&playback_generation, request_generation)?;
                        let generation = cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        current_sink = Some(new_sink.clone());
                        current_source = Some(source);
                        current_duration_ms = duration_ms_val;

                        if let Some(ref old) = old_sink {
                            prev_sink = Some(old.clone());
                            prev_source = old_source;
                            prev_cleanup_deadline = Some(
                                Instant::now()
                                    + Duration::from_millis(
                                        fade_out_ms.max(fade_in_ms) as u64 + FADE_STEP_MS * 2,
                                    ),
                            );
                        } else if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        Self::spawn_crossfade_worker(CrossfadeWorker {
                            old_sink,
                            new_sink,
                            target_volume: current_volume,
                            fade_out_ms,
                            fade_in_ms,
                            transition_generation: transition_generation.clone(),
                            expected_transition_generation: generation,
                            playback_generation: playback_generation.clone(),
                            expected_playback_generation: request_generation,
                        });

                        Ok(duration_ms_val)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::CrossfadeRemote {
                    reader,
                    duration_hint_ms,
                    fade_out_ms,
                    fade_in_ms,
                    playback_generation: request_generation,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_playback_generation(&playback_generation, request_generation)?;
                        eprintln!(
                            "[audio-thread] CrossfadeRemote: hint={}ms",
                            duration_hint_ms
                        );
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let source = AudioSource::Remote(reader, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms_val = if dur > 0 { dur } else { duration_hint_ms };
                        let dec = Self::buffer_decoder(
                            &source,
                            dec,
                            &playback_generation,
                            request_generation,
                        )?;

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Arc::new(Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?);
                        new_sink.set_volume(0.0);
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        ensure_playback_generation(&playback_generation, request_generation)?;
                        let generation = cancel_active_transition(
                            &transition_generation,
                            &mut prev_sink,
                            &mut prev_source,
                            &mut prev_cleanup_deadline,
                        );
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        current_sink = Some(new_sink.clone());
                        current_source = Some(source);
                        current_duration_ms = duration_ms_val;

                        if let Some(ref old) = old_sink {
                            prev_sink = Some(old.clone());
                            prev_source = old_source;
                            prev_cleanup_deadline = Some(
                                Instant::now()
                                    + Duration::from_millis(
                                        fade_out_ms.max(fade_in_ms) as u64 + FADE_STEP_MS * 2,
                                    ),
                            );
                        } else if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        Self::spawn_crossfade_worker(CrossfadeWorker {
                            old_sink,
                            new_sink,
                            target_volume: current_volume,
                            fade_out_ms,
                            fade_in_ms,
                            transition_generation: transition_generation.clone(),
                            expected_transition_generation: generation,
                            playback_generation: playback_generation.clone(),
                            expected_playback_generation: request_generation,
                        });

                        Ok(duration_ms_val)
                    })();
                    let _ = reply.send(result);
                }
            }
        }
    }

    /// 播放本地文件
    pub fn play_file(&mut self, path: &str, expected_playback_generation: u64) -> AppResult<u64> {
        self.play_file_at_with_hint(path, 0, 0, expected_playback_generation)
    }

    pub fn play_file_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.play_file_at_with_hint(path, duration_hint_ms, 0, expected_playback_generation)
    }

    pub fn play_file_at(
        &mut self,
        path: &str,
        start_position_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.play_file_at_with_hint(path, 0, start_position_ms, expected_playback_generation)
    }

    pub fn play_file_at_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        start_position_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayFile {
                path: path.to_string(),
                duration_hint_ms,
                start_position_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some(path.to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = Self::clamp_start_position(start_position_ms, duration_ms);

        Ok(duration_ms)
    }

    /// 播放内存中的音频数据
    pub fn play_bytes(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.play_bytes_at(data, duration_hint_ms, 0, expected_playback_generation)
    }

    pub fn play_bytes_at(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        start_position_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayBytes {
                data,
                duration_hint_ms,
                start_position_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some("__stream__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = Self::clamp_start_position(start_position_ms, duration_ms);

        Ok(duration_ms)
    }

    /// 播放边下载边读取的音频流（用于远程 MP3 首播加速）
    pub fn play_stream(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.play_stream_at(reader, duration_hint_ms, 0, expected_playback_generation)
    }

    pub fn play_stream_at(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        start_position_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayStream {
                reader,
                duration_hint_ms,
                start_position_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some("__streaming__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = Self::clamp_start_position(start_position_ms, duration_ms);

        Ok(duration_ms)
    }

    pub fn play_remote_at(
        &mut self,
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        start_position_ms: u64,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayRemote {
                reader,
                duration_hint_ms,
                start_position_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some("__remote__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = Self::clamp_start_position(start_position_ms, duration_ms);

        Ok(duration_ms)
    }

    /// 获取当前播放位置（毫秒）
    /// 考虑播放速度：wall-clock 经过 1s 但 speed=1.5 时实际播放了 1.5s
    pub fn position_ms(&self) -> u64 {
        let elapsed = match (self.is_playing, self.play_start_time) {
            (true, Some(start)) => {
                let wall_ms = start.elapsed().as_millis() as f64;
                (wall_ms * self.speed as f64) as u64
            }
            _ => 0,
        };
        let pos = self.accumulated_ms + elapsed;
        if self.duration_ms > 0 {
            pos.min(self.duration_ms)
        } else {
            pos
        }
    }

    pub fn pause(&mut self) {
        if let Some(start) = self.play_start_time.take() {
            let wall_ms = start.elapsed().as_millis() as f64;
            self.accumulated_ms += (wall_ms * self.speed as f64) as u64;
        }
        let _ = self.cmd_tx.send(AudioCmd::Pause);
        self.is_playing = false;
    }

    pub fn resume(&mut self) {
        self.play_start_time = Some(Instant::now());
        let _ = self.cmd_tx.send(AudioCmd::Resume);
        self.is_playing = true;
    }

    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(AudioCmd::Stop);
        self.is_playing = false;
        self.current_path = None;
        self.duration_ms = 0;
        self.play_start_time = None;
        self.accumulated_ms = 0;
    }

    pub fn set_volume(&mut self, vol: f32) {
        self.volume = vol.clamp(0.0, 1.0);
        let _ = self.cmd_tx.send(AudioCmd::SetVolume(self.volume));
    }

    pub fn set_speed(&mut self, spd: f32) {
        if self.is_playing {
            if let Some(start) = self.play_start_time.take() {
                let wall_ms = start.elapsed().as_millis() as f64;
                self.accumulated_ms += (wall_ms * self.speed as f64) as u64;
                self.play_start_time = Some(Instant::now());
            }
        }
        self.speed = spd.clamp(0.25, 3.0);
        let _ = self.cmd_tx.send(AudioCmd::SetSpeed(self.speed));
    }

    /// 设置响度增益 (millibels, 0~1500)
    pub fn set_loudness_gain(&self, mb: i32) {
        let mb = mb.clamp(0, 1500);
        if let Ok(mut p) = self.effects_params.lock() {
            p.loudness_gain_mb = mb;
        }
    }

    /// 设置均衡器参数
    pub fn set_equalizer(&self, enabled: bool, bands: &[i32]) {
        if let Ok(mut p) = self.effects_params.lock() {
            p.eq_enabled = enabled;
            for (i, &val) in bands.iter().enumerate().take(5) {
                p.eq_band_levels_mb[i] = val.clamp(-1500, 1500);
            }
        }
    }

    /// 重置所有音效参数
    pub fn reset_effects(&self) {
        if let Ok(mut p) = self.effects_params.lock() {
            p.reset();
        }
    }

    /// Seek 到指定位置
    pub fn seek_to(&mut self, position_ms: u64) -> AppResult<()> {
        self.ensure_alive();
        let position_ms = Self::clamp_start_position(position_ms, self.duration_ms);
        self.cmd_tx
            .send(AudioCmd::Seek { position_ms })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        self.accumulated_ms = position_ms;
        if self.is_playing {
            self.play_start_time = Some(Instant::now());
        } else {
            self.play_start_time = None;
        }
        Ok(())
    }

    /// 检测播放是否自然结束
    pub fn is_finished(&self) -> bool {
        let elapsed_ms = match self.play_start_time {
            Some(start) => start.elapsed().as_millis() as u64,
            None => return false,
        };
        if elapsed_ms < 3000 {
            return false;
        }

        let (tx, rx) = mpsc::channel();
        if self
            .cmd_tx
            .send(AudioCmd::QueryEmpty { reply: tx })
            .is_err()
        {
            return true;
        }
        let sink_empty = query_empty_result(rx.recv_timeout(Duration::from_millis(200)));

        if !sink_empty {
            return false;
        }

        if self.duration_ms > 0 {
            let pos = self.position_ms();
            let threshold = self.duration_ms.saturating_sub(5000);
            if pos < threshold {
                return false;
            }
        }

        true
    }

    /// 标记播放结束
    pub fn mark_ended(&mut self) {
        if let Some(start) = self.play_start_time.take() {
            let wall_ms = start.elapsed().as_millis() as f64;
            self.accumulated_ms += (wall_ms * self.speed as f64) as u64;
        }
        self.is_playing = false;
    }

    /// 渐出后暂停，并等待音频线程完成渐出
    pub fn pause_with_fade(&mut self, duration_ms: u32) -> AppResult<()> {
        self.ensure_alive();
        if let Some(start) = self.play_start_time.take() {
            let wall_ms = start.elapsed().as_millis() as f64;
            self.accumulated_ms += (wall_ms * self.speed as f64) as u64;
        }
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::FadeOutPause {
                duration_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;
        self.is_playing = false;
        receive_fade_result(rx, duration_ms)
    }

    /// 渐入后恢复，并等待音频线程完成渐入
    pub fn resume_with_fade(&mut self, duration_ms: u32) -> AppResult<()> {
        self.ensure_alive();
        self.play_start_time = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::FadeInResume {
                duration_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;
        self.is_playing = true;
        receive_fade_result(rx, duration_ms)
    }

    /// Crossfade 播放内存音频数据
    pub fn crossfade_bytes(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeBytes {
                data,
                duration_hint_ms,
                fade_out_ms,
                fade_in_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        // crossfade 包含 fade 时间，给更长的超时
        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some("__stream__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }

    /// Crossfade 播放本地文件
    pub fn crossfade_file(
        &mut self,
        path: &str,
        fade_out_ms: u32,
        fade_in_ms: u32,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.crossfade_file_with_hint(
            path,
            0,
            fade_out_ms,
            fade_in_ms,
            expected_playback_generation,
        )
    }

    pub fn crossfade_file_with_hint(
        &mut self,
        path: &str,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeFile {
                path: path.to_string(),
                duration_hint_ms,
                fade_out_ms,
                fade_in_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some(path.to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }

    /// Crossfade 播放边下载边读取的音频流
    pub fn crossfade_stream(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeStream {
                reader,
                duration_hint_ms,
                fade_out_ms,
                fade_in_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some("__streaming__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }

    pub fn crossfade_remote(
        &mut self,
        reader: RemoteAudioSource,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        expected_playback_generation: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        self.ensure_playback_request_current(expected_playback_generation)?;
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeRemote {
                reader,
                duration_hint_ms,
                fade_out_ms,
                fade_in_ms,
                playback_generation: expected_playback_generation,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(AppError::Audio)?;
        self.ensure_playback_request_current(expected_playback_generation)?;

        self.is_playing = true;
        self.current_path = Some("__remote__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cancel_active_transition, duration_from_hint_or_else, ensure_playback_generation,
        fade_progress, fade_step_count, is_crossfade_worker_current, is_transition_current,
        next_transition_generation, query_empty_result, take_latest_seek, AudioCmd, AudioSource,
    };
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn fade_step_count_rounds_up_short_durations() {
        assert_eq!(fade_step_count(0), 0);
        assert_eq!(fade_step_count(1), 1);
        assert_eq!(fade_step_count(20), 1);
        assert_eq!(fade_step_count(21), 2);
        assert_eq!(fade_step_count(500), 25);
    }

    #[test]
    fn fade_progress_reaches_one_without_exceeding_range() {
        for duration_ms in [1, 20, 21, 500] {
            let last_step = fade_step_count(duration_ms);
            let progress = fade_progress(last_step, duration_ms);
            assert!((progress - 1.0).abs() < f32::EPSILON);
            assert!((0.0..=1.0).contains(&fade_progress(0, duration_ms)));
            assert!((0.0..=1.0).contains(&fade_progress(last_step + 1, duration_ms)));
        }
    }

    #[test]
    fn zero_duration_is_immediate() {
        assert_eq!(fade_progress(1, 0), 1.0);
    }

    #[test]
    fn known_duration_hint_skips_file_duration_probe() {
        let probed = Cell::new(false);
        let duration_ms = duration_from_hint_or_else(321_000, || {
            probed.set(true);
            123
        });

        assert_eq!(duration_ms, 321_000);
        assert!(!probed.get());
    }

    #[test]
    fn missing_duration_hint_uses_file_duration_probe() {
        let probed = Cell::new(false);
        let duration_ms = duration_from_hint_or_else(0, || {
            probed.set(true);
            123
        });

        assert_eq!(duration_ms, 123);
        assert!(probed.get());
    }

    #[test]
    fn seek_queue_keeps_only_the_latest_consecutive_position() {
        let (tx, rx) = mpsc::channel();
        assert!(tx.send(AudioCmd::Seek { position_ms: 100 }).is_ok());
        assert!(tx.send(AudioCmd::Seek { position_ms: 800 }).is_ok());
        assert!(tx.send(AudioCmd::Seek { position_ms: 1600 }).is_ok());

        let mut latest = 100;
        let mut deferred = None;
        take_latest_seek(&mut latest, &rx, &mut deferred);

        assert_eq!(latest, 1600);
        assert!(deferred.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn seek_queue_preserves_commands_after_the_seek_batch() {
        let (tx, rx) = mpsc::channel();
        assert!(tx.send(AudioCmd::Seek { position_ms: 100 }).is_ok());
        assert!(tx.send(AudioCmd::Pause).is_ok());
        assert!(tx.send(AudioCmd::Seek { position_ms: 800 }).is_ok());

        let mut latest = 100;
        let mut deferred = None;
        take_latest_seek(&mut latest, &rx, &mut deferred);

        assert_eq!(latest, 100);
        assert!(matches!(deferred, Some(AudioCmd::Pause)));
        assert!(matches!(
            rx.try_recv(),
            Ok(AudioCmd::Seek { position_ms: 800 })
        ));
    }

    #[test]
    fn newer_transition_invalidates_older_worker() {
        let generation = AtomicU64::new(0);
        let first = next_transition_generation(&generation);
        assert!(is_transition_current(&generation, first));

        let second = next_transition_generation(&generation);
        assert!(!is_transition_current(&generation, first));
        assert!(is_transition_current(&generation, second));
    }

    #[test]
    fn cancelling_transition_clears_previous_source_and_deadline() {
        let transition_generation = AtomicU64::new(0);
        let mut prev_sink = None;
        let mut prev_source = Some(AudioSource::Bytes(vec![1, 2, 3]));
        let mut deadline = Some(Instant::now() + Duration::from_secs(1));

        let generation = cancel_active_transition(
            &transition_generation,
            &mut prev_sink,
            &mut prev_source,
            &mut deadline,
        );

        assert_eq!(generation, 1);
        assert!(is_transition_current(&transition_generation, generation));
        assert!(prev_source.is_none());
        assert!(deadline.is_none());
    }

    #[test]
    fn playback_generation_rejects_superseded_request() {
        let generation = AtomicU64::new(7);
        assert!(ensure_playback_generation(&generation, 7).is_ok());

        generation.store(8, Ordering::Release);
        assert_eq!(
            ensure_playback_generation(&generation, 7),
            Err("Playback request superseded".to_string())
        );
        assert!(ensure_playback_generation(&generation, 8).is_ok());
    }

    #[test]
    fn crossfade_worker_requires_current_transition_and_playback_generation() {
        let transition_generation = AtomicU64::new(3);
        let playback_generation = AtomicU64::new(11);

        assert!(is_crossfade_worker_current(
            &transition_generation,
            3,
            &playback_generation,
            11,
        ));

        transition_generation.store(4, Ordering::Release);
        assert!(!is_crossfade_worker_current(
            &transition_generation,
            3,
            &playback_generation,
            11,
        ));

        transition_generation.store(3, Ordering::Release);
        playback_generation.store(12, Ordering::Release);
        assert!(!is_crossfade_worker_current(
            &transition_generation,
            3,
            &playback_generation,
            11,
        ));
    }

    #[test]
    fn query_timeout_does_not_report_track_finished() {
        assert!(!query_empty_result(Err(mpsc::RecvTimeoutError::Timeout)));
        assert!(query_empty_result(Err(
            mpsc::RecvTimeoutError::Disconnected
        )));
        assert!(query_empty_result(Ok(true)));
        assert!(!query_empty_result(Ok(false)));
    }
}
