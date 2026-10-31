//! 音频数据通道（UDP）。
//!
//! 这一层的每个设计决定都能追溯到实测事实：
//!
//! - 对端源端口每次随机，所以发送目标必须**从收到的包学习**，不能配死。
//! - 对端是地址+端口依赖型 NAT（对称型），换源端口回送必被丢弃，
//!   因此回程 socket 必须**长期绑定同一个本地端口**，且中途不能重建。
//! - 映射 180 秒空闲仍存活，保活在 60 秒，留三倍余量。
//!
//! 写端与读端刻意分开：写端绑定一个固定端口用于回送，读端负责收。

#![forbid(unsafe_code)]

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use kanami_proto::{Header, PacketKind, HEADER_LEN, KEEPALIVE_INTERVAL_MS};

/// 收发统计，直接喂给 UI 的「当前延迟」显示。
#[derive(Debug, Default)]
pub struct LinkStats {
    pub packets_sent: AtomicU64,
    pub packets_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    /// 最近一包的采集时刻（毫秒），用于算端到端延迟。
    pub last_timestamp_ms: AtomicU32,
    /// 最近一包的到达时刻（本机单调时钟起点的毫秒）。
    pub last_arrival_ms: AtomicU64,
    /// 包序号回退/跳变次数，用于估算丢包。
    pub sequence_gaps: AtomicU64,
}

/// 一个音频通道。
///
/// 音源端用它发音频并记录学到的对端地址；播放端用它发握手与保活。
pub struct AudioSocket {
    /// 绑定的固定端口 socket。回送方向必须始终用它，否则 NAT 会丢弃。
    socket: UdpSocket,
    stats: Arc<LinkStats>,
    /// 最近一次学到的对端地址。
    peer: parking_lot::Mutex<Option<SocketAddr>>,
    /// 最近一次保活时刻。
    last_keepalive_ms: AtomicU64,
    /// 本次通道的流标识。
    stream_id: u16,
    /// 单调时钟起点，用于把 Instant 换算成毫秒。
    epoch: Instant,
}

impl std::fmt::Debug for AudioSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSocket")
            .field("local_addr", &self.socket.local_addr().ok())
            .field("peer", &self.peer.lock())
            .finish()
    }
}

impl AudioSocket {
    /// 绑定到指定端口。传 0 表示让系统分配。
    pub fn bind(port: u16, stream_id: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind(("0.0.0.0", port))?;
        socket.set_nonblocking(true)?;
        // 音频包不需要等满 MTU 再发
        let _ = socket.set_write_timeout(Some(std::time::Duration::from_millis(200)));
        Ok(Self {
            socket,
            stats: Arc::new(LinkStats::default()),
            peer: parking_lot::Mutex::new(None),
            last_keepalive_ms: AtomicU64::new(0),
            stream_id,
            epoch: Instant::now(),
        })
    }

    /// 本机绑定到的实际地址。
    ///
    /// 注意：绑定到 0.0.0.0 时 local_addr 会返回 0.0.0.0:port，
    /// 这个地址**不能直接用来发包**（发往 0.0.0.0 在多数系统上不可达）。
    /// 这里统一把它换成回环地址，便于同机联调与自测。
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        let addr = self.socket.local_addr()?;
        Ok(peer_reachable_addr(addr))
    }

    pub fn stats(&self) -> Arc<LinkStats> {
        Arc::clone(&self.stats)
    }

    /// 当前学到的对端地址。
    pub fn peer(&self) -> Option<SocketAddr> {
        *self.peer.lock()
    }

    /// 显式设置对端（手动 IP 模式用）。
    pub fn set_peer(&self, addr: SocketAddr) {
        *self.peer.lock() = Some(addr);
    }

    /// 从收到的包中学习对端地址。
    ///
    /// 这是整套 NAT 穿透的关键：对端源端口每次随机，
    /// 只有从实际收到的包里学，回程才打得回去。
    pub fn learn_peer(&self, addr: SocketAddr) {
        let mut guard = self.peer.lock();
        if *guard != Some(addr) {
            log::debug!("学到对端地址 {addr}");
            *guard = Some(addr);
        }
    }

    /// 发出一个包。没有对端地址时直接返回，不报错——
    /// 握手阶段还没学到对端是正常状态。
    pub fn send(
        &self,
        kind: PacketKind,
        seq: u32,
        timestamp_ms: u32,
        payload: &[u8],
    ) -> io::Result<bool> {
        let Some(peer) = self.peer() else {
            return Ok(false);
        };
        let mut buf = vec![0u8; HEADER_LEN + payload.len()];
        let header = Header::new(kind, self.stream_id, seq, timestamp_ms, 0);
        let n = header
            .write_packet(payload, &mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        match self.socket.send_to(&buf[..n], peer) {
            Ok(sent) => {
                self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                self.stats.bytes_sent.fetch_add(sent as u64, Ordering::Relaxed);
                Ok(true)
            }
            // 对端暂时不可达不算致命：NAT 映射可能需要一轮重试才通
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// 尝试收一个包。非阻塞，没有数据时返回 None。
    pub fn try_recv(&self) -> io::Result<Option<(Header, Vec<u8>, SocketAddr)>> {
        let mut buf = vec![0u8; 2048];
        match self.socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                self.stats.packets_received.fetch_add(1, Ordering::Relaxed);
                self.stats.bytes_received.fetch_add(n as u64, Ordering::Relaxed);

                match Header::read_packet(&buf[..n]) {
                    Ok((header, payload)) => {
                        // 从收到的包学地址，这是回程能通的唯一依据
                        self.learn_peer(from);
                        self.stats.last_timestamp_ms.store(header.timestamp_ms, Ordering::Relaxed);
                        self.stats.last_arrival_ms.store(self.now_ms(), Ordering::Relaxed);
                        Ok(Some((header, payload.to_vec(), from)))
                    }
                    // 不是我们的包，丢掉即可，不影响链路
                    Err(_) => Ok(None),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 到点了就发一个保活包。返回是否真的发了。
    pub fn maybe_keepalive(&self) -> bool {
        let now = self.now_ms();
        let last = self.last_keepalive_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < u64::from(KEEPALIVE_INTERVAL_MS) {
            return false;
        }
        if self.send(PacketKind::Keepalive, 0, now as u32, &[]).unwrap_or(false) {
            self.last_keepalive_ms.store(now, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// 当前时刻，毫秒。
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// 端到端延迟估算：本机到达时刻减去发送端采集时刻。
    ///
    /// 严格来说两端时钟不同源，这个值只有在做了时钟同步之后才有意义。
    /// 未同步时它至少能反映**抖动**，同步后才是真实延迟。
    pub fn raw_latency_ms(&self) -> Option<i64> {
        let sent = self.stats.last_timestamp_ms.load(Ordering::Relaxed);
        if sent == 0 {
            return None;
        }
        let now = self.now_ms() as i64;
        Some(now - i64::from(sent))
    }
}

/// 把「未指定地址」换成可用于发包的地址。
///
/// 0.0.0.0 与 :: 只在绑定时有意义，作为发送目标时是不可达的。
pub fn peer_reachable_addr(addr: SocketAddr) -> SocketAddr {
    if addr.ip().is_unspecified() {
        match addr {
            SocketAddr::V4(_) => SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, addr.port())),
            SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, addr.port())),
        }
    } else {
        addr
    }
}

/// 探测本机在局域网中的地址。用于生成发现广播与手动 IP 提示。
pub fn local_ipv4_addrs() -> Vec<std::net::Ipv4Addr> {
    let mut out = Vec::new();
    // 连接一个外部地址并读本地端点：不会真的发包，但能拿到出口网卡地址
    for probe in ["8.8.8.8:80", "192.168.1.1:80", "10.0.0.1:80"] {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(probe).is_ok() {
                if let Ok(addr) = sock.local_addr() {
                    if let std::net::IpAddr::V4(v4) = addr.ip() {
                        if !v4.is_loopback() && !out.contains(&v4) {
                            out.push(v4);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod peer_learning_tests {
    use super::*;
    use std::time::Duration;

    /// 没收到过包时，send 必须诚实地报告「没发出去」。
    ///
    /// 这条测试锁住一个真实缺陷：音源端从不调用 try_recv，
    /// peer 永远是 None，send 静默返回 Ok(false)——编码器在跑、
    /// 序号在涨、日志一片正常，实际一个包都没出去。
    #[test]
    fn 没有对端时发送返回未发出() {
        let sock = AudioSocket::bind(0, 1).expect("绑定失败");
        assert!(sock.peer().is_none());
        let sent = sock.send(PacketKind::Audio, 0, 0, &[1, 2, 3]).expect("不该报错");
        assert!(!sent, "没有对端时必须返回 false，不能假装发出去了");
    }

    /// 收到包之后必须学到对端，之后的发送才真的能出去。
    #[test]
    fn 收到包之后学会对端地址() {
        let receiver = AudioSocket::bind(0, 1).expect("音源端绑定失败");
        let sender = AudioSocket::bind(0, 2).expect("播放端绑定失败");

        let target = receiver.local_addr().expect("取地址失败");
        sender.set_peer(target);
        let sent = sender.send(PacketKind::Hello, 0, 0, &[]).expect("发送失败");
        assert!(sent, "播放端应当能发出握手包");

        // 模拟 state.rs 主循环里那一轮收包
        let mut got = None;
        for _ in 0..200 {
            if let Some(pkt) = receiver.try_recv().expect("收包失败") {
                got = Some(pkt);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (header, _, from) = got.expect("音源端应当收到握手包");
        assert_eq!(header.kind, PacketKind::Hello);
        assert_eq!(receiver.peer(), Some(from), "必须从来源地址学到对端");

        // 学到之后，音频才真的发得出去
        let sent = receiver.send(PacketKind::Audio, 0, 0, &[9; 32]).expect("发送失败");
        assert!(sent, "学到对端后音频必须能发出去");
    }
}
