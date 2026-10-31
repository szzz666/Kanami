// 共享业务状态。
//
// 桌面（antd）与移动（antd-mobile）两套界面都从这里取数据，
// 组件只负责画，不负责取——这样加一套界面不用重写业务。

import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  inTauri,
  onStatus,
  onTrayToggleSend,
  type CaptureSourceDto,
  type LocalInfo,
  type PeerDto,
  type PendingRequest,
  type PlatformInfo,
  type StatusSnapshot,
} from "./api";

const EMPTY_STATUS: StatusSnapshot = {
  sending: false,
  receiving: false,
  source_count: 0,
  outgoing_frames: 0,
  mode: "标准",
  sources: [],
  self_id: "",
  self_name: "",
  audio_port: 0,
  signal_port: 0,
  uptime_secs: 0,
};

export interface KanamiState {
  ready: boolean;
  error: string | null;
  status: StatusSnapshot;
  platform: PlatformInfo | null;
  sources: CaptureSourceDto[];
  peers: PeerDto[];
  local: LocalInfo | null;
  pending: PendingRequest[];
  scanning: boolean;
  busy: boolean;
  selectedSource: string;
  mode: string;
}

export function useKanami() {
  const [state, setState] = useState<KanamiState>({
    ready: false,
    error: null,
    status: EMPTY_STATUS,
    platform: null,
    sources: [],
    peers: [],
    local: null,
    pending: [],
    scanning: false,
    busy: false,
    selectedSource: "",
    mode: "标准",
  });

  const mounted = useRef(true);

  const patch = useCallback((p: Partial<KanamiState>) => {
    if (mounted.current) setState((s) => ({ ...s, ...p }));
  }, []);

  const setError = useCallback(
    (e: unknown) => {
      const msg = e instanceof Error ? e.message : String(e);
      patch({ error: msg, busy: false });
    },
    [patch],
  );

  // 初始加载
  useEffect(() => {
    mounted.current = true;

    if (!inTauri()) {
      patch({ ready: true, error: "当前在浏览器里打开，后端不可用。请用 Tauri 应用运行。" });
      return () => {
        mounted.current = false;
      };
    }

    (async () => {
      try {
        const [platform, sources, local, status] = await Promise.all([
          api.platformInfo(),
          api.captureSources(),
          api.localInfo(),
          api.status(),
        ]);
        const defaultSource = sources.find((s) => s.is_default)?.id ?? sources[0]?.id ?? "";
        patch({
          ready: true,
          platform,
          sources,
          local,
          status,
          selectedSource: defaultSource,
          mode: status.mode,
        });
      } catch (e) {
        setError(e);
        patch({ ready: true });
      }
    })();

    return () => {
      mounted.current = false;
    };
  }, [patch, setError]);

  // 订阅状态推送
  useEffect(() => {
    if (!inTauri()) return;
    let unlisten: (() => void) | undefined;
    onStatus((s) => {
      if (!mounted.current) return;
      // 与上一份状态合并，而不是整体替换。
      //
      // 实测在 Android 上，事件 payload 偶尔会缺字段（只到了部分键），
      // 整体替换会让 sources 变 undefined，界面直接崩成 0/8 并且
      // 再也不恢复——因为后续每个事件都只带部分字段。
      // 合并能保证任何一种 payload 都不会把已有数据打回空白。
      setState((prev) => {
        const next: typeof prev.status = { ...EMPTY_STATUS, ...prev.status, ...s };
        // 数组字段单独兜底：undefined 或非数组都退回上一份
        if (!Array.isArray(next.sources)) {
          next.sources = prev.status.sources;
        }
        if (typeof next.source_count !== "number") {
          next.source_count = next.sources.length;
        }
        return { ...prev, status: next, mode: next.mode || prev.mode };
      });
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  // 轮询授权请求
  useEffect(() => {
    if (!inTauri()) return;
    const timer = setInterval(async () => {
      try {
        const pending = await api.pendingRequests();
        if (mounted.current) {
          setState((prev) =>
            prev.pending.length === pending.length &&
            prev.pending.every((p, i) => p.device_id === pending[i].device_id)
              ? prev
              : { ...prev, pending },
          );
        }
      } catch {
        /* 后端还没起来时忽略 */
      }
    }, 1500);
    return () => clearInterval(timer);
  }, []);

  const refreshPeers = useCallback(async () => {
    try {
      const peers = await api.peers();
      patch({ peers });
    } catch (e) {
      setError(e);
    }
  }, [patch, setError]);

  // 定期刷新设备列表
  useEffect(() => {
    if (!inTauri()) return;
    refreshPeers();
    const timer = setInterval(refreshPeers, 2000);
    return () => clearInterval(timer);
  }, [refreshPeers]);

  const scan = useCallback(async () => {
    patch({ scanning: true });
    try {
      const peers = await api.scanPeers();
      patch({ peers, scanning: false });
    } catch (e) {
      setError(e);
      patch({ scanning: false });
    }
  }, [patch, setError]);

  const startSending = useCallback(async () => {
    patch({ busy: true, error: null });
    try {
      await api.startSending(state.selectedSource);
      patch({ busy: false });
    } catch (e) {
      setError(e);
    }
  }, [patch, setError, state.selectedSource]);

  const stopSending = useCallback(async () => {
    patch({ busy: true, error: null });
    try {
      await api.stopSending();
      patch({ busy: false });
    } catch (e) {
      setError(e);
    }
  }, [patch, setError]);

  // 托盘菜单切换发送
  useEffect(() => {
    if (!inTauri()) return;
    let unlisten: (() => void) | undefined;
    onTrayToggleSend(() => {
      setState((prev) => {
        if (prev.status.sending) {
          api.stopSending().catch(setError);
        } else {
          api.startSending(prev.selectedSource).catch(setError);
        }
        return prev;
      });
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, [setError]);

  const pullStatus = useCallback(async () => {
    try {
      const status = await api.status();
      if (!mounted.current) return;
      setState((prev) => ({ ...prev, status, mode: status.mode || prev.mode }));
    } catch {
      /* 后端还没起来时忽略 */
    }
  }, []);

  const setMode = useCallback(
    async (mode: string) => {
      try {
        await api.setMode(mode);
        patch({ mode });
        // 模式影响每一路接收的缓冲水位，必须重新拉状态，
        // 否则混音台里还显示上一档的「缓冲 40ms / 标准」。
        await pullStatus();
      } catch (e) {
        setError(e);
      }
    },
    [patch, setError, pullStatus],
  );

  const addPeerManual = useCallback(
    async (addr: string) => {
      try {
        await api.addPeerManual(addr);
        await refreshPeers();
      } catch (e) {
        setError(e);
      }
    },
    [refreshPeers, setError],
  );

  const removePeer = useCallback(
    async (deviceId: string) => {
      try {
        await api.removePeer(deviceId);
        await refreshPeers();
      } catch (e) {
        setError(e);
      }
    },
    [refreshPeers, setError],
  );

  /// 主动拉一次状态。
  ///
  /// 不依赖 kanami://status 事件：实测在 Android WebView 里，
  /// 事件偶尔会丢失或只带部分字段，界面就会一直停在
  /// 「未在接收 / 0/8」，而后端其实已经有音源在跑。
  /// 每个会改变状态的操作之后都主动拉一次，事件只当锦上添花。

  const connectToPeer = useCallback(
    async (deviceId: string) => {
      patch({ busy: true, error: null });
      try {
        await api.connectToPeer(deviceId);
        patch({ busy: false });
        // 连上之后立刻拉状态：混音台要马上显示出这一路
        await pullStatus();
      } catch (e) {
        setError(e);
      }
    },
    [patch, setError, pullStatus],
  );

  const disconnectPeer = useCallback(
    async (deviceId: string) => {
      try {
        await api.disconnectPeer(deviceId);
        await pullStatus();
      } catch (e) {
        setError(e);
      }
    },
    [setError],
  );

  const approvePeer = useCallback(
    async (deviceId: string) => {
      try {
        await api.approvePeer(deviceId);
        patch({ pending: state.pending.filter((p) => p.device_id !== deviceId) });
      } catch (e) {
        setError(e);
      }
    },
    [patch, setError, state.pending],
  );

  const rejectPeer = useCallback(
    async (deviceId: string) => {
      try {
        await api.rejectPeer(deviceId);
        patch({ pending: state.pending.filter((p) => p.device_id !== deviceId) });
      } catch (e) {
        setError(e);
      }
    },
    [patch, setError, state.pending],
  );

  const setSourceVolume = useCallback(
    async (streamId: number, gain: number) => {
      try {
        await api.setSourceVolume(streamId, gain);
      } catch (e) {
        setError(e);
      }
    },
    [setError],
  );

  const setSourceMuted = useCallback(
    async (streamId: number, muted: boolean) => {
      try {
        await api.setSourceMuted(streamId, muted);
      } catch (e) {
        setError(e);
      }
    },
    [setError],
  );

  const setSourceSolo = useCallback(
    async (streamId: number, solo: boolean) => {
      try {
        await api.setSourceSolo(streamId, solo);
      } catch (e) {
        setError(e);
      }
    },
    [setError],
  );

  const setSelectedSource = useCallback(
    (id: string) => patch({ selectedSource: id }),
    [patch],
  );

  const clearError = useCallback(() => patch({ error: null }), [patch]);

  return {
    ...state,
    scan,
    startSending,
    stopSending,
    setMode,
    addPeerManual,
    removePeer,
    connectToPeer,
    disconnectPeer,
    approvePeer,
    rejectPeer,
    setSourceVolume,
    setSourceMuted,
    setSourceSolo,
    setSelectedSource,
    clearError,
    refreshPeers,
    pullStatus,
  };
}

export type KanamiApi = ReturnType<typeof useKanami>;
