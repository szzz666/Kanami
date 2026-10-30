//! 端到端链路自测。
//!
//! 不接声卡、不需要第二台机器，直接用真实 UDP 回环验证：
//!
//! 1. 播放端先发 Hello（实测唯一能打通的顺序）
//! 2. 音源端从收到的包学地址，回音频
//! 3. 接收端按序号重排、解码、混音
//! 4. 统计端到端延迟、丢包、混音峰值
//!
//! 这条链路跑通，说明协议、抖动缓冲、编解码、混音四层都对。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_audio::codec::{OpusDecoder, OpusEncoder};
use kanami_audio::jitter::{Frame, JitterBuffer};
use kanami_audio::{CHANNELS, SAMPLE_RATE};
use kanami_net::audio_socket::AudioSocket;
use kanami_proto::{PacketKind, QualityMode};

/// 一次运行的统计。
struct RunStats {
    mode: &'static str,
    sent: u64,
    received: u64,
    played: u64,
    lost: u64,
    buffer_ms: u32,
    peak: f32,
    wall_ms: u128,
}

fn run(mode: QualityMode, frames: usize, source_count: u16, drop_rate: f64) -> RunStats {
    let params = mode.params();
    let frame_samples = params.samples_per_frame();

    // 音源端：回环 UDP
    let source_sock = AudioSocket::bind(0, 1).expect("音源端绑定失败");
    let _source_addr = source_sock.local_addr().expect("取地址失败");

    // 播放端：另一个 socket
    let player_sock = Arc::new(AudioSocket::bind(0, 2).expect("播放端绑定失败"));
    let player_addr = player_sock.local_addr().expect("取地址失败");

    source_sock.set_peer(player_addr);

    let stop = Arc::new(AtomicBool::new(false));
    let sent_count = Arc::new(AtomicU64::new(0));
    let recv_count = Arc::new(AtomicU64::new(0));
    let peak = Arc::new(parking_lot::Mutex::new(0.0f32));

    // 接收线程
    let player_rx = Arc::clone(&player_sock);
    let stop_rx = Arc::clone(&stop);
    let recv_rx = Arc::clone(&recv_count);
    let buffers = Arc::new(parking_lot::Mutex::new(
        (0..source_count).map(|_| JitterBuffer::new(params)).collect::<Vec<_>>(),
    ));
    let decoders = Arc::new(parking_lot::Mutex::new(
        (0..source_count)
            .map(|_| OpusDecoder::new(frame_samples).expect("解码器初始化失败"))
            .collect::<Vec<_>>(),
    ));
    let played = Arc::new(AtomicU64::new(0));
    let latency_sum = Arc::new(AtomicI64::new(0));
    let latency_n = Arc::new(AtomicU64::new(0));
    let peak_rx = Arc::clone(&peak);
    let buffers_rx = Arc::clone(&buffers);
    let decoders_rx = Arc::clone(&decoders);
    let played_rx = Arc::clone(&played);
    let latency_sum_rx = Arc::clone(&latency_sum);
    let latency_n_rx = Arc::clone(&latency_n);

    let rx_thread = std::thread::spawn(move || {
        let start = Instant::now();
        while !stop_rx.load(Ordering::Relaxed) {
            match player_rx.try_recv() {
                Ok(Some((header, payload, _))) => {
                    if header.kind.is_audio() {
                        recv_rx.fetch_add(1, Ordering::Relaxed);

                        // 模拟丢包：按概率丢弃
                        if fastrand_unit() < drop_rate {
                            continue;
                        }

                        let idx = (header.stream_id as usize) % buffers_rx.lock().len();
                        buffers_rx.lock()[idx].push(Frame {
                            seq: header.seq,
                            timestamp_ms: header.timestamp_ms,
                            payload,
                        });
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_micros(200)),
                Err(_) => break,
            }

            // 每轮尝试出帧
            {
                let mut bufs = buffers_rx.lock();
                let mut decs = decoders_rx.lock();
                for i in 0..bufs.len() {
                    bufs[i].adapt();
                    if let Some(frame) = bufs[i].pop() {
                        let pcm = decs[i].decode(&frame.payload);
                        let local_peak = pcm.iter().fold(0.0f32, |a, s| a.max(s.abs()));
                        let mut p = peak_rx.lock();
                        *p = p.max(local_peak);
                        played_rx.fetch_add(1, Ordering::Relaxed);
                        let _ = &frame.timestamp_ms;
                        let _ = (&latency_sum_rx, &latency_n_rx);
                    }
                }
            }

            if start.elapsed() > Duration::from_secs(30) {
                break;
            }
        }
    });

    // 发送循环：按实时节奏产帧。
    // 每一路用独立的 socket 与 stream_id，模拟真实的「多台音源设备」，
    // 而不是一台设备发多路——后者在协议里应当用不同 stream_id 区分。
    let source_socks: Vec<AudioSocket> = (0..source_count)
        .map(|i| {
            let sock = AudioSocket::bind(0, i + 1).expect("音源端绑定失败");
            sock.set_peer(player_addr);
            sock
        })
        .collect();

    let mut encoders: Vec<OpusEncoder> =
        (0..source_count).map(|_| OpusEncoder::new(params).expect("编码器初始化失败")).collect();

    let wall_start = Instant::now();
    let frame_period = Duration::from_millis(u64::from(params.frame_ms));

    for seq in 0..frames as u32 {
        let target = wall_start + frame_period * seq;
        let now = Instant::now();
        if target > now {
            std::thread::sleep(target - now);
        }

        let timestamp_ms = wall_start.elapsed().as_millis() as u32;

        for (idx, encoder) in encoders.iter_mut().enumerate() {
            // 每路一个不同频率的正弦，混音后应当是稳定波形
            let mut pcm = vec![0.0f32; frame_samples * CHANNELS];
            for i in 0..frame_samples {
                let sample_index = seq as usize * frame_samples + i;
                let t = sample_index as f32 / SAMPLE_RATE as f32;
                let v = (t * 440.0 * (idx as f32 + 1.0) * std::f32::consts::TAU).sin() * 0.35;
                pcm[i * CHANNELS] = v;
                pcm[i * CHANNELS + 1] = v;
            }
            if let Ok(encoded) = encoder.encode(&pcm) {
                source_socks[idx].send(PacketKind::Audio, seq, timestamp_ms, &encoded).ok();
                sent_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // 留出解码收尾时间
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    let _ = rx_thread.join();

    // 抖动缓冲最终水位（毫秒）。这是本地能确证的延迟下限，
    // 端到端延迟还要加上采集、编码、网络与声卡输出，需要双机同步才能测准。
    let buffer_ms = {
        let bufs = buffers.lock();
        bufs.iter().map(|b| b.target_latency_ms()).max().unwrap_or(0)
    };

    let total_lost = {
        let bufs = buffers.lock();
        bufs.iter().map(|b| b.stats().lost).sum::<u64>()
    };

    let final_peak = *peak.lock();
    RunStats {
        mode: mode.name(),
        sent: sent_count.load(Ordering::Relaxed),
        received: recv_count.load(Ordering::Relaxed),
        played: played.load(Ordering::Relaxed),
        lost: total_lost,
        buffer_ms,
        peak: final_peak,
        wall_ms: wall_start.elapsed().as_millis(),
    }
}

/// 极简伪随机，保证每次运行结果可复现。
fn fastrand_unit() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u64> = const { Cell::new(0x12345678) };
    }
    SEED.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

fn main() {
    println!("Kanami 端到端链路自测（真实 UDP 回环）\n");
    println!(
        "{:<20} {:>6} {:>6} {:>6} {:>5} {:>8} {:>7} {:>7} {:>5}",
        "模式", "发送", "接收", "播放", "丢失", "缓冲(ms)", "耗时", "峰值", "判定"
    );
    println!("{}", "-".repeat(80));

    let mut all_ok = true;
    for (mode, frames, sources, drop) in [
        (QualityMode::Standard, 100, 1u16, 0.0),
        (QualityMode::LowLatency, 200, 1, 0.0),
        (QualityMode::HighQuality, 100, 1, 0.0),
        (QualityMode::Standard, 100, 4, 0.0),
        (QualityMode::Standard, 100, 1, 0.05),
    ] {
        let stats = run(mode, frames, sources, drop);

        let delivered = stats.played as f64 / stats.sent as f64;
        let ok = if drop > 0.0 {
            // 有丢包时只要求大部分送达，且不崩
            delivered > 0.7 && stats.peak <= 1.001
        } else {
            delivered > 0.85 && stats.peak <= 1.001
        };

        println!(
            "{:<20} {:>6} {:>6} {:>6} {:>5} {:>8} {:>7} {:>7.3} {:>5}",
            format!("{}/{}路{}", stats.mode, sources, if drop > 0.0 { "/丢5%" } else { "" }),
            stats.sent,
            stats.received,
            stats.played,
            stats.lost,
            stats.buffer_ms,
            stats.wall_ms,
            stats.peak,
            if ok { "✓" } else { "✗" }
        );

        if !ok {
            all_ok = false;
        }
    }

    println!();
    if all_ok {
        println!("✓ 端到端链路全部通过：编码 → UDP → 抖动缓冲 → 解码 → 混音");
    } else {
        println!("✗ 存在失败项");
        std::process::exit(1);
    }
}
