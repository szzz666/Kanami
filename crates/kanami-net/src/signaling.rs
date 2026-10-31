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
                // **必须显式调回阻塞模式。**
                //
                // Linux（含 Android）上 accept() 出来的 socket 会继承监听
                // socket 的 O_NONBLOCK 标志；macOS 不会。于是同一段代码在
                // Mac 上跑得好好的，到手机上就成了：连接建得起来、数据也
                // 发出去了，但服务端 read_exact 立刻以 EAGAIN（os error 11）
                // 失败，被上层当作「读取超时」丢掉——表现出来就是
                // 「应用在跑，但谁都连不上」。
                //
                // 调回阻塞之后，上层设的 set_read_timeout 才真正生效。
                stream.set_nonblocking(false)?;
                Ok(Some((stream, addr)))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// 播放端：连上音源并发起接入请求，读出应答。
pub fn join_source(addr: SocketAddr, hello: &Hello, timeout: Duration) -> io::Result<Accept> {
    log::info!("[信令] 发起 TCP 连接 → {addr}");
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    log::info!("[信令] TCP 已建立");
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let payload = hello.encode().unwrap_or_default();
    Signal::new(SignalKind::Join, payload).write_to(&mut stream)?;
    log::info!("[信令] 已发出 Join，等待应答");

    let reply = Signal::read_from(&mut stream)?;
    log::info!("[信令] 收到应答：{:?}", reply.kind);
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

#[cfg(test)]
mod isolation_tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;

    /// 一个连上却不发数据的客户端，不能拖住 accept 循环。
    ///
    /// 这条测试锁住一个真实死锁：信令处理原来串在 20ms 主循环里，
    /// 读包是阻塞的。端口扫描器、浏览器预连接、或者对端刚连上就崩溃，
    /// 都会让主循环永久停在 read_exact 上——不再广播、不再收音频、
    /// 不再推状态，界面看起来还活着，实际整个应用已经停摆。
    #[test]
    fn 哑巴连接不阻塞后续接入() {
        let listener = SignalListener::bind(0).expect("绑定失败");
        let port = listener.local_port().expect("取端口失败");

        // 一个连上来但一个字都不发的客户端
        let silent = TcpStream::connect(("127.0.0.1", port)).expect("连接失败");

        // accept 必须立刻返回这个连接，而不是等它说话
        let mut accepted = None;
        for _ in 0..200 {
            if let Ok(Some(pair)) = listener.try_accept() {
                accepted = Some(pair);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (stream, _) = accepted.expect("应当能立刻 accept 到连接");

        // 拿到了流，但读它会卡住——所以调用方必须设超时，
        // 并且必须把读操作放到主循环之外。
        stream.set_read_timeout(Some(Duration::from_millis(150))).expect("设超时失败");

        let started = std::time::Instant::now();
        let result = Signal::read_from(&mut { stream });
        let waited = started.elapsed();

        assert!(result.is_err(), "哑巴连接读不出信令，必须报错而不是无限等");
        assert!(waited < Duration::from_secs(2), "读超时必须生效，实际等了 {waited:?}");

        drop(silent);
    }

    /// 正常的一条信令，在设了超时之后依然能完整读出来。
    #[test]
    fn 设了超时不影响正常读取() {
        let listener = SignalListener::bind(0).expect("绑定失败");
        let port = listener.local_port().expect("取端口失败");

        std::thread::spawn(move || {
            let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接失败");
            Signal::new(SignalKind::Leave, b"bye".to_vec())
                .write_to(&mut client)
                .expect("写入失败");
            // 保持连接，模拟真实客户端
            std::thread::sleep(Duration::from_millis(300));
        });

        let mut accepted = None;
        for _ in 0..200 {
            if let Ok(Some(pair)) = listener.try_accept() {
                accepted = Some(pair);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (mut stream, _) = accepted.expect("应当能 accept");
        stream.set_read_timeout(Some(Duration::from_secs(2))).expect("设超时失败");

        let signal = Signal::read_from(&mut stream).expect("应当能读出信令");
        assert_eq!(signal.kind, SignalKind::Leave);
        assert_eq!(signal.payload, b"bye");
    }

    /// 写端必须把整帧一次写完，不能因为 Nagle 把头部和负载拆开。
    #[test]
    fn 信令帧能被完整写出() {
        let mut buf = Vec::new();
        let signal = Signal::new(SignalKind::Join, vec![1, 2, 3, 4]);
        buf.write_all(&signal.encode()).expect("写入失败");
        assert_eq!(buf.len(), 4 + 1 + 4, "长度前缀 4 + 类型 1 + 负载 4");
        assert_eq!(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize, 5);
    }
}

#[cfg(test)]
mod blocking_mode_tests {
    use super::*;

    /// accept 出来的流必须是阻塞的，否则设了超时也读不到数据。
    ///
    /// 这条测试锁住一个只在 Linux/Android 上出现的缺陷：
    /// accept() 出来的 socket 会继承监听 socket 的 O_NONBLOCK，
    /// 于是 read_exact 立刻返回 EAGAIN，被上层当成超时丢掉。
    /// macOS 不继承这个标志，所以本地怎么测都是好的——
    /// 只有真机上才暴露。
    #[test]
    fn 接受的连接是阻塞模式() {
        let listener = SignalListener::bind(0).expect("绑定失败");
        let port = listener.local_port().expect("取端口失败");

        std::thread::spawn(move || {
            let mut client = TcpStream::connect(("127.0.0.1", port)).expect("连接失败");
            // 故意等一会儿再发：非阻塞模式下读会立刻失败，
            // 阻塞模式下会老实等着。
            std::thread::sleep(Duration::from_millis(250));
            Signal::new(SignalKind::Leave, b"late".to_vec())
                .write_to(&mut client)
                .expect("写入失败");
            std::thread::sleep(Duration::from_millis(400));
        });

        let mut accepted = None;
        for _ in 0..200 {
            if let Ok(Some(pair)) = listener.try_accept() {
                accepted = Some(pair);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (mut stream, _) = accepted.expect("应当能 accept");

        // 客户端要 250ms 后才发数据。设了 2 秒超时的阻塞读应当能等到；
        // 如果流是非阻塞的，这里会立刻报 EAGAIN。
        stream.set_read_timeout(Some(Duration::from_secs(2))).expect("设超时失败");
        let started = std::time::Instant::now();
        let signal = Signal::read_from(&mut stream).expect("应当等到延迟到达的信令");
        let waited = started.elapsed();

        assert_eq!(signal.kind, SignalKind::Leave);
        assert_eq!(signal.payload, b"late");
        assert!(
            waited >= Duration::from_millis(150),
            "必须真的等到了数据（实际 {waited:?}）；立刻返回说明流是非阻塞的"
        );
    }
}
