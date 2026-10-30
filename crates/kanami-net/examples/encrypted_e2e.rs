//! 加密音频链路的端到端验证。
//!
//! 走完整流水线：PCM → Opus 编码 → 加密 → UDP → 解密 → 解码 → PCM，
//! 然后比对输入输出。**加密不能破坏音频**，也不能在 PIN 不匹配时
//! 悄悄输出可听的错误内容。

use std::net::UdpSocket;

use kanami_audio::codec::{OpusDecoder, OpusEncoder};
use kanami_audio::{CHANNELS, SAMPLE_RATE};
use kanami_net::crypto::{PairingSalt, SecureChannel};
use kanami_proto::QualityMode;

/// 跑一轮并返回解码出来的 PCM。
fn roundtrip(pin: &str, check_pin: &str) -> Vec<f32> {
    let salt = PairingSalt([42u8; 16]);
    let params = QualityMode::Standard.params();
    let frame_samples = params.samples_per_frame();

    let mut encoder = OpusEncoder::new(params).expect("编码器");
    let mut decoder = OpusDecoder::new(frame_samples).expect("解码器");
    let tx = SecureChannel::from_pin(pin, &salt).expect("发送通道");
    let mut rx = SecureChannel::from_pin(check_pin, &salt).expect("接收通道");

    let socket = UdpSocket::bind("127.0.0.1:0").expect("绑定");
    socket.set_read_timeout(Some(std::time::Duration::from_millis(200))).expect("超时");
    let target = socket.local_addr().expect("地址");

    let mut out = Vec::new();
    for seq in 0..20u32 {
        // 生成一帧正弦
        let mut pcm = vec![0.0f32; frame_samples * CHANNELS];
        for i in 0..frame_samples {
            let t = (seq as usize * frame_samples + i) as f32 / SAMPLE_RATE as f32;
            let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3;
            pcm[i * CHANNELS] = v;
            pcm[i * CHANNELS + 1] = v;
        }

        let encoded = encoder.encode(&pcm).expect("编码");
        let sealed = tx.seal(seq, &encoded);

        socket.send_to(&sealed, target).expect("发送");

        let mut buf = vec![0u8; 2048];
        if let Ok((n, _)) = socket.recv_from(&mut buf) {
            match rx.open(&buf[..n]) {
                Ok(plain) => out.extend(decoder.decode(&plain)),
                // PIN 不对：输出静音，不能把噪声当音频播出去
                Err(_) => out.extend(vec![0.0f32; frame_samples * CHANNELS]),
            }
        }
    }
    out
}

/// 皮尔逊相关系数（固定对齐）。
fn corr_at(a: &[f32], b: &[f32], offset: usize) -> f64 {
    let n = a.len().min(b.len().saturating_sub(offset));
    if n < 100 {
        return 0.0;
    }
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for i in 0..n {
        let x = f64::from(a[i]);
        let y = f64::from(b[i + offset]);
        sx += x;
        sy += y;
        sxy += x * y;
        sxx += x * x;
        syy += y * y;
    }
    let cnt = n as f64;
    let num = sxy - sx * sy / cnt;
    let den = ((sxx - sx * sx / cnt) * (syy - sy * sy / cnt)).sqrt();
    if den.abs() < 1e-12 {
        0.0
    } else {
        num / den
    }
}

/// 在 Opus 的算法延迟范围内搜索最佳对齐后取相关系数。
///
/// 不能直接逐点比：Opus 有预滚与重采样延迟，输出相对输入有偏移，
/// 直接比会把整体位移算成「不相关」。
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let mut best = 0.0f64;
    for offset in 0..3000usize {
        let c = corr_at(a, b, offset);
        if c.abs() > best.abs() {
            best = c;
        }
    }
    best
}

fn main() {
    let params = QualityMode::Standard.params();
    let frame_samples = params.samples_per_frame();

    // 参考信号（未加密路径的期望内容）
    let mut reference = Vec::new();
    for seq in 0..20usize {
        for i in 0..frame_samples {
            let t = (seq * frame_samples + i) as f32 / SAMPLE_RATE as f32;
            let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3;
            reference.push(v);
            reference.push(v);
        }
    }

    println!("Kanami 加密音频链路验证\n");

    // 1. 正确 PIN：音频必须完好
    let good = roundtrip("1234", "1234");
    let r_good = correlation(&reference, &good[..reference.len().min(good.len())]);
    println!("正确 PIN  相关系数 {r_good:.4}");

    // 2. 错误 PIN：必须解不出正确内容
    let bad = roundtrip("1234", "9999");
    let r_bad = correlation(&reference, &bad[..reference.len().min(bad.len())]);
    println!("错误 PIN  相关系数 {r_bad:.4}");

    let peak_bad = bad.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    println!("错误 PIN  峰值 {peak_bad:.3}（应为 0，即静音而非噪声）");

    let mut ok = true;
    if r_good < 0.95 {
        println!("✗ 加密路径破坏了音频：相关系数只有 {r_good:.4}");
        ok = false;
    }
    if r_bad > 0.5 {
        println!("✗ 错误 PIN 竟然解出了正确内容：相关系数 {r_bad:.4}");
        ok = false;
    }
    if peak_bad > 0.01 {
        println!("✗ 错误 PIN 时输出了噪声而不是静音：峰值 {peak_bad:.3}");
        ok = false;
    }

    println!();
    if ok {
        println!("✓ 加密链路正常：正确 PIN 音频完好，错误 PIN 得到静音而非噪声");
    } else {
        std::process::exit(1);
    }
}
