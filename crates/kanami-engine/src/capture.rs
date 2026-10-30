//! 系统声音采集的平台抽象。
//!
//! 四条实现路径，能力并不对等：
//!
//! - **macOS**：CoreAudio Process Tap（14.2+），可以排除本进程，
//!   用户看到的是「录制系统音频」而不是「录屏」。低版本退回 ScreenCaptureKit。
//! - **Windows**：WASAPI loopback，最干净。Win11 22H2+ 支持进程级排除，
//!   Win10 没有对等能力，只能提示用户戴耳机。
//! - **Linux**：PulseAudio/PipeWire 的 monitor source。
//! - **Android**：AudioPlaybackCapture 需要 REMOTE_SUBMIX，
//!   实测目标设备缺这个 HAL，退回麦克风采集。
//!
//! 统一接口只有两个：列出可用音源、按音源开始采集。

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

/// 一个可采集的音源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSource {
    /// 稳定标识，用于持久化用户选择。
    pub id: String,
    pub name: String,
    pub kind: CaptureKind,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_default: bool,
}

/// 音源种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    /// 整个系统的输出混音。
    SystemOutput,
    /// 指定输出设备的监听。
    OutputDevice,
    /// 麦克风。
    Microphone,
}

impl CaptureKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemOutput => "系统声音",
            Self::OutputDevice => "指定输出设备",
            Self::Microphone => "麦克风",
        }
    }
}

/// 采集回调：拿到交错立体声 f32 样本。
pub type CaptureCallback = Box<dyn FnMut(&[f32]) + Send + 'static>;

/// 采集统计。
#[derive(Debug, Default)]
pub struct CaptureStats {
    pub frames: AtomicU64,
    /// 因为缓冲满而丢弃的样本数。
    pub dropped: AtomicU64,
    /// 采集是否出错。
    pub failed: AtomicBool,
}

/// 正在运行的采集会话。
pub struct CaptureSession {
    pub source: CaptureSource,
    /// 停止标志。各平台的实现自己检查它。
    pub stop: Arc<AtomicBool>,
    pub stats: Arc<CaptureStats>,
    /// 平台资源的持有者。drop 即释放。
    _handle: Box<dyn Send + 'static>,
}

impl std::fmt::Debug for CaptureSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSession").field("source", &self.source).finish()
    }
}

impl CaptureSession {
    pub fn new(
        source: CaptureSource,
        stop: Arc<AtomicBool>,
        stats: Arc<CaptureStats>,
        handle: Box<dyn Send + 'static>,
    ) -> Self {
        Self { source, stop, stats, _handle: handle }
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed) && !self.stats.failed.load(Ordering::Relaxed)
    }

    /// 请求停止。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 采集器工厂。各平台实现这个 trait。
pub trait CaptureBackend: Send + Sync {
    /// 平台标识，用于 UI 显示与诊断。
    fn platform(&self) -> &'static str;

    /// 列出可用音源。
    fn list_sources(&self) -> Vec<CaptureSource>;

    /// 按音源开始采集。
    ///
    /// 实现必须尽快返回，真正的采集在后台线程里跑。
    fn start(
        &self,
        source: &CaptureSource,
        on_samples: CaptureCallback,
        stats: Arc<CaptureStats>,
    ) -> anyhow::Result<CaptureSession>;

    /// 该平台是否支持「采集时排除指定进程」。
    ///
    /// 这直接决定「本机同时收发会不会回授」：
    /// 不支持时必须提示用户。
    fn supports_process_exclusion(&self) -> bool {
        false
    }
}

/// 环形样本缓冲，连接采集线程与网络发送线程。
///
/// 满了就丢最旧的——实时音频里，迟到的数据没有价值，
/// 保住「最新」比保住「完整」重要。
#[derive(Debug)]
pub struct SampleRing {
    buf: Mutex<Vec<f32>>,
    capacity: usize,
    write_pos: usize,
    len: usize,
    dropped: AtomicU64,
}

impl SampleRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: Mutex::new(vec![0.0; capacity]),
            capacity,
            write_pos: 0,
            len: 0,
            dropped: AtomicU64::new(0),
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        let capacity = self.capacity;
        let mut buf = self.buf.lock();
        for &sample in samples {
            if self.len == capacity {
                // 满了丢最旧的
                self.write_pos = (self.write_pos + 1) % capacity;
                self.len -= 1;
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            let pos = (self.write_pos + self.len) % capacity;
            buf[pos] = sample;
            self.len += 1;
        }
    }

    /// 取出最多 out.len() 个样本，返回实际取到的数量。
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let capacity = self.capacity;
        let buf = self.buf.lock();
        let take = self.len.min(out.len());
        let read_pos = (self.write_pos + capacity - self.len) % capacity;
        for i in 0..take {
            out[i] = buf[(read_pos + i) % capacity];
        }
        self.len -= take;
        take
    }

    pub fn available(&self) -> usize {
        self.len
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}
