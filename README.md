# Kanami

把一台设备的声音，通过局域网实时送到另一台设备上播放。

## 现在能做什么

- **桌面（macOS / Windows / Linux）**：抓系统声音发出去，也能接收别人的声音
- **Android**：接收别人的声音；也能用麦克风发出去
- **三档模式**：标准 / 低延迟 / 高音质，播放端选，音源端可降级
- **一播放端接多音源**：混音台，每路独立音量/静音/独奏
- **一音源推多播放端**：每台独立模式
- **局域网自动发现**：mDNS + UDP 广播 + 手动 IP 三件套

## 快速开始

```bash
pnpm install
pnpm tauri dev          # 桌面开发
cargo test --workspace  # 跑内核测试
```

### Android

```bash
source scripts/android-env.sh
npx tauri android build --target aarch64
```

**必须用 `source scripts/android-env.sh`**：这台机器上 `ANDROID_HOME`/`NDK_HOME`/`JAVA_HOME` 全是空的，默认 JDK 是 25（Gradle 不兼容），而且 Android 的 linker 必须显式带 API 26 否则 `-laaudio` 找不到。脚本把这些全部钉死了。

## 架构

```
crates/
  kanami-proto/    协议：16 字节定长包头、握手、三档模式参数表
  kanami-audio/    音频内核：Opus 编解码、抖动缓冲、混音、漂移校正
  kanami-net/      网络：UDP 音频、TCP 信令、局域网发现
  kanami-engine/   引擎：采集、播放、平台能力探测、四平台采集后端
src-tauri/         Tauri 外壳：命令、状态、托盘
src/               React 前端：桌面 antd / 移动 antd-mobile，共用业务层
```

**分层原则**：音频内核不依赖 Tauri，能脱离 UI 单独跑。`cargo run -p kanami-net --example e2e` 就是靠这个性质做的真实 UDP 链路验证。

## 三档模式

| 模式 | 帧长 | 码率 | 抖动缓冲 | 设计取舍 |
|---|---|---|---|---|
| 标准 | 20ms | 96 kbps CBR | 40–120ms 自适应 | 兼顾稳定与延迟 |
| 低延迟 | 10ms | 128 kbps | **固定 20ms** | 宁可丢音，也要守住延迟承诺 |
| 高音质 | 20ms | 256 kbps VBR | 60–200ms 自适应 | 音乐优先，复杂度拉满 |

低延迟档的缓冲**永远不增长**，这条承诺由单元测试和编译期断言双重锁死。

## 为什么这么设计

几个决定是从**实测事实**倒推出来的，不是教科书抄的：

1. **连接由播放端发起。** 实测宿主无法主动寻址到 NAT 后面的客体，反过来可以。
2. **音源端从收到的包学地址。** 实测对端源端口每次随机（54255/59171/65134/63610…），硬编码必挂。
3. **回程 socket 长期绑定同一端口。** 实测是对称型 NAT，换源端口回送直接被丢。
4. **保活 60 秒。** 实测映射 180 秒空闲仍存活，留三倍余量。
5. **发现要三件套。** 实测模拟器上广播与组播双向都不通。
6. **采集时排除本进程。** 不排除的话，本机同时收发会无限回授。

## 平台能力对照

| 平台 | 系统声音采集 | 排除本进程 | 后台播放 |
|---|---|---|---|
| macOS | CoreAudio Process Tap | 是 | 是 |
| Windows | WASAPI loopback | Win11 22H2+ | 是 |
| Linux | PulseAudio monitor | 是 | 是 |
| Android | 仅麦克风（缺 REMOTE_SUBMIX） | 否 | 待补前台服务 |
| iOS | 仅麦克风 | 否 | 配置已备，待 Xcode |

## 测试

```bash
cargo test --workspace                        # 39 个单测
cargo run -p kanami-audio --example loopback  # 音频内核回环
cargo run -p kanami-net --example e2e         # 真实 UDP 端到端
```

端到端自测会跑五种场景：三档模式各一次、四路混音、5% 丢包恢复。

**需要人工验证的项**见 [docs/待人工测试清单.md](docs/待人工测试清单.md)。

## 已知限制

- Android 系统声音采集在多数设备上不可用（缺 REMOTE_SUBMUX HAL），只做麦克风
- Android 锁屏保活的前台服务还没实现
- iOS 完全未开始（本机无 Xcode）
- PIN 配对与音频加密未实现
- 端到端延迟测量缺时钟同步，目前显示的只是本地缓冲时间

## 开发环境

- Rust 1.99+
- Node 22+ / pnpm
- 桌面端另需 cmake + nasm（编 libopus）
- Android 另需 NDK 27 + JDK 21（见 `scripts/android-env.sh`）

Windows / Linux 产物由 GitHub Actions 构建，本机只跑 macOS 与 Android。