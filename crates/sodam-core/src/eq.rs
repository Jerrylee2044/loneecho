//! 音效：10 段图示均衡器（32 Hz ~ 16 kHz）+ 总前级 + Schroeder 空间混响。
//!
//! 作为 rodio `Source` 包装器插在解码器之后、混音器之前，逐采样处理：
//!
//! ```text
//! 采样 → 前级增益 → 10 段 peaking EQ → 干声 ┐
//!                          └→ 混响(4 comb + 2 allpass/声道) → 湿声 ┘ → 混合输出
//! ```
//!
//! * EQ 段用 RBJ Audio EQ Cookbook peaking 公式按解码器采样率现场算系数（Q = 1.1）；
//! * 混响是经典 Schroeder 结构：每声道 4 个并行反馈梳状滤波器（带回路低通阻尼）
//!   + 2 个串联全通，房间大小映射梳状延迟与反馈量，强度映射干湿比；
//! * 参数存放在 [`EqShared`]（`Arc` 共享 + generation 计数），UI 改动后
//!   音频线程在**帧边界**检测并重建滤波器——不打断播放，也不加逐采样锁。
//!
//! 音效总开关关闭时逐采样直通（不乘、不滤、不混响）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source};

use serde::{Deserialize, Serialize};

/// EQ 各段增益滑杆范围（dB），前级同范围。
pub const EQ_GAIN_RANGE_DB: f32 = 12.0;
/// EQ 段数。
pub const BAND_COUNT: usize = 10;
/// 10 段中心频率（Hz）：标准倍频程图示均衡器。
pub const BAND_FREQS: [f64; BAND_COUNT] = [
    32.0, 64.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];
/// UI 上的段标签（数字无 i18n 需求）。
pub const BAND_LABELS: [&str; BAND_COUNT] = [
    "32", "64", "125", "250", "500", "1K", "2K", "4K", "8K", "16K",
];
/// Peaking 段的 Q 值：1.1 左右让相邻段衔接自然，单段拉满也听得出来。
const BAND_Q: f64 = 1.1;
/// 混响强度滑杆最大值（0..1 归一化，UI 显示 0~100%）。
pub const REVERB_PARAM_MAX: f32 = 1.0;

fn default_bands() -> [f32; BAND_COUNT] {
    [0.0; BAND_COUNT]
}

/// 音效设置（持久化在 `Settings.eq`）。
///
/// 旧版本配置里的 `bass` / `mid` / `treble` 字段会被 serde 忽略，
/// 加载时各段回退为 0（原声）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqGains {
    pub enabled: bool,
    /// 前级增益（dB，-12..=+12）：在 EQ 之前施加，用来补偿整体音量变化、防止削波。
    pub preamp_db: f32,
    /// 10 段增益（dB，各 -12..=+12），频率见 [`BAND_FREQS`]。
    #[serde(default = "default_bands")]
    pub bands: [f32; BAND_COUNT],
    /// 混响空间大小（0..=1）：映射梳状滤波器的延迟与反馈。
    pub reverb_room: f32,
    /// 混响强度（0..=1）：干湿比；0 = 无混响。
    pub reverb_wet: f32,
}

impl Default for EqGains {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands: [0.0; BAND_COUNT],
            reverb_room: 0.5,
            reverb_wet: 0.0,
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

    fn clean_unit(value: f32) -> f32 {
        if value.is_finite() {
            value.clamp(0.0, REVERB_PARAM_MAX)
        } else {
            0.0
        }
    }

    /// 清理配置里的异常值（旧配置/手工编辑可能写进 NaN 或越界值）。
    pub fn sanitized(mut self) -> Self {
        self.preamp_db = Self::clean_gain(self.preamp_db);
        for gain in &mut self.bands {
            *gain = Self::clean_gain(*gain);
        }
        self.reverb_room = Self::clean_unit(self.reverb_room);
        self.reverb_wet = Self::clean_unit(self.reverb_wet);
        self
    }

    /// 预设清单（UI 按顺序展示，名称走 i18n）。
    pub const PRESETS: &'static [(&'static str, EqGains)] = &[
        ("原声", PRESET_FLAT),
        ("低音增强", PRESET_BASS_BOOST),
        ("重低音", PRESET_EXTRA_BASS),
        ("流行", PRESET_POP),
        ("摇滚", PRESET_ROCK),
        ("电子", PRESET_ELECTRONIC),
        ("爵士", PRESET_JAZZ),
        ("古典", PRESET_CLASSICAL),
        ("清晰人声", PRESET_VOCAL),
        ("现场", PRESET_LIVE),
        ("明亮", PRESET_BRIGHT),
        ("柔和", PRESET_SOFT),
    ];
}

const fn flat() -> [f32; BAND_COUNT] {
    [0.0; BAND_COUNT]
}

/// 原声：全零。
const PRESET_FLAT: EqGains = EqGains {
    enabled: false,
    preamp_db: 0.0,
    bands: flat(),
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 低音增强：低频整体抬升，适合流行/电子舞曲。
const PRESET_BASS_BOOST: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [4.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 重低音：极低频猛推，中高频略收（避免轰头）。
const PRESET_EXTRA_BASS: EqGains = EqGains {
    enabled: true,
    preamp_db: -5.0,
    bands: [7.0, 8.0, 7.0, 4.0, 1.0, 0.0, -1.0, -1.0, 0.0, 1.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 流行：低高频两头翘、中频微收（现代流行混音的常见曲线）。
const PRESET_POP: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [2.0, 3.0, 1.0, 0.0, -1.0, 0.0, 2.0, 3.0, 4.0, 4.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 摇滚：中低频收、中高频推，突出失真吉他的颗粒感。
const PRESET_ROCK: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [4.0, 3.0, 0.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0, 3.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 电子：低频冲击 + 高频空气感，附带一点空间。
const PRESET_ELECTRONIC: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [5.0, 4.0, 0.0, 0.0, -2.0, 0.0, 1.0, 2.0, 4.0, 5.0],
    reverb_room: 0.4,
    reverb_wet: 0.15,
};
/// 爵士：中频暖、低频松，适合钢琴与黄铜。
const PRESET_JAZZ: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [3.0, 2.0, 0.0, 2.0, 1.0, 0.0, -1.0, 0.0, 2.0, 3.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 古典：两端延展 + 音乐厅混响，模拟现场声场。
const PRESET_CLASSICAL: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [3.0, 2.0, 0.0, 0.0, 0.0, 0.0, -1.0, 1.0, 3.0, 4.0],
    reverb_room: 0.65,
    reverb_wet: 0.25,
};
/// 清晰人声：1k~2k 突出，高低频收，歌词贴耳。
const PRESET_VOCAL: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 现场：EQ 基本平直 + 大空间混响，演唱会感。
const PRESET_LIVE: EqGains = EqGains {
    enabled: true,
    preamp_db: -2.0,
    bands: [2.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0],
    reverb_room: 0.75,
    reverb_wet: 0.35,
};
/// 明亮：高频大推，适合闷的耳机/老录音。
const PRESET_BRIGHT: EqGains = EqGains {
    enabled: true,
    preamp_db: -3.0,
    bands: [0.0, 0.0, 0.0, -1.0, -1.0, 0.0, 2.0, 4.0, 5.0, 6.0],
    reverb_room: 0.5,
    reverb_wet: 0.0,
};
/// 柔和：高频大收 + 轻混响，睡前/背景听。
const PRESET_SOFT: EqGains = EqGains {
    enabled: true,
    preamp_db: -3.0,
    bands: [3.0, 2.0, 1.0, 0.0, 0.0, 0.0, -2.0, -3.0, -2.0, 0.0],
    reverb_room: 0.5,
    reverb_wet: 0.15,
};

/// UI 与音频线程共享的音效状态。写入时 generation +1，
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

/// 反馈梳状滤波器（带回路低通阻尼），混响的核心构件。
struct Comb {
    buffer: Vec<f64>,
    index: usize,
    feedback: f64,
    damp: f64,
    store: f64,
}

impl Comb {
    fn new(delay_samples: usize, feedback: f64, damp: f64) -> Self {
        Self {
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
            feedback,
            damp,
            store: 0.0,
        }
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.buffer[self.index];
        // 回路低通：高频衰减快 → 混响尾音变"暗"，更像真实房间
        self.store = output * (1.0 - self.damp) + self.store * self.damp;
        self.buffer[self.index] = input + self.store * self.feedback;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

/// 全通滤波器：调整相位而不大幅改变幅频，梳状滤波器之后用来打散回声感。
struct AllPass {
    buffer: Vec<f64>,
    index: usize,
}

impl AllPass {
    const GAIN: f64 = 0.5;

    fn new(delay_samples: usize) -> Self {
        Self {
            buffer: vec![0.0; delay_samples.max(1)],
            index: 0,
        }
    }

    fn process(&mut self, input: f64) -> f64 {
        let buffered = self.buffer[self.index];
        let output = buffered - Self::GAIN * input;
        self.buffer[self.index] = input + buffered * Self::GAIN;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

/// Schroeder 混响：4 并行梳状 + 2 串联全通（每声道）。
struct Reverb {
    combs: Vec<Comb>,
    allpasses: Vec<AllPass>,
    wet: f64,
    dry: f64,
}

impl Reverb {
    /// 经典 Schroeder 延迟（44.1 kHz 下的毫秒值），按实际采样率缩放；
    /// 房间越大延迟越长、反馈越强。
    const COMB_MS: [f64; 4] = [29.7, 37.1, 41.1, 43.7];
    const ALLPASS_MS: [f64; 2] = [5.0, 1.7];
    const DAMP: f64 = 0.25;

    fn new(room: f64, wet: f64, sample_rate: f64) -> Self {
        let scale = 0.55 + room * 0.9;
        let feedback = 0.7 + room * 0.22;
        let mk = |list: &[f64]| -> Vec<_> {
            list.iter()
                .map(|ms| {
                    Comb::new(
                        (ms * scale * sample_rate / 1000.0) as usize,
                        feedback,
                        Self::DAMP,
                    )
                })
                .collect()
        };
        let combs = mk(&Self::COMB_MS);
        let allpasses = Self::ALLPASS_MS
            .iter()
            .map(|ms| AllPass::new((ms * scale * sample_rate / 1000.0) as usize))
            .collect();
        Self {
            combs,
            allpasses,
            wet,
            dry: 1.0 - wet * 0.6,
        }
    }

    fn process(&mut self, input: f64) -> f64 {
        let comb_sum: f64 = self.combs.iter_mut().map(|c| c.process(input)).sum::<f64>() * 0.25;
        let mut tail = comb_sum;
        for allpass in &mut self.allpasses {
            tail = allpass.process(tail);
        }
        input * self.dry + tail * self.wet
    }
}

/// 一条声道的完整滤波链：前级 → 10 段 EQ → 混响。
struct ChannelChain {
    preamp: f64,
    bands: Vec<Biquad>,
    reverb: Option<Reverb>,
}

impl ChannelChain {
    fn new(gains: EqGains, sample_rate: f64) -> Self {
        let preamp = if gains.preamp_db.abs() > 1e-6 {
            10f64.powf(gains.preamp_db as f64 / 20.0)
        } else {
            1.0
        };
        let bands = BAND_FREQS
            .iter()
            .zip(gains.bands.iter())
            .map(|(&freq, &gain)| Biquad::peaking(sample_rate, freq, BAND_Q, gain as f64))
            .collect();
        let reverb = (gains.reverb_wet > 1e-3).then(|| {
            Reverb::new(
                gains.reverb_room as f64,
                gains.reverb_wet as f64,
                sample_rate,
            )
        });
        Self {
            preamp,
            bands,
            reverb,
        }
    }

    fn process(&mut self, sample: f64) -> f64 {
        let mut y = sample * self.preamp;
        for band in &mut self.bands {
            y = band.process(y);
        }
        match &mut self.reverb {
            Some(reverb) => reverb.process(y),
            None => y,
        }
    }
}

/// 包在解码器外面的音效 `Source`：透传位置/声道/时长元数据，逐采样处理。
pub struct EqSource<S> {
    inner: S,
    shared: Arc<EqShared>,
    /// 已应用到滤波器的 generation。
    generation: u64,
    /// 缓存在本结构里的参数（音频线程本地读，不逐采样加锁）。
    gains: EqGains,
    channels: u16,
    /// 下一个采样属于的声道（声道内样本依次轮换）。
    next_channel: u16,
    /// 每条声道一条滤波器链；`None` 表示要在下一帧按当前参数重建。
    chains: Option<Vec<ChannelChain>>,
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

    fn chain_for(&mut self, channel: usize) -> &mut ChannelChain {
        if self.chains.is_none() {
            let sample_rate = self.inner.sample_rate().get() as f64;
            self.chains = Some(
                (0..self.channels.max(1))
                    .map(|_| ChannelChain::new(self.gains, sample_rate))
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
        // 滤波器状态按声道轮转；在帧边界同步外部参数，避免中间重置引入噪声。
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
        let mut chain = ChannelChain::new(gains, sample_rate);
        input.iter().map(|&x| chain.process(x)).collect()
    }

    #[test]
    fn zero_db_is_transparent() {
        let input = sine(220.0, 44_100.0, 0.25);
        let output = run_chain(
            EqGains {
                enabled: true,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        );
        let max_diff = input
            .iter()
            .zip(&output)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(max_diff < 1e-9, "0 dB 直通误差应极小，实际 {max_diff}");
    }

    #[test]
    fn band_boost_raises_nearby_frequency_energy() {
        let input = sine(60.0, 44_100.0, 1.0);
        let mut bands = [0.0; BAND_COUNT];
        bands[1] = 9.0; // 64 Hz
        let boosted = rms(&run_chain(
            EqGains {
                enabled: true,
                bands,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        let flat = rms(&run_chain(
            EqGains {
                enabled: true,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        assert!(
            boosted > flat * 1.5,
            "60 Hz 正弦在 64 Hz 段 +9 dB 后 RMS 应显著变大：flat={flat}, boosted={boosted}"
        );
    }

    #[test]
    fn band_cut_reduces_frequency_energy() {
        let input = sine(8_000.0, 44_100.0, 0.5);
        let mut bands = [0.0; BAND_COUNT];
        bands[8] = -12.0; // 8 kHz
        let cut = rms(&run_chain(
            EqGains {
                enabled: true,
                bands,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        let flat = rms(&run_chain(
            EqGains {
                enabled: true,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        assert!(
            cut < flat * 0.6,
            "8 kHz 正弦在 8 kHz 段 -12 dB 后 RMS 应显著变小：flat={flat}, cut={cut}"
        );
    }

    #[test]
    fn reverb_adds_tail_after_input_stops() {
        // 单脉冲输入：混响应在输入结束后继续输出能量（尾音）。
        let mut input = vec![0.0; 44_100];
        input[0] = 1.0;
        let output = run_chain(
            EqGains {
                enabled: true,
                reverb_room: 0.7,
                reverb_wet: 0.5,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        );
        let tail_energy: f64 = output[4_410..].iter().map(|v| v * v).sum();
        assert!(
            tail_energy > 0.001,
            "混响尾音能量应明显大于 0，实际 {tail_energy}"
        );
    }

    #[test]
    fn preamp_scales_overall_energy() {
        let input = sine(440.0, 44_100.0, 0.5);
        let plain = rms(&run_chain(
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
                preamp_db: 6.0,
                ..EqGains::default()
            },
            44_100.0,
            &input,
        ));
        let ratio = boosted / plain;
        assert!(
            (ratio - 10f64.powf(6.0 / 20.0)).abs() < 0.05,
            "+6 dB 前级应把振幅放大 10^(6/20)≈2 倍，实际 {ratio}"
        );
    }

    #[test]
    fn gains_are_sanitized() {
        let mut bands = [0.0; BAND_COUNT];
        bands[0] = f32::NAN;
        bands[3] = 40.0;
        let cleaned = EqGains {
            enabled: true,
            preamp_db: -30.0,
            bands,
            reverb_room: 7.0,
            reverb_wet: -1.0,
        }
        .sanitized();
        assert_eq!(cleaned.bands[0], 0.0);
        assert_eq!(cleaned.bands[3], EQ_GAIN_RANGE_DB);
        assert_eq!(cleaned.preamp_db, -EQ_GAIN_RANGE_DB);
        assert_eq!(cleaned.reverb_room, REVERB_PARAM_MAX);
        assert_eq!(cleaned.reverb_wet, 0.0);
    }

    #[test]
    fn all_presets_are_within_safe_ranges() {
        for (name, preset) in EqGains::PRESETS {
            let cleaned = preset.sanitized();
            assert_eq!(cleaned, *preset, "预设 {name} 应已是合法值");
            for &gain in &preset.bands {
                assert!(
                    (-EQ_GAIN_RANGE_DB..=EQ_GAIN_RANGE_DB).contains(&gain),
                    "预设 {name} 有越频段：{gain}"
                );
            }
        }
    }

    #[test]
    fn shared_bumps_generation_on_set() {
        let shared = EqShared::new(EqGains::default());
        let before = shared.generation();
        shared.set(PRESET_BASS_BOOST);
        assert_eq!(shared.generation(), before + 1);
        assert_eq!(shared.get(), PRESET_BASS_BOOST);
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
