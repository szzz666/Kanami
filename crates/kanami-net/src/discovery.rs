//! 局域网设备发现。
//!
//! 三件套并行，因为实测证明没有哪一种单独够用：
//!
//! - **mDNS/DNS-SD**：标准做法，桌面端之间最可靠。
//! - **UDP 广播**：mDNS 被路由器拦掉时的备胎。
//! - **手动 IP**：实测模拟器上广播与组播双向都不通，
//!   这种情况下唯一能用的就是手填地址。
//!
//! 每种方式发现的设备都进同一个设备表，按稳定设备 ID 去重。

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_proto::{QualityMode, Role};

/// mDNS 服务类型。
pub const SERVICE_TYPE: &str = "_kanami._udp.local.";
/// 广播发现使用的固定端口。所有安装本应用的设备都监听它。
pub const DISCOVERY_PORT: u16 = 47821;
/// 广播间隔。
pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
/// 多久没听到某设备的广播就认为它下线了。
pub const STALE_AFTER: Duration = Duration::from_secs(8);

/// 绑定一个允许同机多进程共享的 UDP 端口。
///
/// 只开 SO_REUSEADDR 不够：BSD/macOS 上它不让两个 socket 绑同一 UDP 端口，
/// 必须再加 SO_REUSEPORT。Linux 上两者语义不同，但一起开是安全的。
///
/// 为什么必须共享：广播是「一人发、所有人收」的语义，
/// 端口被第一个实例独占的话，同一台机器上的第二个实例（诊断探针、
/// 模拟器转发、开发时开两个窗口）就绑不上，于是「互相发现」永远失败。
fn bind_shared_port(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(not(windows))]
    sock.set_reuse_port(true)?;
    let addr: SocketAddr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    sock.bind(&addr.into())?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

/// 一台被发现的对端设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDevice {
    /// 稳定设备标识，用于白名单与去重。
    pub device_id: String,
    pub name: String,
    pub role: Role,
    /// 音频端口。
    pub audio_port: u16,
    /// 信令端口。
    pub signal_port: u16,
    /// 该设备当前使用的模式。
    pub mode: QualityMode,
    /// 最后一次听到它的地址。
    pub addr: SocketAddr,
    /// 通过哪条途径发现的。
    pub via: DiscoveredVia,
    /// 最后一次听到它的时刻。
    pub last_seen: Instant,
}

/// 发现途径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredVia {
    Mdns,
    Broadcast,
    Manual,
}

impl DiscoveredVia {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mdns => "mDNS",
            Self::Broadcast => "广播",
            Self::Manual => "手动添加",
        }
    }
}

/// 广播报文。用纯文本，方便用 nc 直接调试。
///
/// 格式：KANAMI1|device_id|name|role|audio_port|signal_port|mode
fn encode_announce(d: &PeerDevice) -> Vec<u8> {
    format!(
        "KANAMI1|{}|{}|{}|{}|{}|{}",
        d.device_id, d.name, d.role as u8, d.audio_port, d.signal_port, d.mode as u8
    )
    .into_bytes()
}

fn decode_announce(buf: &[u8], from: SocketAddr) -> Option<PeerDevice> {
    let text = std::str::from_utf8(buf).ok()?;
    let mut parts = text.split('|');
    if parts.next()? != "KANAMI1" {
        return None;
    }
    let device_id = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    let role = Role::from_u8(parts.next()?.parse().ok()?)?;
    let audio_port = parts.next()?.parse().ok()?;
    let signal_port = parts.next()?.parse().ok()?;
    let mode = QualityMode::from_u8(parts.next()?.parse().ok()?)?;

    Some(PeerDevice {
        device_id,
        name,
        role,
        audio_port,
        signal_port,
        mode,
        addr: SocketAddr::new(from.ip(), audio_port),
        via: DiscoveredVia::Broadcast,
        last_seen: Instant::now(),
    })
}

/// 设备表。所有发现途径共用。
#[derive(Debug, Default)]
pub struct PeerTable {
    peers: parking_lot::Mutex<HashMap<String, PeerDevice>>,
}

impl PeerTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入或更新一台设备。返回是否是首次发现。
    pub fn upsert(&self, mut device: PeerDevice) -> bool {
        let mut guard = self.peers.lock();
        let is_new = !guard.contains_key(&device.device_id);
        device.last_seen = Instant::now();
        guard.insert(device.device_id.clone(), device);
        is_new
    }

    /// 当前所有仍在线的设备。
    pub fn list(&self) -> Vec<PeerDevice> {
        let guard = self.peers.lock();
        let now = Instant::now();
        guard
            .values()
            // 手动添加的设备永不过期：用户明确指定了它，不该因为收不到广播就消失
            .filter(|d| {
                d.via == DiscoveredVia::Manual || now.duration_since(d.last_seen) < STALE_AFTER
            })
            .cloned()
            .collect()
    }

    /// 移除一台设备。
    pub fn remove(&self, device_id: &str) -> bool {
        self.peers.lock().remove(device_id).is_some()
    }

    pub fn get(&self, device_id: &str) -> Option<PeerDevice> {
        self.peers.lock().get(device_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.peers.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 广播发现服务。
pub struct Broadcaster {
    socket: UdpSocket,
    table: Arc<PeerTable>,
    self_id: String,
}

impl Broadcaster {
    /// 绑定发现端口并准备收发广播。
    ///
    /// 端口被占用时不报错返回 None——同一台机器上跑两个实例是常见情况
    /// （比如桌面端 + 模拟器转发），不能因此整个应用起不来。
    pub fn start(table: Arc<PeerTable>, self_id: String) -> std::io::Result<Option<Self>> {
        Self::start_on_port(table, self_id, DISCOVERY_PORT)
    }

    /// 绑定指定端口。传 0 让系统分配（诊断与多实例场景用）。
    pub fn start_on_port(
        table: Arc<PeerTable>,
        self_id: String,
        port: u16,
    ) -> std::io::Result<Option<Self>> {
        // 用 socket2 建 socket 并开 SO_REUSEADDR / SO_REUSEPORT。
        //
        // 直接 UdpSocket::bind 会独占端口：同一台机器上开第二个实例
        // （桌面端 + 诊断探针，或桌面端 + 模拟器转发）就绑不上，
        // 于是「两个实例互相发现」永远失败。广播是「一人发、所有人收」
        // 的语义，端口本来就该允许多个进程共享。
        let socket = match bind_shared_port(port) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("发现端口 {port} 绑定失败：{e}，广播发现不可用");
                return Ok(None);
            }
        };
        socket.set_nonblocking(true)?;
        let _ = socket.set_broadcast(true);
        Ok(Some(Self { socket, table, self_id }))
    }

    /// 便于日志的一句话描述。
    pub fn local_addr_label(&self) -> String {
        match self.socket.local_addr() {
            Ok(a) => format!("绑定 {a}"),
            Err(e) => format!("地址未知（{e}）"),
        }
    }

    /// 发出一次广播通告，并顺带收一轮别人的通告。
    pub fn tick(&self, me: &PeerDevice) {
        let payload = encode_announce(me);

        // 有限广播 + 各网卡的子网定向广播 + 本地回环。
        //
        // 只发 255.255.255.255 是不够的：Android 与不少 WiFi 驱动/AP
        // 会过滤有限广播，但放行子网定向广播（如 192.168.3.255）。
        // 实测手机上「能发现电脑、电脑发现不了手机」这类单向问题，
        // 一半是组播锁没拿（见 MainActivity），另一半就是这里只发了有限广播。
        let mut targets = vec![
            SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, DISCOVERY_PORT)),
        ];
        for ip in crate::audio_socket::local_ipv4_addrs() {
            // 按 /24 推子网广播。家用与办公网基本都是 /24，
            // 猜错也只是多一个无效目标，不会造成误投递。
            let o = ip.octets();
            let bcast = Ipv4Addr::new(o[0], o[1], o[2], 255);
            let addr = SocketAddr::from((bcast, DISCOVERY_PORT));
            if !targets.contains(&addr) {
                targets.push(addr);
            }
        }
        for target in targets {
            let _ = self.socket.send_to(&payload, target);
        }

        // 收一轮
        let mut buf = [0u8; 1024];
        for _ in 0..32 {
            match self.socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if let Some(device) = decode_announce(&buf[..n], from) {
                        // 忽略自己
                        if device.device_id != self.self_id {
                            self.table.upsert(device);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }
}
#[cfg(test)]
mod announce_tests {
    use super::*;

    fn sample_peer() -> PeerDevice {
        PeerDevice {
            device_id: "abc-123".into(),
            name: "测试设备".into(),
            role: Role::Both,
            audio_port: 51234,
            signal_port: 52877,
            mode: QualityMode::Standard,
            addr: SocketAddr::from(([192, 168, 1, 10], 51234)),
            via: DiscoveredVia::Broadcast,
            last_seen: Instant::now(),
        }
    }

    /// 广播报文必须完整带上两个端口。
    ///
    /// 这条测试是为了锁住一个真实 bug：signal_port 从未被赋值，
    /// 广播出去的是 0，别的设备能发现你但永远连不上。
    #[test]
    fn 广播报文包含真实端口() {
        let peer = sample_peer();
        let bytes = encode_announce(&peer);
        let from = SocketAddr::from(([192, 168, 1, 10], DISCOVERY_PORT));
        let decoded = decode_announce(&bytes, from).expect("应当能解析");

        assert_eq!(decoded.audio_port, 51234, "音频端口必须正确传递");
        assert_eq!(decoded.signal_port, 52877, "信令端口必须正确传递");
        assert_ne!(decoded.signal_port, 0, "信令端口为 0 意味着对端永远连不上");
    }

    #[test]
    fn 端口为零时能解析但会被上层判定为不可用() {
        let mut peer = sample_peer();
        peer.signal_port = 0;
        let bytes = encode_announce(&peer);
        let from = SocketAddr::from(([192, 168, 1, 10], DISCOVERY_PORT));
        let decoded = decode_announce(&bytes, from).expect("格式本身仍然合法");
        // 解析层不拒绝 0，由使用方判断——这样诊断工具能看到原始值
        assert_eq!(decoded.signal_port, 0);
    }

    #[test]
    fn 拒绝格式错误的报文() {
        let from = SocketAddr::from(([127, 0, 0, 1], 1000));
        assert!(decode_announce(b"GARBAGE", from).is_none());
        assert!(decode_announce(b"KANAMI1|only|three", from).is_none());
    }

    #[test]
    fn 中文字符串不会破坏报文() {
        let peer = sample_peer();
        let bytes = encode_announce(&peer);
        let from = SocketAddr::from(([127, 0, 0, 1], 1000));
        let decoded = decode_announce(&bytes, from).expect("中文设备名应当能往返");
        assert_eq!(decoded.name, "测试设备");
    }
}
