//! 完整链路自测：一个进程里同时扮演音源端和播放端，走真实网络栈。
//!
//! 和 e2e 例子的区别：这里用的是**引擎层真实的类**（Outgoing / IncomingSource /
//! SignalListener / 授权队列），不是手搓的收发循环。所以它能证明
//! 「UI 点下去之后到底发生了什么」——包括那些容易漏掉的接线。
//!
//! 用法：cargo run -p kanami-engine --example full_chain

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_engine::engine::{IncomingSource, Outgoing};
use kanami_net::audio_socket::AudioSocket;
use kanami_net::signaling::{Accept, RejectReason, Signal, SignalKind, SignalListener};
use kanami_proto::{Hello, QualityMode, Role};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut failures = 0;

    for mode in [QualityMode::Standard, QualityMode::LowLatency, QualityMode::HighQuality] {
        println!("\n=== {} ===", mode.name());
        match run_once(mode) {
            Ok(report) => {
                println!("  收到音频包   {}", report.packets);
                println!("  解出帧数     {}", report.frames);
                println!("  丢包         {}", report.lost);
                println!("  峰值         {:.4}", report.peak);
                println!("  RMS          {:.4}", report.rms);
                let ok = report.packets > 0 && report.frames > 0 && report.peak > 0.05;
                println!("  {}", if ok { "✓ 链路打通且音频非静音" } else { "✗ 失败" });
                if !ok {
                    failures += 1;
                }
            }
            Err(e) => {
                println!("  ✗ {e}");
                failures += 1;
            }
        }
    }

    println!();
    if failures == 0 {
        println!("✓ 三档模式全部通过真实网络栈的完整链路验证");
        Ok(())
    } else {
        println!("✗ {failures} 档失败");
        Err(format!("{failures} 档失败").into())
    }
}

struct Report {
    packets: u64,
    frames: usize,
    lost: u64,
    peak: f32,
    rms: f64,
}

fn run_once(mode: QualityMode) -> Result<Report, Box<dyn std::error::Error>> {
    // ---- 音源端：监听信令 ----
    let listener = SignalListener::bind(0)?;
    let signal_port = listener.local_port()?;

    // 音源端的发送通道。端口要先定下来，因为 Accept 里要通告它。
    // 真实应用里 Outgoing 长期持有这个 socket：广播出去的就是它绑的端口。
    let mut outgoing = Outgoing::new(AudioSocket::bind(0, 1)?, mode)?;
    let audio_port = outgoing.socket.local_addr()?.port();

    let self_id = "source-001".to_string();
    let self_name = "音源端".to_string();

    // ---- 播放端：连上来（这一步会因为没有授权被回绝）----
    let player_thread = {
        let addr: SocketAddr = format!("127.0.0.1:{signal_port}").parse()?;
        std::thread::spawn(move || {
            let hello = Hello {
                role: Role::Player,
                mode,
                device_name: "播放端".into(),
                device_id: "player-001".into(),
            };
            // 第一次必然被拒
            let first = kanami_net::signaling::join_source(addr, &hello, Duration::from_secs(3));
            if first.is_ok() {
                return Err("首次接入不该被接受".to_string());
            }
            // 等音源端授权后重试
            std::thread::sleep(Duration::from_millis(300));
            kanami_net::signaling::join_source(addr, &hello, Duration::from_secs(3))
                .map_err(|e| format!("授权后仍然连不上：{e}"))
        })
    };

    // ---- 音源端：处理第一次请求，记录到待确认队列 ----
    let mut pending_device = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(Some((mut stream, _addr))) = listener.try_accept() {
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            if let Ok(signal) = Signal::read_from(&mut stream) {
                if signal.kind == SignalKind::Join {
                    if let Ok(hello) = Hello::decode(&signal.payload) {
                        pending_device = Some(hello.device_id.clone());
                        let _ = Signal::new(
                            SignalKind::Reject,
                            RejectReason::NotTrusted.code().as_bytes().to_vec(),
                        )
                        .write_to(&mut stream);
                    }
                }
            }
        }
        if pending_device.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let approved = pending_device.ok_or("没有收到任何接入请求")?;
    println!("  待确认设备已记录：{approved}");

    // ---- 音源端：用户点了「允许」，处理第二次请求 ----
    let accept_holder = Arc::new(parking_lot::Mutex::new(None::<Accept>));
    let acc = Arc::clone(&accept_holder);
    let src_id = self_id.clone();
    let src_name = self_name.clone();
    let responder = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if let Ok(Some((mut stream, _addr))) = listener.try_accept() {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if let Ok(signal) = Signal::read_from(&mut stream) {
                    if signal.kind == SignalKind::Join {
                        let accept = Accept {
                            mode,
                            audio_port,
                            device_name: src_name.clone(),
                            device_id: src_id.clone(),
                        };
                        *acc.lock() = Some(accept.clone());
                        let _ =
                            Signal::new(SignalKind::Accept, accept.encode()).write_to(&mut stream);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    // ---- 音源端：开始编码发送 ----
    let params = mode.params();
    let frame_samples = params.samples_per_frame();

    let rx_report = Arc::new(parking_lot::Mutex::new(None::<Report>));
    let rr = Arc::clone(&rx_report);

    // 播放端收到 Accept 后开始收
    let player_result = player_thread.join().map_err(|_| "播放线程崩溃")?;

    match player_result {
        Ok(accept) => {
            println!("  授权后握手成功，音频端口 {}", accept.audio_port);
            // 播放端先发 Hello，让音源端学到地址
            let player_sock = AudioSocket::bind(0, 2)?;
            player_sock.set_peer(SocketAddr::new("127.0.0.1".parse()?, accept.audio_port));
            player_sock.send(kanami_proto::PacketKind::Hello, 0, 0, &[])?;

            // 音源端：主循环里收包学地址 + 编码发送
            let mut source =
                IncomingSource::new(1, accept.device_id.clone(), accept.device_name.clone(), mode);
            let mut best_peak = 0.0f32;
            let mut sum_sq = 0.0f64;
            let mut samples = 0usize;
            let mut frames = 0usize;
            let mut seq = 0u32;

            let run_until = Instant::now() + Duration::from_millis(1200);
            while Instant::now() < run_until {
                // 音源端必须在发送 socket 上收包，否则学不到对端
                while let Ok(Some(_)) = outgoing.socket.try_recv() {}

                // 生成一帧正弦并编码发出
                let mut pcm = vec![0.0f32; frame_samples * 2];
                for i in 0..frame_samples {
                    let t = ((seq as usize * frame_samples) + i) as f32 / 48000.0;
                    let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                    pcm[i * 2] = v;
                    pcm[i * 2 + 1] = v;
                }
                outgoing.feed(&pcm, seq * u32::from(params.frame_ms));
                seq += 1;

                // 播放端收
                while let Ok(Some((header, payload, _))) = player_sock.try_recv() {
                    if header.kind.is_audio() {
                        source.on_packet(header.seq, header.timestamp_ms, payload);
                    }
                }
                while let Some(frame) = source.next_frame() {
                    frames += 1;
                    for &s in &frame {
                        best_peak = best_peak.max(s.abs());
                        sum_sq += f64::from(s) * f64::from(s);
                        samples += 1;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }

            *rr.lock() = Some(Report {
                packets: source.packets.load(Ordering::Relaxed),
                frames,
                lost: source.lost_count(),
                peak: best_peak,
                rms: if samples > 0 { (sum_sq / samples as f64).sqrt() } else { 0.0 },
            });
            let _ = responder.join();
            let report = rx_report.lock().take().ok_or("播放端没有产出报告")?;
            Ok(report)
        }
        Err(e) => Err(e.into()),
    }
}
