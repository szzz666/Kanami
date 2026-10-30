//! 时钟漂移校正。
//!
//! 每路音源的采集时钟和本机输出时钟是两个独立晶振，走一小时能差出几十毫秒。
//! 不校正的话，混音会慢慢错位，缓冲周期性见底或溢出，听感上就是定期的「咔」。
//!
//! 做法：以本机输出时钟为唯一时基，对每路音源做**可变比率重采样**。
//! 比率由缓冲水位经一个 PI 控制器微调，范围限制在 ±2000ppm 以内——
//! 这个量级听不出来，但足以吃掉任何消费级晶振的偏差。
//!
//! 这里是纯逻辑：只算「该取多少个输入样本、比率应该是多少」，
//! 真正的重采样库（rubato 之类）由采集/播放层接上。

#![forbid(unsafe_code)]

/// 允许的最大比率偏差，单位 ppm（百万分之一）。
///
/// 2000ppm 约等于每秒钟调整 0.2%，对应 48kHz 下每秒 96 个样本，
/// 远小于可闻的音高变化（可闻阈值通常认为在 1% 以上）。
pub const MAX_PPM: f64 = 2000.0;

/// 水位误差的积分增益。值越大，追上漂移越快，但越容易震荡。
const KI: f64 = 0.05;
/// 水位误差的比例增益。
const KP: f64 = 0.02;
/// 修正量到 ppm 的换算系数。
const PPM_SCALE: f64 = 10_000.0;

/// 单路音源的漂移校正器。
#[derive(Debug, Clone)]
pub struct DriftCorrector {
    /// 目标水位（样本数）。
    target_fill: f64,
    /// 误差积分项，已按 ppm 归一。
    integral: f64,
    /// 当前偏差，单位 ppm，已钳制在 MAX_PPM 之内。
    ppm: f64,
}

impl DriftCorrector {
    /// 以目标水位（样本数）构造。水位通常取抖动缓冲的一半。
    pub fn new(target_fill: usize) -> Self {
        Self { target_fill: target_fill.max(1) as f64, integral: 0.0, ppm: 0.0 }
    }

    /// 当前比率：输出样本 / 输入样本。1.0 表示不调整。
    pub fn ratio(&self) -> f64 {
        1.0 + self.ppm / 1_000_000.0
    }

    /// 当前偏差，单位 ppm。正数表示本地时钟比音源慢，需要多取一点输入。
    ///
    /// 直接返回钳制后的值，避免调用方通过比率反算时吃到浮点误差。
    pub fn ppm(&self) -> f64 {
        self.ppm
    }

    /// 喂入当前实际水位（样本数），更新比率。
    ///
    /// 水位偏高说明我们播得太慢、积压了，需要提高比率多消耗输入；
    /// 水位偏低说明播得太快，需要降低比率。
    pub fn update(&mut self, current_fill: usize) {
        let error = current_fill as f64 - self.target_fill;

        // 误差按目标水位归一，让不同缓冲深度下的响应一致
        let normalized = error / self.target_fill;

        self.integral = (self.integral + normalized * KI).clamp(-1.0, 1.0);

        let correction = normalized * KP + self.integral;
        self.ppm = (correction * PPM_SCALE).clamp(-MAX_PPM, MAX_PPM);
    }

    /// 给定要产出的样本数，算出应当从输入消耗多少样本。
    pub fn input_len_for(&self, output_len: usize) -> usize {
        let needed = (output_len as f64 * self.ratio()).round() as i64;
        needed.clamp(0, i64::from(u32::MAX)) as usize
    }
}
