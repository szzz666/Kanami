//! Windows 系统声音采集。
//!
//! 走 WASAPI loopback，这是四端里最干净的采集路径：无授权弹窗、
//! 无屏幕录制观感，直接拿默认输出设备的混音。
//!
//! # 为什么能用纯安全 Rust 实现
//!
//! cpal 的 WASAPI 后端有个关键特性（见其 `host/wasapi/mod.rs`）：
//!
//! > If you use a WASAPI output device as an input device it will
//! > transparently enable loopback mode.
//!
//! 也就是说「把一个输出设备当输入设备打开」就等于 loopback 采集。
//! 不需要自己写 COM、不需要 unsafe、不需要引 windows crate 的音频接口。
//!
//! # 回授问题
//!
//! Win11 22H2+ 支持进程级 loopback 的 EXCLUDE 模式，可以把本应用
//! 自己的输出排除掉；Windows 10 没有对等能力，这时只能降级处理——
//! UI 上提示用户「同时收发时建议戴耳机」。

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};
use crate::sample_conv::{to_mono, LinearResampler};

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
        on_samples: CaptureCallback,
        stats: Arc<CaptureStats>,
    ) -> anyhow::Result<CaptureSession> {
        let stop = Arc::new(AtomicBool::new(false));

        // 和移动端一样：cpal 的 Stream 是 !Send，必须活在创建它的线程里。
        let ready = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(parking_lot::Mutex::new(None::<String>));

        let stop_thread = Arc::clone(&stop);
        let ready_thread = Arc::clone(&ready);
        let failed_thread = Arc::clone(&failed);
        let stats_thread = Arc::clone(&stats);
        let source_thread = source.clone();

        std::thread::Builder::new().name("kanami-windows-capture".into()).spawn(move || {
            match run_loopback_stream(
                &source_thread,
                on_samples,
                Arc::clone(&stats_thread),
                &stop_thread,
            ) {
                Ok(stream) => {
                    ready_thread.store(true, Ordering::Relaxed);
                    while !stop_thread.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    stats_thread.failed.store(true, Ordering::Relaxed);
                    *failed_thread.lock() = Some(e.to_string());
                    ready_thread.store(true, Ordering::Relaxed);
                }
            }
        })?;

        // 等采集真正起来（或确定失败）再返回。
        // 不然 start_sending 会立刻报成功，实际上一帧都没采到。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !ready.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Some(err) = failed.lock().clone() {
            return Err(anyhow::anyhow!("WASAPI loopback 打开失败：{err}"));
        }

        Ok(CaptureSession::new(source.clone(), stop, stats, Box::new(())))
    }

    fn supports_process_exclusion(&self) -> bool {
        supports_process_loopback()
    }
}

/// 打开 WASAPI loopback 流并把样本转成项目统一的 f32 双声道。
///
/// 关键点：**用 build_input_stream 打开一个输出设备**，
/// cpal 会自动切到 loopback 模式。
fn run_loopback_stream(
    source: &CaptureSource,
    mut on_samples: CaptureCallback,
    stats: Arc<CaptureStats>,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();

    // 选设备：指定了具体设备就用它，否则用默认输出设备
    let device = if let Some(name) = source.id.strip_prefix("device:") {
        host.output_devices()
            .map_err(|e| anyhow::anyhow!("枚举输出设备失败：{e}"))?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow::anyhow!("找不到输出设备：{name}"))?
    } else {
        host.default_output_device().ok_or_else(|| anyhow::anyhow!("没有可用的输出设备"))?
    };

    // 注意：这里查的是 **output** config。
    // 因为设备本身是输出设备，loopback 用的是它的混音格式。
    let supported =
        device.default_output_config().map_err(|e| anyhow::anyhow!("查询输出配置失败：{e}"))?;
    let in_rate = supported.sample_rate().0;
    let in_channels = supported.channels() as usize;
    let sample_format = supported.sample_format();

    let config = cpal::StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    // 设备采样率通常不是 48k，重采样到项目统一频率，
    // 否则音高会整体偏移（听起来像慢放/快放）。
    let mut resampler = LinearResampler::new(in_rate, SAMPLE_RATE);

    let stats_cb = Arc::clone(&stats);
    let stop_cb = Arc::clone(stop);
    let err_stats = Arc::clone(&stats);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if stop_cb.load(Ordering::Relaxed) {
                    return;
                }
                let mono = to_mono(data, in_channels);
                let out = resampler.process(&mono);
                stats_cb.frames.fetch_add(1, Ordering::Relaxed);
                on_samples(&out);
            },
            move |e| {
                log::error!("WASAPI loopback 流错误：{e}");
                err_stats.failed.store(true, Ordering::Relaxed);
            },
            None,
        )?,
        cpal::SampleFormat::I16 => {
            let mut resampler = LinearResampler::new(in_rate, SAMPLE_RATE);
            let stats_cb = Arc::clone(&stats);
            let stop_cb = Arc::clone(stop);
            let err_stats = Arc::clone(&stats);
            device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if stop_cb.load(Ordering::Relaxed) {
                        return;
                    }
                    let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                    let mono = to_mono(&f, in_channels);
                    let out = resampler.process(&mono);
                    stats_cb.frames.fetch_add(1, Ordering::Relaxed);
                    on_samples(&out);
                },
                move |e| {
                    log::error!("WASAPI loopback 流错误：{e}");
                    err_stats.failed.store(true, Ordering::Relaxed);
                },
                None,
            )?
        }
        other => return Err(anyhow::anyhow!("不支持的采样格式：{other:?}")),
    };

    stream.play().map_err(|e| anyhow::anyhow!("启动 loopback 流失败：{e}"))?;
    log::info!(
        "WASAPI loopback 已启动：{} {}Hz {}ch",
        device.name().unwrap_or_default(),
        in_rate,
        in_channels
    );
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 降混把立体声取平均() {
        let data = [1.0f32, 0.0, 0.5, 0.5];
        let mono = to_mono(&data, 2);
        assert_eq!(mono, vec![0.5, 0.5]);
    }

    #[test]
    fn 同采样率时只做声道复制() {
        let mut r = LinearResampler::new(48000, 48000);
        let out = r.process(&[0.1, 0.2]);
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn 重采样保持时长大致不变() {
        let mut r = LinearResampler::new(44100, 48000);
        let out = r.process(&vec![0.5f32; 4410]);
        let frames = out.len() / CHANNELS;
        assert!((frames as i64 - 4800).abs() < 100, "实际 {frames} 帧");
    }

    #[test]
    fn 空输入不会崩() {
        let mut r = LinearResampler::new(44100, 48000);
        assert!(r.process(&[]).is_empty());
    }
}
