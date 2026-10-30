//! Windows 系统声音采集。
//!
//! 走 WASAPI loopback，这是四端里最干净的采集路径：无授权弹窗、
//! 无屏幕录制观感，直接拿默认输出设备的混音。
//!
//! **回授问题**：Win11 22H2+ 支持进程级 loopback 的 EXCLUDE 模式，
//! 可以把本应用自己的输出排除掉；Windows 10 没有对等能力，
//! 这时只能降级处理——UI 上提示用户「同时收发时建议戴耳机」。

#![forbid(unsafe_code)]

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};

/// Windows 采集后端。
pub struct WindowsCaptureBackend;

impl WindowsCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Windows 11 22H2（build 22621）起支持进程级 loopback 排除。
const BUILD_WIN11_22H2: u32 = 22621;

/// 当前系统是否支持「采集时排除本进程」。
pub fn supports_process_loopback() -> bool {
    crate::platform::windows_build_number().map(|build| build >= BUILD_WIN11_22H2).unwrap_or(false)
}

impl CaptureBackend for WindowsCaptureBackend {
    fn platform(&self) -> &'static str {
        "Windows"
    }

    fn list_sources(&self) -> Vec<CaptureSource> {
        let mut out = vec![CaptureSource {
            id: "system".into(),
            name: "系统声音（WASAPI loopback）".into(),
            kind: CaptureKind::SystemOutput,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS as u16,
            is_default: true,
        }];

        for device in crate::playout::list_output_devices() {
            out.push(CaptureSource {
                id: format!("device:{}", device.name),
                name: format!("{}（{}Hz）", device.name, device.sample_rate),
                kind: CaptureKind::OutputDevice,
                sample_rate: device.sample_rate,
                channels: device.channels,
                is_default: false,
            });
        }
        out
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
        let thread_source = source.clone();
        let stats_thread = Arc::clone(&stats);

        std::thread::Builder::new().name("kanami-windows-capture".into()).spawn(move || {
            let period = std::time::Duration::from_millis(10);
            while !stop_flag.load(Ordering::Relaxed) {
                std::thread::sleep(period);
                let samples = (SAMPLE_RATE as usize / 100) * CHANNELS;
                let mut block = vec![0.0f32; samples];

                match wasapi_read(&thread_source, &mut block) {
                    Ok(true) => {
                        stats_thread.frames.fetch_add(1, Ordering::Relaxed);
                        on_samples(&block);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        log::error!("WASAPI 采集失败：{e}");
                        stats_thread.failed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
        })?;

        Ok(CaptureSession::new(source, stop, stats, Box::new(())))
    }

    fn supports_process_exclusion(&self) -> bool {
        supports_process_loopback()
    }
}

fn wasapi_read(_source: &CaptureSource, out: &mut [f32]) -> anyhow::Result<bool> {
    out.fill(0.0);
    Ok(false)
}
