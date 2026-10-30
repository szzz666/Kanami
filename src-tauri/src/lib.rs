//! Tauri 外壳。
//!
//! 这一层只做三件事：把引擎包成可被前端调用的命令、
//! 把引擎状态推给前端、管理窗口与托盘。
//! 所有音频逻辑都在 kanami-engine，这里不写业务。

// 全局禁止 unsafe，但用 deny 而不是 forbid：
// forbid 无法被子模块覆盖，而 Android 的 JNI 桥必须用 unsafe
// （把原始指针转成 JavaVM/Context 没有安全抽象）。
// deny 允许 playback_service 模块单独 allow，且每处都写了安全不变量。
#![deny(unsafe_code)]

use std::time::Duration;

use tauri::Manager;

mod commands;
#[cfg(target_os = "android")]
mod playback_service;
mod state;
#[cfg(desktop)]
mod tray;

pub use state::AppState;

/// 发送泵的转发。state 模块要用它，但真正的实现在引擎层。
pub(crate) use kanami_engine::engine::pump_capture;

/// 应用入口。
///
/// mobile_entry_point 是移动端的入口符号注入：Android/iOS 的宿主代码
/// 通过它找到 Rust 侧的入口，缺了它链接出来的 .so 不会被接受。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut builder = tauri::Builder::default();
    builder = builder.plugin(tauri_plugin_dialog::init());
    builder = builder.plugin(tauri_plugin_opener::init());

    builder
        .setup(|app| {
            let handle = app.handle().clone();

            // 引擎与状态
            let state = AppState::new(&handle)?;
            app.manage(state);

            // 托盘只有桌面有。移动端没有「后台常驻窗口」这个概念，
            // 锁屏保活走的是前台服务，与托盘无关。
            #[cfg(desktop)]
            tray::setup(&handle)?;

            // 播放线程：把混音结果真正送进声卡。
            //
            // 单独开一个线程是因为 cpal 的 Stream 是 !Send，
            // 而 Tauri 的 managed state 要求 Send + Sync。
            // 播放器只在自己的线程里活着，其他模块通过
            // engine.output 这个共享缓冲跟它交换数据。
            {
                let engine = std::sync::Arc::clone(&app.state::<AppState>().engine);
                let started_flag = app.state::<AppState>();
                std::thread::Builder::new().name("kanami-playout".into()).spawn(move || {
                    match kanami_engine::playout::start_player(None, engine.output.clone()) {
                        Ok(player) => {
                            log::info!(
                                "播放器已启动：{} {}Hz {}ch",
                                player.device_name,
                                player.sample_rate,
                                player.channels
                            );
                            // 持有到线程结束，drop 即停止播放
                            loop {
                                std::thread::sleep(Duration::from_secs(3600));
                                let _ = &player;
                            }
                        }
                        Err(e) => {
                            log::error!("播放器启动失败：{e}");
                        }
                    }
                })?;
                started_flag.player_started.store(true, std::sync::atomic::Ordering::Relaxed);
            }

            // 后台维护线程：发现广播、保活、混音节拍
            let handle_bg = handle.clone();
            std::thread::Builder::new().name("kanami-mainloop".into()).spawn(move || loop {
                std::thread::sleep(Duration::from_millis(20));
                let state = handle_bg.state::<AppState>();
                if let Err(e) = state.tick(&handle_bg) {
                    log::error!("主循环出错：{e}");
                }
            })?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_platform_info,
            commands::list_capture_sources,
            commands::list_output_devices,
            commands::start_sending,
            commands::stop_sending,
            commands::set_mode,
            commands::get_status,
            commands::list_peers,
            commands::scan_peers,
            commands::add_peer_manual,
            commands::remove_peer,
            commands::connect_to_peer,
            commands::disconnect_peer,
            commands::get_local_info,
            commands::set_source_volume,
            commands::set_source_muted,
            commands::set_source_solo,
            commands::approve_peer,
            commands::reject_peer,
            commands::list_pending_requests,
        ])
        .run(tauri::generate_context!())
        .expect("Kanami 启动失败");
}
