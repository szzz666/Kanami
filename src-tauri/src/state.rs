//! 应用运行时状态。
//!
//! 把引擎、发现、授权、连接管理放在一起，供命令层调用。

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_engine::engine::Engine;
use kanami_net::discovery::{Broadcaster, DiscoveredVia, PeerDevice, PeerTable};
use kanami_net::signaling::{Accept, RejectReason, Signal, SignalKind, SignalListener};
use kanami_proto::{Hello, QualityMode, Role};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

/// 播放端等待音源端确认的请求。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingRequest {
    pub device_id: String,
    pub name: String,
    pub addr: String,
}

/// 应用全局状态。
pub struct AppState {
    pub engine: Arc<Engine>,
    pub peers: Arc<PeerTable>,
    pub broadcaster: Mutex<Option<Broadcaster>>,
    /// 本机设备标识。
    pub self_id: String,
    pub self_name: String,
    /// 本机音频端口。
    pub audio_port: AtomicU32,
    /// 本机信令端口。
    pub signal_port: AtomicU32,
    /// 信令监听器（作为音源端）。
    pub listener: Mutex<Option<SignalListener>>,
    /// 已授权可接入的播放端。
    pub trusted: Mutex<HashSet<String>>,
    /// 等待用户确认的接入请求。
    pub pending: Mutex<Vec<PendingRequest>>,
    /// 当前模式。
    pub mode: Mutex<QualityMode>,
    /// 已连接的对端（播放端视角）。
    pub connections: Mutex<HashMap<String, SocketAddr>>,
    /// 手动添加的设备。
    pub manual_peers: Mutex<HashMap<String, SocketAddr>>,
    /// 上次广播时刻。
    last_announce: Mutex<Option<Instant>>,
    /// 统计。
    pub started_at: Instant,
    pub tick_count: AtomicU64,
}

impl AppState {
    pub fn new(_app: &AppHandle) -> anyhow::Result<Self> {
        let peers = Arc::new(PeerTable::new());
        let self_id = device_identity();
        let self_name = hostname();

        let broadcaster = Broadcaster::start(Arc::clone(&peers), self_id.clone()).ok().flatten();

        // 音源端信令监听：绑定 0 端口让系统分配
        let listener = SignalListener::bind(0).ok();

        Ok(Self {
            engine: Arc::new(Engine::new()),
            peers,
            broadcaster: Mutex::new(broadcaster),
            self_id,
            self_name,
            audio_port: AtomicU32::new(0),
            signal_port: AtomicU32::new(0),
            listener: Mutex::new(listener),
            trusted: Mutex::new(HashSet::new()),
            pending: Mutex::new(Vec::new()),
            mode: Mutex::new(QualityMode::Standard),
            connections: Mutex::new(HashMap::new()),
            manual_peers: Mutex::new(HashMap::new()),
            last_announce: Mutex::new(None),
            started_at: Instant::now(),
            tick_count: AtomicU64::new(0),
        })
    }

    /// 周期性维护。
    pub fn tick(&self, app: &AppHandle) -> anyhow::Result<()> {
        self.tick_count.fetch_add(1, Ordering::Relaxed);

        // 每 2 秒广播一次，顺带收别人的广播
        let should_announce = {
            let last = self.last_announce.lock();
            last.map(|t| t.elapsed() >= Duration::from_secs(2)).unwrap_or(true)
        };

        if should_announce {
            *self.last_announce.lock() = Some(Instant::now());
            let me = PeerDevice {
                device_id: self.self_id.clone(),
                name: self.self_name.clone(),
                role: Role::Both,
                audio_port: self.audio_port.load(Ordering::Relaxed) as u16,
                signal_port: self.signal_port.load(Ordering::Relaxed) as u16,
                mode: *self.mode.lock(),
                addr: SocketAddr::from(([127, 0, 0, 1], 0)),
                via: DiscoveredVia::Broadcast,
                last_seen: Instant::now(),
            };
            if let Some(b) = self.broadcaster.lock().as_ref() {
                b.tick(&me);
            }
        }

        // 信令接入
        self.accept_signals();

        // 混音节拍：主循环 20ms 一轮，正好是标准档一帧。
        // 真正的出声时刻由 cpal 按设备时钟拉取，这里只负责把解码结果
        // 推进输出缓冲，两者速度不匹配时由环形缓冲吸收。
        self.engine.mix_once();

        // 保活
        if let Some(out) = self.engine.outgoing.lock().as_ref() {
            out.socket.maybe_keepalive();
        }

        // 推状态给前端
        let _ = app.emit("kanami://status", self.status_snapshot());
        Ok(())
    }

    /// 接受音源端的接入请求。
    fn accept_signals(&self) {
        let guard = self.listener.lock();
        let Some(listener) = guard.as_ref() else { return };

        while let Ok(Some((mut stream, addr))) = listener.try_accept() {
            let Ok(signal) = Signal::read_from(&mut stream) else { continue };

            match signal.kind {
                SignalKind::Join => {
                    let Ok(hello) = Hello::decode(&signal.payload) else { continue };

                    // 未授权：挂起等待用户确认
                    if !self.trusted.lock().contains(&hello.device_id) {
                        self.pending.lock().push(PendingRequest {
                            device_id: hello.device_id.clone(),
                            name: hello.device_name.clone(),
                            addr: addr.to_string(),
                        });
                        let reason = RejectReason::NotTrusted;
                        let _ = Signal::new(SignalKind::Reject, reason.code().as_bytes().to_vec())
                            .write_to(&mut stream);
                        continue;
                    }

                    let mode = *self.mode.lock();
                    let accept = Accept {
                        mode,
                        audio_port: self.audio_port.load(Ordering::Relaxed) as u16,
                        device_name: self.self_name.clone(),
                        device_id: self.self_id.clone(),
                    };
                    let _ = Signal::new(SignalKind::Accept, accept.encode()).write_to(&mut stream);
                }
                SignalKind::Leave => {
                    log::info!("播放端 {addr} 断开");
                }
                _ => {}
            }
        }
    }

    /// 当前状态快照，推给前端。
    pub fn status_snapshot(&self) -> serde_json::Value {
        let engine_status = self.engine.status();
        let sources: Vec<serde_json::Value> = self
            .engine
            .incoming
            .lock()
            .iter()
            .map(|s| {
                serde_json::json!({
                    "stream_id": s.stream_id,
                    "device_id": s.device_id,
                    "name": s.name,
                    "mode": s.mode.name(),
                    "packets": s.packets.load(Ordering::Relaxed),
                    "lost": s.lost_count(),
                    "buffer_ms": s.buffer_ms.load(Ordering::Relaxed),
                    "active": s.active.load(Ordering::Relaxed),
                })
            })
            .collect();

        serde_json::json!({
            "sending": engine_status.sending,
            "receiving": engine_status.receiving,
            "source_count": engine_status.source_count,
            "outgoing_frames": engine_status.outgoing_frames,
            "mode": engine_status.mode.name(),
            "sources": sources,
            "self_id": self.self_id,
            "self_name": self.self_name,
            "audio_port": self.audio_port.load(Ordering::Relaxed),
            "signal_port": self.signal_port.load(Ordering::Relaxed),
            "uptime_secs": self.started_at.elapsed().as_secs(),
        })
    }
}

/// 生成稳定的设备标识。
///
/// 存到应用配置目录，第一次生成后一直复用——
/// 「首次接入需确认」的白名单靠它认设备。
fn device_identity() -> String {
    let dir = dirs_config_dir();
    let path = dir.join("device_id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, &id);
    id
}

fn dirs_config_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join("Library/Application Support/Kanami");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return std::path::PathBuf::from(appdata).join("Kanami");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return std::path::PathBuf::from(xdg).join("kanami");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(".config/kanami");
        }
    }
    std::path::PathBuf::from("./kanami-data")
}

/// 取一个能让人认出来的设备名。
///
/// Android 上没有 hostname 可执行文件，直接调会失败并回退成
/// 毫无辨识度的占位名（实测显示成了 "localhost"）。
/// 移动端改用系统属性里的机型，桌面端仍然优先用主机名。
fn hostname() -> String {
    #[cfg(target_os = "android")]
    {
        // ro.product.model 通常是 "SM-G998B" 这类机型名，比主机名更有辨识度
        for prop in ["ro.product.model", "ro.product.device", "ro.product.name"] {
            if let Ok(output) = std::process::Command::new("getprop").arg(prop).output() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return format!("Android {trimmed}");
                    }
                }
            }
        }
        return "Android 设备".into();
    }

    #[cfg(not(target_os = "android"))]
    {
        for cmd in ["hostname", "uname"] {
            let mut command = std::process::Command::new(cmd);
            if cmd == "uname" {
                command.arg("-n");
            }
            if let Ok(output) = command.output() {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    let trimmed = text.trim();
                    // "localhost" 这种名字没有任何辨识度，等于没有
                    if !trimmed.is_empty() && trimmed != "localhost" {
                        return trimmed.to_string();
                    }
                }
            }
        }
        "Kanami 设备".into()
    }
}
