//! Kanami 引擎层。
//!
//! 把采集、编解码、传输、混音、播放拼成完整链路。
//! 这一层不依赖 Tauri，可以脱离 UI 单独跑与测试。

#![deny(unsafe_code)]

pub mod capture;
pub mod capture_linux;
pub mod capture_macos;
pub mod capture_mobile;
pub mod capture_windows;
pub mod engine;
pub mod platform;
pub mod playout;
#[cfg(target_os = "android")]
pub mod system_audio_bridge;

pub use capture::{CaptureBackend, CaptureKind, CaptureSource};
pub use engine::{Engine, EngineStatus, IncomingSource, Outgoing, SharedOutput};
pub use platform::{caps, PlatformCaps};
