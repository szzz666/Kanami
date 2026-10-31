// 与 Rust 后端通信的唯一入口。
//
// 所有 invoke 调用都经过这里，组件不直接碰 @tauri-apps/api——
// 这样以后换通信方式（比如加一层本地 HTTP 方便调试）只改这一处。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface PlatformInfo {
  platform: string;
  system_audio_capture: boolean;
  process_exclusion: boolean;
  background_playback: boolean;
  notes: string[];
  version: string;
}

export interface CaptureSourceDto {
  id: string;
  name: string;
  kind: string;
  sample_rate: number;
  channels: number;
  is_default: boolean;
}

export interface OutputDeviceDto {
  id: string;
  name: string;
  is_default: boolean;
  sample_rate: number;
  channels: number;
}

export interface PeerDto {
  device_id: string;
  name: string;
  role: string;
  addr: string;
  audio_port: number;
  signal_port: number;
  mode: string;
  via: string;
  age_secs: number;
}

export interface LocalInfo {
  device_id: string;
  name: string;
  addresses: string[];
  audio_port: number;
  signal_port: number;
  platform: string;
}

export interface IncomingSourceDto {
  stream_id: number;
  device_id: string;
  name: string;
  mode: string;
  packets: number;
  lost: number;
  buffer_ms: number;
  /** 端到端延迟（毫秒）。-1 表示时钟尚未同步。 */
  latency_ms: number;
  /** 时钟是否已同步。未同步时 latency_ms 不可信。 */
  synced: boolean;
  active: boolean;
  /** 这一路的音量倍数（1.0 = 原始音量）。 */
  gain: number;
  /** 是否静音。 */
  muted: boolean;
  /** 是否独奏。独奏时其它路会闭嘴。 */
  solo: boolean;
}

export interface StatusSnapshot {
  sending: boolean;
  receiving: boolean;
  source_count: number;
  outgoing_frames: number;
  mode: string;
  sources: IncomingSourceDto[];
  self_id: string;
  self_name: string;
  audio_port: number;
  signal_port: number;
  uptime_secs: number;
}

export interface PendingRequest {
  device_id: string;
  name: string;
  addr: string;
}

/// 是否是运行在 Tauri 里。浏览器里直接打开时后端不存在。
export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!inTauri()) {
    throw new Error("不在 Tauri 环境里运行，后端命令不可用");
  }
  return invoke<T>(cmd, args);
}

export const api = {
  platformInfo: () => call<PlatformInfo>("get_platform_info"),
  captureSources: () => call<CaptureSourceDto[]>("list_capture_sources"),
  outputDevices: () => call<OutputDeviceDto[]>("list_output_devices"),
  localInfo: () => call<LocalInfo>("get_local_info"),

  startSending: (sourceId: string) => call<void>("start_sending", { sourceId }),
  stopSending: () => call<void>("stop_sending"),
  setMode: (mode: string) => call<void>("set_mode", { mode }),
  status: () => call<StatusSnapshot>("get_status"),

  peers: () => call<PeerDto[]>("list_peers"),
  scanPeers: () => call<PeerDto[]>("scan_peers"),
  addPeerManual: (addr: string) => call<PeerDto>("add_peer_manual", { addr }),
  removePeer: (deviceId: string) => call<boolean>("remove_peer", { deviceId }),
  connectToPeer: (deviceId: string) => call<void>("connect_to_peer", { deviceId }),
  disconnectPeer: (deviceId: string) => call<boolean>("disconnect_peer", { deviceId }),

  setSourceVolume: (streamId: number, gain: number) =>
    call<boolean>("set_source_volume", { streamId, gain }),
  setSourceMuted: (streamId: number, muted: boolean) =>
    call<boolean>("set_source_muted", { streamId, muted }),
  setSourceSolo: (streamId: number, solo: boolean) =>
    call<boolean>("set_source_solo", { streamId, solo }),

  pendingRequests: () => call<PendingRequest[]>("list_pending_requests"),
  approvePeer: (deviceId: string) => call<boolean>("approve_peer", { deviceId }),
  rejectPeer: (deviceId: string) => call<boolean>("reject_peer", { deviceId }),
};

/// 订阅后端推送的状态。
export function onStatus(cb: (s: StatusSnapshot) => void): Promise<UnlistenFn> {
  return listen<StatusSnapshot>("kanami://status", (e) => cb(e.payload));
}

/// 托盘菜单触发的「开始/停止发送」。
export function onTrayToggleSend(cb: () => void): Promise<UnlistenFn> {
  return listen("kanami://tray-toggle-send", () => cb());
}