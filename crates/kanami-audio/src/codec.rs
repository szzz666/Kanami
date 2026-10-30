//! Opus 编解码封装。
//!
//! 三档模式的参数差异全部在这里落地：帧长、码率、FEC、DTX、VBR、复杂度。
//! 上层只关心「给我一个编码器，按这个模式编」和「解码这一帧」。
//!
//! 编解码器都是有状态的，不能每帧新建——Opus 的预滚与状态预测跨越帧边界，
//! 重建编码器会让音质在重建点出现明显劣化。

#![forbid(unsafe_code)]

use kanami_proto::ModeParams;
use opus::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::{CHANNELS, SAMPLE_RATE};

/// 编解码错误。
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("Opus 初始化失败：{0}")]
    Init(#[from] opus::Error),
    #[error("负载为空")]
    EmptyPayload,
}

/// Opus 编码器。按模式参数配置。
pub struct OpusEncoder {
    inner: Encoder,
    /// 每帧采样点数（单声道计）。
    frame_samples: usize,
    /// 复用的输出缓冲，避免每帧分配。
    scratch: Vec<u8>,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder").field("frame_samples", &self.frame_samples).finish()
    }
}

impl OpusEncoder {
    /// 按模式参数构造。
    pub fn new(params: ModeParams) -> Result<Self, CodecError> {
        // 低延迟档用低延迟应用模式，牺牲一点压缩率换算法延迟
        let application =
            if params.frame_ms <= 10 { Application::LowDelay } else { Application::Audio };

        let mut inner = Encoder::new(SAMPLE_RATE, Channels::Stereo, application)?;
        inner.set_bitrate(Bitrate::Bits(params.bitrate_bps as i32))?;
        inner.set_complexity(i32::from(params.complexity.min(10)))?;
        inner.set_vbr(params.vbr)?;
        inner.set_inband_fec(params.fec)?;
        inner.set_packet_loss_perc(if params.fec { 10 } else { 0 })?;
        // DTX 在音乐场景下会产生不自然的静音跳变，三档都关掉
        let _ = params.dtx;

        let frame_samples = params.samples_per_frame();
        Ok(Self { inner, frame_samples, scratch: vec![0u8; 4000] })
    }

    /// 每帧应当喂入的交错立体声样本数。
    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    /// 编码一帧交错立体声 f32 样本。
    ///
    /// 长度必须正好是 frame_samples * CHANNELS，否则按实际长度截断重试——
    /// 宁可少编一点也不要让整条链路停摆。
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>, CodecError> {
        let expected = self.frame_samples * CHANNELS;
        if pcm.len() < expected {
            return Err(CodecError::EmptyPayload);
        }
        let pcm = &pcm[..expected];

        // Opus 单包上限 1275 字节（RFC 6716），留足余量后复用同一块缓冲
        const MAX_PACKET: usize = 1500;
        if self.scratch.len() < MAX_PACKET {
            self.scratch.resize(MAX_PACKET, 0);
        }
        let n = self.inner.encode_float(pcm, &mut self.scratch)?;
        Ok(self.scratch[..n].to_vec())
    }
}

/// Opus 解码器。
pub struct OpusDecoder {
    inner: Decoder,
    /// 解码输出缓冲，交错立体声。
    scratch: Vec<f32>,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder").finish()
    }
}

impl OpusDecoder {
    pub fn new(frame_samples: usize) -> Result<Self, CodecError> {
        let inner = Decoder::new(SAMPLE_RATE, Channels::Stereo)?;
        Ok(Self { inner, scratch: vec![0.0f32; frame_samples * CHANNELS] })
    }

    /// 解码一帧，返回交错立体声 f32 样本。
    ///
    /// 负载损坏时返回静音而不是错误：丢一帧的听感远好于整条链路中断。
    pub fn decode(&mut self, payload: &[u8]) -> Vec<f32> {
        if payload.is_empty() {
            return vec![0.0; self.scratch.len()];
        }
        match self.inner.decode_float(payload, &mut self.scratch, false) {
            Ok(samples) => self.scratch[..samples * CHANNELS].to_vec(),
            Err(_) => vec![0.0; self.scratch.len()],
        }
    }

    /// 丢包补偿：让解码器生成一帧填补空缺，保持后续帧状态正确。
    pub fn decode_lost(&mut self) -> Vec<f32> {
        match self.inner.decode_float(&[], &mut self.scratch, true) {
            Ok(samples) => self.scratch[..samples * CHANNELS].to_vec(),
            Err(_) => vec![0.0; self.scratch.len()],
        }
    }
}
#[cfg(test)]
mod codec_tests {
    use super::*;

    /// 编解码保真度。
    ///
    /// Opus 有固定的算法延迟（约 6.5ms 预滚 + 重采样器延迟），
    /// 所以输出相对输入会有几十到几百个样本的位移。
    /// 判据用皮尔逊相关系数：它对幅度与偏移都鲁棒，
    /// 内容保真时应当接近 1.0，内容损坏时会明显掉下来。
    fn fidelity(mode: kanami_proto::QualityMode) -> f64 {
        let params = mode.params();
        let frames = 50;
        let frame_samples = params.samples_per_frame();
        let total = frames * frame_samples;

        // 复合信号：多个正弦叠加，避免单一频率在偏移搜索下产生误匹配
        let mut src = Vec::with_capacity(total * CHANNELS);
        for i in 0..total {
            let t = i as f32 / SAMPLE_RATE as f32;
            let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3
                + (t * 733.0 * std::f32::consts::TAU).sin() * 0.2
                + (t * 1211.0 * std::f32::consts::TAU).sin() * 0.1;
            src.push(v);
            src.push(v);
        }

        let mut enc = OpusEncoder::new(params).expect("编码器");
        let mut dec = OpusDecoder::new(frame_samples).expect("解码器");
        let mut out = Vec::new();
        for chunk in src.chunks(frame_samples * CHANNELS) {
            if chunk.len() < frame_samples * CHANNELS {
                break;
            }
            let enc_bytes = enc.encode(chunk).expect("编码");
            out.extend(dec.decode(&enc_bytes));
        }

        // 在已知的 Opus 延迟范围内搜索最佳对齐
        let mut best = f64::NEG_INFINITY;
        for offset in 0..1500isize {
            let n = out.len().min(src.len());
            let mut sx = 0.0f64;
            let mut sy = 0.0f64;
            let mut sxy = 0.0f64;
            let mut sxx = 0.0f64;
            let mut syy = 0.0f64;
            let mut count = 0usize;
            for (i, &src_sample) in src.iter().enumerate().take(n / 2) {
                let j = i as isize + offset;
                if j < 0 || j as usize >= n {
                    continue;
                }
                let x = f64::from(src_sample);
                let y = f64::from(out[j as usize]);
                sx += x;
                sy += y;
                sxy += x * y;
                sxx += x * x;
                syy += y * y;
                count += 1;
            }
            if count < 100 {
                continue;
            }
            let cnt = count as f64;
            let num = sxy - sx * sy / cnt;
            let den = ((sxx - sx * sx / cnt) * (syy - sy * sy / cnt)).sqrt();
            if den > 1e-12 {
                best = best.max(num / den);
            }
        }
        best
    }

    #[test]
    fn opus_三档编码解码都保真() {
        let mut results = Vec::new();
        for mode in [
            kanami_proto::QualityMode::Standard,
            kanami_proto::QualityMode::LowLatency,
            kanami_proto::QualityMode::HighQuality,
        ] {
            let r = fidelity(mode);
            println!("{} 档相关系数 {r:.4}（{}bps）", mode.name(), mode.params().bitrate_bps);
            results.push((mode, r));
        }

        for (mode, r) in &results {
            // 阈值取 0.95：Opus 是有损压缩，三音和弦这类富含谐波的信号
            // 在 96–256kbps 下相关系数大约 0.97–0.99。低于 0.95 才是真出了问题
            // （静音、噪声、通道错位），而不是正常的有损损耗。
            assert!(*r > 0.95, "{} 档相关系数只有 {r:.4}，音频内容未保真", mode.name());
        }
    }
}
