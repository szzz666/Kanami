//! 音频内核的契约测试。
//!
//! 这里锁定的都是「一旦改坏，用户立刻能听出来」的行为：
//! 低延迟档的缓冲必须不涨、多路混音绝不能爆音、断流的一路不能拖垮其他路、
//! 漂移校正必须收敛且幅度在人耳不可闻的范围内。

use kanami_audio::jitter::{Frame, JitterBuffer};
use kanami_audio::mixer::{Mixer, MAX_SOURCES};
use kanami_audio::resample::{DriftCorrector, MAX_PPM};
use kanami_proto::QualityMode;

fn frame(seq: u32) -> Frame {
    // 序号可能贴近 u32 上界，时间戳换算必须回绕安全
    Frame { seq, timestamp_ms: seq.wrapping_mul(10), payload: vec![0u8; 8] }
}

// ---------- 抖动缓冲 ----------

#[test]
fn 低延迟档的缓冲水位永不增长() {
    // 这是对用户的承诺：低延迟模式宁可丢音，也不偷偷加大延迟
    let mut buf = JitterBuffer::new(QualityMode::LowLatency.params());
    assert!(!buf.is_adaptive());
    let initial = buf.target_latency_ms();

    // 反复制造欠载，缓冲也不应该涨
    for round in 0..50u32 {
        for i in 0..3u32 {
            buf.push(frame(round * 10 + i));
        }
        for _ in 0..10 {
            buf.pop();
            buf.adapt();
        }
        assert_eq!(buf.target_latency_ms(), initial, "低延迟档的水位被改动了");
    }
    assert_eq!(initial, 20);
}

#[test]
fn 标准档在抖动时抬高水位() {
    let mut buf = JitterBuffer::new(QualityMode::Standard.params());
    let initial = buf.target_latency_ms();

    // 先把水位喂满，进入正常播放态
    for seq in 0..8u32 {
        buf.push(frame(seq));
    }
    while buf.pop().is_some() {}

    // 然后反复制造欠载：网络抖动大到缓冲见底
    for seq in 100..160u32 {
        buf.push(frame(seq));
        buf.pop();
        buf.adapt();
    }

    assert!(buf.target_latency_ms() > initial, "连续欠载后水位应当抬高");
    assert!(buf.target_latency_ms() <= QualityMode::Standard.params().jitter_max_ms);
}

#[test]
fn 水位不会超过模式规定的上限() {
    let params = QualityMode::Standard.params();
    let mut buf = JitterBuffer::new(params);

    for _ in 0..2000 {
        buf.pop();
        buf.adapt();
    }

    assert!(
        buf.target_latency_ms() <= params.jitter_max_ms,
        "水位 {} 超过了上限 {}",
        buf.target_latency_ms(),
        params.jitter_max_ms
    );
}

#[test]
fn 水位不足时不吐帧() {
    let mut buf = JitterBuffer::new(QualityMode::Standard.params());
    // 标准档需要 2 帧水位
    buf.push(frame(1));
    assert!(buf.pop().is_none(), "水位不足时必须返回 None，让调用方补静音");
}

#[test]
fn 乱序到达仍按序输出() {
    let mut buf = JitterBuffer::new(QualityMode::LowLatency.params());
    let mut frames = vec![frame(1), frame(2), frame(3)];
    frames.reverse();

    for f in frames {
        buf.push(f);
    }

    // 低延迟档目标水位 2 帧：吐出 1、2 之后只剩 1 帧，水位不足必须停下等后续包，
    // 而不是把最后一帧提前播掉——这就是抖动缓冲存在的意义。
    let out: Vec<u32> = std::iter::from_fn(|| buf.pop()).map(|f| f.seq).collect();
    assert_eq!(out, vec![1, 2]);

    // 再补一帧，剩余的按序吐出
    buf.push(frame(4));
    let rest: Vec<u32> = std::iter::from_fn(|| buf.pop()).map(|f| f.seq).collect();
    assert_eq!(rest, vec![3]);
}

#[test]
fn 重复包被丢弃且计数() {
    let mut buf = JitterBuffer::new(QualityMode::LowLatency.params());
    assert!(buf.push(frame(1)));
    assert!(!buf.push(frame(1)), "重复包必须被拒绝");
    assert_eq!(buf.stats().duplicate, 1);
}

#[test]
fn 晚到的包被丢弃且计数() {
    let mut buf = JitterBuffer::new(QualityMode::LowLatency.params());
    buf.push(frame(5));
    buf.push(frame(6));
    while buf.pop().is_some() {}

    // 序号 5 已经播过，再来一次属于晚到
    assert!(!buf.push(frame(5)), "已播过的序号必须被拒绝");
    assert!(buf.stats().late > 0);
}

#[test]
fn 序号回绕不出错() {
    let mut buf = JitterBuffer::new(QualityMode::LowLatency.params());
    let start = u32::MAX - 1;
    for i in 0..4u32 {
        buf.push(frame(start.wrapping_add(i)));
    }

    // 序号跨过 u32 上界后必须继续单调输出，不能把 0、1 当成「更早」的旧包丢掉
    let out: Vec<u32> = std::iter::from_fn(|| buf.pop()).map(|f| f.seq).collect();
    assert_eq!(out, vec![u32::MAX - 1, u32::MAX, 0], "回绕处的帧被误判为晚到");

    buf.push(frame(2));
    let rest: Vec<u32> = std::iter::from_fn(|| buf.pop()).map(|f| f.seq).collect();
    assert_eq!(rest, vec![1]);
}

// ---------- 混音 ----------

#[test]
fn 单路混音保持原样() {
    let mut mixer = Mixer::new();
    assert!(mixer.add_source(1, MAX_SOURCES));

    let input = [0.5f32, -0.5, 0.25, -0.25];
    let mut out = vec![0.0f32; 4];
    mixer.mix(&[(1, &input)], &mut out);

    assert_eq!(out, input.to_vec());
}

#[test]
fn 多路叠加不爆音() {
    let mut mixer = Mixer::new();
    for id in 1..=4u16 {
        assert!(mixer.add_source(id, MAX_SOURCES));
    }

    // 四路都是满刻度，直接相加会是 4.0
    let loud = vec![1.0f32; 64];
    let inputs: Vec<(u16, &[f32])> = (1..=4u16).map(|id| (id, loud.as_slice())).collect();

    let mut out = vec![0.0f32; 64];
    mixer.mix(&inputs, &mut out);

    for sample in &out {
        assert!(sample.abs() <= 1.0, "混音后样本 {sample} 超出了 [-1, 1]");
        assert!(sample.is_finite());
    }
}

#[test]
fn 静音路不影响其他路() {
    let mut mixer = Mixer::new();
    mixer.add_source(1, MAX_SOURCES);
    mixer.add_source(2, MAX_SOURCES);
    mixer.source_mut(2).unwrap().muted = true;

    let a = vec![0.5f32; 8];
    let b = vec![0.5f32; 8];
    let mut out = vec![0.0f32; 8];
    mixer.mix(&[(1, &a), (2, &b)], &mut out);

    assert_eq!(out, a.to_vec(), "静音的那一路不应参与混音");
}

#[test]
fn 独奏只在有一路独奏时生效() {
    let mut mixer = Mixer::new();
    mixer.add_source(1, MAX_SOURCES);
    mixer.add_source(2, MAX_SOURCES);
    assert!(!mixer.any_solo());

    mixer.source_mut(1).unwrap().solo = true;
    assert!(mixer.any_solo());

    let a = vec![0.5f32; 4];
    let b = vec![0.5f32; 4];
    let mut out = vec![0.0f32; 4];
    mixer.mix(&[(1, &a), (2, &b)], &mut out);
    assert_eq!(out, a.to_vec(), "非独奏的那一路必须闭嘴");
}

#[test]
fn 断流的一路按静音参与混音() {
    let mut mixer = Mixer::new();
    mixer.add_source(1, MAX_SOURCES);
    mixer.add_source(2, MAX_SOURCES);
    // 第 2 路断流
    mixer.source_mut(2).unwrap().active = false;

    let a = vec![0.5f32; 4];
    let b = vec![0.5f32; 4];
    let mut out = vec![0.0f32; 4];
    mixer.mix(&[(1, &a), (2, &b)], &mut out);

    assert_eq!(out, a.to_vec(), "断流的一路不应拖累其他路");
}

#[test]
fn 未登记的来源不参与混音() {
    let mixer = Mixer::new();
    let samples = vec![1.0f32; 4];
    let mut out = vec![0.5f32; 4];
    mixer.mix(&[(99, &samples)], &mut out);
    assert_eq!(out, vec![0.0f32; 4], "来路不明的数据必须被忽略");
}

#[test]
fn 音源数量受软上限约束() {
    let mut mixer = Mixer::new();
    for id in 0..MAX_SOURCES as u16 {
        assert!(mixer.add_source(id, MAX_SOURCES));
    }
    assert!(!mixer.add_source(999, MAX_SOURCES), "超过上限必须被拒绝");
    assert_eq!(mixer.source_count(), MAX_SOURCES);
}

#[test]
fn 重复添加同一路不会撑爆上限() {
    let mut mixer = Mixer::new();
    for _ in 0..100 {
        assert!(mixer.add_source(1, MAX_SOURCES));
    }
    assert_eq!(mixer.source_count(), 1);
}

// ---------- 漂移校正 ----------

#[test]
fn 水位正常时几乎不调整() {
    let mut corrector = DriftCorrector::new(1024);
    corrector.update(1024);
    assert!(corrector.ppm().abs() < 1.0, "水位正常时不应引入偏差");
}

#[test]
fn 水位偏高时提高比率() {
    let mut corrector = DriftCorrector::new(1024);
    for _ in 0..20 {
        corrector.update(2048);
    }
    assert!(corrector.ratio() > 1.0, "积压时必须提高比率多消耗输入");
    assert!(corrector.ppm() <= MAX_PPM);
}

#[test]
fn 水位偏低时降低比率() {
    let mut corrector = DriftCorrector::new(1024);
    for _ in 0..20 {
        corrector.update(0);
    }
    assert!(corrector.ratio() < 1.0, "见底时必须降低比率少消耗输入");
    assert!(corrector.ppm() >= -MAX_PPM);
}

#[test]
fn 极端水位也不超出可闻阈值() {
    let mut corrector = DriftCorrector::new(1024);
    for current in [0usize, 1, 1024, 100_000, usize::MAX / 2] {
        for _ in 0..100 {
            corrector.update(current);
        }
        assert!(
            corrector.ppm().abs() <= MAX_PPM,
            "水位 {current} 下偏差 {} ppm 超出上限",
            corrector.ppm()
        );
    }
}

#[test]
fn 输入长度随比率变化() {
    let mut corrector = DriftCorrector::new(1024);
    assert_eq!(corrector.input_len_for(1024), 1024);

    for _ in 0..20 {
        corrector.update(4096);
    }
    assert!(corrector.input_len_for(1024) > 1024);
}
