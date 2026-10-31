//! 局域网发现诊断。
//!
//! 两种模式：
//!   cargo run -p kanami-net --example discovery_probe            # 监听别人的广播
//!   cargo run -p kanami-net --example discovery_probe -- listen   # 同上
//!   cargo run -p kanami-net --example discovery_probe -- peer <IP:端口>
//!
//! 「listen」模式下如果端口被占用（已有 Kanami 在跑），
//! 会改用临时端口发广播，仍能收到别人的通告。

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use kanami_net::discovery::{PeerTable, DISCOVERY_PORT};
use kanami_proto::{QualityMode, Role};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(|s| s.as_str()) == Some("peer") {
        probe_peer(args.get(2).cloned());
        return;
    }

    println!("局域网发现诊断（监听模式）");
    println!("端口 {DISCOVERY_PORT}，等待 10 秒…\n");

    let table = std::sync::Arc::new(PeerTable::new());
    let me = format!("probe-{}", std::process::id() & 0xFFFF);

    // 端口被占用时退回临时端口：仍然能发广播、也能收到回复
    let broadcaster = match kanami_net::discovery::Broadcaster::start(table.clone(), me.clone()) {
        Ok(Some(b)) => b,
        _ => {
            match kanami_net::discovery::Broadcaster::start_on_port(table.clone(), me.clone(), 0) {
                Ok(Some(b)) => {
                    println!("（发现端口被占用，改用临时端口监听）\n");
                    b
                }
                _ => {
                    println!("✗ 无法启动发现监听");
                    return;
                }
            }
        }
    };

    let self_device = kanami_net::discovery::PeerDevice {
        device_id: me.clone(),
        name: format!("诊断探针 {me}"),
        role: Role::Both,
        audio_port: 12345,
        signal_port: 12346,
        mode: QualityMode::Standard,
        addr: SocketAddr::from(([127, 0, 0, 1], 12345)),
        via: kanami_net::discovery::DiscoveredVia::Broadcast,
        last_seen: Instant::now(),
    };

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        broadcaster.tick(&self_device);
        std::thread::sleep(Duration::from_millis(500));
        if !table.list().is_empty() {
            break;
        }
    }

    let found = table.list();
    for p in &found {
        println!(
            "  ✓ {} [{}] 音频={} 信令={} 方式={}",
            p.name,
            &p.device_id[..p.device_id.len().min(16)],
            p.audio_port,
            p.signal_port,
            p.via.label()
        );
    }
    println!();
    println!("发现 {} 台设备", found.len());
    if !found.is_empty() {
        let bad: Vec<_> =
            found.iter().filter(|p| p.signal_port == 0 || p.audio_port == 0).collect();
        if bad.is_empty() {
            println!("✓ 所有设备的端口都已正确通告");
        } else {
            println!("✗ 有 {} 台设备的端口为 0，无法连接", bad.len());
        }
    }
}

/// 直接探测指定地址的信令端口是否可连。
fn probe_peer(addr: Option<String>) {
    let Some(addr) = addr else {
        println!("用法：discovery_probe peer <IP:端口>");
        return;
    };
    let Ok(parsed) = addr.parse::<SocketAddr>() else {
        println!("地址格式错误：{addr}");
        return;
    };

    println!("探测 {parsed}");
    match std::net::TcpStream::connect_timeout(&parsed, Duration::from_secs(3)) {
        Ok(_) => println!("  ✓ TCP 可连通"),
        Err(e) => println!("  ✗ TCP 不可达：{e}"),
    }
}
