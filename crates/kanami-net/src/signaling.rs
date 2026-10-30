//! TCP 信令通道。
//!
//! 负责握手、模式协商、保活与断开。音频本身走 UDP，不走这里。
//!
//! 消息用「4 字节长度 + 1 字节类型 + 负载」的帧格式，
//! 这样在流式 TCP 上不会粘包。
//!
//! 协议方向由实测决定：**播放端先连**。音源端只监听，
//! 因为实测宿主无法主动连到 NAT 后面的客体，反过来才行。

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use kanami_proto::{Hello, QualityMode, Role};

/// 信令帧类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalKind {
    /// 播放端 → 音源端：我要接你的流。
    Join = 1,
    /// 音源端 → 播放端：接受，这是实际参数与音频端口。
    Accept = 2,
    /// 音源端 → 播放端：拒绝（未授权/已满/模式不支持）。
    Reject = 3,
    /// 运行中重新协商模式（热切换）。
    Reconfigure = 4,
    /// 保活。
    Ping = 5,
    Pong = 6,
    /// 正常断开。
    Leave = 7,
}

impl SignalKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Join),
            2 => Some(Self::Accept),
            3 => Some(Self::Reject),
            4 => Some(Self::Reconfigure),
            5 => Some(Self::Ping),
            6 => Some(Self::Pong),
            7 => Some(Self::Leave),
            _ => None,
        }
    }
}

/// 一条信令消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub kind: SignalKind,
    pub payload: Vec<u8>,
}

impl Signal {
    pub fn new(kind: SignalKind, payload: Vec<u8>) -> Self {
        Self { kind, payload }
    }

    /// 编码成可写进 TCP 的字节：长度(4) + 类型(1) + 负载。
    pub fn encode(&self) -> Vec<u8> {
        let total = (1 + self.payload.len()) as u32;
        let mut out = Vec::with_capacity(4 + total as usize);
        out.extend_from_slice(&total.to_be_bytes());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.payload);
        out
    }

    /// 从流里读一条消息。阻塞。
    pub fn read_from(stream: &mut TcpStream) -> io::Result<Self> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let total = u32::from_be_bytes(len_buf) as usize;
        // 防止对端发来荒谬的长度把内存撑爆
        if total == 0 || total > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "信令帧长度非法"));
        }
        let mut buf = vec![0u8; total];
        stream.read_exact(&mut buf)?;
        let kind = SignalKind::from_u8(buf[0])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "未知信令类型"))?;
        Ok(Self { kind, payload: buf[1..].to_vec() })
    }

    pub fn write_to(&self, stream: &mut TcpStream) -> io::Result<()> {
        stream.write_all(&self.encode())?;
        stream.flush()
    }
}

/// 接流的应答内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accept {
    /// 音源端实际采用的模式，可能比请求的低。
    pub mode: QualityMode,
    /// 音源端监听的音频端口。
    pub audio_port: u16,
    pub device_name: String,
    /// 音源端从现在起监听的 UDP 端口，播放端要往这里打第一个包。
    pub device_id: String,
}

impl Accept {
    pub fn encode(&self) -> Vec<u8> {
        let name = self.device_name.as_bytes();
        let id = self.device_id.as_bytes();
        let mut out = Vec::with_capacity(4 + name.len() + id.len());
        out.push(self.mode as u8);
        out.extend_from_slice(&self.audio_port.to_be_bytes());
        out.push(name.len().min(255) as u8);
        out.extend_from_slice(&name[..name.len().min(255)]);
        out.push(id.len().min(255) as u8);
        out.extend_from_slice(&id[..id.len().min(255)]);
        out
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 3 {
            return None;
        }
        let mode = QualityMode::from_u8(buf[0])?;
        let audio_port = u16::from_be_bytes([buf[1], buf[2]]);
        let name_len = *buf.get(3)? as usize;
        let name_end = 4 + name_len;
        let device_name = std::str::from_utf8(buf.get(4..name_end)?).ok()?.to_string();
        let id_len = *buf.get(name_end)? as usize;
        let id_end = name_end + 1 + id_len;
        let device_id = std::str::from_utf8(buf.get(name_end + 1..id_end)?).ok()?.to_string();
        Some(Self { mode, audio_port, device_name, device_id })
    }
}

/// 拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// 播放端还没在音源端确认过接入。
    NotTrusted,
    /// 音源已经推给太多设备了。
    TooManyPlayers,
    /// 模式不支持。
    UnsupportedMode,
    /// 端口被占用。
    ResourceBusy,
}

impl RejectReason {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotTrusted => "not_trusted",
            Self::TooManyPlayers => "too_many_players",
            Self::UnsupportedMode => "unsupported_mode",
            Self::ResourceBusy => "resource_busy",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::NotTrusted => "播放端尚未确认接入此音源",
            Self::TooManyPlayers => "音源已达同时推送上限",
            Self::UnsupportedMode => "不支持该模式",
            Self::ResourceBusy => "音频端口被占用",
        }
    }

    pub fn from_code(code: &str) -> Self {
        match code {
            "too_many_players" => Self::TooManyPlayers,
            "unsupported_mode" => Self::UnsupportedMode,
            "resource_busy" => Self::ResourceBusy,
            _ => Self::NotTrusted,
        }
    }
}

/// 音源端的信令监听器。
pub struct SignalListener {
    listener: TcpListener,
}

impl SignalListener {
    /// 绑定信令端口。传 0 让系统分配。
    pub fn bind(port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener })
    }

    pub fn local_port(&self) -> io::Result<u16> {
        Ok(self.listener.local_addr()?.port())
    }

    /// 接受一个连接。非阻塞。
    pub fn try_accept(&self) -> io::Result<Option<(TcpStream, SocketAddr)>> {
        match self.listener.accept() {
            Ok((stream, addr)) => {
                stream.set_nodelay(true)?;
                Ok(Some((stream, addr)))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// 播放端：连上音源并发起接入请求，读出应答。
pub fn join_source(addr: SocketAddr, hello: &Hello, timeout: Duration) -> io::Result<Accept> {
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    Signal::new(SignalKind::Join, hello.encode().unwrap_or_default()).write_to(&mut stream)?;

    let reply = Signal::read_from(&mut stream)?;
    match reply.kind {
        SignalKind::Accept => Accept::decode(&reply.payload)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "应答格式错误")),
        SignalKind::Reject => {
            let reason = RejectReason::from_code(&String::from_utf8_lossy(&reply.payload));
            Err(io::Error::new(io::ErrorKind::PermissionDenied, reason.message()))
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "意外的信令应答")),
    }
}

/// 播放端的角色常量，避免调用处写魔法值。
pub const PLAYER_ROLE: Role = Role::Player;
