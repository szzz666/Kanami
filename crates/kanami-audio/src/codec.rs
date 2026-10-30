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
