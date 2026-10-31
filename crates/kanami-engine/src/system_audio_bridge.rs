//! Android 系统声音采集的 Rust 入口。
//!
//! Kotlin 侧用 AudioPlaybackCapture（底层是 REMOTE_SUBMIX 路由）抓
//! 其它应用正在播放的声音，通过 JNI 把 PCM 送回来。
//!
//! 只在 Android 目标下编译。

#![cfg(target_os = "android")]
#![allow(unsafe_code)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 全局的样本接收器。Kotlin 线程调用 nativeOnSystemAudio 时用它。
type Sink = Box<dyn FnMut(Vec<f32>) + Send>;
static SINK: parking_lot::Mutex<Option<Sink>> = parking_lot::Mutex::new(None);

/// Kotlin 侧的采集线程通过这个 JNI 函数把样本送进来。
///
/// 名字必须严格匹配 Kotlin 里 external 声明的包名与方法名。
#[no_mangle]
pub extern "system" fn Java_com_szzz666_kanami_SystemAudioBridge_nativeOnSystemAudio(
    env: jni::JNIEnv,
    _this: jni::objects::JObject,
    samples: jni::objects::JFloatArray,
) {
    if samples.is_null() {
        return;
    }
    let Ok(len) = env.get_array_length(&samples) else { return };
    if len <= 0 {
        return;
    }
    let mut buf = vec![0.0f32; len as usize];
    if env.get_float_array_region(&samples, 0, &mut buf).is_err() {
        return;
    }
    if let Some(cb) = SINK.lock().as_mut() {
        cb(buf);
    }
}

fn with_env<F: FnOnce(&mut jni::JNIEnv) -> jni::errors::Result<()>>(f: F) {
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }) else {
        log::warn!("JavaVM 转换失败");
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        log::warn!("附加到 JVM 失败");
        return;
    };
    if let Err(e) = f(&mut env) {
        log::warn!("调用系统声音桥失败：{e}");
        if let Err(je) = env.exception_describe() {
            log::warn!("打印 Java 异常栈失败：{je}");
        }
        let _ = env.exception_clear();
    }
}

fn activity<'a>(env: &mut jni::JNIEnv<'a>) -> jni::errors::Result<jni::objects::JObject<'a>> {
    let ctx = ndk_context::android_context();
    let raw = ctx.context() as jni::sys::jobject;
    let global = unsafe { jni::objects::JObject::from_raw(raw) };
    env.new_local_ref(&global)
}

/// 这台设备是否支持系统声音采集。
pub fn is_supported() -> bool {
    let mut supported = false;
    with_env(|env| {
        let act = activity(env)?;
        let r = env.call_static_method(
            "com/szzz666/kanami/MainActivity",
            "isSystemAudioSupported",
            "(Landroid/content/Context;)Z",
            &[(&act).into()],
        )?;
        supported = r.z().unwrap_or(false);
        Ok(())
    });
    supported
}

/// 请求开始采集系统声音。会拉起 MediaProjection 授权框。
pub fn request_start(mut on_samples: impl FnMut(Vec<f32>) + Send + 'static) -> bool {
    *SINK.lock() = Some(Box::new(on_samples));
    let mut ok = false;
    with_env(|env| {
        env.call_static_method(
            "com/szzz666/kanami/MainActivity",
            "requestSystemAudioCapture",
            "()V",
            &[],
        )?;
        ok = true;
        Ok(())
    });
    if ok {
        log::info!("已请求系统声音采集授权");
    }
    ok
}

/// 停止采集并清掉回调。
pub fn stop() {
    *SINK.lock() = None;
    with_env(|env| {
        env.call_static_method(
            "com/szzz666/kanami/MainActivity",
            "stopSystemAudioCapture",
            "()V",
            &[],
        )?;
        Ok(())
    });
    log::info!("已停止系统声音采集");
}

/// 当前是否在采集。
pub fn is_capturing() -> bool {
    let mut r = false;
    with_env(|env| {
        let v = env.call_static_method(
            "com/szzz666/kanami/MainActivity",
            "isSystemAudioCapturing",
            "()Z",
            &[],
        )?;
        r = v.z().unwrap_or(false);
        Ok(())
    });
    r
}

/// 采集会话的停止标志。
pub fn stop_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
