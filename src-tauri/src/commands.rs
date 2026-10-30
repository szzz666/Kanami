//! 前端可调用的命令。
//!
//! 每个命令都保持薄：参数校验 + 调引擎 + 返回可序列化结果。
//! 业务逻辑不在这一层。

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use kanami_audio::mixer::MAX_SOURCES;
use kanami_engine::engine::{IncomingSource, Outgoing};
use kanami_engine::platform;
use kanami_net::audio_socket::AudioSocket;
use kanami_net::discovery::{DiscoveredVia, PeerDevice};
use kanami_net::signaling::{self, Accept};
use kanami_proto::{Hello, QualityMode, Role};
use serde::Serialize;
use tauri::State;

use crate::state::{AppState, PendingRequest};

/// 平台信息。
#[derive(Debug, Serialize)]
pub struct PlatformInfo {
    pub platform: String,
    pub system_audio_capture: bool,
    pub process_exclusion: bool,
    pub background_playback: bool,
    pub notes: Vec<String>,
    pub version: String,
}

#[tauri::command]
pub fn get_platform_info() -> PlatformInfo {
    let caps = platform::caps();
    PlatformInfo {
        platform: caps.platform,
        system_audio_capture: caps.system_audio_capture,
        process_exclusion: caps.process_exclusion,
        background_playback: caps.background_playback,
        notes: caps.notes,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// 可采集的音源。
#[derive(Debug, Serialize)]
pub struct CaptureSourceDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_default: bool,
}

#[tauri::command]
pub fn list_capture_sources() -> Vec<CaptureSourceDto> {
    platform::capture_backend()
        .list_sources()
        .into_iter()
        .map(|s| CaptureSourceDto {
            id: s.id,
            name: s.name,
            kind: s.kind.label().to_string(),
            sample_rate: s.sample_rate,
            channels: s.channels,
            is_default: s.is_default,
        })
        .collect()
}

/// 输出设备。
#[derive(Debug, Serialize)]
pub struct OutputDeviceDto {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

#[tauri::command]
pub fn list_output_devices() -> Vec<OutputDeviceDto> {
    kanami_engine::playout::list_output_devices()
        .into_iter()
        .map(|d| OutputDeviceDto {
            id: d.id,
            name: d.name,
            is_default: d.is_default,
            sample_rate: d.sample_rate,
            channels: d.channels,
        })
        .collect()
}

/// 开始发送：绑定端口、启动采集、开始编码。
#[tauri::command]
pub fn start_sending(state: State<'_, AppState>, source_id: String) -> Result<(), String> {
    let engine = &state.engine;

    // 已在发送就不重复启动
    if engine.outgoing.lock().is_some() {
        return Err("已经在发送中".into());
    }

    let mode = *state.mode.lock();
    let socket = AudioSocket::bind(0, 1).map_err(|e| format!("音频端口绑定失败：{e}"))?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    state.audio_port.store(u32::from(port), Ordering::Relaxed);

    let outgoing = Outgoing::new(socket, mode).map_err(|e| format!("编码器初始化失败：{e}"))?;
    *engine.outgoing.lock() = Some(outgoing);

    // 启动采集
    let backend = platform::capture_backend();
    let source = backend
        .list_sources()
        .into_iter()
        .find(|s| s.id == source_id)
        .or_else(|| backend.list_sources().into_iter().next())
        .ok_or_else(|| "没有可用的音源".to_string())?;

    let ring = std::sync::Arc::clone(&engine.capture_ring);
    let stats = std::sync::Arc::new(kanami_engine::capture::CaptureStats::default());
    let session = backend
        .start(
            &source,
            Box::new(move |samples| {
                ring.lock().push(samples);
            }),
            stats,
        )
        .map_err(|e| format!("采集启动失败：{e}"))?;

    *engine.capture.lock() = Some(session);
    engine.running.store(true, Ordering::Relaxed);
    log::info!("开始发送，音源={} 端口={}", source.name, port);
    Ok(())
}

#[tauri::command]
pub fn stop_sending(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(session) = state.engine.capture.lock().take() {
        session.stop();
    }
    *state.engine.outgoing.lock() = None;
    state.engine.running.store(false, Ordering::Relaxed);
    state.audio_port.store(0, Ordering::Relaxed);
    Ok(())
}

/// 切换模式。发送中会重建编码器，接收中会重建抖动缓冲。
#[tauri::command]
pub fn set_mode(state: State<'_, AppState>, mode: String) -> Result<(), String> {
    let parsed = match mode.as_str() {
        "标准" | "standard" | "Standard" => QualityMode::Standard,
        "低延迟" | "low" | "LowLatency" => QualityMode::LowLatency,
        "高音质" | "high" | "HighQuality" => QualityMode::HighQuality,
        other => return Err(format!("未知模式：{other}")),
    };

    *state.mode.lock() = parsed;

    if let Some(outgoing) = state.engine.outgoing.lock().as_mut() {
        outgoing.set_mode(parsed).map_err(|e| format!("切换失败：{e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> serde_json::Value {
    state.status_snapshot()
}

/// 对端设备。
#[derive(Debug, Serialize)]
pub struct PeerDto {
    pub device_id: String,
    pub name: String,
    pub role: String,
    pub addr: String,
    pub audio_port: u16,
    pub signal_port: u16,
    pub mode: String,
    pub via: String,
    pub age_secs: u64,
}

#[tauri::command]
pub fn list_peers(state: State<'_, AppState>) -> Vec<PeerDto> {
    state
        .peers
        .list()
        .into_iter()
        .map(|p| PeerDto {
            device_id: p.device_id,
            name: p.name,
            role: format!("{:?}", p.role),
            addr: p.addr.to_string(),
            audio_port: p.audio_port,
            signal_port: p.signal_port,
            mode: p.mode.name().to_string(),
            via: p.via.label().to_string(),
            age_secs: p.last_seen.elapsed().as_secs(),
        })
        .collect()
}

/// 立刻扫一轮。
#[tauri::command]
pub fn scan_peers(state: State<'_, AppState>) -> Vec<PeerDto> {
    let me = PeerDevice {
        device_id: state.self_id.clone(),
        name: state.self_name.clone(),
        role: Role::Both,
        audio_port: state.audio_port.load(Ordering::Relaxed) as u16,
        signal_port: state.signal_port.load(Ordering::Relaxed) as u16,
        mode: *state.mode.lock(),
        addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        via: DiscoveredVia::Broadcast,
        last_seen: std::time::Instant::now(),
    };
    if let Some(b) = state.broadcaster.lock().as_ref() {
        b.tick(&me);
    }
    list_peers(state)
}

/// 手动添加设备。广播被拦截时这是唯一能用的方式。
#[tauri::command]
pub fn add_peer_manual(state: State<'_, AppState>, addr: String) -> Result<PeerDto, String> {
    let parsed: SocketAddr =
        addr.parse().map_err(|_| "地址格式不对，应当形如 192.168.1.10:47822".to_string())?;

    let device = PeerDevice {
        device_id: format!("manual:{}", parsed),
        name: format!("手动设备 {}", parsed),
        role: Role::Both,
        audio_port: parsed.port(),
        signal_port: parsed.port(),
        mode: QualityMode::Standard,
        addr: parsed,
        via: DiscoveredVia::Manual,
        last_seen: std::time::Instant::now(),
    };
    state.peers.upsert(device.clone());
    state.manual_peers.lock().insert(device.device_id.clone(), parsed);

    Ok(PeerDto {
        device_id: device.device_id,
        name: device.name,
        role: format!("{:?}", device.role),
        addr: device.addr.to_string(),
        audio_port: device.audio_port,
        signal_port: device.signal_port,
        mode: device.mode.name().to_string(),
        via: device.via.label().to_string(),
        age_secs: 0,
    })
}

#[tauri::command]
pub fn remove_peer(state: State<'_, AppState>, device_id: String) -> bool {
    state.manual_peers.lock().remove(&device_id);
    state.peers.remove(&device_id)
}

/// 播放端：连接一台音源设备并开始接收。
#[tauri::command]
pub fn connect_to_peer(state: State<'_, AppState>, device_id: String) -> Result<(), String> {
    // 幂等：已经在收这台设备就不重复连
    if state.connections.lock().contains_key(&device_id) {
        return Err("已经连接到这台设备了".into());
    }

    let peer = state.peers.get(&device_id).ok_or("设备不在列表里")?;
    let mode = *state.mode.lock();

    // 分配一个独立的 stream_id。
    //
    // 这个号必须每路不同：混音台按 stream_id 区分音源，
    // 写死成 1 的话第二路会盖掉第一路的数据。
    let stream_id = state.next_stream_id();
    if state.engine.incoming.lock().len() >= MAX_SOURCES {
        return Err(format!("同时接入的音源已达上限 {MAX_SOURCES} 路"));
    }

    // 信令连接：播放端主动发起
    let signal_addr = SocketAddr::new(peer.addr.ip(), peer.signal_port);
    let hello = Hello {
        role: Role::Player,
        mode,
        device_name: state.self_name.clone(),
        device_id: state.self_id.clone(),
    };

    let accept: Accept =
        signaling::join_source(signal_addr, &hello, std::time::Duration::from_secs(5))
            .map_err(|e| format!("连接失败：{e}"))?;

    // 建立接收通道，绑定这个流自己的 stream_id
    let socket = AudioSocket::bind(0, stream_id).map_err(|e| e.to_string())?;
    let audio_addr = SocketAddr::new(peer.addr.ip(), accept.audio_port);
    socket.set_peer(audio_addr);

    // 播放端先发一个 hello 包，让音源端学到我们的地址
    // 这是实测环境里唯一能打通的顺序：客体先发，宿主才能回
    socket
        .send(kanami_proto::PacketKind::Hello, 0, 0, &[])
        .map_err(|e| format!("握手失败：{e}"))?;

    let source = IncomingSource::new(
        stream_id,
        accept.device_id.clone(),
        accept.device_name.clone(),
        accept.mode,
    );
    if !state.engine.add_source(source) {
        return Err("加入混音台失败".into());
    }
    state.connections.lock().insert(device_id.clone(), audio_addr);

    // 接收线程：按 stream_id 精确投递到对应那一路，
    // 不能写 first_mut()——那样所有音源的数据会挤进同一路。
    let engine = std::sync::Arc::clone(&state.engine);
    std::thread::Builder::new()
        .name(format!("kanami-rx-{stream_id}"))
        .spawn(move || loop {
            match socket.try_recv() {
                Ok(Some((header, payload, _))) => {
                    if header.kind.is_audio() {
                        let mut list = engine.incoming.lock();
                        if let Some(s) = list.iter_mut().find(|s| s.stream_id == stream_id) {
                            s.on_packet(header.seq, header.timestamp_ms, payload);
                        }
                    }
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(_) => break,
            }
        })
        .map_err(|e| e.to_string())?;

    log::info!("已连接 {device_id}，stream_id={stream_id}");
    Ok(())
}

#[tauri::command]
pub fn disconnect_peer(state: State<'_, AppState>, device_id: String) -> bool {
    let Some(addr) = state.connections.lock().remove(&device_id) else {
        return false;
    };
    // 按对端地址找到对应那一路并移除，而不是盲目移除第一路
    let stream_id =
        state.engine.incoming.lock().iter().find(|s| s.device_id == device_id).map(|s| s.stream_id);
    if let Some(id) = stream_id {
        state.engine.remove_source(id);
    } else {
        let _ = addr;
    }
    true
}

/// 本机信息：给用户看「让别人连我该填什么」。
#[derive(Debug, Serialize)]
pub struct LocalInfo {
    pub device_id: String,
    pub name: String,
    pub addresses: Vec<String>,
    pub audio_port: u16,
    pub signal_port: u16,
    pub platform: String,
}

#[tauri::command]
pub fn get_local_info(state: State<'_, AppState>) -> LocalInfo {
    let addrs =
        kanami_net::audio_socket::local_ipv4_addrs().into_iter().map(|ip| ip.to_string()).collect();
    LocalInfo {
        device_id: state.self_id.clone(),
        name: state.self_name.clone(),
        addresses: addrs,
        audio_port: state.audio_port.load(Ordering::Relaxed) as u16,
        signal_port: state.signal_port.load(Ordering::Relaxed) as u16,
        platform: platform::caps().platform,
    }
}

#[tauri::command]
pub fn set_source_volume(state: State<'_, AppState>, stream_id: u16, gain: f32) -> bool {
    let mut mixer = state.engine.mixer.lock();
    if let Some(s) = mixer.source_mut(stream_id) {
        s.gain = gain.clamp(0.0, 4.0);
        true
    } else {
        false
    }
}

#[tauri::command]
pub fn set_source_muted(state: State<'_, AppState>, stream_id: u16, muted: bool) -> bool {
    let mut mixer = state.engine.mixer.lock();
    if let Some(s) = mixer.source_mut(stream_id) {
        s.muted = muted;
        true
    } else {
        false
    }
}

#[tauri::command]
pub fn set_source_solo(state: State<'_, AppState>, stream_id: u16, solo: bool) -> bool {
    let mut mixer = state.engine.mixer.lock();
    if let Some(s) = mixer.source_mut(stream_id) {
        s.solo = solo;
        true
    } else {
        false
    }
}

/// 音源端：批准一个播放端接入。
#[tauri::command]
pub fn approve_peer(state: State<'_, AppState>, device_id: String) -> bool {
    state.trusted.lock().insert(device_id.clone());
    state.pending.lock().retain(|p| p.device_id != device_id);
    true
}

#[tauri::command]
pub fn reject_peer(state: State<'_, AppState>, device_id: String) -> bool {
    state.pending.lock().retain(|p| p.device_id != device_id);
    true
}

#[tauri::command]
pub fn list_pending_requests(state: State<'_, AppState>) -> Vec<PendingRequest> {
    state.pending.lock().clone()
}
