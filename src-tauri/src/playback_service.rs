//! Android 前台服务的 Rust 入口。
//!
//! 服务本身是 Kotlin 写的（见 gen/android 里的 PlaybackService.kt），
//! 这里只负责通过 JNI 把它拉起来。不接这一步，锁屏后音频线程会被
//! Android 冻结，表现为「手机一锁屏就没声了」。
//!
//! 只在 Android 目标下编译，桌面构建完全不受影响。

#![cfg(target_os = "android")]
// JNI 必须用 unsafe：把 ndk-context 给的原始指针转成 JavaVM 没有安全抽象。
// 每处 unsafe 都注明了安全不变量。
#![allow(unsafe_code)]

/// 拿 JavaVM 并跑一段逻辑。失败只记日志，不向上传播。
///
/// 为什么不传播：前台服务起不来只是锁屏会断流，
/// 不应该让「连接音源」这个主功能跟着失败。
fn with_env<F: FnOnce(&mut jni::JNIEnv) -> jni::errors::Result<()>>(f: F) {
    let ctx = ndk_context::android_context();

    // SAFETY: vm() 返回的指针由 ndk-glue 在初始化时写入，
    // Android 只加载一次 .so，因此在整个进程生命周期内有效。
    let Ok(vm) = (unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }) else {
        log::warn!("JavaVM 转换失败");
        return;
    };

    let Ok(mut env) = vm.attach_current_thread() else {
        log::warn!("附加到 JVM 失败，前台服务未启动");
        return;
    };

    if let Err(e) = f(&mut env) {
        // 把 Java 侧的异常堆栈打出来。
        //
        // jni 的 Display 只给一句 "Java exception was thrown"，看不出原因。
        // 实际排查时这一句毫无用处——可能是缺权限、可能是服务类型不对、
        // 也可能是方法签名写错。有堆栈才能定位。
        log::warn!("调用前台服务失败：{e}");
        if let Err(je) = env.exception_describe() {
            log::warn!("打印 Java 异常栈失败：{je}");
        }
        let _ = env.exception_clear();
    }
}

/// 取 Activity 的 jobject 并转成本地引用。
///
/// SAFETY: context() 返回全局 Activity 引用，生命周期由 JVM 管理。
/// 这里只在调用期间借用，不保存、不释放。
fn activity<'a>(env: &mut jni::JNIEnv<'a>) -> jni::errors::Result<jni::objects::JObject<'a>> {
    let ctx = ndk_context::android_context();
    // context() 直接返回 jobject（*mut c_void 强转即可）。
    //
    // SAFETY: 该引用由 ndk-glue 在 JNI_OnLoad 后设置且不再变更，
    // 生命周期由 JVM 管理。这里只在调用期间借用，不保存也不释放。
    // 注意：jni::sys::jobject 本身就是 *mut _jobject，
    // 所以这里用 as 直接转，不能用 cast::<jobject>()（那样会多包一层）。
    let raw = ctx.context() as jni::sys::jobject;
    let global = unsafe { jni::objects::JObject::from_raw(raw) };
    env.new_local_ref(&global)
}

/// 拉起前台服务。
pub fn start(title: &str) {
    with_env(|env| {
        let act = activity(env)?;
        let jtitle = env.new_string(title)?;
        env.call_static_method(
            "com/szzz666/kanami/MainActivity",
            "startPlaybackService",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[(&act).into(), (&jtitle).into()],
        )?;
        log::info!("前台服务已启动：{title}");
        Ok(())
    });
}

/// 停止前台服务。
pub fn stop() {
    with_env(|env| {
        let act = activity(env)?;
        env.call_static_method(
            "com/szzz666/kanami/MainActivity",
            "stopPlaybackService",
            "(Landroid/content/Context;)V",
            &[(&act).into()],
        )?;
        log::info!("前台服务已停止");
        Ok(())
    });
}
