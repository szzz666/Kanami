//! 应用运行时状态。
//!
//! 把引擎、发现、授权、连接管理放在一起，供命令层调用。

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_engine::engine::Engine;
use kanami_net::discovery::{Broadcaster, DiscoveredVia, PeerDevice, PeerTable};
use kanami_net::signaling::{Accept, RejectReason, Signal, SignalKind, SignalListener};
use kanami_proto::{Hello, QualityMode, Role};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

/// 读一条信令的最大等待时间。
///
/// 信令帧很小，正常情况下一毫秒内就到齐了。给 2 秒是留足 Wi-Fi 抖动的余量，
/// 同时保证一个哑巴客户端最多把主循环拖住 2 秒，而不是无限期。
const SIGNAL_READ_TIMEOUT: Duration = Duration::from_secs(2);

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
    ///
    /// 用 Arc 包着是为了能交给信令线程：处理接入请求不能占用主循环，
    /// 而线程需要 'static 的句柄。
    pub trusted: Arc<Mutex<HashSet<String>>>,
    /// 等待用户确认的接入请求。
    pub pending: Arc<Mutex<Vec<PendingRequest>>>,
    /// 当前模式。
    pub mode: Mutex<QualityMode>,
    /// 已连接的对端（播放端视角）。
    pub connections: Mutex<HashMap<String, SocketAddr>>,
    /// 下一个可用的 stream_id。
    ///
    /// 每接一路音源分配一个新号：混音台靠它区分来源，
    /// 复用同一个号会让后接的那路盖掉前面的数据。
    next_stream: AtomicU32,
    /// 手动添加的设备。
    pub manual_peers: Mutex<HashMap<String, SocketAddr>>,
    /// 上次广播时刻。
    last_announce: Mutex<Option<Instant>>,
    /// 统计。
    pub started_at: Instant,
    pub tick_count: AtomicU64,
    /// 播放器线程是否已启动。
    ///
    /// 播放器本身（cpal Stream）不能放进这里：它是 !Send，
    /// 而 Tauri 的 managed state 要求 Send + Sync。
    /// 所以播放器活在自己的线程里，这里只留一个标志。
    pub player_started: AtomicBool,
}

impl AppState {
    pub fn new(app: &AppHandle) -> anyhow::Result<Self> {
        let peers = Arc::new(PeerTable::new());
        let self_id = device_identity();
        let self_name = hostname();

        // 广播器失败不能静默吞掉：这是「局域网自动发现」的唯一出口，
        // 它挂了的表现是「设备列表永远空着」，而其它一切（手动 IP、
        // 信令、音频）都正常——最难查的那类故障。
        let broadcaster = match Broadcaster::start(Arc::clone(&peers), self_id.clone()) {
            Ok(Some(b)) => {
                log::info!("发现广播已启动（{}）", b.local_addr_label());
                Some(b)
            }
            Ok(None) => {
                log::warn!("发现端口被占用，自动发现不可用；仍可用手动 IP 连接");
                None
            }
            Err(e) => {
                log::error!("发现广播启动失败：{e}；自动发现不可用");
                None
            }
        };

        // 音源端信令监听：绑定 0 端口让系统分配。
        //
        // 绑完必须把实际端口记下来，否则广播出去的是 0，
        // 别的设备发现得到但你永远连不上——实测就是这个问题：
        // 诊断探针看到「Android SM-G998B 音频端口=0 信令端口=0」。
        // 绑定失败必须记日志，不能 .ok() 一吞了之——
        // 那样信令端口会静默变成 0，广播出去后别人永远连不上，
        // 而界面上看不出任何异常。
        let listener = match SignalListener::bind(0) {
            Ok(l) => Some(l),
            Err(e) => {
                log::error!("信令监听绑定失败：{e}（其他设备将无法连接本机）");
                None
            }
        };
        let signal_port = listener
            .as_ref()
            .and_then(|l| match l.local_port() {
                Ok(p) => Some(p),
                Err(e) => {
                    log::error!("取信令端口失败：{e}");
                    None
                }
            })
            .unwrap_or(0);

        if signal_port == 0 {
            log::error!("信令端口为 0，本机将无法被其他设备连接");
        } else {
            log::info!("信令监听端口 {signal_port}");
        }

        Ok(Self {
            engine: Arc::new(Engine::new()),
            peers,
            broadcaster: Mutex::new(broadcaster),
            self_id,
            self_name,
            audio_port: AtomicU32::new(0),
            signal_port: AtomicU32::new(u32::from(signal_port)),
            listener: Mutex::new(listener),
            trusted: Arc::new(Mutex::new(crate::persist::load_trusted(app))),
            pending: Arc::new(Mutex::new(Vec::new())),
            mode: Mutex::new(QualityMode::Standard),
            connections: Mutex::new(HashMap::new()),
            next_stream: AtomicU32::new(1),
            manual_peers: Mutex::new(HashMap::new()),
            last_announce: Mutex::new(None),
            started_at: Instant::now(),
            tick_count: AtomicU64::new(0),
            player_started: AtomicBool::new(false),
        })
    }

    /// 分配一个新的 stream_id。从 1 开始，0 保留给控制消息。
    pub fn next_stream_id(&self) -> u16 {
        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
        // u16 用满后回绕到 1，跳过 0
        if id == 0 || id > u16::MAX as u32 {
            self.next_stream.store(2, Ordering::Relaxed);
            return 1;
        }
        id as u16
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

        // 信令接入（只做非阻塞 accept，真正的读包交给独立线程）
        self.accept_signals(app);

        // 发送泵：把采集到的样本喂给编码器并发 UDP。
        // 不调这一步的话采集回调拿到的数据只是躺在环形缓冲里，
        // 一个包都不会发出去——整条发送链路等于没接通。
        if self.engine.outgoing.lock().is_some() {
            let now_ms = self.started_at.elapsed().as_millis() as u32;
            crate::pump_capture(self.engine.as_ref(), now_ms);
        }

        // 混音节拍：主循环 20ms 一轮，正好是标准档一帧。
        // 真正的出声时刻由 cpal 按设备时钟拉取，这里只负责把解码结果
        // 推进输出缓冲，两者速度不匹配时由环形缓冲吸收。
        self.engine.mix_once();

        // 在发送 socket 上收一轮包。
        //
        // 这一步看着多余（音源端不是只管发吗），实际是整条链路的前提：
        // AudioSocket::send 在 peer 为 None 时静默返回 Ok(false)，
        // 而 peer 只能靠「从收到的包里学」——播放端连上来后会先发一个
        // Hello 到这个端口。不在这里收，音源端永远学不到播放端地址，
        // 编码照跑、序号照加，包却一个都没出去。
        if let Some(out) = self.engine.outgoing.lock().as_ref() {
            while let Ok(Some(_)) = out.socket.try_recv() {}
        }

        // 保活
        if let Some(out) = self.engine.outgoing.lock().as_ref() {
            out.socket.maybe_keepalive();
        }

        // 推状态给前端
        // 状态推送失败要能看见：前端完全依赖这个事件更新界面，
        // 它静默失败的表现是「后端一切正常，界面数字永远不动」。
        if let Err(e) = app.emit("kanami://status", self.status_snapshot()) {
            log::warn!("状态推送失败：{e}");
        }
        Ok(())
    }

    /// 接受音源端的接入请求。
    ///
    /// 这里**只做非阻塞的 accept**，读包回包都丢给独立线程。
    ///
    /// 原因是实测出来的一个死锁：这个函数原来串在主循环里（20ms 一轮），
    /// 而读信令是阻塞的。只要有一个客户端连上却不发数据——浏览器探活、
    /// 端口扫描、或者对端在写之前就崩了——整个主循环就停在那里：
    /// 不再广播（别的设备看不到本机）、不再收音频、不再推状态。
    /// 表现出来就是「应用还在，但谁都连不上，界面数字全冻住」。
    fn accept_signals(&self, _app: &AppHandle) {
        let guard = self.listener.lock();
        let Some(listener) = guard.as_ref() else { return };

        while let Ok(Some((stream, addr))) = listener.try_accept() {
            // 线程里要用到的状态先克隆出来。都是 Arc 或小字符串，
            // 克隆很便宜，而且能避免把 &self 借进 'static 线程。
            let info = SignalContext {
                self_id: self.self_id.clone(),
                self_name: self.self_name.clone(),
                audio_port: self.audio_port.load(Ordering::Relaxed) as u16,
                mode: *self.mode.lock(),
                trusted: Arc::clone(&self.trusted),
                pending: Arc::clone(&self.pending),
            };

            let spawned = std::thread::Builder::new()
                .name("kanami-signal".into())
                .spawn(move || handle_signal(stream, addr, &info));
            if let Err(e) = spawned {
                log::error!("信令线程启动失败：{e}");
            }
        }
    }

    /// 当前状态快照，推给前端。
    pub fn status_snapshot(&self) -> serde_json::Value {
        let engine_status = self.engine.status();
        let now_local = self.started_at.elapsed().as_millis() as i64;

        let sources: Vec<serde_json::Value> = self
            .engine
            .incoming
            .lock()
            .iter()
            .map(|s| {
                // 端到端延迟需要时钟同步才有意义。未同步时给 -1，
                // 前端据此显示「同步中」而不是一个假的数字。
                let latency = s.latency_ms(now_local).unwrap_or(-1);
                serde_json::json!({
                    "stream_id": s.stream_id,
                    "device_id": s.device_id,
                    "name": s.name,
                    "mode": s.mode.name(),
                    "packets": s.packets.load(Ordering::Relaxed),
                    "lost": s.lost_count(),
                    "buffer_ms": s.buffer_ms.load(Ordering::Relaxed),
                    "latency_ms": latency,
                    "synced": latency >= 0,
                    "active": s.active.load(Ordering::Relaxed),
                })
            })
            .collect();

        // 每 50 次 tick 打一次状态摘要。
        // 排查「后端连上了但界面没反应」这类问题时，
        // 需要能确认快照里到底有没有音源，而不是靠猜。
        let tick = self.tick_count.load(Ordering::Relaxed);
        if tick % 50 == 0 {
            log::info!(
                "状态快照：音源 {} 路，接收={}，包数={:?}",
                sources.len(),
                engine_status.receiving,
                sources.iter().map(|s| s["packets"].clone()).collect::<Vec<_>>()
            );
        }

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
/// 处理一条接入请求所需的全部状态。
///
/// 单独打包是因为信令处理已经挪到独立线程：线程拿不到 &AppState
/// （生命周期不够长），只能带一份克隆出来、'static 的上下文。
struct SignalContext {
    self_id: String,
    self_name: String,
    audio_port: u16,
    mode: QualityMode,
    trusted: Arc<Mutex<HashSet<String>>>,
    pending: Arc<Mutex<Vec<PendingRequest>>>,
}

/// 在一个独立线程里读完一条信令并作出应答。
///
/// 独立线程的意义不只是并发——是**隔离**。读 TCP 是阻塞的，
/// 放在主循环里等于把一个饿死的客户端变成整个应用的停机。
fn handle_signal(mut stream: std::net::TcpStream, addr: SocketAddr, ctx: &SignalContext) {
    let _ = stream.set_read_timeout(Some(SIGNAL_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SIGNAL_READ_TIMEOUT));

    let signal = match Signal::read_from(&mut stream) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("来自 {addr} 的信令读取失败：{e}");
            return;
        }
    };
    log::info!("收到来自 {addr} 的信令：{:?}", signal.kind);

    match signal.kind {
        SignalKind::Join => {
            let Ok(hello) = Hello::decode(&signal.payload) else {
                log::warn!("来自 {addr} 的 Hello 解析失败");
                return;
            };
            log::info!("播放端请求接入：{} [{}]", hello.device_name, hello.device_id);

            // 未授权：挂起等待用户确认
            if !ctx.trusted.lock().contains(&hello.device_id) {
                let mut pending = ctx.pending.lock();
                // 同一个设备重试多次时不重复堆积
                if !pending.iter().any(|p| p.device_id == hello.device_id) {
                    pending.push(PendingRequest {
                        device_id: hello.device_id.clone(),
                        name: hello.device_name.clone(),
                        addr: addr.to_string(),
                    });
                }
                drop(pending);

                let reason = RejectReason::NotTrusted;
                let _ = Signal::new(SignalKind::Reject, reason.code().as_bytes().to_vec())
                    .write_to(&mut stream);
                log::info!("回绝 {addr}：尚未在本机确认");
                return;
            }

            let accept = Accept {
                mode: ctx.mode,
                audio_port: ctx.audio_port,
                device_name: ctx.self_name.clone(),
                device_id: ctx.self_id.clone(),
            };
            match Signal::new(SignalKind::Accept, accept.encode()).write_to(&mut stream) {
                Ok(()) => log::info!("已接受 {addr}，通告音频端口 {}", ctx.audio_port),
                Err(e) => log::warn!("应答 {addr} 失败：{e}"),
            }
        }
        SignalKind::Leave => {
            log::info!("播放端 {addr} 断开");
        }
        other => log::debug!("忽略信令 {other:?}"),
    }
}
