#!/usr/bin/env bash
# Android 构建环境。
#
# 这台机器上 ANDROID_HOME / NDK_HOME / JAVA_HOME 全是空的，adb 也不在 PATH 里，
# 而默认 java 是 JDK 25（Tauri 2 的 Gradle 对它有兼容问题），
# 所以这里把所有依赖显式钉死，避免每次构建靠运气。

set -euo pipefail

export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"

# opus 的 C 库走 cmake，cmake 的 Android 工具链需要这些变量
export ANDROID_NDK_HOME="$NDK_HOME"
export ANDROID_NDK_ROOT="$NDK_HOME"
export CMAKE_TOOLCHAIN_FILE="$NDK_HOME/build/cmake/android.toolchain.cmake"

# 用 JDK 21 而不是默认的 25
if [ -d "$HOME/Library/Java/JavaVirtualMachines/azul-21.0.12.1/Contents/Home" ]; then
  export JAVA_HOME="$HOME/Library/Java/JavaVirtualMachines/azul-21.0.12.1/Contents/Home"
fi

export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"
export PATH="$NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"

# 链接器必须显式带 API 26。
#
# 不带 API 级别的 aarch64-linux-android-clang 默认按 API 24 链接，
# 而 cpal 的 AAudio 后端需要 -laaudio，这个库在 NDK 里只存在于
# API 26 及以上的目录，于是链接直接失败。
# .cargo/config.toml 里的 linker 设置会被 Tauri CLI 覆盖，
# 只有环境变量能压得住。
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="aarch64-linux-android26-clang"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="armv7a-linux-androideabi26-clang"
export CARGO_TARGET_I686_LINUX_ANDROID_LINKER="i686-linux-android26-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="x86_64-linux-android26-clang"

# 模拟器通过 127.0.0.1:5555 接入
export ADB_DEVICE="${ADB_DEVICE:-127.0.0.1:5555}"
