//! 移动端（Android / iOS）声音采集。
//!
//! 两条路，能力差一个量级：
//!
//! - **麦克风（RECORD_AUDIO）**：普通运行时权限，直接可用。
//! - **系统声音（Android: MediaProjection + AudioPlaybackCapture；
//!   iOS: 完全没有等价能力）**：Android 侧需要 REMOTE_SUBMIX 虚拟输入路径
//!   + 每次投屏授权 + 前台服务；iOS 侧平台根本不开放。
//!
//! 因此这里只实现麦克风，并在能力探测时如实报告「系统声音不可用」。
//!
//! # 为什么用 cpal 而不是各自的系统 API
//!
//! cpal 0.16 在 Android 上走 **AAudio**（API 26+），在 iOS 上走
//! **CoreAudio / AVAudioEngine**。两者都是系统推荐的实时音频路径，
//! 延迟比 Java 层的 AudioRecord 更低，而且不用写 JNI 胶水。
//!
//! # 曾经的实现是假的
//!
//! 这里一开始只按 10ms 节拍推**静音**块——时序和生命周期都是对的，
//! 界面上「正在发送」也显示得出来，唯独没有真实声音。
//! 这是最难被发现的一类缺陷：所有日志正常，音量表却永远是 0。

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kanami_audio::{CHANNELS, SAMPLE_RATE};

use crate::capture::{
    CaptureBackend, CaptureCallback, CaptureKind, CaptureSession, CaptureSource, CaptureStats,
};

/// 移动端采集后端。
pub struct MobileCaptureBackend {
    platform: &'static str,
}

impl MobileCaptureBackend {
    pub fn new(platform: &'static str) -> Self {
        Self { platform }
    }
}

impl CaptureBackend for MobileCaptureBackend {
    fn platform(&self) -> &'static str {
        self.platform
    }

    fn list_sources(&self) -> Vec<CaptureSource> {
        let mut out = vec![CaptureSource {
            id: "mic".into(),
            name: "麦克风".into(),
            kind: CaptureKind::Microphone,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS as u16,
            is_default: true,
        }];

        // 系统声音只有 Android 能给。
        //
        // 之前这里写死「多数定制 ROM 没有 REMOTE_SUBMIX，不列出来」——
        // 那个结论来自模拟器实测，在真机上不成立：
        // 零售机的 remote_submix 混音路径基本都在，AudioPlaybackCapture
        // 走的就是它。所以按平台能力判断，而不是按机型猜。
        #[cfg(target_os = "android")]
        {
            if system_audio_supported() {
                out.push(CaptureSource {
                    id: "system".into(),
                    name: "系统声音".into(),
                    kind: CaptureKind::SystemOutput,
                    sample_rate: SAMPLE_RATE,
                    channels: CHANNELS as u16,
                    is_default: false,
                });
            }
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

        // 系统声音走另一条路径：AudioPlaybackCapture 由 Kotlin 侧驱动，
        // 样本通过 JNI 送回来，不需要 cpal。
        #[cfg(target_os = "android")]
        if source.kind == CaptureKind::SystemOutput {
            return start_system_audio(source, on_samples, stats, stop);
        }

        // cpal 的 Stream 是 !Send，必须活在创建它的线程里。
        // 这里开一个专用线程持有它，stop 置位时线程退出、Stream 随之 drop。
        let ready = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(parking_lot::Mutex::new(None::<String>));

        let stop_thread = Arc::clone(&stop);
        let ready_thread = Arc::clone(&ready);
        let failed_thread = Arc::clone(&failed);
        let stats_thread = Arc::clone(&stats);

        std::thread::Builder::new().name("kanami-mobile-capture".into()).spawn(move || {
            match run_input_stream(on_samples, Arc::clone(&stats_thread), &stop_thread) {
                Ok(stream) => {
                    ready_thread.store(true, Ordering::Relaxed);
                    // 持有 stream 直到收到停止信号
                    while !stop_thread.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    // 失败要显式记录下来：上层据此把状态标成「采集失败」，
                    // 而不是让界面一直显示「正在发送」却没有任何声音。
                    stats_thread.failed.store(true, Ordering::Relaxed);
                    *failed_thread.lock() = Some(e.to_string());
                    ready_thread.store(true, Ordering::Relaxed);
                }
            }
        })?;

        // 等采集真正起来（或确定失败），再返回。
        // 不然 start_sending 会立刻报成功，而实际上麦克风可能根本没打开。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !ready.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Some(err) = failed.lock().clone() {
            return Err(anyhow::anyhow!("麦克风打开失败：{err}"));
        }

        Ok(CaptureSession::new(source.clone(), stop, stats, Box::new(())))
    }

    fn supports_process_exclusion(&self) -> bool {
        false
    }
}

/// 建一条输入流并把采到的样本转成项目统一的 f32 双声道。
fn run_input_stream(
    mut on_samples: CaptureCallback,
    stats: Arc<CaptureStats>,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device =
        host.default_input_device().ok_or_else(|| anyhow::anyhow!("没有可用的输入设备"))?;

    let supported =
        device.default_input_config().map_err(|e| anyhow::anyhow!("查询输入配置失败：{e}"))?;
    let in_rate = supported.sample_rate().0;
    let in_channels = supported.channels() as usize;
    let sample_format = supported.sample_format();

    let config = cpal::StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    // 设备采样率通常不是 48k（手机常见 44.1k/48k，iOS 常 48k）。
    // 这里做线性插值重采样到项目统一的 SAMPLE_RATE，
    // 否则音高会整体偏移——听起来像「慢放」或「快放」。
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
                log::error!("输入流错误：{e}");
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
                    log::error!("输入流错误：{e}");
                    err_stats.failed.store(true, Ordering::Relaxed);
                },
                None,
            )?
        }
        other => return Err(anyhow::anyhow!("不支持的输入采样格式：{other:?}")),
    };

    stream.play().map_err(|e| anyhow::anyhow!("启动输入流失败：{e}"))?;
    Ok(stream)
}

/// 多声道取平均降成单声道，再复制到双声道（项目内部统一双声道）。
fn to_mono<T: Copy + Into<f32>>(data: &[T], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let frames = data.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let mut sum = 0.0f32;
        for c in 0..channels {
            sum += data[i * channels + c].into();
        }
        out.push(sum / channels as f32);
    }
    out
}

/// 极简线性插值重采样。
///
/// 采集侧不需要高阶滤波：Opus 编码前的抗混叠由编码器自己处理，
/// 而这里只是把设备时钟换算到项目统一的 48k。用线性插值足够，
/// 且没有额外延迟（rubato 的多相滤波器会引入几十个样本的群延迟）。
struct LinearResampler {
    from: u32,
    to: u32,
    /// 上一块最后一个样本，用于跨块连续插值。
    last: f32,
    /// 当前在小数样本上的位置（0..1）。
    frac: f32,
    primed: bool,
}

impl LinearResampler {
    fn new(from: u32, to: u32) -> Self {
        Self { from, to, last: 0.0, frac: 0.0, primed: false }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.from == self.to {
            return duplicate_to_stereo(input);
        }
        if input.is_empty() {
            return Vec::new();
        }

        let ratio = self.from as f32 / self.to as f32;
        let mut mono_out = Vec::with_capacity((input.len() as f32 / ratio) as usize + 2);

        if !self.primed {
            self.last = input[0];
            self.primed = true;
        }

        let mut pos = self.frac;
        while pos < input.len() as f32 {
            let i = pos.floor() as usize;
            let t = pos - i as f32;
            let a = if i == 0 { self.last } else { input[i - 1] };
            let b = input.get(i).copied().unwrap_or(a);
            mono_out.push(a + (b - a) * t);
            pos += ratio;
        }
        self.frac = pos - input.len() as f32;
        self.last = *input.last().unwrap();

        duplicate_to_stereo(&mono_out)
    }
}

fn duplicate_to_stereo(mono: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * CHANNELS);
    for &s in mono {
        for _ in 0..CHANNELS {
            out.push(s);
        }
    }
    out
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
        assert_eq!(out.len(), 4, "两帧单声道 → 两帧双声道");
        assert!((out[0] - 0.1).abs() < 1e-6);
        assert!((out[3] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn 重采样保持时长大致不变() {
        let mut r = LinearResampler::new(44100, 48000);
        let input = vec![0.5f32; 4410];
        let out = r.process(&input);
        let frames = out.len() / CHANNELS;
        // 4410 个 44.1k 样本 ≈ 4800 个 48k 样本
        assert!((frames as i64 - 4800).abs() < 100, "实际 {frames} 帧");
    }

    #[test]
    fn 重采样保持幅度() {
        let mut r = LinearResampler::new(44100, 48000);
        let out = r.process(&vec![0.5f32; 4410]);
        for &s in &out {
            assert!((s - 0.5).abs() < 1e-5, "直流信号重采样后仍应是 0.5，实际 {s}");
        }
    }

    #[test]
    fn 空输入不会崩() {
        let mut r = LinearResampler::new(44100, 48000);
        assert!(r.process(&[]).is_empty());
    }
}

/// 这台设备是否支持采集系统声音。
///
/// 桌面端返回 false：系统声音在 PC 上走的是另一套（WASAPI loopback /
/// PulseAudio monitor），由各自的 capture 后端实现，不经过这里。
pub fn system_audio_supported() -> bool {
    #[cfg(target_os = "android")]
    {
        crate::system_audio_bridge::is_supported()
    }
    #[cfg(not(target_os = "android"))]
    {
        false
    }
}

/// 启动系统声音采集（仅 Android）。
///
/// Kotlin 侧会弹出 MediaProjection 授权框。用户点允许后
/// 采集才开始，样本通过 JNI 回调送进来。
#[cfg(target_os = "android")]
fn start_system_audio(
    source: &CaptureSource,
    mut on_samples: CaptureCallback,
    stats: Arc<CaptureStats>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<CaptureSession> {
    if !crate::system_audio_bridge::is_supported() {
        return Err(anyhow::anyhow!("这台设备不支持系统声音采集"));
    }

    let stats_cb = Arc::clone(&stats);
    let requested = crate::system_audio_bridge::request_start(move |samples: Vec<f32>| {
        stats_cb.frames.fetch_add(1, Ordering::Relaxed);
        on_samples(&samples);
    });

    if !requested {
        return Err(anyhow::anyhow!("无法请求系统声音采集授权"));
    }

    // 等采集真正跑起来再返回，而不是「弹了授权框就算成功」。
    //
    // 为什么：request_start 只是拉起授权对话框。用户可能拒绝，
    // 也可能一直不点。这时若直接返回 Ok，界面会显示
    // 「正在发送系统声音」而实际一个字节都没采——用户只会觉得坏了。
    //
    // 判据用采集状态而不是样本计数：设备上没声音在播时，
    // 采集是正常运行的但样本峰值恒为 0，拿样本数判断会误报超时。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            crate::system_audio_bridge::stop();
            return Err(anyhow::anyhow!("已取消"));
        }
        if crate::system_audio_bridge::is_capturing() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    if !ready {
        crate::system_audio_bridge::stop();
        return Err(anyhow::anyhow!("系统声音采集未启动：需要在弹出的对话框中允许录制"));
    }

    log::info!("系统声音采集已就绪，开始传输");
    Ok(CaptureSession::new(source.clone(), stop, stats, Box::new(())))
}
