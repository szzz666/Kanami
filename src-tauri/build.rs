// 构建脚本：把 macOS 采集助手编译好，放进应用包的 Resources 里。
//
// 为什么必须在打包阶段做：TCC 的「系统音频录制」权限按应用包归属，
// 采集助手作为 Kanami.app 的子进程运行时，才能继承这个授权。
//
// 顺序很重要：tauri_build::build() 会立刻校验 tauri.conf.json 里声明的
// 资源是否存在，所以助手必须先编译并落盘，再调用它。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 先确保助手存在，否则 tauri_build 会因为资源缺失直接失败
    stage_capture_helper();

    tauri_build::build();
}

/// 编译 Swift 采集助手并复制到 resources 目录。
#[cfg(target_os = "macos")]
fn stage_capture_helper() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let swift_src = manifest.join("capture/macos_capture.swift");
    let staged = manifest.join("capture/kanami-capture");

    if !swift_src.exists() {
        println!("cargo:warning=未找到采集助手源码，将以静音模式运行");
        let _ = std::fs::write(&staged, b"");
        return;
    }
    println!("cargo:rerun-if-changed=capture/macos_capture.swift");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bin = out_dir.join("kanami-capture");

    match Command::new("xcrun")
        .args(["--sdk", "macosx", "swiftc", "-O", "-o"])
        .arg(&bin)
        .arg(&swift_src)
        .status()
    {
        Ok(s) if s.success() => {
            if let Err(e) = std::fs::copy(&bin, &staged) {
                println!("cargo:warning=复制采集助手失败：{e}");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
            }
        }
        Ok(s) => {
            println!("cargo:warning=采集助手编译失败（{s}），将以静音模式运行");
            let _ = std::fs::write(&staged, b"");
        }
        Err(e) => {
            println!("cargo:warning=无法调用 xcrun（{e}），将以静音模式运行");
            let _ = std::fs::write(&staged, b"");
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn stage_capture_helper() {}
