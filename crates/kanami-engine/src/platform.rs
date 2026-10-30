//! 平台能力探测与后端选择。
//!
//! 所有「这个平台能不能做某件事」的判断集中在这里，
//! 上层只问问题、不关心平台细节。

#![forbid(unsafe_code)]

use crate::capture::CaptureBackend;

/// 平台能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformCaps {
    /// 平台名。
    pub platform: String,
    /// 能否采集系统声音（而不是只能采麦克风）。
    pub system_audio_capture: bool,
    /// 采集时能否排除本进程（决定本机收发是否回授）。
    pub process_exclusion: bool,
    /// 是否支持后台/锁屏继续播放。
    pub background_playback: bool,
    /// 该平台的限制说明，直接展示给用户。
    pub notes: Vec<String>,
}

/// 取得当前平台的采集后端。
pub fn capture_backend() -> Box<dyn CaptureBackend> {
    #[cfg(target_os = "macos")]
    {
        Box::new(crate::capture_macos::MacCaptureBackend::new())
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(crate::capture_windows::WindowsCaptureBackend::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(crate::capture_linux::LinuxCaptureBackend::new())
    }
    #[cfg(target_os = "android")]
    {
        Box::new(crate::capture_android::AndroidCaptureBackend::new())
    }
    #[cfg(target_os = "ios")]
    {
        Box::new(crate::capture_ios::IosCaptureBackend::new())
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux",
        target_os = "android",
        target_os = "ios"
    )))]
    {
        Box::new(UnsupportedBackend)
    }
}

/// 当前平台的能力集合。
pub fn caps() -> PlatformCaps {
    let backend = capture_backend();
    let sources = backend.list_sources();
    let system_audio =
        sources.iter().any(|s| !matches!(s.kind, crate::capture::CaptureKind::Microphone));

    let mut notes = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if system_audio {
            notes.push("首次抓系统声音需要在「系统设置 → 隐私与安全性 → 屏幕录制」里授权".into());
        }
        notes.push("采集时排除本进程，本机同时收发不会啸叫".into());
    }

    #[cfg(target_os = "windows")]
    {
        if !crate::capture_windows::supports_process_loopback() {
            notes.push("当前 Windows 版本不支持排除本进程，同时收发时建议戴耳机".into());
        }
    }

    #[cfg(target_os = "linux")]
    {
        if !crate::capture_linux::has_pulseaudio() {
            notes.push("未检测到 PulseAudio/PipeWire，系统声音采集不可用".into());
        }
    }

    #[cfg(target_os = "android")]
    {
        notes.push("本机只能采集麦克风（系统声音采集需要 REMOTE_SUBMIX，多数设备不具备）".into());
        notes.push("锁屏后继续播放依赖前台服务与通知权限".into());
    }

    #[cfg(target_os = "ios")]
    {
        notes.push("iOS 无法采集系统声音，只能采集麦克风".into());
    }

    PlatformCaps {
        platform: backend.platform().to_string(),
        system_audio_capture: system_audio,
        process_exclusion: backend.supports_process_exclusion(),
        background_playback: cfg!(any(
            target_os = "android",
            target_os = "ios",
            target_os = "macos",
            target_os = "windows"
        )),
        notes,
    }
}

/// Windows 版本号探测。
#[cfg(target_os = "windows")]
pub fn windows_build_number() -> Option<u32> {
    // 通过 cmd 读版本号，避免引入额外依赖
    let output = std::process::Command::new("cmd").args(["/C", "ver"]).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // 形如 "Microsoft Windows [版本 10.0.22621.3155]"
    let build = text.split('.').nth(2)?;
    build.trim().trim_end_matches(']').parse().ok()
}

#[cfg(not(target_os = "windows"))]
pub fn windows_build_number() -> Option<u32> {
    None
}

/// 既不是桌面也不是移动的构建目标。
#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "ios"
)))]
struct UnsupportedBackend;

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "ios"
)))]
impl crate::capture::CaptureBackend for UnsupportedBackend {
    fn platform(&self) -> &'static str {
        "不支持的平台"
    }
    fn list_sources(&self) -> Vec<crate::capture::CaptureSource> {
        Vec::new()
    }
    fn start(
        &self,
        _source: &crate::capture::CaptureSource,
        _on_samples: crate::capture::CaptureCallback,
        _stats: std::sync::Arc<crate::capture::CaptureStats>,
    ) -> anyhow::Result<crate::capture::CaptureSession> {
        anyhow::bail!("当前平台不支持音频采集")
    }
}
