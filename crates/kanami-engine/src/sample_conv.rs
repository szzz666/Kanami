//! 采集侧的样本处理工具：降混与重采样。
//!
//! 移动端（AAudio / CoreAudio）和 Windows（WASAPI loopback）都走
//! 「设备原生采样率 → 项目统一的 48kHz 双声道」这条路，
//! 两边的换算逻辑必须是同一份代码。
//!
//! 之前是复制粘贴的：两处 `to_mono` 一字不差，`LinearResampler`
//! 只差注释。这种重复迟早会出现一边改了另一边忘了，
//! 表现出来是「Windows 上音高不对，Mac 上正常」这种极难查的问题。

#![forbid(unsafe_code)]

use kanami_audio::CHANNELS;

/// 多声道取平均降成单声道。
pub fn to_mono<T: Copy + Into<f32>>(data: &[T], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let frames = data.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let mut sum = 0.0f32;
        for c in 0..channels {
            sum += data[i * channels + c].into();
        }
        out.push(sum / channels as f32);
    }
    out
}

/// 把单声道复制到双声道（项目内部统一立体声）。
pub fn duplicate_to_stereo(mono: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * CHANNELS);
    for &s in mono {
        for _ in 0..CHANNELS {
            out.push(s);
        }
    }
    out
}

/// 极简线性插值重采样。
///
/// 采集侧不需要高阶滤波：Opus 编码前的抗混叠由编码器自己处理，
/// 这里只是把设备时钟换算到项目统一的 48k。线性插值足够，
/// 且没有额外延迟（多相滤波器会引入几十个样本的群延迟）。
pub struct LinearResampler {
    from: u32,
    to: u32,
    /// 上一块最后一个样本，用于跨块连续插值。
    last: f32,
    /// 当前在小数样本上的位置（0..1）。
    frac: f32,
    primed: bool,
}

impl LinearResampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self { from, to, last: 0.0, frac: 0.0, primed: false }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.from == self.to {
            return duplicate_to_stereo(input);
        }
        if input.is_empty() {
            return Vec::new();
        }

        let ratio = self.from as f32 / self.to as f32;
        let mut mono_out = Vec::with_capacity((input.len() as f32 / ratio) as usize + 2);

        if !self.primed {
            self.last = input[0];
            self.primed = true;
        }

        let mut pos = self.frac;
        while pos < input.len() as f32 {
            let i = pos.floor() as usize;
            let t = pos - i as f32;
            let a = if i == 0 { self.last } else { input[i - 1] };
            let b = input.get(i).copied().unwrap_or(a);
            mono_out.push(a + (b - a) * t);
            pos += ratio;
        }
        self.frac = pos - input.len() as f32;
        self.last = *input.last().unwrap();

        duplicate_to_stereo(&mono_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanami_audio::SAMPLE_RATE;

    #[test]
    fn 降混把立体声取平均() {
        let data = [1.0f32, 0.0, 0.5, 0.5];
        assert_eq!(to_mono(&data, 2), vec![0.5, 0.5]);
    }

    #[test]
    fn 降混处理单声道() {
        let data = [0.3f32, -0.3];
        assert_eq!(to_mono(&data, 1), vec![0.3, -0.3]);
    }

    #[test]
    fn 同采样率时只做声道复制() {
        let mut r = LinearResampler::new(48000, 48000);
        let out = r.process(&[0.1, 0.2]);
        assert_eq!(out.len(), 4, "两帧单声道 → 两帧双声道");
        assert!((out[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn 重采样保持时长大致不变() {
        let mut r = LinearResampler::new(44100, SAMPLE_RATE);
        let out = r.process(&vec![0.5f32; 4410]);
        let frames = out.len() / CHANNELS;
        // 4410 个 44.1k 样本 ≈ 4800 个 48k 样本
        assert!((frames as i64 - 4800).abs() < 100, "实际 {frames} 帧");
    }

    #[test]
    fn 重采样保持幅度() {
        let mut r = LinearResampler::new(44100, SAMPLE_RATE);
        let out = r.process(&vec![0.5f32; 4410]);
        for &s in &out {
            assert!((s - 0.5).abs() < 1e-5, "直流信号重采样后仍应是 0.5，实际 {s}");
        }
    }

    #[test]
    fn 空输入不会崩() {
        let mut r = LinearResampler::new(44100, 48000);
        assert!(r.process(&[]).is_empty());
    }

    /// 跨块连续性：分两块处理与一次处理，结果应当接近。
    ///
    /// 这条锁住 `last`/`frac` 两个跨块状态的处理——
    /// 忘了保存它们会在块边界产生可听见的咔哒声。
    #[test]
    fn 分块处理不产生边界跳变() {
        let input: Vec<f32> = (0..2000).map(|i| (i as f32 / 100.0).sin()).collect();
        let mut whole = LinearResampler::new(44100, SAMPLE_RATE);
        let a = whole.process(&input);

        let mut chunked = LinearResampler::new(44100, SAMPLE_RATE);
        let mut b = chunked.process(&input[..1000]);
        b.extend(chunked.process(&input[1000..]));

        assert!(
            (a.len() as i64 - b.len() as i64).abs() <= 4,
            "分块与整块输出长度应接近：{} vs {}",
            a.len(),
            b.len()
        );
        // 末尾几个样本应当一致（跨块状态正确的话不会跳变）
        let n = a.len().min(b.len());
        for i in (n - 8)..n {
            assert!((a[i] - b[i]).abs() < 1e-3, "第 {i} 个样本跳变：{} vs {}", a[i], b[i]);
        }
    }
}
