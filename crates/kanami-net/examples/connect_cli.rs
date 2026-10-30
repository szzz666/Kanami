//! 连接对端 Kanami 实例，验证服务端握手链路。
//!
//! 模拟一个播放端：发起 TCP 信令连接 → 收到 Accept → 往对端 UDP 端口打音频。
//! 用来验证「另一个进程里的 Kanami 是否真的能被连上」，这是 UI 点按钮
//! 背后那条链路的真实检验。
//!
//! 用法：
//!   cargo run -p kanami-net --example connect_cli -- <IP:信令端口> [发送秒数]

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use kanami_audio::codec::OpusEncoder;
use kanami_audio::{CHANNELS, SAMPLE_RATE};
use kanami_net::signaling::{self, Signal, SignalKind};
use kanami_proto::{Header, Hello, PacketKind, QualityMode, Role, HEADER_LEN};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let addr_str = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1:60072".into());
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);

    let addr: SocketAddr = addr_str.parse()?;
    println!("连接音源 {addr}");

    let hello = Hello {
        role: Role::Player,
        mode: QualityMode::Standard,
        device_name: "Kanami 测试客户端".into(),
        device_id: "test-client-001".into(),
    };

    // 第一步：TCP 信令握手
    let started = Instant::now();
    let accept = match signaling::join_source(addr, &hello, Duration::from_secs(5)) {
        Ok(a) => {
            println!(
                "✓ 信令握手成功（{}ms）：对端={} 模式={} 音频端口={}",
                started.elapsed().as_millis(),
                a.device_name,
                a.mode.name(),
                a.audio_port
            );
            a
        }
        Err(e) => {
            println!("✗ 信令握手失败：{e}");
            // 未授权是预期结果：说明「首次接入需确认」在起作用
            if e.to_string().contains("尚未确认") || e.to_string().contains("Permission") {
                println!("  → 这是预期的「首次接入需授权」行为，说明授权机制在工作");
                return Ok(());
            }
            return Err(e.into());
        }
    };

    // 第二步：UDP 音频
    // 关键：播放端先发包，音源端才能学到地址回送（对称 NAT 下唯一可行的顺序）
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    let target = SocketAddr::new(addr.ip(), accept.audio_port);

    let params = QualityMode::Standard.params();
    let frame_samples = params.samples_per_frame();
    let mut encoder = OpusEncoder::new(params)?;

    let mut sent = 0u32;
    let mut received = 0u32;
    let mut first_payload: Option<Vec<f32>> = None;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut seq = 0u32;

    while Instant::now() < deadline {
        // 生成一帧
        let mut pcm = vec![0.0f32; frame_samples * CHANNELS];
        for i in 0..frame_samples {
            let t = (seq as usize * frame_samples + i) as f32 / SAMPLE_RATE as f32;
            let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3;
            pcm[i * CHANNELS] = v;
            pcm[i * CHANNELS + 1] = v;
        }

        let encoded = encoder.encode(&pcm)?;
        let mut packet = vec![0u8; HEADER_LEN + encoded.len()];
        let header = Header::new(PacketKind::Audio, 1, seq, seq * params.frame_ms, 0);
        let n = header.write_packet(&encoded, &mut packet)?;

        socket.send_to(&packet[..n], target)?;
        sent += 1;
        seq += 1;

        // 收对端回送的音频
        let mut buf = vec![0u8; 2048];
        if let Ok((len, from)) = socket.recv_from(&mut buf) {
            received += 1;
            if first_payload.is_none() {
                if let Ok((_h, payload)) = Header::read_packet(&buf[..len]) {
                    let mut decoder = kanami_audio::codec::OpusDecoder::new(frame_samples)?;
                    first_payload = Some(decoder.decode(payload));
                }
                println!("  ✓ 收到对端音频包，来自 {from}");
            }
        }

        std::thread::sleep(Duration::from_millis(u64::from(params.frame_ms)));
    }

    println!("发送 {sent} 帧，收到 {received} 帧回程音频");
    if let Some(pcm) = &first_payload {
        let peak = pcm.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        println!("  回程音频解码成功，{} 样本，峰值 {peak:.4}", pcm.len());
        if peak > 0.0 {
            println!("✓ 对端确实在发送真实音频");
        } else {
            println!("  ⚠ 回程是静音（对端可能没有启动发送，或未授权采集）");
        }
    }

    // 第三步：正常断开
    if let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        let _ = Signal::new(SignalKind::Leave, Vec::new()).write_to(&mut stream);
        println!("✓ 已发送断开信号");
    }

    Ok(())
}
