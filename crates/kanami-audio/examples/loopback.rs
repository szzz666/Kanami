//! 本机回环验证。
//!
//! 不做真实声卡采集与播放，只跑音频内核这一段流水线，用来回答一个具体问题：
//! **在给定的网络故障模型下，三档模式各自的缓冲水位、丢帧与混音结果如何。**
//!
//! 这是「先内核、后 UI」路线里的第一步验证：先确认调度逻辑对，再去接声卡。
//!
//! 运行方式：
//!
//! ```bash
//! cargo run -p kanami-audio --example loopback
//! ```

use kanami_audio::jitter::{Frame, JitterBuffer};
use kanami_audio::mixer::{Mixer, MAX_SOURCES};
use kanami_audio::resample::DriftCorrector;
use kanami_proto::QualityMode;

/// 一次模拟的统计结果。
struct Report {
    mode: &'static str,
    frames_sent: u32,
    frames_played: u32,
    dropped_late: u64,
    lost: u64,
    underruns: u64,
    final_latency_ms: u32,
    peak_sample: f32,
}

/// 用一个确定性的伪随机序列驱动，保证每次运行结果一致，便于回归对比。
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    /// 返回 0.0–1.0。
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// 模拟一条链路：按固定节奏产帧，随机让一部分包晚到或丢失，
/// 接收端用抖动缓冲整理，最后多路混音。
fn simulate(mode: QualityMode, sources: u16, loss_rate: f64, jitter_late_rate: f64) -> Report {
    let params = mode.params();
    let mut buffers: Vec<JitterBuffer> = (0..sources).map(|_| JitterBuffer::new(params)).collect();
    let mut mixer = Mixer::new();
    for id in 0..sources {
        mixer.add_source(id, MAX_SOURCES);
    }

    let mut rng = Lcg(0xC0FFEE);
    let frames: u32 = 500;
    let mut frames_sent = 0u32;
    let mut frames_played = 0u32;
    let mut peak_sample = 0.0f32;

    // 每路一条正弦，频率错开，混音后应当仍是稳定波形而不是削波噪声
    let mut phases: Vec<f64> = (0..sources).map(|i| i as f64 * 0.7).collect();
    let samples_per_frame = params.samples_per_frame();

    for seq in 0..frames {
        // 生产端：每路同一序号的一帧
        for (i, buffer) in buffers.iter_mut().enumerate() {
            if rng.unit() < loss_rate {
                continue; // 这一路这一帧丢了
            }
            let _ = i;
            frames_sent += 1;
            let payload = vec![seq as u8; 16];
            buffer.push(Frame { seq, timestamp_ms: seq * params.frame_ms, payload });
        }

        // 接收端：每路取一帧，取不到的补静音
        let mut decoded: Vec<(u16, Vec<f32>)> = Vec::new();
        for (i, buffer) in buffers.iter_mut().enumerate() {
            buffer.adapt();
            let samples: Vec<f32> = match buffer.pop() {
                Some(_) => {
                    frames_played += 1;
                    // 用正弦模拟解码出来的 PCM
                    let mut block = Vec::with_capacity(samples_per_frame * 2);
                    for _ in 0..samples_per_frame {
                        phases[i] += 0.05;
                        let v = (phases[i].sin() * 0.4) as f32;
                        block.push(v);
                        block.push(v);
                    }
                    block
                }
                // 该路没数据：按静音参与混音，绝不拖住其他路
                None => vec![0.0f32; samples_per_frame * 2],
            };
            decoded.push((i as u16, samples));
        }

        // 模拟偶发的乱序：把一路的包晚一步再投进去（这里用丢弃模拟晚到超时）
        if rng.unit() < jitter_late_rate {
            if let Some(buffer) = buffers.first_mut() {
                buffer.push(Frame {
                    seq,
                    timestamp_ms: seq * params.frame_ms,
                    payload: vec![0u8; 8],
                });
            }
        }

        // 混音
        let inputs: Vec<(u16, &[f32])> =
            decoded.iter().map(|(id, s)| (*id, s.as_slice())).collect();
        let mut out = vec![0.0f32; samples_per_frame * 2];
        mixer.mix(&inputs, &mut out);

        for sample in &out {
            peak_sample = peak_sample.max(sample.abs());
        }
    }

    let mut dropped_late = 0u64;
    let mut lost = 0u64;
    let mut underruns = 0u64;
    let mut final_latency_ms = 0u32;
    for buffer in &buffers {
        let stats = buffer.stats();
        dropped_late += stats.late;
        lost += stats.lost;
        underruns += stats.underruns;
        final_latency_ms = final_latency_ms.max(buffer.target_latency_ms());
    }

    Report {
        mode: mode.name(),
        frames_sent,
        frames_played,
        dropped_late,
        lost,
        underruns,
        final_latency_ms,
        peak_sample,
    }
}

fn main() {
    println!("Kanami 音频内核回环验证\n");
    println!(
        "{:<8} {:>8} {:>8} {:>8} {:>6} {:>8} {:>10} {:>8}",
        "模式", "发帧", "播帧", "晚到", "丢失", "欠载", "缓冲(ms)", "峰值"
    );
    println!("{}", "-".repeat(72));

    let mut all_ok = true;
    for mode in [QualityMode::Standard, QualityMode::LowLatency, QualityMode::HighQuality] {
        let report = simulate(mode, 2, 0.01, 0.02);
        println!(
            "{:<8} {:>8} {:>8} {:>8} {:>6} {:>8} {:>10} {:>8.3}",
            report.mode,
            report.frames_sent,
            report.frames_played,
            report.dropped_late,
            report.lost,
            report.underruns,
            report.final_latency_ms,
            report.peak_sample
        );

        // 硬性约束：混音后绝不能超过满刻度
        if report.peak_sample > 1.0 {
            println!("  ✗ 混音峰值 {} 超出 [-1, 1]", report.peak_sample);
            all_ok = false;
        }
        // 低延迟档的缓冲必须守在 20ms
        if mode == QualityMode::LowLatency && report.final_latency_ms > 20 {
            println!("  ✗ 低延迟档缓冲涨到了 {}ms", report.final_latency_ms);
            all_ok = false;
        }
    }

    println!();
    // 漂移校正的收敛性验证。
    //
    // 闭环模型：音源的晶振比本机快 SOURCE_PPM，所以每轮实际产出比标称多一点；
    // 消费端按当前校正比率取数。若校正生效，水位应当稳定在目标附近不漂走，
    // 且校正量最终收敛到与晶振偏差相等的值。
    const SOURCE_PPM: f64 = 2000.0;
    const TARGET_FILL: usize = 1024;
    let mut corrector = DriftCorrector::new(TARGET_FILL);
    let mut fill = TARGET_FILL as f64;

    for _ in 0..4000 {
        corrector.update(fill as usize);
        let produced = TARGET_FILL as f64 * (1.0 + SOURCE_PPM / 1_000_000.0);
        let consumed = TARGET_FILL as f64 * corrector.ratio();
        fill = (fill + produced - consumed).clamp(0.0, (TARGET_FILL * 4) as f64);
    }

    let drift = fill - TARGET_FILL as f64;
    println!(
        "漂移校正：音源快 {SOURCE_PPM:.0}ppm，收敛于 {:.0}ppm，水位稳定在 {:.0}（目标 {TARGET_FILL}）",
        corrector.ppm(),
        fill
    );

    // 校正量必须落在可闻阈值内
    if corrector.ppm().abs() > 2000.0 {
        println!("  ✗ 漂移校正超出了可闻阈值");
        all_ok = false;
    }
    // 水位必须被拉住，不能一路漂走
    if drift.abs() > TARGET_FILL as f64 * 0.1 {
        println!("  ✗ 水位漂离目标 {drift:.0} 个样本，校正没有收敛");
        all_ok = false;
    }
    // 校正量应当接近真实偏差
    if (corrector.ppm() - SOURCE_PPM).abs() > 200.0 {
        println!("  ✗ 校正量 {:.0}ppm 与真实偏差 {SOURCE_PPM:.0}ppm 不符", corrector.ppm());
        all_ok = false;
    }

    println!();
    if all_ok {
        println!("✓ 全部硬性约束通过");
    } else {
        println!("✗ 存在约束失败");
        std::process::exit(1);
    }
}
