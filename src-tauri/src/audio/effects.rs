// 音效 DSP 模块：响度增益 + 5频段参数均衡器
// 通过 Arc<Mutex<AudioEffectsParams>> 与前端实时同步参数

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::audio::pcm::{PcmSeekError, PcmSource};

// 共享音效参数
/// 运行时可变的音效参数，通过 Arc<Mutex<>> 共享给音频线程
#[derive(Clone, Copy, Default)]
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

/// 读取实时参数，并从锁毒化中恢复
///
/// `AudioEffectsParams` 全是标量与定长数组，panic 打断一次写入不会留下
/// 半成品状态，所以毒化后 `into_inner()` 拿到的值照样可用。
/// 这里必须恢复而不是 panic：这个锁在音频回调路径上，一旦有别的线程
/// 持锁时 panic，后续每一帧都会跟着炸。也不能静默用默认值——那等于
/// 用户的 EQ 和响度设置无声失效。
fn read_params(params: &Mutex<AudioEffectsParams>) -> AudioEffectsParams {
    match params.lock() {
        Ok(guard) => *guard,
        Err(poisoned) => {
            log::warn!(
                target: "audio-effects",
                "effects params lock was poisoned, recovering the last value",
            );
            *poisoned.into_inner()
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
            let p = read_params(&params);
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
        for (i, &freq) in EQ_FREQS.iter().enumerate() {
            // 关闭时强制按 0dB 更新系数，重新开启前不会带着旧增益
            let new_db = if self.enabled {
                p.eq_band_levels_mb[i] as f64 / 100.0
            } else {
                0.0
            };
            if (new_db - self.band_gains_db[i]).abs() > 0.01 {
                self.band_gains_db[i] = new_db;
                self.filters[i].compute_coefficients(freq, new_db, sr);
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

// 音量均衡（对齐 Android VolumeNormalizationAudioProcessor.kt 的代数与常量）
//
// 语义是「平衡不同歌曲之间的响度」，一首歌内部的强弱起伏是原声，绝不能动。
// 因此用**整轨积分 RMS**而不是滑动窗口：积分统计从曲子开头累到当前，
// 样本越多越稳，播放约半分钟后增益自然收敛为常数——之后副歌再响、
// 间隙再静，积分值都几乎不动，增益恒定，动态范围原样保留。
// （此前两版都用 EMA 滑动窗口，无论时间常数多长都在「遗忘旧数据、
// 追新数据」，本质是慢速压缩器，曲内必然忽大忽小。）
//
// 行业同款：ReplayGain 2.0 / EBU R128 也是整轨积分响度 -> 每曲一个静态增益。

/// 目标 RMS：-18 dBFS，与 ReplayGain 2.0 的目标响度一致
const NORMALIZE_TARGET_RMS: f64 = 0.125_892_54;
/// 静音门：低于 -55 dBFS 的块不参与积分（前奏留白/淡出不拉偏统计）
const NORMALIZE_SILENCE_GATE_RMS: f64 = 0.001_778_28;
/// 增益下限 -12 dB / 上限 +6 dB
const NORMALIZE_MIN_GAIN: f64 = 0.251_188_64;
const NORMALIZE_MAX_GAIN: f64 = 1.995_262_3;
/// 峰值封顶 -2 dBFS：增益不得把已观测峰推过此线，防削波
const NORMALIZE_PEAK_CEILING: f64 = 0.794_328_2;
/// 增益下调时间常数（防爆音要快）
const NORMALIZE_REDUCTION_SECONDS: f64 = 0.25;
/// 增益上调时间常数（缓慢抬，收敛期不突兀）
const NORMALIZE_INCREASE_SECONDS: f64 = 4.0;
/// 积分统计的观测块：每块重算一次目标增益（48kHz 立体声约 43ms）
const NORMALIZE_BLOCK_SAMPLES: usize = 4_096;
/// 起播预热窗：先测后放，避免响曲以全响度起头再被压下
const NORMALIZE_WARMUP_SECONDS: f64 = 0.2;

/// 整轨积分归一化器状态
#[derive(Default)]
struct IntegratedNormalizer {
    /// 全曲累积平方和 / 样本数 / 峰值——只增不减，收敛后即为常数
    accumulated_sum_squares: f64,
    analyzed_samples: u64,
    analyzed_peak: f64,
    /// 当前观测块
    block_sum_squares: f64,
    block_peak: f64,
    block_samples: usize,
    /// 目标增益与逐样本平滑后的当前增益
    target_gain: f64,
    current_gain: f64,
}

impl IntegratedNormalizer {
    fn new() -> Self {
        Self {
            target_gain: 1.0,
            current_gain: 1.0,
            ..Self::default()
        }
    }

    /// 预热期只累计统计，不产生增益决策
    fn observe(&mut self, sample: f32) {
        let magnitude = f64::from(sample).abs();
        self.block_sum_squares += magnitude * magnitude;
        self.block_peak = self.block_peak.max(magnitude);
        self.block_samples += 1;
        if self.block_samples >= NORMALIZE_BLOCK_SAMPLES {
            self.fold_block();
        }
    }

    /// 预热结束：用已观测的统计直接定初始增益，当前增益与目标同时落位
    ///
    /// 没有这一步，响曲会以 1.0 全响度开播、43ms 后才被 0.25s 时间常数
    /// 压下来——「开头特别响然后忽然压下去」。预热样本先测后放，
    /// 第一个可闻样本就是正确响度。
    fn seed_from_observations(&mut self, total_static_gain: f64) {
        // 把不足一块的残余也算进去，短预热窗不浪费
        if self.block_samples > 0 {
            self.fold_block();
        }
        if self.analyzed_samples > 0 {
            self.recompute_target(total_static_gain);
            self.current_gain = self.target_gain;
        }
    }

    fn fold_block(&mut self) {
        let block_rms = (self.block_sum_squares / self.block_samples.max(1) as f64).sqrt();
        if block_rms >= NORMALIZE_SILENCE_GATE_RMS {
            self.accumulated_sum_squares += self.block_sum_squares;
            self.analyzed_samples += self.block_samples as u64;
            self.analyzed_peak = self.analyzed_peak.max(self.block_peak);
        }
        self.block_sum_squares = 0.0;
        self.block_peak = 0.0;
        self.block_samples = 0;
    }

    fn recompute_target(&mut self, total_static_gain: f64) {
        if self.analyzed_samples == 0 {
            return;
        }
        let integrated_rms =
            (self.accumulated_sum_squares / self.analyzed_samples as f64).sqrt();
        let rms_gain = (NORMALIZE_TARGET_RMS / integrated_rms)
            .clamp(NORMALIZE_MIN_GAIN, NORMALIZE_MAX_GAIN);
        let peak_gain = if self.analyzed_peak > 0.0 {
            NORMALIZE_PEAK_CEILING
                / (self.analyzed_peak * total_static_gain.max(f64::MIN_POSITIVE))
        } else {
            NORMALIZE_MAX_GAIN
        };
        self.target_gain = rms_gain
            .min(peak_gain)
            .clamp(NORMALIZE_MIN_GAIN, NORMALIZE_MAX_GAIN);
    }

    /// 推进一个样本，返回应施加的归一化增益
    ///
    /// `total_static_gain` 是用户手动响度档的线性增益，峰值封顶要按
    /// 总增益算，否则手动 +9dB 时归一化以为还有余量。
    fn advance(&mut self, sample: f32, samples_per_second: f64, total_static_gain: f64) -> f64 {
        let magnitude = f64::from(sample).abs();
        self.block_sum_squares += magnitude * magnitude;
        self.block_peak = self.block_peak.max(magnitude);
        self.block_samples += 1;

        if self.block_samples >= NORMALIZE_BLOCK_SAMPLES {
            let had = self.analyzed_samples;
            self.fold_block();
            if self.analyzed_samples > had {
                self.recompute_target(total_static_gain);
            }
        }

        // 非对称逐样本平滑：降快（防爆音）升慢（收敛不突兀）
        let seconds = if self.target_gain < self.current_gain {
            NORMALIZE_REDUCTION_SECONDS
        } else {
            NORMALIZE_INCREASE_SECONDS
        };
        let alpha = 1.0 - (-1.0 / (samples_per_second * seconds)).exp();
        self.current_gain += alpha * (self.target_gain - self.current_gain);

        // 即时防削波：瞬时新峰还没进积分统计时，单样本先封顶到 -2 dBFS，
        // 等价于零前瞻限幅，仅极端瞬态触发
        let total = self.current_gain * total_static_gain;
        if magnitude * total > NORMALIZE_PEAK_CEILING && magnitude > 0.0 {
            return NORMALIZE_PEAK_CEILING / (magnitude * total_static_gain);
        }
        self.current_gain
    }
}

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
    normalizer: IntegratedNormalizer,
    /// 预热窗：起播先测后放，第一个可闻样本就是正确响度
    warmup_queue: std::collections::VecDeque<f32>,
    warmup_pending: usize,
}

impl<S> LoudnessSource<S>
where
    S: PcmSource,
{
    pub fn new(source: S, params: Arc<Mutex<AudioEffectsParams>>) -> Self {
        let channels = source.channels();
        let sample_rate = source.sample_rate();

        let (gain_mb, normalize_enabled) = {
            let p = read_params(&params);
            (p.loudness_gain_mb, p.normalize_volume)
        };
        let gain = mb_to_linear(gain_mb);

        // 预热窗 200ms：解码远快于实时，起播只多花几毫秒；
        // 未开均衡时窗为 0，路径完全不变
        let warmup_pending = if normalize_enabled {
            (f64::from(sample_rate) * f64::from(channels.max(1)) * NORMALIZE_WARMUP_SECONDS)
                as usize
        } else {
            0
        };
        Self {
            inner: source,
            params,
            channels,
            sample_rate,
            gain,
            gain_mb,
            sample_counter: 0,
            normalize_enabled,
            normalizer: IntegratedNormalizer::new(),
            warmup_queue: std::collections::VecDeque::new(),
            warmup_pending,
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
                // 关掉时重置：下次开启从头积分，不带旧曲残留
                if !self.normalize_enabled {
                    self.normalizer = IntegratedNormalizer::new();
                }
            }
        }
    }
}

impl<S> Iterator for LoudnessSource<S>
where
    S: PcmSource,
{
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        // 预热：拉满测量窗（或源提前结束），以测得响度落位初始增益。
        // 没有它，响曲以 1.0 全响度开播、43ms 后被 0.25s 时间常数压下来，
        // 即「开头特别响然后忽然压下去」。
        if self.warmup_pending > 0 {
            while self.warmup_pending > 0 {
                match self.inner.next() {
                    Some(sample) => {
                        self.normalizer.observe(sample);
                        self.warmup_queue.push_back(sample);
                        self.warmup_pending -= 1;
                    }
                    None => break,
                }
            }
            self.warmup_pending = 0;
            self.normalizer.seed_from_observations(self.gain);
        }

        let sample = match self.warmup_queue.pop_front() {
            Some(sample) => sample,
            None => self.inner.next()?,
        };

        self.sample_counter += 1;
        if self.sample_counter >= PARAM_CHECK_INTERVAL {
            self.sample_counter = 0;
            self.update_params();
        }

        if self.gain_mb == 0 && !self.normalize_enabled {
            return Some(sample);
        }

        let normalize_gain = if self.normalize_enabled {
            let samples_per_second =
                f64::from(self.sample_rate) * f64::from(self.channels.max(1));
            self.normalizer.advance(sample, samples_per_second, self.gain)
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

#[cfg(test)]
mod tests {
    use super::{read_params, AudioEffectsParams, IntegratedNormalizer};
    use std::sync::{Arc, Mutex};

    /// 96kHz 采样流（48kHz 立体声），方波信号 RMS 恰等于幅度
    const SAMPLES_PER_SECOND: f64 = 96_000.0;

    fn feed_square(
        normalizer: &mut IntegratedNormalizer,
        amplitude: f32,
        seconds: f64,
    ) -> (f64, f64) {
        let count = (SAMPLES_PER_SECOND * seconds) as usize;
        let mut last_gain = 1.0;
        let mut sum_squares = 0.0;
        for index in 0..count {
            let sample = if index % 2 == 0 { amplitude } else { -amplitude };
            last_gain = normalizer.advance(sample, SAMPLES_PER_SECOND, 1.0);
            let out = f64::from(sample) * last_gain;
            sum_squares += out * out;
        }
        (last_gain, (sum_squares / count as f64).sqrt())
    }

    /// 语义 1：一首歌内部的强弱是原声，收敛后增益必须近乎恒定
    ///
    /// 旧实现是 EMA 滑动窗口——本质是慢速压缩器，副歌被压、间隙被抬，
    /// 增益在 0.5×~2× 之间来回摆。整轨积分收敛后，乐句强弱对积分值的
    /// 影响趋近于零，增益不动，动态范围原样保留。
    #[test]
    fn gain_stays_flat_across_loud_and_quiet_passages() {
        let mut normalizer = IntegratedNormalizer::new();
        // 20 秒 -14dBFS 正文让积分收敛
        let (converged_gain, _) = feed_square(&mut normalizer, 0.2, 20.0);
        // 随后 4 秒安静乐句 + 4 秒响乐句
        let (quiet_gain, _) = feed_square(&mut normalizer, 0.05, 4.0);
        let (loud_gain, _) = feed_square(&mut normalizer, 0.2, 4.0);

        let swing = (quiet_gain / loud_gain - 1.0).abs();
        assert!(
            swing < 0.15,
            "收敛后乐句强弱不得驱动增益: quiet={quiet_gain:.3} loud={loud_gain:.3} 摆动 {:.1}%",
            swing * 100.0,
        );
        assert!(
            (converged_gain / loud_gain - 1.0).abs() < 0.15,
            "收敛值不得漂移: converged={converged_gain:.3} loud={loud_gain:.3}",
        );
    }

    /// 语义 2：不同歌曲各自收敛到不同增益，输出响度靠拢同一目标
    #[test]
    fn different_tracks_converge_to_the_same_target_loudness() {
        let mut quiet_track = IntegratedNormalizer::new();
        let mut loud_track = IntegratedNormalizer::new();
        let (_, quiet_out_rms) = feed_square(&mut quiet_track, 0.05, 30.0);
        let (_, loud_out_rms) = feed_square(&mut loud_track, 0.4, 30.0);

        // 输入差 8 倍（18dB），输出差必须收敛到 1.6 倍（4dB）以内
        let ratio = loud_out_rms / quiet_out_rms;
        assert!(
            ratio < 1.6,
            "曲间响度未对齐: quiet_rms={quiet_out_rms:.4} loud_rms={loud_out_rms:.4} ratio={ratio:.2}",
        );
        // 且都朝目标方向移动：安静曲被抬、响曲被压
        assert!(quiet_out_rms > 0.05 * 1.5, "安静曲应被抬升");
        assert!(loud_out_rms < 0.4 * 0.7, "响曲应被压低");
    }

    /// 语义 4：响曲的第一个可闻样本就必须是压好的响度
    ///
    /// 冷启动缺陷的回归：current_gain 从 1.0 起步时，响曲开头以全响度
    /// 播出，43ms 后第一块统计出来才被 0.25s 时间常数压下——
    /// 「开头特别响然后忽然压下去」。预热窗先测后放，杜绝这个台阶。
    #[test]
    fn warmup_seeds_gain_before_the_first_audible_sample() {
        struct SquareSource {
            remaining: usize,
            amplitude: f32,
            flip: bool,
        }
        impl Iterator for SquareSource {
            type Item = f32;
            fn next(&mut self) -> Option<f32> {
                if self.remaining == 0 {
                    return None;
                }
                self.remaining -= 1;
                self.flip = !self.flip;
                Some(if self.flip { self.amplitude } else { -self.amplitude })
            }
        }
        impl super::super::pcm::PcmSource for SquareSource {
            fn channels(&self) -> u16 { 2 }
            fn sample_rate(&self) -> u32 { 48_000 }
            fn total_duration(&self) -> Option<std::time::Duration> { None }
            fn try_seek(
                &mut self,
                _position: std::time::Duration,
            ) -> Result<(), super::super::pcm::PcmSeekError> {
                Ok(())
            }
        }

        let params = Arc::new(Mutex::new(AudioEffectsParams {
            loudness_gain_mb: 0,
            eq_enabled: false,
            eq_band_levels_mb: [0; 5],
            normalize_volume: true,
        }));
        // -6dBFS 的响曲：目标增益约 0.25（-18dBFS / 0.5）
        let mut source = super::LoudnessSource::new(
            SquareSource { remaining: 96_000 * 2, amplitude: 0.5, flip: false },
            params,
        );

        let first = source.next().expect("should produce audio").abs();
        assert!(
            first < 0.5 * 0.6,
            "第一个样本应已被压好而不是全响度: {first}",
        );
    }

    /// 语义 3：增益不得把峰值推过 -2 dBFS 削波
    #[test]
    fn normalization_never_pushes_peaks_into_clipping() {
        let mut normalizer = IntegratedNormalizer::new();
        // 安静正文把增益推向上限
        feed_square(&mut normalizer, 0.05, 20.0);
        // 突发接近满幅的瞬态
        let gain = normalizer.advance(0.98, SAMPLES_PER_SECOND, 1.0);
        assert!(
            0.98 * gain <= super::NORMALIZE_PEAK_CEILING + 1e-6,
            "瞬态峰被推过削波线: gain={gain:.3} -> {:.3}",
            0.98 * gain,
        );
    }

    /// 回归：锁毒化后必须还能读到用户设置，而不是 panic 或退回默认值
    ///
    /// 这个锁在音频回调路径上。之前一处 `lock().unwrap()`、另一处
    /// `unwrap_or((0, false))`，同一个锁两种策略：前者会让毒化之后的每一帧
    /// 都 panic，后者会让 EQ 与响度设置无声失效。
    #[test]
    fn poisoned_params_recover_the_last_written_value() {
        let params = Arc::new(Mutex::new(AudioEffectsParams {
            loudness_gain_mb: 900,
            eq_enabled: true,
            eq_band_levels_mb: [100, -200, 300, -400, 500],
            normalize_volume: true,
        }));

        // 持锁时 panic：标准库会把这个 Mutex 标记为毒化
        let poisoner = Arc::clone(&params);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("simulated panic while holding the effects lock");
        })
        .join();
        assert!(params.is_poisoned(), "前置条件：锁应已毒化");

        let recovered = read_params(&params);
        assert_eq!(recovered.loudness_gain_mb, 900);
        assert!(recovered.eq_enabled);
        assert_eq!(recovered.eq_band_levels_mb, [100, -200, 300, -400, 500]);
        assert!(recovered.normalize_volume);
    }
}
