//! 音效均衡器：三段双二阶（biquad）滤波器组成的 EQ（低音 / 中音 / 高音）。
//!
//! 作为 rodio `Source` 包装器插在解码器之后、混音器之前，逐采样处理。
//! 系数用 RBJ Audio EQ Cookbook 公式在运行时按解码器的采样率计算：
//!
//! * 低音：120 Hz low-shelf（增益 ±12 dB， shelf slope 1.0）
//! * 中音：1 kHz peaking（Q = 0.8）
//! * 高音：7 kHz high-shelf
//!
//! 增益存放在 [`EqShared`]（`Arc` 共享 + generation 计数），UI 线程改配置后
//! 音频线程在**帧边界**检测到 generation 变化才重建滤波器——不逐采样加锁，
//! 也不在帧中间重置滤波器状态（会引入拉链噪声）。全零增益 + 关闭时逐采样直通。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use serde::{Deserialize, Serialize};

/// 单段增益滑杆范围（dB）。
pub const EQ_GAIN_RANGE_DB: f32 = 12.0;
/// 低音 low-shelf 转折频率。
const BASS_FREQ_HZ: f64 = 120.0;
/// 中音 peaking 中心频率与 Q 值。
const MID_FREQ_HZ: f64 = 1_000.0;
const MID_Q: f64 = 0.8;
/// 高音 high-shelf 转折频率。
const TREBLE_FREQ_HZ: f64 = 7_000.0;
/// low/high-shelf 的 shelf slope（RBJ 公式里的 S）。
const SHELF_SLOPE: f64 = 1.0;

/// 三段 EQ 的用户设置（持久化在 `Settings.eq`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqGains {
    pub enabled: bool,
    /// 低音增益（dB，-12..=+12）。
    pub bass: f32,
    /// 中音增益（dB，-12..=+12）。
    pub mid: f32,
    /// 高音增益（dB，-12..=+12）。
    pub treble: f32,
}

impl Default for EqGains {
    fn default() -> Self {
        Self {
            enabled: false,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
        }
    }
}

impl EqGains {
    fn clean_gain(value: f32) -> f32 {
        if value.is_finite() {
            value.clamp(-EQ_GAIN_RANGE_DB, EQ_GAIN_RANGE_DB)
        } else {
            0.0
        }
    }

    /// 清理配置里的异常值（旧配置/手工编辑可能写进 NaN 或越界值）。
    pub fn sanitized(mut self) -> Self {
        self.bass = Self::clean_gain(self.bass);
        self.mid = Self::clean_gain(self.mid);
        self.treble = Self::clean_gain(self.treble);
        self
    }

    /// 音效界面的预设（名称走 UI i18n，这里只给数值）。
    pub fn preset_flat() -> Self {
        Self::default()
    }

    pub fn preset_bass_boost() -> Self {
        Self {
            enabled: true,
            bass: 6.0,
            mid: 0.0,
            treble: 1.0,
        }
    }

    pub fn preset_pop() -> Self {
        Self {
            enabled: true,
            bass: 3.0,
            mid: -1.0,
            treble: 3.0,
        }
    }

    pub fn preset_vocal() -> Self {
        Self {
            enabled: true,
            bass: -2.0,
            mid: 4.0,
            treble: 2.0,
        }
    }
}

/// UI 与音频线程共享的 EQ 状态。写入时 generation +1，
/// 音频线程只在帧边界比对 generation，变化才重建滤波器。
pub struct EqShared {
    gains: Mutex<EqGains>,
    generation: AtomicU64,
}

impl EqShared {
    pub fn new(gains: EqGains) -> Self {
        Self {
            gains: Mutex::new(gains.sanitized()),
            generation: AtomicU64::new(0),
        }
    }

    pub fn set(&self, gains: EqGains) {
        if let Ok(mut slot) = self.gains.lock() {
            *slot = gains.sanitized();
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn get(&self) -> EqGains {
        self.gains.lock().map(|gains| *gains).unwrap_or_default()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

/// RBJ Audio EQ Cookbook 双二阶滤波器（Direct Form I + 防 denormal）。
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// 增益为 0 dB 时系数精确（在浮点误差内）退化为直通。
    fn passthrough() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn new(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn low_shelf(sample_rate: f64, freq: f64, gain_db: f64) -> Self {
        if gain_db.abs() < 1e-6 || freq >= sample_rate * 0.5 {
            return Self::passthrough();
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / SHELF_SLOPE - 1.0) + 2.0).sqrt();
        let sqrt_a_2alpha = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_2alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_2alpha),
            (a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_2alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_2alpha,
        )
    }

    fn high_shelf(sample_rate: f64, freq: f64, gain_db: f64) -> Self {
        if gain_db.abs() < 1e-6 || freq >= sample_rate * 0.5 {
            return Self::passthrough();
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / SHELF_SLOPE - 1.0) + 2.0).sqrt();
        let sqrt_a_2alpha = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_2alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_2alpha),
            (a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_2alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_2alpha,
        )
    }

    fn peaking(sample_rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        if gain_db.abs() < 1e-6 || freq >= sample_rate * 0.5 {
            return Self::passthrough();
        }
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);
        Self::new(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        )
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        // flush-to-zero：长时间静默输入时防止 denormal 拖慢 FP 单元
        if y.abs() < 1e-30 {
            0.0
        } else {
            y
        }
    }
}

/// 一条声道的三段滤波器链。
struct BandChain {
    bass: Biquad,
    mid: Biquad,
    treble: Biquad,
}

impl BandChain {
    fn new(gains: EqGains, sample_rate: f64) -> Self {
        Self {
            bass: Biquad::low_shelf(sample_rate, BASS_FREQ_HZ, gains.bass as f64),
            mid: Biquad::peaking(sample_rate, MID_FREQ_HZ, MID_Q, gains.mid as f64),
            treble: Biquad::high_shelf(sample_rate, TREBLE_FREQ_HZ, gains.treble as f64),
        }
    }

    fn process(&mut self, sample: f64) -> f64 {
        self.treble
            .process(self.mid.process(self.bass.process(sample)))
    }
}

/// 包在解码器外面的 EQ `Source`：透传位置/声道/时长元数据，逐采样滤波。
pub struct EqSource<S> {
    inner: S,
    shared: Arc<EqShared>,
    /// 已应用到滤波器的 generation。
    generation: u64,
    /// 缓存在本结构里的增益（音频线程本地读，不逐采样加锁）。
    gains: EqGains,
    channels: u16,
    /// 下一个采样属于的声道（声道内样本依次轮换）。
    next_channel: u16,
    /// 每条声道一条滤波器链；`None` 表示要在下一帧按当前增益重建。
    chains: Option<Vec<BandChain>>,
}

impl<S: Source> EqSource<S> {
    pub fn new(inner: S, shared: Arc<EqShared>) -> Self {
        let gains = shared.get();
        Self {
            channels: inner.channels().get(),
            inner,
            shared,
            generation: u64::MAX,
            gains,
            next_channel: 0,
            chains: None,
        }
    }

    /// 帧边界调用：generation 变化或声道数变化时让滤波器链下一帧重建。
    fn refresh_if_needed(&mut self) {
        let channels_changed = self.channels != self.inner.channels().get();
        if self.shared.generation() == self.generation && !channels_changed {
            return;
        }
        self.gains = self.shared.get();
        self.channels = self.inner.channels().get();
        self.generation = self.shared.generation();
        self.chains = None;
    }

    fn chain_for(&mut self, channel: usize) -> &mut BandChain {
        if self.chains.is_none() {
            let sample_rate = self.inner.sample_rate().get() as f64;
            self.chains = Some(
                (0..self.channels.max(1))
                    .map(|_| BandChain::new(self.gains, sample_rate))
                    .collect(),
            );
        }
        &mut self.chains.as_mut().expect("chains 刚初始化")[channel]
    }
}

impl<S: Source> Iterator for EqSource<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        // 滤波器状态按声道轮转；在帧边界同步外部增益，避免中间重置引入噪声。
        if self.next_channel == 0 {
            self.refresh_if_needed();
        }
        let channel = self.next_channel as usize;
        self.next_channel = (self.next_channel + 1) % self.channels.max(1);

        if !self.gains.enabled {
            return Some(sample);
        }
        let filtered = self.chain_for(channel).process(sample as f64) as Sample;
        Some(if filtered.is_finite() { filtered } else { 0.0 })
    }
}

impl<S: Source> Source for EqSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, sample_rate: f64, seconds: f64) -> Vec<f64> {
        let n = (sample_rate * seconds) as usize;
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sample_rate).sin())
            .collect()
    }

    fn rms(samples: &[f64]) -> f64 {
        (samples.iter().map(|v| v * v).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// 手动跑一条声道链（与 EqSource 内部同一条路径）。
    fn run_chain(gains: EqGains, sample_rate: f64, input: &[f64]) -> Vec<f64> {
        let mut chain = BandChain::new(gains, sample_rate);
        input.iter().map(|&x| chain.process(x)).collect()
    }

    #[test]
    fn zero_db_is_transparent() {
        let input = sine(220.0, 44_100.0, 0.25);
        let output = run_chain(EqGains::default(), 44_100.0, &input);
        let max_diff = input
            .iter()
            .zip(&output)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(max_diff < 1e-9, "0 dB 直通误差应极小，实际 {max_diff}");
    }

    #[test]
    fn bass_boost_raises_low_frequency_energy() {
        let input = sine(60.0, 44_100.0, 1.0);
        let flat = rms(&run_chain(
            EqGains {
                enabled: true,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        let boosted = rms(&run_chain(
            EqGains {
                enabled: true,
                bass: 9.0,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        assert!(
            boosted > flat * 1.5,
            "60 Hz 正弦 +9 dB 低音后 RMS 应显著变大：flat={flat}, boosted={boosted}"
        );
    }

    #[test]
    fn treble_cut_reduces_high_frequency_energy() {
        let input = sine(8_000.0, 44_100.0, 0.5);
        let flat = rms(&run_chain(
            EqGains {
                enabled: true,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        let cut = rms(&run_chain(
            EqGains {
                enabled: true,
                treble: -12.0,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        assert!(
            cut < flat * 0.6,
            "8 kHz 正弦 -12 dB 高音后 RMS 应显著变小：flat={flat}, cut={cut}"
        );
    }

    #[test]
    fn gains_are_sanitized() {
        let cleaned = EqGains {
            enabled: true,
            bass: f32::NAN,
            mid: 40.0,
            treble: -40.0,
        }
        .sanitized();
        assert_eq!(cleaned.bass, 0.0);
        assert_eq!(cleaned.mid, EQ_GAIN_RANGE_DB);
        assert_eq!(cleaned.treble, -EQ_GAIN_RANGE_DB);
    }

    #[test]
    fn shared_bumps_generation_on_set() {
        let shared = EqShared::new(EqGains::default());
        let before = shared.generation();
        shared.set(EqGains::preset_bass_boost());
        assert_eq!(shared.generation(), before + 1);
        assert_eq!(shared.get(), EqGains::preset_bass_boost());
    }

    #[test]
    fn disabled_source_passes_samples_through() {
        let shared = Arc::new(EqShared::new(EqGains::default()));
        let source = rodio::source::SineWave::new(440.0)
            .take_duration(Duration::from_millis(50))
            .amplify(0.5);
        let eq = EqSource::new(source, shared);
        let mut collected = 0usize;
        for sample in eq {
            assert!(sample.is_finite());
            collected += 1;
        }
        assert!(collected > 1_000, "应取到若干采样，实际 {collected}");
    }
}
