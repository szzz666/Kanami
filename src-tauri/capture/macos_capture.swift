// Kanami macOS 系统声音采集助手。
//
// 用 CoreAudio Process Tap（macOS 14.2+）抓整机输出，并排除本应用自己，
// 断掉「本机同时收发 → 采到自己刚收到的流 → 再发回去」这条回授回路。
//
// 输出：stdout 上不间断的 f32le、48kHz、立体声交错样本。
// stderr 上输出状态文本，Rust 侧据此判断是「没授权」还是「没在播放」。

import AVFoundation
import AudioToolbox
import CoreAudio
import Foundation

let sampleRate: Float64 = 48_000
let channelCount: UInt32 = 2

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write("ERROR: \(msg)\n".data(using: .utf8)!)
    exit(1)
}

let logPath = ProcessInfo.processInfo.environment["KANAMI_LOG"] ?? "/tmp/kanami-capture.log"

/// 追加写日志。用 POSIX 的 O_APPEND，避免 FileHandle 在子进程里的各种怪问题。
func info(_ msg: String) {
    let line = "INFO: \(msg)\n"
    FileHandle.standardError.write(line.data(using: .utf8)!)

    let fd = open(logPath, O_WRONLY | O_APPEND | O_CREAT, 0o644)
    if fd >= 0 {
        let bytes = Array(line.utf8)
        _ = bytes.withUnsafeBufferPointer { write(fd, $0.baseAddress, $0.count) }
        close(fd)
    }
}

guard #available(macOS 14.2, *) else {
    fail("需要 macOS 14.2 或更高版本才能采集系统声音")
}

// 0. 不调用 AVCaptureDevice.requestAccess。
//
// 这条路径有个陷阱：它只在有 GUI 会话的进程里才会返回，
// 从终端或作为子进程启动时会一直挂着，导致整个助手什么都不输出。
// 正确做法是直接去建 tap —— 系统会在需要时弹授权窗，
// 失败时再用错误码告诉用户去系统设置里手动允许。
let permStatus = AVCaptureDevice.authorizationStatus(for: .audio)
switch permStatus {
case .authorized:
    info("音频录制权限：已授权")
case .notDetermined:
    info("音频录制权限：尚未决定，系统会在创建采集时弹窗")
case .denied:
    info("音频录制权限：已被拒绝，请到 系统设置 → 隐私与安全性 → 麦克风 中允许 Kanami")
case .restricted:
    info("音频录制权限：受系统限制")
@unknown default:
    info("音频录制权限：未知状态")
}

// 1. 把本进程的 PID 翻成 CoreAudio 的「进程对象 ID」。
//
// 这里最容易踩的坑：CATapDescription 要的是 AudioObjectID，
// 不是 PID。直接传 PID 会拿到 '!obj'（无效对象）错误。
func processObjectId(for pid: pid_t) -> AudioObjectID? {
    var pidCopy = pid
    var objectId = AudioObjectID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    let status = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &address,
        UInt32(MemoryLayout<pid_t>.size),
        &pidCopy,
        &size,
        &objectId
    )
    guard status == noErr, objectId != AudioObjectID(kAudioObjectUnknown) else { return nil }
    return objectId
}

let myPid = ProcessInfo.processInfo.processIdentifier
var excluded: [AudioObjectID] = []
if let selfObject = processObjectId(for: myPid) {
    excluded = [selfObject]
    info("已解析本进程音频对象 id=\(selfObject)，将从采集中排除")
} else {
    // 解析不出来就抓全局混音，回授风险交给上层提示用户
    info("未能解析本进程音频对象，改为抓全局混音（本机同时收发时可能回授）")
}

// 全局混音 + 排除列表。排除列表为空时等同于抓全部。
let desc = CATapDescription(stereoGlobalTapButExcludeProcesses: excluded)
desc.uuid = UUID()
desc.isPrivate = true

// 2. 创建 process tap
var tapId = AudioObjectID(kAudioObjectUnknown)
let createStatus = AudioHardwareCreateProcessTap(desc, &tapId)
guard createStatus == noErr, tapId != AudioObjectID(kAudioObjectUnknown) else {
    if createStatus == kAudioHardwareIllegalOperationError {
        fail("没有系统音频录制权限，请在系统设置 → 隐私与安全性 中授权")
    }
    fail("创建 process tap 失败，状态码 \(createStatus)")
}
info("process tap 已创建 id=\(tapId)")

// 3. 建一个聚合设备，把 tap 挂进去
let aggregateUid = "com.szzz666.kanami.tap.\(UUID().uuidString)"
let aggregateDesc: [String: Any] = [
    kAudioAggregateDeviceNameKey as String: "Kanami Tap",
    kAudioAggregateDeviceUIDKey as String: aggregateUid,
    kAudioAggregateDeviceIsPrivateKey as String: true,
    kAudioAggregateDeviceIsStackedKey as String: false,
    kAudioAggregateDeviceSubDeviceListKey as String: [],
    kAudioAggregateDeviceTapListKey as String: [
        [
            kAudioSubTapDriftCompensationKey as String: true,
            kAudioSubTapUIDKey as String: desc.uuid.uuidString,
        ]
    ],
]

var aggregateId = AudioObjectID(kAudioObjectUnknown)
let aggStatus = AudioHardwareCreateAggregateDevice(aggregateDesc as CFDictionary, &aggregateId)
guard aggStatus == noErr, aggregateId != AudioObjectID(kAudioObjectUnknown) else {
    fail("创建聚合设备失败，状态码 \(aggStatus)")
}
info("聚合设备已创建 id=\(aggregateId)")

// 4. 把聚合设备的 IOProc 设成直接读 tap 的格式（非交错 f32）
var asbd = AudioStreamBasicDescription(
    mSampleRate: sampleRate,
    mFormatID: kAudioFormatLinearPCM,
    mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked | kAudioFormatFlagIsNonInterleaved,
    mBytesPerPacket: 4,
    mFramesPerPacket: 1,
    mBytesPerFrame: 4,
    mChannelsPerFrame: channelCount,
    mBitsPerChannel: 32,
    mReserved: 0
)
var fmtSize = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
var fmtAddr = AudioObjectPropertyAddress(
    mSelector: kAudioTapPropertyFormat,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain
)
AudioObjectGetPropertyData(tapId, &fmtAddr, 0, nil, &fmtSize, &asbd)
info("tap 格式：\(asbd.mSampleRate)Hz \(asbd.mChannelsPerFrame)ch nonInterleaved=\((asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0)")

let output = FileHandle.standardOutput
var running = true
signal(SIGINT) { _ in running = false }
signal(SIGTERM) { _ in running = false }
info("准备开始采集")

// 5. IOProc：把每块数据转成交错 f32 写 stdout
var interleaveBuffer = [Float32]()

let ioBlock: AudioDeviceIOProc = { _, _, _, _, outData, _, _ in
    let outList = UnsafeMutableAudioBufferListPointer(outData)
    let frames = Int(outList[0].mDataByteSize) / MemoryLayout<Float32>.size
    if frames == 0 { return noErr }

    if outList.count >= 2,
       let left = outList[0].mData?.assumingMemoryBound(to: Float32.self),
       let right = outList[1].mData?.assumingMemoryBound(to: Float32.self) {
        // 非交错：左右各一块
        interleaveBuffer.removeAll(keepingCapacity: true)
        interleaveBuffer.reserveCapacity(frames * 2)
        for i in 0..<frames {
            interleaveBuffer.append(left[i])
            interleaveBuffer.append(right[i])
        }
    } else if let mono = outList[0].mData?.assumingMemoryBound(to: Float32.self) {
        // 交错或单声道：原样复制
        interleaveBuffer.removeAll(keepingCapacity: true)
        let total = Int(outList[0].mDataByteSize) / MemoryLayout<Float32>.size
        interleaveBuffer.append(contentsOf: UnsafeBufferPointer(start: mono, count: total))
    } else {
        return noErr
    }

    interleaveBuffer.withUnsafeBufferPointer { ptr in
        guard let base = ptr.baseAddress else { return }
        let data = Data(bytes: base, count: ptr.count * MemoryLayout<Float32>.size)
        try? output.write(contentsOf: data)
    }
    return noErr
}

// AudioDeviceCreateIOProcID / AudioDeviceStart 在缺少 TCC 授权时会**阻塞**
// 等待系统弹窗，而作为子进程运行时那个窗口永远不会出现，进程就静默卡死。
// 用一个看门狗把这种情况变成明确的错误，而不是让用户对着空数据发呆。
info("创建 IOProc…")
let watchdog = DispatchWorkItem {
    fail("采集启动超时：系统正在等待「音频录制」授权，但本进程无法显示授权窗口。\n"
        + "请在 系统设置 → 隐私与安全性 → 麦克风 中手动允许 Kanami，或先从桌面启动一次 Kanami.app 完成授权。")
}
DispatchQueue.global().asyncAfter(deadline: .now() + 3.0, execute: watchdog)

var procId: AudioDeviceIOProcID?
let procStatus = AudioDeviceCreateIOProcID(aggregateId, ioBlock, nil, &procId)
watchdog.cancel()
guard procStatus == noErr, let pid = procId else {
    fail("创建 IOProc 失败，状态码 \(procStatus)")
}
info("IOProc 创建成功")

// 启动前先确认设备已就绪。AudioDeviceStart 在没有 TCC 授权时
// 会**阻塞**等待系统弹窗，而子进程永远等不到那个窗口，
// 结果就是整个采集线程静默卡死。所以这里先做一次非阻塞检查。
info("正在启动采集…")
var deviceAlive: UInt32 = 0
var aliveSize = UInt32(MemoryLayout<UInt32>.size)
var aliveAddr = AudioObjectPropertyAddress(
    mSelector: kAudioDevicePropertyDeviceIsAlive,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain
)
let aliveStatus = AudioObjectGetPropertyData(aggregateId, &aliveAddr, 0, nil, &aliveSize, &deviceAlive)
if aliveStatus != noErr {
    fail("聚合设备不可用（状态 \(aliveStatus)），通常是缺少系统音频录制权限")
}
info("聚合设备存活检查通过")

let startStatus = AudioDeviceStart(aggregateId, pid)
if startStatus != noErr {
    let code = String(format: "0x%08X", startStatus)
    if startStatus == kAudioHardwareIllegalOperationError || startStatus == 0x10004003 {
        fail("启动采集失败（\(code)）：这是缺少「系统音频录制」权限。请到 系统设置 → 隐私与安全性 → 麦克风/屏幕录制 中允许 Kanami，然后重开应用")
    }
    fail("启动采集失败，状态码 \(startStatus) (\(code))")
}
info("开始采集")

while running {
    RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.1))
}

info("停止采集")
AudioDeviceStop(aggregateId, pid)
AudioDeviceDestroyIOProcID(aggregateId, pid)
AudioHardwareDestroyAggregateDevice(aggregateId)
AudioHardwareDestroyProcessTap(tapId)
