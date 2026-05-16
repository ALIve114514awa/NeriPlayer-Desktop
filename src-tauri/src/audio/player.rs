// 音频播放引擎 — 专用线程架构
// OutputStream 是 !Send，必须在创建它的线程上操作。
// 所有 Sink 操作通过 channel 发送到专用音频线程执行。

use rodio::source::SeekError;
use rodio::{Decoder, OutputStream, Sink, Source};
use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::audio::analyzer::{AudioAnalyzer, SharedAudioLevel};
use crate::audio::effects::{AudioEffectsParams, EqualizerSource, LoudnessSource};
use crate::audio::growing::GrowingAudioReader;
use crate::error::{AppError, AppResult};

/// 播放操作 recv 超时（网络音频解码可能慢）
const RECV_TIMEOUT: Duration = Duration::from_secs(30);
/// seek/query 等快操作超时
const FAST_RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// 分析帧大小（样本数），~46ms@44.1kHz
const ANALYSIS_FRAME_SIZE: usize = 2048;

// 音频来源——seek 时需要重建 decoder
enum AudioSource {
    Bytes(Vec<u8>),
    File(String),
    Growing(GrowingAudioReader, u64),
}

impl AudioSource {
    fn abort_if_stream(&self) {
        if let AudioSource::Growing(reader, _) = self {
            reader.abort();
        }
    }
}

/// Fade 步进间隔
const FADE_STEP_MS: u64 = 20;

// 音频线程命令
enum AudioCmd {
    PlayBytes {
        data: Vec<u8>,
        duration_hint_ms: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    PlayFile {
        path: String,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    PlayStream {
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
    SetSpeed(f32),
    Seek {
        position_ms: u64,
        reply: mpsc::Sender<Result<(), String>>,
    },
    QueryEmpty {
        reply: mpsc::Sender<bool>,
    },
    /// 渐出后暂停：在 duration_ms 内将音量降至 0，然后 pause
    FadeOutPause {
        duration_ms: u32,
    },
    /// resume 后渐入：先 resume，然后在 duration_ms 内将音量从 0 升至目标音量
    FadeInResume {
        duration_ms: u32,
    },
    /// Crossfade 播放新字节流：对当前 sink fade out，新 sink fade in
    CrossfadeBytes {
        data: Vec<u8>,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    /// Crossfade 播放新文件
    CrossfadeFile {
        path: String,
        fade_out_ms: u32,
        fade_in_ms: u32,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    /// Crossfade 播放边下边读的音频流
    CrossfadeStream {
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
        reply: mpsc::Sender<Result<u64, String>>,
    },
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
        }
    }

    fn flush_buffer(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let result = self.analyzer.analyze_frame(&self.buffer);
        if let Ok(mut lock) = self.shared.lock() {
            lock.level = result.level;
            lock.beat_impulse = result.beat_impulse;
        }
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
        // 转换为 f32 并存入 buffer（仅取单声道用于分析）
        // 对于多声道，仅取第一个声道的样本
        let buf_len = self.buffer.len();
        let ch = self.channels as usize;
        // 每 channels 个样本取一个（左声道），保持分析帧对应实际时长
        if ch <= 1 || buf_len == 0 || (buf_len % 1 == 0) {
            // 简化：所有样本都收集，analyze_frame 按总样本数计算
            self.buffer.push(sample as f32 / 32768.0);
        }
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
}

unsafe impl Send for PlayerEngine {}

impl PlayerEngine {
    pub fn new() -> Self {
        let shared_level = SharedAudioLevel::new();
        let effects_params = AudioEffectsParams::new_shared();
        let (tx, alive) = Self::spawn_audio_thread(shared_level.clone(), effects_params.clone());
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
        }
    }

    /// 启动音频线程，返回 (命令发送端, 存活标记)
    fn spawn_audio_thread(
        shared_level: Arc<Mutex<SharedAudioLevel>>,
        effects_params: Arc<std::sync::Mutex<AudioEffectsParams>>,
    ) -> (mpsc::Sender<AudioCmd>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel::<AudioCmd>();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_flag = alive.clone();

        std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || {
                if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::audio_thread(rx, shared_level, effects_params);
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
            );
            self.cmd_tx = tx;
            self.thread_alive = alive;
            let _ = self.cmd_tx.send(AudioCmd::SetVolume(self.volume));
            if (self.speed - 1.0).abs() > 0.01 {
                let _ = self.cmd_tx.send(AudioCmd::SetSpeed(self.speed));
            }
        }
    }

    /// 从 source 创建 decoder，成功返回 (decoder_box, duration_ms)
    fn make_decoder(
        source: &AudioSource,
    ) -> Result<(Box<dyn Source<Item = i16> + Send>, u64), String> {
        match source {
            AudioSource::Bytes(data) => {
                let cursor = Cursor::new(data.clone());
                let dec = Decoder::new(cursor).map_err(|e| format!("Decode error: {}", e))?;
                let dur = dec
                    .total_duration()
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                Ok((Box::new(dec), dur))
            }
            AudioSource::File(path) => {
                let file =
                    std::fs::File::open(path).map_err(|e| format!("Cannot open file: {}", e))?;
                let reader = BufReader::new(file);
                let dec = Decoder::new(reader).map_err(|e| format!("Decode error: {}", e))?;
                let dur = dec
                    .total_duration()
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                Ok((Box::new(dec), dur))
            }
            AudioSource::Growing(reader, duration_hint_ms) => {
                let dec = Decoder::new(BufReader::new(reader.clone()))
                    .map_err(|e| format!("Decode error: {}", e))?;
                let dur = dec
                    .total_duration()
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(*duration_hint_ms);
                Ok((Box::new(dec), dur))
            }
        }
    }

    /// 在音频线程中执行 fade out（阻塞调用线程）
    fn do_fade_out_sink(sink: &Sink, target_volume: f32, duration_ms: u32) {
        if duration_ms == 0 {
            sink.set_volume(0.0);
            return;
        }
        let steps = (duration_ms as u64 / FADE_STEP_MS).max(1);
        let start_vol = target_volume;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
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
        let steps = (duration_ms as u64 / FADE_STEP_MS).max(1);
        sink.set_volume(0.0);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            sink.set_volume(target_volume * t);
            std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
        }
        sink.set_volume(target_volume);
    }

    /// 音频线程主循环
    fn audio_thread(
        rx: mpsc::Receiver<AudioCmd>,
        shared_level: Arc<Mutex<SharedAudioLevel>>,
        effects_params: Arc<std::sync::Mutex<AudioEffectsParams>>,
    ) {
        let mut stream: Option<OutputStream> = None;
        let mut handle: Option<rodio::OutputStreamHandle> = None;
        let mut current_sink: Option<Sink> = None;
        let mut prev_sink: Option<Sink> = None; // crossfade 过渡用
        let mut current_volume: f32 = 1.0;
        let mut current_speed: f32 = 1.0;
        // 保留当前音频来源，用于 seek 时重建 decoder
        let mut current_source: Option<AudioSource> = None;
        let mut current_duration_ms: u64 = 0;

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
            let cmd = match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    eprintln!("[audio-thread] channel disconnected, exiting");
                    break;
                }
            };

            match cmd {
                AudioCmd::PlayBytes {
                    data,
                    duration_hint_ms,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        let data_len = data.len();
                        eprintln!(
                            "[audio-thread] PlayBytes: {} bytes, hint={}ms",
                            data_len, duration_hint_ms
                        );

                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }

                        let source = AudioSource::Bytes(data);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms = if dur > 0 { dur } else { duration_hint_ms };

                        eprintln!("[audio-thread] decoded ok, duration={}ms", duration_ms);

                        // 音效链: Decoder → Equalizer → Loudness → Analyzer
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink = Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
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

                AudioCmd::PlayFile { path, reply } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }

                        let source = AudioSource::File(path);
                        let (dec, dur) = Self::make_decoder(&source)?;

                        // 音效链: Decoder → Equalizer → Loudness → Analyzer
                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink = Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
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
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        eprintln!("[audio-thread] PlayStream: hint={}ms", duration_hint_ms);
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        if let Some(old_source) = current_source.take() {
                            old_source.abort_if_stream();
                        }
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }

                        let source = AudioSource::Growing(reader, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms = if dur > 0 { dur } else { duration_hint_ms };
                        eprintln!(
                            "[audio-thread] streaming decoder ok, duration={}ms",
                            duration_ms
                        );

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink = Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
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

                AudioCmd::Seek { position_ms, reply } => {
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

                        // 原生 seek 失败——重建 decoder + skip samples（最后手段）
                        let source = current_source
                            .as_ref()
                            .ok_or_else(|| "Nothing is playing".to_string())?;
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        let was_paused = current_sink
                            .as_ref()
                            .map(|s| s.is_paused())
                            .unwrap_or(false);
                        if let Some(old_sink) = current_sink.take() {
                            old_sink.stop();
                        }

                        let (dec, _) = Self::make_decoder(source)?;
                        let skip_duration = Duration::from_millis(position_ms);
                        let skipped = dec.skip_duration(skip_duration);

                        // 音效链: Decoder → Equalizer → Loudness → Analyzer
                        let eq = EqualizerSource::new(skipped, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let sink = Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
                        sink.set_volume(current_volume);
                        sink.set_speed(current_speed);
                        sink.append(analyzing);
                        if was_paused {
                            sink.pause();
                        }

                        current_sink = Some(sink);
                        eprintln!("[audio-thread] Seek via rebuild OK");
                        Ok(())
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::QueryEmpty { reply } => {
                    let empty = match &current_sink {
                        Some(sink) => sink.empty(),
                        None => true,
                    };
                    let _ = reply.send(empty);
                }

                AudioCmd::FadeOutPause { duration_ms } => {
                    if let Some(ref sink) = current_sink {
                        Self::do_fade_out_sink(sink, current_volume, duration_ms);
                        sink.pause();
                        // 恢复 volume 设置（pause 状态下不影响听感）
                        sink.set_volume(current_volume);
                    }
                }

                AudioCmd::FadeInResume { duration_ms } => {
                    if let Some(ref sink) = current_sink {
                        sink.set_volume(0.0);
                        sink.play();
                        Self::do_fade_in_sink(sink, current_volume, duration_ms);
                    }
                }

                AudioCmd::CrossfadeBytes {
                    data,
                    duration_hint_ms,
                    fade_out_ms,
                    fade_in_ms,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        // 将当前 sink 移入 prev_sink 进行 fade out
                        if let Some(old_prev) = prev_sink.take() {
                            old_prev.stop();
                        }
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        // 创建新 sink
                        let source = AudioSource::Bytes(data);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms_val = if dur > 0 { dur } else { duration_hint_ms };

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
                        new_sink.set_volume(0.0); // 从 0 开始 fade in
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        current_sink = Some(new_sink);
                        current_source = Some(source);
                        current_duration_ms = duration_ms_val;

                        // 执行 crossfade（在音频线程中阻塞）
                        let fade_duration = fade_out_ms.max(fade_in_ms);
                        let steps = (fade_duration as u64 / FADE_STEP_MS).max(1);
                        for i in 1..=steps {
                            // fade out 旧 sink
                            if let Some(ref old) = old_sink {
                                let out_t = (i as f32
                                    / (fade_out_ms as u64 / FADE_STEP_MS).max(1) as f32)
                                    .min(1.0);
                                old.set_volume(current_volume * (1.0 - out_t));
                            }
                            // fade in 新 sink
                            if let Some(ref new_s) = current_sink {
                                let in_t = (i as f32
                                    / (fade_in_ms as u64 / FADE_STEP_MS).max(1) as f32)
                                    .min(1.0);
                                new_s.set_volume(current_volume * in_t);
                            }
                            std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
                        }
                        // 清理旧 sink
                        if let Some(old) = old_sink {
                            old.stop();
                        }
                        if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        // 确保新 sink 到目标音量
                        if let Some(ref s) = current_sink {
                            s.set_volume(current_volume);
                        }

                        Ok(duration_ms_val)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::CrossfadeFile {
                    path,
                    fade_out_ms,
                    fade_in_ms,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        // 将当前 sink 移入 prev_sink 进行 fade out
                        if let Some(old_prev) = prev_sink.take() {
                            old_prev.stop();
                        }
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        // 创建新 sink
                        let source = AudioSource::File(path);
                        let (dec, dur) = Self::make_decoder(&source)?;

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
                        new_sink.set_volume(0.0);
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        current_sink = Some(new_sink);
                        current_source = Some(source);
                        current_duration_ms = dur;

                        // 执行 crossfade
                        let fade_duration = fade_out_ms.max(fade_in_ms);
                        let steps = (fade_duration as u64 / FADE_STEP_MS).max(1);
                        for i in 1..=steps {
                            if let Some(ref old) = old_sink {
                                let out_t = (i as f32
                                    / (fade_out_ms as u64 / FADE_STEP_MS).max(1) as f32)
                                    .min(1.0);
                                old.set_volume(current_volume * (1.0 - out_t));
                            }
                            if let Some(ref new_s) = current_sink {
                                let in_t = (i as f32
                                    / (fade_in_ms as u64 / FADE_STEP_MS).max(1) as f32)
                                    .min(1.0);
                                new_s.set_volume(current_volume * in_t);
                            }
                            std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
                        }
                        if let Some(old) = old_sink {
                            old.stop();
                        }
                        if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        if let Some(ref s) = current_sink {
                            s.set_volume(current_volume);
                        }

                        Ok(dur)
                    })();
                    let _ = reply.send(result);
                }

                AudioCmd::CrossfadeStream {
                    reader,
                    duration_hint_ms,
                    fade_out_ms,
                    fade_in_ms,
                    reply,
                } => {
                    let result = (|| -> Result<u64, String> {
                        eprintln!(
                            "[audio-thread] CrossfadeStream: hint={}ms",
                            duration_hint_ms
                        );
                        ensure_output!();
                        let h = handle
                            .as_ref()
                            .ok_or_else(|| "No audio output available".to_string())?;

                        if let Some(old_prev) = prev_sink.take() {
                            old_prev.stop();
                        }
                        let old_source = current_source.take();
                        let old_sink = current_sink.take();

                        let source = AudioSource::Growing(reader, duration_hint_ms);
                        let (dec, dur) = Self::make_decoder(&source)?;
                        let duration_ms_val = if dur > 0 { dur } else { duration_hint_ms };

                        let eq = EqualizerSource::new(dec, effects_params.clone());
                        let loud = LoudnessSource::new(eq, effects_params.clone());
                        let analyzing = AnalyzingSource::new(loud, shared_level.clone());

                        let new_sink =
                            Sink::try_new(h).map_err(|e| format!("Sink error: {}", e))?;
                        new_sink.set_volume(0.0);
                        new_sink.set_speed(current_speed);
                        new_sink.append(analyzing);

                        current_sink = Some(new_sink);
                        current_source = Some(source);
                        current_duration_ms = duration_ms_val;

                        let fade_duration = fade_out_ms.max(fade_in_ms);
                        let steps = (fade_duration as u64 / FADE_STEP_MS).max(1);
                        for i in 1..=steps {
                            if let Some(ref old) = old_sink {
                                let out_t = (i as f32
                                    / (fade_out_ms as u64 / FADE_STEP_MS).max(1) as f32)
                                    .min(1.0);
                                old.set_volume(current_volume * (1.0 - out_t));
                            }
                            if let Some(ref new_s) = current_sink {
                                let in_t = (i as f32
                                    / (fade_in_ms as u64 / FADE_STEP_MS).max(1) as f32)
                                    .min(1.0);
                                new_s.set_volume(current_volume * in_t);
                            }
                            std::thread::sleep(Duration::from_millis(FADE_STEP_MS));
                        }
                        if let Some(old) = old_sink {
                            old.stop();
                        }
                        if let Some(old_source) = old_source {
                            old_source.abort_if_stream();
                        }
                        if let Some(ref s) = current_sink {
                            s.set_volume(current_volume);
                        }

                        Ok(duration_ms_val)
                    })();
                    let _ = reply.send(result);
                }
            }

            // 清理已完成的 prev_sink（crossfade 残留）
            if let Some(ref ps) = prev_sink {
                if ps.empty() {
                    prev_sink = None;
                }
            }
        }
    }

    /// 播放本地文件
    pub fn play_file(&mut self, path: &str) -> AppResult<u64> {
        self.ensure_alive();
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayFile {
                path: path.to_string(),
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

        self.is_playing = true;
        self.current_path = Some(path.to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }

    /// 播放内存中的音频数据
    pub fn play_bytes(&mut self, data: Vec<u8>, duration_hint_ms: u64) -> AppResult<u64> {
        self.ensure_alive();
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayBytes {
                data,
                duration_hint_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

        self.is_playing = true;
        self.current_path = Some("__stream__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }

    /// 播放边下载边读取的音频流（用于远程 MP3 首播加速）
    pub fn play_stream(
        &mut self,
        reader: GrowingAudioReader,
        duration_hint_ms: u64,
    ) -> AppResult<u64> {
        self.ensure_alive();
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::PlayStream {
                reader,
                duration_hint_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let duration_ms = rx
            .recv_timeout(RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

        self.is_playing = true;
        self.current_path = Some("__streaming__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

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
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::Seek {
                position_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        rx.recv_timeout(FAST_RECV_TIMEOUT)
            .map_err(|e| AppError::Audio(format!("Seek timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

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
        let sink_empty = rx.recv_timeout(Duration::from_millis(200)).unwrap_or(true);

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

    /// 渐出后暂停（fire-and-forget，音频线程异步执行）
    pub fn pause_with_fade(&mut self, duration_ms: u32) {
        if let Some(start) = self.play_start_time.take() {
            let wall_ms = start.elapsed().as_millis() as f64;
            self.accumulated_ms += (wall_ms * self.speed as f64) as u64;
        }
        let _ = self.cmd_tx.send(AudioCmd::FadeOutPause { duration_ms });
        self.is_playing = false;
    }

    /// 渐入后恢复（fire-and-forget）
    pub fn resume_with_fade(&mut self, duration_ms: u32) {
        self.play_start_time = Some(Instant::now());
        let _ = self.cmd_tx.send(AudioCmd::FadeInResume { duration_ms });
        self.is_playing = true;
    }

    /// Crossfade 播放内存音频数据
    pub fn crossfade_bytes(
        &mut self,
        data: Vec<u8>,
        duration_hint_ms: u64,
        fade_out_ms: u32,
        fade_in_ms: u32,
    ) -> AppResult<u64> {
        self.ensure_alive();
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeBytes {
                data,
                duration_hint_ms,
                fade_out_ms,
                fade_in_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        // crossfade 包含 fade 时间，给更长的超时
        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

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
    ) -> AppResult<u64> {
        self.ensure_alive();
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeFile {
                path: path.to_string(),
                fade_out_ms,
                fade_in_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

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
    ) -> AppResult<u64> {
        self.ensure_alive();
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(AudioCmd::CrossfadeStream {
                reader,
                duration_hint_ms,
                fade_out_ms,
                fade_in_ms,
                reply: tx,
            })
            .map_err(|_| AppError::Audio("Audio thread disconnected".into()))?;

        let timeout =
            RECV_TIMEOUT + Duration::from_millis((fade_out_ms.max(fade_in_ms) + 1000) as u64);
        let duration_ms = rx
            .recv_timeout(timeout)
            .map_err(|e| AppError::Audio(format!("Audio thread timeout: {}", e)))?
            .map_err(|e| AppError::Audio(e))?;

        self.is_playing = true;
        self.current_path = Some("__streaming__".to_string());
        self.duration_ms = duration_ms;
        self.play_start_time = Some(Instant::now());
        self.accumulated_ms = 0;

        Ok(duration_ms)
    }
}
