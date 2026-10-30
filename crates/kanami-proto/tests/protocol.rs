//! 协议层的契约测试。
//!
//! 这些用例锁定的是**对外承诺**：包头布局、模式参数表、以及对畸形输入的拒绝行为。
//! 任何一条失败都意味着线格式不再兼容，或三种模式的参数被悄悄改动。

use kanami_proto::{
    Header, Hello, PacketKind, ProtoError, QualityMode, Role, HEADER_LEN, KEEPALIVE_INTERVAL_MS,
    MAX_PAYLOAD,
};

fn sample_header() -> Header {
    Header::new(PacketKind::Audio, 7, 123_456, 9_876_543, 320)
}

#[test]
fn 包头往返编解码保持一致() {
    let original = sample_header();
    let mut raw = [0u8; HEADER_LEN];
    original.encode(&mut raw);
    assert_eq!(Header::decode(&raw).expect("应能解析"), original);
}

#[test]
fn 包头固定为十六字节() {
    // 线格式长度是对外契约，改动会让新旧版本无法互通
    assert_eq!(HEADER_LEN, 16);
    let header = Header::new(PacketKind::Keepalive, 1, 1, 1, 0);
    let mut raw = [0u8; HEADER_LEN];
    header.encode(&mut raw);
    let (decoded, payload) = Header::read_packet(&raw).expect("应能解析");
    assert_eq!(decoded, header);
    assert!(payload.is_empty());
}

#[test]
fn 包体往返编解码保持一致() {
    let original = sample_header();
    let payload = vec![0xABu8; 320];
    let mut buf = vec![0u8; HEADER_LEN + payload.len()];
    let written = original.write_packet(&payload, &mut buf).expect("应能写入");
    assert_eq!(written, HEADER_LEN + payload.len());

    let (decoded, got) = Header::read_packet(&buf).expect("应能解析");
    assert_eq!(decoded, original);
    assert_eq!(got, payload.as_slice());
}

#[test]
fn 拒绝错误魔术字() {
    let mut raw = [0u8; HEADER_LEN];
    sample_header().encode(&mut raw);
    raw[0] = 0xFF;
    assert!(matches!(Header::decode(&raw), Err(ProtoError::BadMagic(_))));
}

#[test]
fn 拒绝不支持的协议版本() {
    let mut raw = [0u8; HEADER_LEN];
    sample_header().encode(&mut raw);
    raw[2] = 99;
    assert_eq!(Header::decode(&raw), Err(ProtoError::BadVersion(99)));
}

#[test]
fn 拒绝未知包类型() {
    let mut raw = [0u8; HEADER_LEN];
    sample_header().encode(&mut raw);
    raw[3] = 200;
    assert_eq!(Header::decode(&raw), Err(ProtoError::UnknownKind(200)));
}

#[test]
fn 拒绝过短的包() {
    assert!(matches!(Header::decode(&[0u8; 8]), Err(ProtoError::TooShort { .. })));
}

#[test]
fn 拒绝被截断的负载() {
    let header = Header::new(PacketKind::Audio, 1, 1, 1, 100);
    let mut buf = [0u8; HEADER_LEN];
    header.encode(&mut buf);
    // 声明 100 字节负载，实际一个都没有
    assert!(matches!(Header::read_packet(&buf), Err(ProtoError::Truncated { .. })));
}

#[test]
fn 拒绝超出上限的负载声明() {
    let header = Header::new(PacketKind::Audio, 1, 1, 1, (MAX_PAYLOAD + 1) as u16);
    let mut raw = [0u8; HEADER_LEN];
    header.encode(&mut raw);
    assert!(matches!(Header::decode(&raw), Err(ProtoError::PayloadTooLarge(_))));
}

#[test]
fn 缓冲区不足时报错而非静默截断() {
    let payload = vec![0u8; 320];
    let mut too_small = vec![0u8; 100];
    assert!(matches!(
        sample_header().write_packet(&payload, &mut too_small),
        Err(ProtoError::BufferTooSmall { .. })
    ));
}

#[test]
fn 握手消息往返保持一致() {
    let hello = Hello {
        role: Role::Both,
        mode: QualityMode::LowLatency,
        device_name: "MacBook Pro".into(),
        device_id: "a1b2c3d4".into(),
    };
    let bytes = hello.encode().expect("应能编码");
    assert_eq!(Hello::decode(&bytes).expect("应能解码"), hello);
}

#[test]
fn 握手支持中文设备名() {
    let hello = Hello {
        role: Role::Player,
        mode: QualityMode::HighQuality,
        device_name: "客厅的音箱".into(),
        device_id: "dev-01".into(),
    };
    let bytes = hello.encode().expect("应能编码");
    assert_eq!(Hello::decode(&bytes).expect("应能解码"), hello);
}

#[test]
fn 握手拒绝被截断的消息() {
    let hello = Hello {
        role: Role::Source,
        mode: QualityMode::Standard,
        device_name: "Master".into(),
        device_id: "id".into(),
    };
    let bytes = hello.encode().expect("应能编码");
    for cut in 0..bytes.len() {
        assert!(
            Hello::decode(&bytes[..cut]).is_err(),
            "截断到 {cut} 字节时应当报错而不是解析出半条消息"
        );
    }
}

#[test]
fn 三种模式的参数符合已定共识() {
    let standard = QualityMode::Standard.params();
    assert_eq!((standard.frame_ms, standard.bitrate_bps), (20, 96_000));
    assert!(standard.adaptive_jitter());
    assert!(standard.fec);

    let low = QualityMode::LowLatency.params();
    assert_eq!((low.frame_ms, low.bitrate_bps), (10, 128_000));
    // 低延迟档缓冲固定，不随网络增长——这是对用户的承诺
    assert_eq!(low.jitter_min_ms, 20);
    assert_eq!(low.jitter_max_ms, 20);
    assert!(!low.adaptive_jitter());
    assert!(!low.fec);

    let high = QualityMode::HighQuality.params();
    assert_eq!((high.frame_ms, high.bitrate_bps), (20, 256_000));
    assert!(high.vbr);
    assert!(high.adaptive_jitter());
    assert_eq!(high.complexity, 10);
}

#[test]
fn 每帧采样点数与帧长一致() {
    assert_eq!(QualityMode::Standard.params().samples_per_frame(), 960);
    assert_eq!(QualityMode::LowLatency.params().samples_per_frame(), 480);
    assert_eq!(QualityMode::HighQuality.params().samples_per_frame(), 960);
}

#[test]
fn 角色能力判定() {
    assert!(Role::Both.can_send() && Role::Both.can_play());
    assert!(Role::Source.can_send() && !Role::Source.can_play());
    assert!(!Role::Player.can_send() && Role::Player.can_play());
}

#[test]
fn 保活间隔必须远小于实测的对端映射存活时长() {
    // 实测：对称 NAT 映射在 180 秒空闲后仍然存活。保活必须留足余量，
    // 否则映射会在两次保活之间过期，回程包被 NAT 直接丢弃。
    // 用编译期断言，让「把保活间隔调大」这件事在构建阶段就失败。
    const _: () = assert!(KEEPALIVE_INTERVAL_MS < 180_000, "保活间隔超过了实测映射存活时长");
    const _: () = assert!(KEEPALIVE_INTERVAL_MS <= 90_000, "保活间隔过大，映射可能先过期");
    assert_eq!(KEEPALIVE_INTERVAL_MS, 60_000);
}

#[test]
fn 未知枚举值一律拒绝() {
    assert_eq!(Role::from_u8(0), None);
    assert_eq!(QualityMode::from_u8(0), None);
    assert_eq!(QualityMode::from_u8(4), None);
    assert_eq!(PacketKind::from_u8(7), None);
}
