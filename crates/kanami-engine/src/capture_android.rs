//! Android 声音采集。
//!
//! 两条路，能力差一个量级：
//!
//! - **麦克风（RECORD_AUDIO）**：普通运行时权限，直接可用。
//! - **系统声音（MediaProjection + AudioPlaybackCapture）**：需要 REMOTE_SUBMIX
//!   虚拟输入路径 + 每次投屏授权 + 前台服务。
//!
//! 实测目标设备（BlueStacks / Android 13）上 r_submix 未加载、
//! HAL 库全盘不存在、AudioFlinger 无 record 线程——系统声音采集物理上不可行。
//! 因此默认走麦克风，并在能力探测时如实报告。

#![forbid(unsafe_code)]

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};

/// Android 采集后端。
pub struct AndroidCaptureBackend;

impl AndroidCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AndroidCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for AndroidCaptureBackend {
    fn platform(&self) -> &'static str {
        "Android"
    }

    fn list_sources(&self) -> Vec<CaptureSource> {
        // 系统声音采集在缺少 REMOTE_SUBMIX 的设备上不可用，
        // 不把它列出来，免得用户选了之后莫名其妙没有声音。
        vec![CaptureSource {
            id: "mic".into(),
            name: "麦克风".into(),
            kind: CaptureKind::Microphone,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS as u16,
            is_default: true,
        }]
    }

    fn start(
        &self,
        source: &CaptureSource,
        mut on_samples: CaptureCallback,
        stats: Arc<CaptureStats>,
    ) -> anyhow::Result<CaptureSession> {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let source = source.clone();
        let stats_thread = Arc::clone(&stats);

        std::thread::Builder::new().name("kanami-android-capture".into()).spawn(move || {
            // AudioRecord 的读取由 JNI 层驱动；这里维护节拍与生命周期。
            let period = std::time::Duration::from_millis(10);
            while !stop_flag.load(Ordering::Relaxed) {
                std::thread::sleep(period);
                if stats_thread.failed.load(Ordering::Relaxed) {
                    break;
                }
                // 未接入 JNI 前保持静音，保证时序与生命周期正确
                let block = vec![0.0f32; (SAMPLE_RATE as usize / 100) * CHANNELS];
                stats_thread.frames.fetch_add(1, Ordering::Relaxed);
                on_samples(&block);
            }
        })?;

        Ok(CaptureSession::new(source, stop, stats, Box::new(())))
    }

    fn supports_process_exclusion(&self) -> bool {
        false
    }
}
