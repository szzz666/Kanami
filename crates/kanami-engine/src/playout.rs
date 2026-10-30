//! 播放输出。
//!
//! 用 cpal 拉取本机输出设备的时钟作为**唯一时基**。所有音源的抖动缓冲、
//! 漂移校正都以它为准——这是多路混音能对齐的前提。
//!
//! 输出设备规格与内部 48kHz 立体声不一致时在这里重采样，
//! 不把这个问题甩给上层。实测本机默认输出是 44.1kHz 的蓝牙耳机，
//! 这条路径是必然会走到的。

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::engine::SharedOutput;

/// 一个可选的声音输出设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

/// 枚举本机所有可用的声音输出设备。
pub fn list_output_devices() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());

    let mut out = Vec::new();
    let Ok(devices) = host.output_devices() else {
        return out;
    };
    for device in devices {
        let Ok(name) = device.name() else { continue };
        let config = device.default_output_config().ok();
        let (sample_rate, channels) = match &config {
            Some(c) => (c.sample_rate().0, c.channels()),
            None => (48_000, 2),
        };
        out.push(OutputDeviceInfo {
            is_default: default_name.as_deref() == Some(name.as_str()),
            id: name.clone(),
            name,
            sample_rate,
            channels,
        });
    }
    out
}

/// 正在运行的播放器。
pub struct Player {
    /// 保持 stream 存活，drop 即停止。
    _stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("device_name", &self.device_name)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .finish()
    }
}

/// 打开一个输出设备并开始拉取。
///
/// 回调里从 shared 取混音结果；取不到就补静音，**绝不阻塞**——
/// 音频回调里阻塞会直接导致爆音和卡顿。
pub fn start_player(
    device_name: Option<&str>,
    shared: Arc<SharedOutput>,
) -> anyhow::Result<Player> {
    let host = cpal::default_host();

    let device = match device_name {
        Some(name) if !name.is_empty() => host
            .output_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| anyhow::anyhow!("找不到输出设备 {name}"))?,
        _ => host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("系统没有可用的声音输出设备"))?,
    };

    let name = device.name().unwrap_or_else(|_| "未知设备".into());
    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let format = supported.sample_format();

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    shared.set_output_format(sample_rate, channels);

    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, &shared),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, &shared),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, &shared),
        other => Err(anyhow::anyhow!("不支持的采样格式 {other:?}")),
    }?;

    stream.play()?;

    Ok(Player { _stream: stream, sample_rate, channels, device_name: name })
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<SharedOutput>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut pending: Vec<f32> = Vec::new();
    let mut write_pos = 0usize;
    let mut scratch = vec![0.0f32; 4096];

    let pull_shared = Arc::clone(shared);
    let data_shared = Arc::clone(shared);
    let err_shared = Arc::clone(shared);
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let needed = data.len();
            // 先把残留样本喂掉
            let mut filled = 0usize;
            if write_pos < pending.len() {
                let take = (pending.len() - write_pos).min(needed);
                for i in 0..take {
                    data[i] = T::from_sample(pending[write_pos + i]);
                }
                write_pos += take;
                filled = take;
                if write_pos >= pending.len() {
                    pending.clear();
                    write_pos = 0;
                }
            }

            // 不够就问共享缓冲要
            while filled < needed {
                if scratch.len() < 4096 {
                    scratch.resize(4096, 0.0);
                }
                let got = pull_shared.pull(&mut scratch);
                if got == 0 {
                    // 没有数据：补静音，不阻塞
                    for item in data.iter_mut().skip(filled) {
                        *item = T::from_sample(0.0f32);
                    }
                    break;
                }
                let take = got.min(needed - filled);
                for i in 0..take {
                    data[filled + i] = T::from_sample(scratch[i]);
                }
                filled += take;
                if take < got {
                    pending.extend_from_slice(&scratch[take..got]);
                    write_pos = 0;
                }
            }

            data_shared.note_callback(channels);
        },
        move |err| {
            log::error!("输出流错误：{err}");
            err_shared.mark_failed();
        },
        None,
    )?;
    Ok(stream)
}

/// 输出设备是否在正常出数。
#[derive(Debug, Default)]
pub struct OutputHealth {
    pub failed: AtomicBool,
    /// 累计回调次数。
    pub callbacks: AtomicU32,
}
