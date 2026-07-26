// 音效 DSP 模块：响度增益 + 5频段参数均衡器
// 通过 Arc<Mutex<AudioEffectsParams>> 与前端实时同步参数

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::audio::pcm::{PcmSeekError, PcmSource};

// 共享音效参数
/// 运行时可变的音效参数，通过 Arc<Mutex<>> 共享给音频线程
pub struct AudioEffectsParams {
    /// 响度增益 (millibels)，范围 0~1500 (0 ~ +15.0 dB)
    pub loudness_gain_mb: i32,
    /// 均衡器是否启用
    pub eq_enabled: bool,
    /// 5 频段增益 (millibels)，范围 -1500~1500 per band
    /// 频段中心频率: 60, 230, 910, 3600, 14000 Hz
    pub eq_band_levels_mb: [i32; 5],
    /// 音量均衡：把不同曲目的响度拉到统一目标，避免切歌忽大忽小
    pub normalize_volume: bool,
}

impl Default for AudioEffectsParams {
    fn default() -> Self {
        Self {
            loudness_gain_mb: 0,
            eq_enabled: false,
            eq_band_levels_mb: [0; 5],
            normalize_volume: false,
        }
    }
}

impl AudioEffectsParams {
    pub fn new_shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }

    /// 检查是否所有参数都是默认值
    pub fn is_default(&self) -> bool {
        self.loudness_gain_mb == 0
            && !self.eq_enabled
            && !self.normalize_volume
            && self.eq_band_levels_mb.iter().all(|&v| v == 0)
    }

    /// 重置所有参数
    pub fn reset(&mut self) {
        self.loudness_gain_mb = 0;
        self.eq_enabled = false;
        self.eq_band_levels_mb = [0; 5];
        self.normalize_volume = false;
    }
}

// Biquad 滤波器
// 参考: Audio EQ Cookbook (Robert Bristow-Johnson)
// 使用 Peaking EQ 类型，Q = 1.0

#[derive(Clone)]
struct BiquadFilter {
    // 系数
    b0: f64, b1: f64, b2: f64,
    a1: f64, a2: f64,
    // 延迟线 (per channel)
    x1: Vec<f64>, x2: Vec<f64>,
    y1: Vec<f64>, y2: Vec<f64>,
    // 当前参数快照
    center_freq: f64,
    gain_db: f64,
    sample_rate: f64,
}

impl BiquadFilter {
    fn new(center_freq: f64, gain_db: f64, sample_rate: f64, channels: usize) -> Self {
        let mut f = Self {
            b0: 1.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            x1: vec![0.0; channels],
            x2: vec![0.0; channels],
            y1: vec![0.0; channels],
            y2: vec![0.0; channels],
            center_freq,
            gain_db,
            sample_rate,
        };
        f.compute_coefficients(center_freq, gain_db, sample_rate);
        f
    }

    /// 计算 Peaking EQ Biquad 系数 (Audio EQ Cookbook)
    fn compute_coefficients(&mut self, freq: f64, gain_db: f64, sample_rate: f64) {
        self.center_freq = freq;
        self.gain_db = gain_db;
        self.sample_rate = sample_rate;

        if gain_db.abs() < 0.01 {
            // 增益为 0，直通
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
            return;
        }

        let a = 10.0_f64.powf(gain_db / 40.0); // sqrt(10^(dB/20))
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let q = 1.0; // Q factor
        let alpha = w0.sin() / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * w0.cos();
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * w0.cos();
        let a2 = 1.0 - alpha / a;

        // 归一化
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    /// 处理单个样本（指定声道）
    fn process(&mut self, sample: f64, ch: usize) -> f64 {
        let out = self.b0 * sample + self.b1 * self.x1[ch] + self.b2 * self.x2[ch]
            - self.a1 * self.y1[ch] - self.a2 * self.y2[ch];
        self.x2[ch] = self.x1[ch];
        self.x1[ch] = sample;
        self.y2[ch] = self.y1[ch];
        self.y1[ch] = out;
        out
    }

    /// 重置延迟线
    fn reset(&mut self) {
        for v in self.x1.iter_mut().chain(self.x2.iter_mut())
            .chain(self.y1.iter_mut()).chain(self.y2.iter_mut()) {
            *v = 0.0;
        }
    }
}

// EqualizerSource
// 5 频段参数均衡器，串联 5 个 Biquad Peaking EQ

/// 5 频段均衡器中心频率
const EQ_FREQS: [f64; 5] = [60.0, 230.0, 910.0, 3600.0, 14000.0];
/// 参数更新检查间隔（样本数）
/// 解码线程每帧都会推进；配合 ring 尾部丢弃，开关体感 <100ms
const PARAM_CHECK_INTERVAL: usize = 32;

pub struct EqualizerSource<S> {
    inner: S,
    params: Arc<Mutex<AudioEffectsParams>>,
    filters: [BiquadFilter; 5],
    channels: u16,
    sample_rate: u32,
    // 当前参数快照
    enabled: bool,
    band_gains_db: [f64; 5],
    // 样本计数器，用于周期性检查参数
    sample_counter: usize,
    // 当前声道索引
    current_channel: usize,
}

impl<S> EqualizerSource<S>
where
    S: PcmSource,
{
    pub fn new(source: S, params: Arc<Mutex<AudioEffectsParams>>) -> Self {
        let channels = source.channels();
        let sample_rate = source.sample_rate();
        let ch = channels as usize;

        let (enabled, band_gains_db) = {
            let p = params.lock().unwrap();
            let gains: [f64; 5] = std::array::from_fn(|i| p.eq_band_levels_mb[i] as f64 / 100.0);
            (p.eq_enabled, gains)
        };

        let filters: [BiquadFilter; 5] = std::array::from_fn(|i| {
            BiquadFilter::new(EQ_FREQS[i], band_gains_db[i], sample_rate as f64, ch)
        });

        Self {
            inner: source,
            params,
            filters,
            channels,
            sample_rate,
            enabled,
            band_gains_db,
            sample_counter: 0,
            current_channel: 0,
        }
    }

    fn update_params(&mut self) {
        let p = match self.params.try_lock() {
            Ok(p) => p,
            Err(_) => return,
        };

        let was_enabled = self.enabled;
        self.enabled = p.eq_enabled;

        // 关闭均衡器时清空延迟线，避免关闭后仍残留滤波状态
        if was_enabled && !self.enabled {
            for filter in self.filters.iter_mut() {
                filter.reset();
            }
        }

        let sr = self.sample_rate as f64;
        for i in 0..5 {
            // 关闭时强制按 0dB 更新系数，重新开启前不会带着旧增益
            let new_db = if self.enabled {
                p.eq_band_levels_mb[i] as f64 / 100.0
            } else {
                0.0
            };
            if (new_db - self.band_gains_db[i]).abs() > 0.01 {
                self.band_gains_db[i] = new_db;
                self.filters[i].compute_coefficients(EQ_FREQS[i], new_db, sr);
            }
        }
    }
}

impl<S> Iterator for EqualizerSource<S>
where
    S: PcmSource,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;

        // 周期性检查参数；开关边沿在 update_params 内立刻 reset 延迟线
        self.sample_counter += 1;
        if self.sample_counter >= PARAM_CHECK_INTERVAL {
            self.sample_counter = 0;
            self.update_params();
        }

        if !self.enabled {
            // 关闭时完全直通，不消耗滤波器状态
            self.current_channel = (self.current_channel + 1) % self.channels as usize;
            return Some(sample);
        }

        let ch = self.current_channel;
        self.current_channel = (self.current_channel + 1) % self.channels as usize;

        // 串联 5 个滤波器
        let mut val = f64::from(sample);
        for filter in self.filters.iter_mut() {
            val = filter.process(val, ch);
        }

        Some(val.clamp(-1.0, 1.0) as f32)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> PcmSource for EqualizerSource<S>
where
    S: PcmSource,
{
    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), PcmSeekError> {
        let result = self.inner.try_seek(pos);
        if result.is_ok() {
            for f in self.filters.iter_mut() {
                f.reset();
            }
            self.current_channel = 0;
        }
        result
    }
}

// LoudnessSource
// 响度增益：将 millibels 转为线性增益，对每个 sample 乘以增益

/// 音量均衡目标 RMS：约 -20 dBFS，接近流媒体常用的响度基准
const NORMALIZE_TARGET_RMS: f64 = 0.1;
/// 均衡增益上下限（线性），对应约 -6 dB ~ +6 dB，避免把噪底也抬起来
const NORMALIZE_MIN_GAIN: f64 = 0.5;
const NORMALIZE_MAX_GAIN: f64 = 2.0;
/// RMS 观测窗的 EMA 系数：越小越慢，慢才不会把强弱起伏压平（泵浦感）
const NORMALIZE_RMS_ALPHA: f64 = 0.000_02;
/// 增益本身再平滑一次，切换时不会有可听见的台阶
const NORMALIZE_GAIN_ALPHA: f64 = 0.000_05;
/// 低于此 RMS 视为静音/前奏，不参与测量也不调整增益
const NORMALIZE_SILENCE_RMS: f64 = 0.001;

pub struct LoudnessSource<S> {
    inner: S,
    params: Arc<Mutex<AudioEffectsParams>>,
    channels: u16,
    sample_rate: u32,
    // 当前增益快照
    gain: f64,
    gain_mb: i32,
    sample_counter: usize,
    normalize_enabled: bool,
    /// 观测到的信号 RMS（慢 EMA）
    normalize_rms: f64,
    /// 实际施加的均衡增益（再平滑一次）
    normalize_gain: f64,
}

impl<S> LoudnessSource<S>
where
    S: PcmSource,
{
    pub fn new(source: S, params: Arc<Mutex<AudioEffectsParams>>) -> Self {
        let channels = source.channels();
        let sample_rate = source.sample_rate();

        let (gain_mb, normalize_enabled) = params
            .lock()
            .map(|p| (p.loudness_gain_mb, p.normalize_volume))
            .unwrap_or((0, false));
        let gain = mb_to_linear(gain_mb);

        Self {
            inner: source,
            params,
            channels,
            sample_rate,
            gain,
            gain_mb,
            sample_counter: 0,
            normalize_enabled,
            normalize_rms: 0.0,
            normalize_gain: 1.0,
        }
    }

    fn update_params(&mut self) {
        if let Ok(p) = self.params.try_lock() {
            if p.loudness_gain_mb != self.gain_mb {
                self.gain_mb = p.loudness_gain_mb;
                self.gain = mb_to_linear(self.gain_mb);
            }
            if p.normalize_volume != self.normalize_enabled {
                self.normalize_enabled = p.normalize_volume;
                // 关掉时把增益收回 1.0，避免残留偏置
                if !self.normalize_enabled {
                    self.normalize_gain = 1.0;
                    self.normalize_rms = 0.0;
                }
            }
        }
    }

    /// 跟踪信号 RMS 并把增益缓慢推向目标响度
    fn advance_normalizer(&mut self, sample: f32) -> f64 {
        let magnitude = f64::from(sample).abs();
        self.normalize_rms += NORMALIZE_RMS_ALPHA * (magnitude - self.normalize_rms);
        if self.normalize_rms <= NORMALIZE_SILENCE_RMS {
            return self.normalize_gain;
        }
        let desired =
            (NORMALIZE_TARGET_RMS / self.normalize_rms).clamp(NORMALIZE_MIN_GAIN, NORMALIZE_MAX_GAIN);
        self.normalize_gain += NORMALIZE_GAIN_ALPHA * (desired - self.normalize_gain);
        self.normalize_gain
    }
}

impl<S> Iterator for LoudnessSource<S>
where
    S: PcmSource,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let sample = self.inner.next()?;

        self.sample_counter += 1;
        if self.sample_counter >= PARAM_CHECK_INTERVAL {
            self.sample_counter = 0;
            self.update_params();
        }

        if self.gain_mb == 0 && !self.normalize_enabled {
            return Some(sample);
        }

        let normalize_gain = if self.normalize_enabled {
            self.advance_normalizer(sample)
        } else {
            1.0
        };
        let val = f64::from(sample) * self.gain * normalize_gain;
        Some(val.clamp(-1.0, 1.0) as f32)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> PcmSource for LoudnessSource<S>
where
    S: PcmSource,
{
    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), PcmSeekError> {
        self.inner.try_seek(pos)
    }
}

/// millibels -> 线性增益: gain = 10^(mb / 2000)
fn mb_to_linear(mb: i32) -> f64 {
    if mb == 0 { 1.0 } else { 10.0_f64.powf(mb as f64 / 2000.0) }
}
