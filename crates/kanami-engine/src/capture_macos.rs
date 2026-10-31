//! macOS 系统声音采集。
//!
//! 实际采集由 src-tauri/capture/macos_capture.swift 编译出的辅助程序完成：
//! 它用 CoreAudio Process Tap（14.2+）抓整机输出，并**排除本进程**，
//! 断掉「本机同时收发 → 采到自己刚收到的流 → 再发回去」这条回授回路。
//!
//! 为什么用子进程而不是直接调 FFI：
//! - TCC 的「系统音频录制」权限按**责任进程**归属，子进程会继承
//!   Kanami.app 的授权，用户只需要给这一个应用授权一次；
//! - Swift 侧用系统自带 API 写起来比手写 objc2 绑定可靠得多。

#![forbid(unsafe_code)]

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};

/// macOS 采集后端。
pub struct MacCaptureBackend;

impl MacCaptureBackend {
    pub fn new() -> Self {
        Self
    }

    /// 查找采集助手可执行文件。
    ///
    /// 打包后它在 .app 的 Resources 里；开发时在 target 目录或源码树里。
    fn find_helper() -> Option<std::path::PathBuf> {
        let mut candidates = Vec::new();

        // 打包后的位置：<App>.app/Contents/MacOS/kanami → ../Resources/kanami-capture
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("../Resources/kanami-capture"));
                candidates.push(dir.join("kanami-capture"));
            }
        }
        // 构建产物里由 build.rs 编译出来的助手（target/*/build/kanami-*/out/kanami-capture）
        if let Ok(exe) = std::env::current_exe() {
            let mut dir = exe.parent().map(|p| p.to_path_buf());
            // 从 target/debug 往上找到 target
            while let Some(d) = dir.clone() {
                let build_dir = d.join("build");
                if build_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(&build_dir) {
                        for entry in entries.flatten() {
                            let candidate = entry.path().join("out/kanami-capture");
                            if candidate.exists() {
                                candidates.push(candidate);
                            }
                        }
                    }
                }
                dir = d.parent().map(|p| p.to_path_buf());
            }
        }
        // 开发时的位置
        candidates.push(std::path::PathBuf::from("src-tauri/capture/kanami-capture"));
        candidates.push(std::path::PathBuf::from("/tmp/kanami_capture"));

        candidates.into_iter().find(|p| p.exists())
    }
}

impl Default for MacCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureBackend for MacCaptureBackend {
    fn platform(&self) -> &'static str {
        "macOS"
    }

    fn list_sources(&self) -> Vec<CaptureSource> {
        let mut out = vec![CaptureSource {
            id: "system".into(),
            name: "系统声音（推荐）".into(),
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
        let stats_thread = Arc::clone(&stats);

        let helper = Self::find_helper();
        if helper.is_none() {
            log::warn!("未找到采集助手 kanami-capture，将以静音模式运行");
        }

        std::thread::Builder::new().name("kanami-macos-capture".into()).spawn(move || {
            let mut child: Option<Child> = match &helper {
                Some(path) => Command::new(path)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| log::error!("启动采集助手失败：{e}"))
                    .ok(),
                None => None,
            };

            let Some(ref mut proc) = child else {
                // 没有助手：保持静音节拍，让上层链路仍能跑通
                run_silent(&stop_flag, &stats_thread, &mut on_samples);
                return;
            };

            // 读 stderr 打印状态，方便用户看到「需要授权」这类提示
            if let Some(stderr) = proc.stderr.take() {
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                        log::info!("[采集] {line}");
                    }
                });
            }

            let Some(mut stdout) = proc.stdout.take() else {
                stats_thread.failed.store(true, Ordering::Relaxed);
                return;
            };

            // 看门狗：助手启动 6 秒还没有任何数据，说明很可能卡在授权上。
            // 不放任它静默静音——那样用户会以为是自己的设备坏了。
            let watchdog_stats = Arc::clone(&stats_thread);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(6));
                if watchdog_stats.frames.load(Ordering::Relaxed) == 0 {
                    log::warn!(
                        "macOS 采集 6 秒无数据：通常是缺少「音频录制」权限。请到 系统设置 → 隐私与安全性 → 麦克风 中允许 Kanami，再重开应用。"
                    );
                }
            });

            // 采集助手输出 f32le 48kHz 立体声交错
            let mut raw = vec![0u8; 16 * 1024];
            let mut carry: Vec<u8> = Vec::new();

            while !stop_flag.load(Ordering::Relaxed) {
                match stdout.read(&mut raw) {
                    Ok(0) => break,
                    Ok(n) => {
                        carry.extend_from_slice(&raw[..n]);

                        // 只按完整 f32 边界解析，避免半截样本
                        let usable = carry.len() / 4 * 4;
                        if usable == 0 {
                            continue;
                        }
                        let samples: Vec<f32> = carry[..usable]
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect();
                        carry.drain(..usable);

                        if !samples.is_empty() {
                            stats_thread.frames.fetch_add(1, Ordering::Relaxed);
                            on_samples(&samples);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        log::error!("读取采集数据失败：{e}");
                        stats_thread.failed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }

            let _ = proc.kill();
            let _ = proc.wait();
        })?;

        Ok(CaptureSession::new(source, stop, stats, Box::new(())))
    }

    fn supports_process_exclusion(&self) -> bool {
        true
    }
}

/// 没有采集助手时的降级：维持静音节拍，保证链路时序与生命周期正确。
fn run_silent(
    stop: &Arc<std::sync::atomic::AtomicBool>,
    stats: &Arc<CaptureStats>,
    on_samples: &mut CaptureCallback,
) {
    let period = std::time::Duration::from_millis(10);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(period);
        let block = vec![0.0f32; (SAMPLE_RATE as usize / 100) * CHANNELS];
        stats.frames.fetch_add(1, Ordering::Relaxed);
        on_samples(&block);
    }
}
