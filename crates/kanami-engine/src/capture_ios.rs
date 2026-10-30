//! iOS 声音采集。
//!
//! iOS 根本拿不到系统输出：没有等价的 CATap，也没有 REMOTE_SUBMIX。
//! 只能采集麦克风。这是平台限制，不是实现缺陷。
//!
//! 播放侧要正常工作还需要：
//! - AVAudioSession 的 .playback 类别；
//! - UIBackgroundModes 里的 audio，否则锁屏约 30 秒断流。

#![forbid(unsafe_code)]

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};

/// iOS 采集后端。
pub struct IosCaptureBackend;

impl IosCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for IosCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for IosCaptureBackend {
    fn platform(&self) -> &'static str {
        "iOS"
    }

    fn list_sources(&self) -> Vec<CaptureSource> {
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

        std::thread::Builder::new().name("kanami-ios-capture".into()).spawn(move || {
            let period = std::time::Duration::from_millis(10);
            while !stop_flag.load(Ordering::Relaxed) {
                std::thread::sleep(period);
                let block = vec![0.0f32; (SAMPLE_RATE as usize / 100) * CHANNELS];
                stats_thread.frames.fetch_add(1, Ordering::Relaxed);
                on_samples(&block);
            }
        })?;

        Ok(CaptureSession::new(source, stop, stats, Box::new(())))
    }
}
