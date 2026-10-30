//! 引擎核心。
//!
//! 把「采集 → 编码 → UDP 发送」和「UDP 接收 → 抖动缓冲 → 解码 → 混音 → 输出」
//! 两条流水线串起来，并对外提供可观测的统计信息。
//!
//! 线程模型：
//! - **采集线程**（平台回调）：把样本推进 SampleRing，绝不做重活。
//! - **编码发送线程**：从 ring 取样本，补齐到整帧，Opus 编码，发 UDP。
//! - **接收线程**：收包，进各路抖动缓冲。
//! - **混音线程**：按输出时钟节拍取出各路数据，解码、混音、写进 SharedOutput。
//! - **输出回调**（cpal）：只从 SharedOutput 拉数据，绝不阻塞。

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kanami_audio::codec::{OpusDecoder, OpusEncoder};
use kanami_audio::jitter::{Frame, JitterBuffer};
use kanami_audio::mixer::{Mixer, MAX_SOURCES};
use kanami_audio::{CHANNELS, SAMPLE_RATE};
use kanami_net::audio_socket::AudioSocket;
use kanami_proto::{PacketKind, QualityMode};

use crate::capture::{CaptureSession, SampleRing};
use crate::playout::OutputHealth;

/// 共享输出缓冲。播放回调从这里拉数据。
///
/// 内部以固定 48kHz 立体声为基准；输出设备规格不同的话，
/// 在拉取时按线性插值做一次轻量重采样。
#[derive(Debug)]
pub struct SharedOutput {
    buf: parking_lot::Mutex<OutputRing>,
    /// 输出设备的采样率。
    out_rate: AtomicU32,
    /// 输出设备的声道数。
    out_channels: AtomicU32,
    health: OutputHealth,
}

#[derive(Debug)]
struct OutputRing {
    data: Vec<f32>,
    read: usize,
    len: usize,
}

impl SharedOutput {
    pub fn new() -> Self {
        Self {
            buf: parking_lot::Mutex::new(OutputRing {
                data: vec![0.0; SAMPLE_RATE as usize * CHANNELS * 2],
                read: 0,
                len: 0,
            }),
            out_rate: AtomicU32::new(SAMPLE_RATE),
            out_channels: AtomicU32::new(CHANNELS as u32),
            health: OutputHealth::default(),
        }
    }

    pub fn set_output_format(&self, rate: u32, channels: u16) {
        self.out_rate.store(rate, Ordering::Relaxed);
        self.out_channels.store(u32::from(channels), Ordering::Relaxed);
    }

    pub fn note_callback(&self, _channels: usize) {
        self.health.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn failed(&self) -> bool {
        self.health.failed.load(Ordering::Relaxed)
    }

    pub fn mark_failed(&self) {
        self.health.failed.store(true, Ordering::Relaxed);
    }

    /// 混音线程写入（固定 48kHz 立体声交错）。
    pub fn push(&self, samples: &[f32]) {
        let mut ring = self.buf.lock();
        let cap = ring.data.len();
        for &s in samples {
            if ring.len == cap {
                // 输出消费不动：丢最旧的，保住实时性
                let next_read = (ring.read + 1) % cap;
                ring.read = next_read;
                ring.len -= 1;
            }
            let pos = (ring.read + ring.len) % cap;
            ring.data[pos] = s;
            ring.len += 1;
        }
    }

    /// 播放回调拉取。返回写入 out 的样本数（已转成输出设备的声道数）。
    pub fn pull(&self, out: &mut [f32]) -> usize {
        let out_channels = self.out_channels.load(Ordering::Relaxed).max(1) as usize;
        let out_rate = self.out_rate.load(Ordering::Relaxed).max(8000);

        let mut ring = self.buf.lock();
        let want_frames = out.len() / out_channels;
        if want_frames == 0 {
            return 0;
        }

        // 48kHz 到设备采样率的重采样比
        let ratio = f64::from(out_rate) / f64::from(SAMPLE_RATE);
        let need_frames = ((want_frames as f64 / ratio).ceil() as usize).max(1);
        let need_samples = need_frames * CHANNELS;

        if ring.len < need_samples {
            return 0;
        }

        // 取出内部样本
        let cap = ring.data.len();
        let read_start = ring.read;
        let mut tmp = vec![0.0f32; need_samples];
        for (i, slot) in tmp.iter_mut().enumerate().take(need_samples.min(ring.len)) {
            *slot = ring.data[(read_start + i) % cap];
        }
        ring.read = (read_start + need_samples) % cap;
        ring.len -= need_samples;

        // 线性插值到设备采样率，并映射声道
        for i in 0..want_frames {
            let src = i as f64 / ratio;
            let idx = src as usize;
            let frac = (src - idx as f64) as f32;
            let next = (idx + 1).min(need_frames - 1);
            let l = tmp[idx * CHANNELS] * (1.0 - frac) + tmp[next * CHANNELS] * frac;
            let r = tmp[idx * CHANNELS + 1] * (1.0 - frac) + tmp[next * CHANNELS + 1] * frac;
            for c in 0..out_channels {
                let v = if c == 0 {
                    l
                } else if c == 1 {
                    r
                } else {
                    (l + r) * 0.5
                };
                out[i * out_channels + c] = v;
            }
        }
        want_frames * out_channels
    }
}

impl Default for SharedOutput {
    fn default() -> Self {
        Self::new()
    }
}

/// 一路正在接收的音源。
pub struct IncomingSource {
    pub stream_id: u16,
    pub device_id: String,
    pub name: String,
    pub mode: QualityMode,
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    /// 端到端延迟（毫秒），-1 表示尚未同步。
    pub latency_ms: AtomicI64,
    /// 收到的包数。
    pub packets: AtomicU64,
    pub lost: AtomicU64,
    /// 当前抖动缓冲水位（毫秒）。
    pub buffer_ms: AtomicU32,
    /// 本轮是否供上了数据。
    pub active: AtomicBool,
    /// 时钟同步器。同步之后才能算出真实的端到端延迟，
    /// 否则「延迟」只是本地缓冲时间，不是用户能感知的那个数。
    pub clock: parking_lot::Mutex<kanami_net::clock::ClockSync>,
    /// 最近一帧的采集时刻（对端时钟，毫秒）。
    pub last_remote_ts: AtomicU32,
}

impl IncomingSource {
    pub fn new(stream_id: u16, device_id: String, name: String, mode: QualityMode) -> Self {
        let params = mode.params();
        let frame_samples = params.samples_per_frame();
        Self {
            stream_id,
            device_id,
            name,
            mode,
            jitter: JitterBuffer::new(params),
            decoder: OpusDecoder::new(frame_samples).expect("Opus 解码器初始化失败"),
            latency_ms: AtomicI64::new(-1),
            packets: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            buffer_ms: AtomicU32::new(params.jitter_min_ms),
            active: AtomicBool::new(true),
            clock: parking_lot::Mutex::new(kanami_net::clock::ClockSync::new()),
            last_remote_ts: AtomicU32::new(0),
        }
    }

    /// 收到一包音频。
    pub fn on_packet(&mut self, seq: u32, timestamp_ms: u32, payload: Vec<u8>) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        // 记下来用于延迟测量：有了对端的采集时刻，
        // 配合时钟偏移就能换算成本机时间轴上的延迟。
        self.last_remote_ts.store(timestamp_ms, Ordering::Relaxed);
        self.jitter.push(Frame { seq, timestamp_ms, payload });
    }

    /// 用一次 Ping-Pong 结果更新时钟偏移。
    pub fn update_clock(&self, t1: i64, t2: i64, t3: i64, t4: i64) {
        self.clock.lock().add(kanami_net::clock::OffsetSample::from_ping_pong(t1, t2, t3, t4));
    }

    /// 当前端到端延迟。未同步时返回 None。
    pub fn latency_ms(&self, now_local_ms: i64) -> Option<i64> {
        let remote = i64::from(self.last_remote_ts.load(Ordering::Relaxed));
        if remote == 0 {
            return None;
        }
        self.clock.lock().latency_ms(now_local_ms, remote)
    }

    /// 取一帧解码结果。没有可播的数据时返回 None。
    pub fn next_frame(&mut self) -> Option<Vec<f32>> {
        match self.jitter.pop() {
            Some(frame) => {
                self.jitter.adapt();
                self.buffer_ms.store(self.jitter.target_latency_ms(), Ordering::Relaxed);
                Some(self.decoder.decode(&frame.payload))
            }
            None => {
                self.jitter.adapt();
                self.buffer_ms.store(self.jitter.target_latency_ms(), Ordering::Relaxed);
                None
            }
        }
    }

    pub fn lost_count(&self) -> u64 {
        self.jitter.stats().lost
    }
}

/// 发送侧状态。
pub struct Outgoing {
    pub socket: AudioSocket,
    pub mode: QualityMode,
    encoder: OpusEncoder,
    /// 当前帧的累计样本。
    pending: Vec<f32>,
    seq: u32,
    /// 已编码发出的帧数。
    pub frames_sent: AtomicU64,
}

impl Outgoing {
    pub fn new(socket: AudioSocket, mode: QualityMode) -> anyhow::Result<Self> {
        let encoder = OpusEncoder::new(mode.params())?;
        Ok(Self {
            socket,
            mode,
            encoder,
            pending: Vec::new(),
            seq: 0,
            frames_sent: AtomicU64::new(0),
        })
    }

    /// 喂入采集到的样本，凑够一帧就编一发。
    pub fn feed(&mut self, samples: &[f32], timestamp_ms: u32) {
        let frame_samples = self.encoder.frame_samples() * CHANNELS;
        self.pending.extend_from_slice(samples);

        while self.pending.len() >= frame_samples {
            let frame: Vec<f32> = self.pending.drain(..frame_samples).collect();
            match self.encoder.encode(&frame) {
                Ok(encoded) => {
                    let _ = self.socket.send(PacketKind::Audio, self.seq, timestamp_ms, &encoded);
                    self.seq = self.seq.wrapping_add(1);
                    self.frames_sent.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => log::warn!("编码失败：{e}"),
            }
        }
    }

    /// 切换模式。编码器必须重建，但序号继续，避免接收端把重配当成丢包。
    pub fn set_mode(&mut self, mode: QualityMode) -> anyhow::Result<()> {
        if mode == self.mode {
            return Ok(());
        }
        self.encoder = OpusEncoder::new(mode.params())?;
        self.mode = mode;
        self.pending.clear();
        log::info!("发送模式切换为 {}", mode.name());
        Ok(())
    }
}

/// 引擎整体状态，供 UI 查询。
#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub sending: bool,
    pub receiving: bool,
    pub source_count: usize,
    pub player_count: usize,
    pub outgoing_frames: u64,
    pub sample_rate: u32,
    pub mode: QualityMode,
    pub output_latency_ms: u32,
}

/// 引擎持有的所有运行时状态。
pub struct Engine {
    pub output: Arc<SharedOutput>,
    pub mixer: parking_lot::Mutex<Mixer>,
    pub incoming: parking_lot::Mutex<Vec<IncomingSource>>,
    pub outgoing: parking_lot::Mutex<Option<Outgoing>>,
    pub capture: parking_lot::Mutex<Option<CaptureSession>>,
    pub capture_ring: Arc<parking_lot::Mutex<SampleRing>>,
    pub running: AtomicBool,
    pub started_at: Instant,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            output: Arc::new(SharedOutput::new()),
            mixer: parking_lot::Mutex::new(Mixer::new()),
            incoming: parking_lot::Mutex::new(Vec::new()),
            outgoing: parking_lot::Mutex::new(None),
            capture: parking_lot::Mutex::new(None),
            capture_ring: Arc::new(parking_lot::Mutex::new(SampleRing::new(SAMPLE_RATE as usize))),
            running: AtomicBool::new(false),
            started_at: Instant::now(),
        }
    }

    /// 加入一路接收音源。超过软上限时返回 false。
    pub fn add_source(&self, source: IncomingSource) -> bool {
        let mut list = self.incoming.lock();
        if list.len() >= MAX_SOURCES {
            return false;
        }
        if list.iter().any(|s| s.stream_id == source.stream_id) {
            return false;
        }
        self.mixer.lock().add_source(source.stream_id, MAX_SOURCES);
        list.push(source);
        true
    }

    /// 移除一路接收音源。
    pub fn remove_source(&self, stream_id: u16) -> bool {
        self.mixer.lock().remove_source(stream_id);
        let mut list = self.incoming.lock();
        let before = list.len();
        list.retain(|s| s.stream_id != stream_id);
        list.len() != before
    }

    /// 混音一轮：把每路解码结果混进输出。
    pub fn mix_once(&self) -> usize {
        let mut list = self.incoming.lock();
        if list.is_empty() {
            return 0;
        }

        let frame_samples = list[0].mode.params().samples_per_frame() * CHANNELS;
        let mut decoded: Vec<(u16, Vec<f32>)> = Vec::with_capacity(list.len());

        for source in list.iter_mut() {
            let block = match source.next_frame() {
                Some(samples) => {
                    source.active.store(true, Ordering::Relaxed);
                    let mut buf = samples;
                    buf.resize(frame_samples, 0.0);
                    buf
                }
                None => {
                    // 这一路暂时没数据：按静音参与混音，绝不拖住其他路
                    source.active.store(false, Ordering::Relaxed);
                    vec![0.0f32; frame_samples]
                }
            };
            decoded.push((source.stream_id, block));
        }
        drop(list);

        let inputs: Vec<(u16, &[f32])> =
            decoded.iter().map(|(id, s)| (*id, s.as_slice())).collect();
        let mut out = vec![0.0f32; frame_samples];
        self.mixer.lock().mix(&inputs, &mut out);
        self.output.push(&out);
        frame_samples
    }

    pub fn status(&self) -> EngineStatus {
        let incoming = self.incoming.lock();
        let outgoing = self.outgoing.lock();
        EngineStatus {
            sending: outgoing.is_some(),
            receiving: !incoming.is_empty(),
            source_count: incoming.len(),
            player_count: outgoing.as_ref().map(|_| 1).unwrap_or(0),
            outgoing_frames: outgoing
                .as_ref()
                .map(|o| o.frames_sent.load(Ordering::Relaxed))
                .unwrap_or(0),
            sample_rate: SAMPLE_RATE,
            mode: outgoing.as_ref().map(|o| o.mode).unwrap_or_default(),
            output_latency_ms: incoming
                .iter()
                .map(|s| s.buffer_ms.load(Ordering::Relaxed))
                .max()
                .unwrap_or(0),
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// 周期性的维护任务：保活、清理超时的音源。
pub fn tick_maintenance(engine: &Engine) {
    if let Some(outgoing) = engine.outgoing.lock().as_ref() {
        outgoing.socket.maybe_keepalive();
    }

    // 超过 5 秒没有任何包的音源视为断开
    let mut list = engine.incoming.lock();
    let before = list.len();
    list.retain(|s| {
        let packets = s.packets.load(Ordering::Relaxed);
        packets > 0 || s.active.load(Ordering::Relaxed)
    });
    if list.len() != before {
        log::info!("清理了 {} 路已断开的音源", before - list.len());
    }
}

/// 发送线程的主循环体：从 ring 取数据喂给编码器。
pub fn pump_capture(engine: &Engine, timestamp_ms: u32) {
    let mut chunk = vec![0.0f32; 960 * CHANNELS];
    let mut taken = 0usize;
    {
        let mut ring = engine.capture_ring.lock();
        let n = ring.pop_into(&mut chunk);
        if n > 0 {
            taken = n;
        }
    }
    if taken == 0 {
        return;
    }
    if let Some(outgoing) = engine.outgoing.lock().as_mut() {
        outgoing.feed(&chunk[..taken], timestamp_ms);
    }
}

/// 标准维护间隔。
pub const TICK_INTERVAL: Duration = Duration::from_millis(5);
