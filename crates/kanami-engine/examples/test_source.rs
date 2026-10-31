//! 一个可以自动批准的测试音源端，用来验证「手机作为播放端」这条路径。
//!
//! 为什么要单独写：真实应用当音源需要 macOS 的音频录制授权（TCC），
//! 而那个授权必须用户手动点，无法脚本化。但这个测试要验的不是采集，
//! 是**跨设备的信令与音频通道**——用程序生成的正弦波完全够用，
//! 而且还能精确知道发出去的信号长什么样。
//!
//! 用法：
//!   cargo run -p kanami-engine --example test_source
//!
//! 输出里会打印信令端口，把它填进手机端的「手动添加设备」。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_engine::engine::Outgoing;
use kanami_net::audio_socket::AudioSocket;
use kanami_net::discovery::{Broadcaster, DiscoveredVia, PeerDevice, PeerTable};
use kanami_net::signaling::{Accept, RejectReason, Signal, SignalKind, SignalListener};
use kanami_proto::{Hello, QualityMode, Role};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // 音源端的发送通道。端口要先定下来，Accept 里要通告它。
    let mut outgoing = Outgoing::new(AudioSocket::bind(0, 1)?, QualityMode::Standard)?;
    let audio_port = outgoing.socket.local_addr()?.port();

    let listener = SignalListener::bind(0)?;
    let signal_port = listener.local_port()?;

    let lan = kanami_net::audio_socket::local_ipv4_addrs();
    println!("=== Kanami 测试音源 ===");
    println!("信令端口 {signal_port}  音频端口 {audio_port}");
    for ip in &lan {
        println!("  在手机上填：{ip}:{signal_port}");
    }
    println!("等待播放端接入…（Ctrl-C 退出）");
    println!();

    // 广播出去，让手机能在「设备」列表里直接看到，不用手填 IP
    let table = Arc::new(PeerTable::new());
    let broadcaster = Broadcaster::start(Arc::clone(&table), "test-source-001".to_string())?;
    match &broadcaster {
        Some(_) => println!("✓ 广播已启动（每 2 秒一次）"),
        None => println!("⚠ 发现端口被占用，广播不可用——只能手动填 IP"),
    }

    let mode = Arc::new(parking_lot::Mutex::new(QualityMode::Standard));
    let frames = Arc::new(AtomicU32::new(0));
    let f = Arc::clone(&frames);

    // 发送线程：一旦学到对端就一直发正弦
    let sender = std::thread::spawn(move || {
        let params = QualityMode::Standard.params();
        let frame_samples = params.samples_per_frame();
        let mut seq = 0u32;
        let mut last_log = Instant::now();
        let mut last_announce = Instant::now() - Duration::from_secs(5);
        let mut sent_at_last = 0u32;

        loop {
            // 关键：音源端必须在发送 socket 上收包，否则永远学不到播放端地址
            while let Ok(Some((header, _, from))) = outgoing.socket.try_recv() {
                log::info!("收到 {:?} 来自 {from}", header.kind);
            }

            let mut pcm = vec![0.0f32; frame_samples * 2];
            for i in 0..frame_samples {
                let t = ((seq as usize * frame_samples) + i) as f32 / 48000.0;
                // 440Hz 正弦，幅度 0.5：接收端量峰值应该看到 ~0.5
                let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                pcm[i * 2] = v;
                pcm[i * 2 + 1] = v;
            }
            outgoing.feed(&pcm, seq * params.frame_ms);
            seq = seq.wrapping_add(1);
            f.store(seq, Ordering::Relaxed);

            // 每 2 秒广播一次，顺带收别人的广播
            if last_announce.elapsed() >= Duration::from_secs(2) {
                last_announce = Instant::now();
                if let Some(b) = broadcaster.as_ref() {
                    log::info!("广播一次（音源 {} 信令 {}）", audio_port, signal_port);
                    b.tick(&PeerDevice {
                        device_id: "test-source-001".into(),
                        name: "Mac 测试音源".into(),
                        role: Role::Source,
                        audio_port,
                        signal_port,
                        mode: QualityMode::Standard,
                        addr: SocketAddr::from(([0, 0, 0, 0], audio_port)),
                        via: DiscoveredVia::Broadcast,
                        last_seen: Instant::now(),
                    });
                }
            }

            if last_log.elapsed() >= Duration::from_secs(5) {
                let cur = f.load(Ordering::Relaxed);
                log::info!(
                    "已发 {} 帧（近 5 秒 {} 帧），对端={:?}",
                    cur,
                    cur - sent_at_last,
                    outgoing.socket.peer()
                );
                sent_at_last = cur;
                last_log = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });

    // 信令线程：自动批准所有接入请求
    let _ = sender;
    loop {
        if let Ok(Some((mut stream, addr))) = listener.try_accept() {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_nodelay(true);
            println!("接收入站连接 {addr}");

            match Signal::read_from(&mut stream) {
                Ok(signal) => match signal.kind {
                    SignalKind::Join => match Hello::decode(&signal.payload) {
                        Ok(hello) => {
                            println!("  Join：{} [{}]", hello.device_name, hello.device_id);
                            let m = *mode.lock();
                            let accept = Accept {
                                mode: m,
                                audio_port,
                                device_name: "Mac 测试音源".into(),
                                device_id: "test-source-001".into(),
                            };
                            match Signal::new(SignalKind::Accept, accept.encode())
                                .write_to(&mut stream)
                            {
                                Ok(()) => println!("  ✓ 已接受，通告音频端口 {audio_port}"),
                                Err(e) => println!("  ✗ 应答失败：{e}"),
                            }
                        }
                        Err(e) => println!("  ✗ Hello 解析失败：{e}"),
                    },
                    other => {
                        println!("  信令 {other:?}");
                        let _ = Signal::new(
                            SignalKind::Reject,
                            RejectReason::UnsupportedMode.code().as_bytes().to_vec(),
                        )
                        .write_to(&mut stream);
                    }
                },
                Err(e) => println!("  ✗ 读取失败：{e}"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
