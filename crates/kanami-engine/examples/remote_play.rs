//! 跨设备播放端：连上远端音源，接收真实音频并给出可量化的结论。
//!
//! 这个例子存在的理由：单元测试和同机 e2e 都跑在一条回环上，
//! 用不到真实的网卡、DHCP 分配的地址、还有对端那张声卡。
//! 「手机当音源、电脑当播放端」这条路径上到底有没有声音出来，
//! 只有真的跨设备跑一遍才知道。
//!
//! 用法：
//!   cargo run -p kanami-engine --example remote_play -- <IP:信令端口> [秒数]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use kanami_engine::engine::IncomingSource;
use kanami_net::audio_socket::AudioSocket;
use kanami_net::signaling;
use kanami_proto::{Hello, PacketKind, QualityMode, Role};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let addr_str = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1:0".into());
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(6);

    let addr: SocketAddr = addr_str.parse()?;
    println!("连接远端音源 {addr}");

    let hello = Hello {
        role: Role::Player,
        mode: QualityMode::Standard,
        device_name: "Kanami 跨设备测试".into(),
        // 支持固定 device_id（用 KANAMI_PROBE_ID 环境变量），
        // 便于验证「授权一次后重启仍记得」这条链路。
        device_id: std::env::var("KANAMI_PROBE_ID")
            .unwrap_or_else(|_| format!("probe-{}", std::process::id())),
    };

    let started = Instant::now();
    let accept = match signaling::join_source(addr, &hello, Duration::from_secs(8)) {
        Ok(a) => {
            println!("✓ 信令握手成功（{}ms）", started.elapsed().as_millis());
            println!("  对端={} 模式={} 音频端口={}", a.device_name, a.mode.name(), a.audio_port);
            a
        }
        Err(e) => {
            println!("✗ 信令握手失败：{e}");
            if e.to_string().contains("尚未确认") || e.to_string().contains("Permission") {
                println!("  → 音源端还没允许本机接入，需要在音源端界面上点「允许」");
            }
            return Err(e.into());
        }
    };

    if accept.audio_port == 0 {
        println!("✗ 音源端通告的音频端口是 0，无法发送");
        return Err("音频端口为 0".into());
    }

    // 播放端先发握手包，音源端才能从来源地址学到我们
    let socket = AudioSocket::bind(0, 1)?;
    let audio_addr = SocketAddr::new(addr.ip(), accept.audio_port);
    socket.set_peer(audio_addr);
    socket.send(PacketKind::Hello, 0, 0, &[])?;
    println!("✓ 已向 {audio_addr} 发出握手包");

    let mut source =
        IncomingSource::new(1, accept.device_id.clone(), accept.device_name.clone(), accept.mode);

    println!("接收 {seconds} 秒…");
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut decoded_frames = 0usize;
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    let mut sample_count = 0usize;

    while Instant::now() < deadline {
        let mut got = false;
        while let Ok(Some((header, payload, _from))) = socket.try_recv() {
            if header.kind.is_audio() {
                source.on_packet(header.seq, header.timestamp_ms, payload);
                got = true;
            }
        }
        // 把抖动缓冲里攒够的帧拿出来解码
        while let Some(frame) = source.next_frame() {
            decoded_frames += 1;
            for &s in &frame {
                peak = peak.max(s.abs());
                sum_sq += f64::from(s) * f64::from(s);
                sample_count += 1;
            }
        }
        if !got {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    let packets = source.packets.load(Ordering::Relaxed);
    let lost = source.lost_count();
    let rms = if sample_count > 0 { (sum_sq / sample_count as f64).sqrt() } else { 0.0 };

    println!();
    println!("=== 结果 ===");
    println!("  收到音频包   {packets}");
    println!("  丢包         {lost}");
    println!("  解出帧数     {decoded_frames}");
    println!("  峰值         {peak:.4}");
    println!("  RMS          {rms:.4}");
    println!("  缓冲水位     {} ms", source.buffer_ms.load(Ordering::Relaxed));

    if packets == 0 {
        println!("✗ 一个音频包都没收到——音源端没有在发送，或包发不到本机");
        return Err("未收到音频".into());
    }
    if decoded_frames == 0 {
        println!("✗ 收到了包但一帧都没解出来——抖动缓冲水位或序号有问题");
        return Err("解码失败".into());
    }
    if peak < 1e-4 {
        println!("⚠ 链路是通的，但音频是静音（音源端可能没授权采集）");
        return Ok(());
    }
    println!("✓ 跨设备音频链路打通：收到 {decoded_frames} 帧真实音频，峰值 {peak:.4}");
    Ok(())
}
