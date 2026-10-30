//! Linux 系统声音采集。
//!
//! 走 PulseAudio/PipeWire 的 monitor source：把某个 sink 的监听源当作输入。
//! 这是 Linux 上抓系统声音的标准做法，PipeWire 完全兼容 Pulse 接口。
//!
//! **回授问题**：与 Windows 不同，Linux 可以干净地解决——
//! 把本应用的输出挂到一个独立的 sink，采集时选另一个 sink 的 monitor。

#![forbid(unsafe_code)]

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};

/// Linux 采集后端。
pub struct LinuxCaptureBackend;

impl LinuxCaptureBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LinuxCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// 是否安装了 PulseAudio 或 PipeWire。
pub fn has_pulseaudio() -> bool {
    std::path::Path::new("/run/user").exists()
        && (std::process::Command::new("pactl")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false))
}

/// 列出 monitor source。
fn list_monitors() -> Vec<(String, String)> {
    let Ok(output) =
        std::process::Command::new("pactl").args(["list", "short", "sources"]).output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 2 && cols[1].contains(".monitor") {
                Some((cols[1].to_string(), cols[1].to_string()))
            } else {
                None
            }
        })
        .collect()
}

impl CaptureBackend for LinuxCaptureBackend {
    fn platform(&self) -> &'static str {
        "Linux"
    }

    fn list_sources(&self) -> Vec<CaptureSource> {
        let monitors = list_monitors();
        if monitors.is_empty() {
            return vec![CaptureSource {
                id: "system".into(),
                name: "系统声音（未检测到 PulseAudio/PipeWire）".into(),
                kind: CaptureKind::SystemOutput,
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS as u16,
                is_default: true,
            }];
        }

        monitors
            .into_iter()
            .enumerate()
            .map(|(i, (id, name))| CaptureSource {
                id: format!("monitor:{id}"),
                name: name.replace(".monitor", "（监听）"),
                kind: CaptureKind::OutputDevice,
                sample_rate: SAMPLE_RATE,
                channels: CHANNELS as u16,
                is_default: i == 0,
            })
            .collect()
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
        let monitor = source.id.trim_start_matches("monitor:").to_string();

        std::thread::Builder::new().name("kanami-linux-capture".into()).spawn(move || {
            // 用 parec 做实际采集：s16le 48kHz 立体声直出到 stdout
            let mut child = match std::process::Command::new("parec")
                .args([
                    "--device",
                    &monitor,
                    "--format=s16le",
                    "--rate=48000",
                    "--channels=2",
                    "--latency-msec=10",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    log::error!("parec 启动失败：{e}");
                    stats_thread.failed.store(true, Ordering::Relaxed);
                    return;
                }
            };

            use std::io::Read;
            let Some(mut stdout) = child.stdout.take() else {
                stats_thread.failed.store(true, Ordering::Relaxed);
                return;
            };

            let mut raw = vec![0u8; 4096];
            while !stop_flag.load(Ordering::Relaxed) {
                match stdout.read(&mut raw) {
                    Ok(0) => break,
                    Ok(n) => {
                        // s16le → f32
                        let samples: Vec<f32> = raw[..n]
                            .chunks_exact(2)
                            .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                            .collect();
                        if !samples.is_empty() {
                            stats_thread.frames.fetch_add(1, Ordering::Relaxed);
                            on_samples(&samples);
                        }
                    }
                    Err(e) => {
                        log::error!("parec 读取失败：{e}");
                        stats_thread.failed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            let _ = child.kill();
        })?;

        Ok(CaptureSession::new(source, stop, stats, Box::new(())))
    }

    fn supports_process_exclusion(&self) -> bool {
        // 通过独立 sink 实现等价效果
        has_pulseaudio()
    }
}
