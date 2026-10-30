//! Kanami 传输协议。
//!
//! 设计约束（来自需求共识）：
//!
//! - 音频走 UDP，信令走 TCP，两条链路共用同一套定长包头。
//! - 包头的 `timestamp_ms` 记录采集时刻，接收端据此计算端到端延迟。
//! - 连接由**播放端发起**，音源端从收到的包中学习对端地址并原样回送。
//!   实测对端是对称型 NAT：换源端口回送会被丢弃，因此回程 socket
//!   必须长期绑定同一本地端口，且保活间隔要远小于映射存活时长。
//! - 包头保持 16 字节，一帧 Opus 加上包头仍在单个 MTU 内。

#![forbid(unsafe_code)]

use std::fmt;

/// 魔术字，用于在 UDP 上快速丢弃无关流量。
pub const MAGIC: u16 = 0x4B4D; // "KM"
/// 协议版本。不兼容变更时递增。
pub const VERSION: u8 = 1;
/// 定长包头长度。
pub const HEADER_LEN: usize = 16;
/// 单个包允许的最大负载，留出余量后不超过常见 1500 字节 MTU。
pub const MAX_PAYLOAD: usize = 1200;

/// 保活间隔。
///
/// 实测对端 NAT 映射在 180 秒空闲后仍然存活，这里取 60 秒，
/// 留三倍余量，同时不至于在大规模部署时产生明显流量。
pub const KEEPALIVE_INTERVAL_MS: u32 = 60_000;

/// 包类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketKind {
    /// 播放端 → 音源端，发起连接并声明期望的模式。
    Hello = 1,
    /// 音源端 → 播放端，确认连接并回送实际参数。
    HelloAck = 2,
    /// 音频数据。
    Audio = 3,
    /// 保活。
    Keepalive = 4,
    /// 运行中重新协商模式（热切换）。
    Reconfig = 5,
    /// 正常断开。
    Bye = 6,
}

impl PacketKind {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Hello),
            2 => Some(Self::HelloAck),
            3 => Some(Self::Audio),
            4 => Some(Self::Keepalive),
            5 => Some(Self::Reconfig),
            6 => Some(Self::Bye),
            _ => None,
        }
    }

    /// 该类型是否承载音频负载。
    pub fn is_audio(self) -> bool {
        matches!(self, Self::Audio)
    }
}

/// 定长包头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: PacketKind,
    /// 流标识。一个播放端同时接收多路时用于区分来源。
    pub stream_id: u16,
    /// 包序号，单调递增，用于排序与丢包检测。
    pub seq: u32,
    /// 采集时刻（毫秒，发送端单调时钟）。用于端到端延迟测量。
    pub timestamp_ms: u32,
    /// 负载长度。
    pub payload_len: u16,
}

impl Header {
    pub fn new(
        kind: PacketKind,
        stream_id: u16,
        seq: u32,
        timestamp_ms: u32,
        payload_len: u16,
    ) -> Self {
        Self { kind, stream_id, seq, timestamp_ms, payload_len }
    }

    /// 写入 16 字节包头。
    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0..2].copy_from_slice(&MAGIC.to_be_bytes());
        out[2] = VERSION;
        out[3] = self.kind as u8;
        out[4..6].copy_from_slice(&self.stream_id.to_be_bytes());
        out[6..10].copy_from_slice(&self.seq.to_be_bytes());
        out[10..14].copy_from_slice(&self.timestamp_ms.to_be_bytes());
        out[14..16].copy_from_slice(&self.payload_len.to_be_bytes());
    }

    /// 从数据报头部解析。
    pub fn decode(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtoError::TooShort { got: buf.len(), need: HEADER_LEN });
        }
        let magic = u16::from_be_bytes([buf[0], buf[1]]);
        if magic != MAGIC {
            return Err(ProtoError::BadMagic(magic));
        }
        if buf[2] != VERSION {
            return Err(ProtoError::BadVersion(buf[2]));
        }
        let kind = PacketKind::from_u8(buf[3]).ok_or(ProtoError::UnknownKind(buf[3]))?;
        let stream_id = u16::from_be_bytes([buf[4], buf[5]]);
        let seq = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]);
        let timestamp_ms = u32::from_be_bytes([buf[10], buf[11], buf[12], buf[13]]);
        let payload_len = u16::from_be_bytes([buf[14], buf[15]]);
        if payload_len as usize > MAX_PAYLOAD {
            return Err(ProtoError::PayloadTooLarge(payload_len as usize));
        }
        Ok(Self { kind, stream_id, seq, timestamp_ms, payload_len })
    }

    /// 把包头与负载写进同一块缓冲区，返回写入的总长度。
    pub fn write_packet(&self, payload: &[u8], out: &mut [u8]) -> Result<usize, ProtoError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtoError::PayloadTooLarge(payload.len()));
        }
        let total = HEADER_LEN + payload.len();
        if out.len() < total {
            return Err(ProtoError::BufferTooSmall { got: out.len(), need: total });
        }
        let header = Header { payload_len: payload.len() as u16, ..*self };
        let mut hdr = [0u8; HEADER_LEN];
        header.encode(&mut hdr);
        out[..HEADER_LEN].copy_from_slice(&hdr);
        out[HEADER_LEN..total].copy_from_slice(payload);
        Ok(total)
    }

    /// 解析整包，返回包头与负载切片。
    pub fn read_packet(buf: &[u8]) -> Result<(Self, &[u8]), ProtoError> {
        let header = Self::decode(buf)?;
        let end = HEADER_LEN + header.payload_len as usize;
        if buf.len() < end {
            return Err(ProtoError::Truncated { have: buf.len(), need: end });
        }
        Ok((header, &buf[HEADER_LEN..end]))
    }
}

/// 设备角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    /// 只发送。
    Source = 1,
    /// 只播放。
    Player = 2,
    /// 既发送也播放（桌面端的典型形态）。
    Both = 3,
}

impl Role {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Source),
            2 => Some(Self::Player),
            3 => Some(Self::Both),
            _ => None,
        }
    }

    pub fn can_send(self) -> bool {
        matches!(self, Self::Source | Self::Both)
    }

    pub fn can_play(self) -> bool {
        matches!(self, Self::Player | Self::Both)
    }
}

/// 音质模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum QualityMode {
    /// 标准：兼顾稳定与延迟。
    #[default]
    Standard = 1,
    /// 低延迟：抖动缓冲不增长，宁可丢音。
    LowLatency = 2,
    /// 高音质：音乐优先。
    HighQuality = 3,
}

impl QualityMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Standard),
            2 => Some(Self::LowLatency),
            3 => Some(Self::HighQuality),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Standard => "标准",
            Self::LowLatency => "低延迟",
            Self::HighQuality => "高音质",
        }
    }

    /// 该模式的 Opus 与缓冲参数。
    pub fn params(self) -> ModeParams {
        match self {
            Self::Standard => ModeParams {
                frame_ms: 20,
                bitrate_bps: 96_000,
                jitter_min_ms: 40,
                jitter_max_ms: 120,
                fec: true,
                dtx: false,
                vbr: false,
                complexity: 10,
            },
            Self::LowLatency => ModeParams {
                frame_ms: 10,
                bitrate_bps: 128_000,
                // 低延迟档的缓冲不增长：守住延迟承诺，代价是弱网下丢音。
                jitter_min_ms: 20,
                jitter_max_ms: 20,
                fec: false,
                dtx: false,
                vbr: false,
                complexity: 5,
            },
            Self::HighQuality => ModeParams {
                frame_ms: 20,
                bitrate_bps: 256_000,
                jitter_min_ms: 60,
                jitter_max_ms: 200,
                fec: true,
                dtx: false,
                vbr: true,
                complexity: 10,
            },
        }
    }
}

/// 单个模式的完整参数集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeParams {
    /// 每帧时长（毫秒）。
    pub frame_ms: u32,
    /// 目标码率（bps）。
    pub bitrate_bps: u32,
    /// 抖动缓冲下限（毫秒）。
    pub jitter_min_ms: u32,
    /// 抖动缓冲上限（毫秒）。等于下限时不自适应。
    pub jitter_max_ms: u32,
    /// 是否启用带内前向纠错。
    pub fec: bool,
    /// 是否启用不连续传输。
    pub dtx: bool,
    /// 是否启用可变码率。
    pub vbr: bool,
    /// 编码复杂度 0–10。
    pub complexity: u8,
}

impl ModeParams {
    /// 缓冲是否随网络状况自适应。
    pub fn adaptive_jitter(&self) -> bool {
        self.jitter_max_ms > self.jitter_min_ms
    }

    /// 每帧采样点数（48kHz 采样率下）。
    pub fn samples_per_frame(&self) -> usize {
        (self.frame_ms as usize * 48_000) / 1000
    }
}

/// 握手消息体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub role: Role,
    /// 播放端期望的模式；音源端可在 ack 中降级。
    pub mode: QualityMode,
    pub device_name: String,
    /// 稳定设备标识，用于「首次接入需确认」的白名单。
    pub device_id: String,
}

impl Hello {
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let name = self.device_name.as_bytes();
        let id = self.device_id.as_bytes();
        if name.len() > u8::MAX as usize || id.len() > u8::MAX as usize {
            return Err(ProtoError::FieldTooLong);
        }
        let mut out = Vec::with_capacity(5 + name.len() + id.len());
        out.push(VERSION);
        out.push(self.role as u8);
        out.push(self.mode as u8);
        out.push(name.len() as u8);
        out.extend_from_slice(name);
        out.push(id.len() as u8);
        out.extend_from_slice(id);
        Ok(out)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < 4 {
            return Err(ProtoError::Truncated { have: buf.len(), need: 4 });
        }
        if buf[0] != VERSION {
            return Err(ProtoError::BadVersion(buf[0]));
        }
        let role = Role::from_u8(buf[1]).ok_or(ProtoError::UnknownKind(buf[1]))?;
        let mode = QualityMode::from_u8(buf[2]).ok_or(ProtoError::UnknownKind(buf[2]))?;
        let name_len = buf[3] as usize;
        let name_end = 4 + name_len;
        if buf.len() < name_end + 1 {
            return Err(ProtoError::Truncated { have: buf.len(), need: name_end + 1 });
        }
        let device_name =
            std::str::from_utf8(&buf[4..name_end]).map_err(|_| ProtoError::InvalidUtf8)?.to_owned();
        let id_len = buf[name_end] as usize;
        let id_end = name_end + 1 + id_len;
        if buf.len() < id_end {
            return Err(ProtoError::Truncated { have: buf.len(), need: id_end });
        }
        let device_id = std::str::from_utf8(&buf[name_end + 1..id_end])
            .map_err(|_| ProtoError::InvalidUtf8)?
            .to_owned();
        Ok(Self { role, mode, device_name, device_id })
    }
}

/// 协议错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
    TooShort { got: usize, need: usize },
    Truncated { have: usize, need: usize },
    BadMagic(u16),
    BadVersion(u8),
    UnknownKind(u8),
    PayloadTooLarge(usize),
    BufferTooSmall { got: usize, need: usize },
    FieldTooLong,
    InvalidUtf8,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { got, need } => write!(f, "包过短：{got} 字节，至少需要 {need}"),
            Self::Truncated { have, need } => write!(f, "包被截断：{have} 字节，声明需要 {need}"),
            Self::BadMagic(v) => write!(f, "魔术字不匹配：0x{v:04X}"),
            Self::BadVersion(v) => write!(f, "协议版本不支持：{v}"),
            Self::UnknownKind(v) => write!(f, "未知类型：{v}"),
            Self::PayloadTooLarge(v) => write!(f, "负载过大：{v} 字节"),
            Self::BufferTooSmall { got, need } => write!(f, "缓冲区过小：{got} 字节，需要 {need}"),
            Self::FieldTooLong => write!(f, "字段过长"),
            Self::InvalidUtf8 => write!(f, "字符串不是合法 UTF-8"),
        }
    }
}

impl std::error::Error for ProtoError {}
