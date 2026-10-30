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
      setState((prev) => ({ ...prev, status: s, mode: s.mode }));
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

  const setMode = useCallback(
    async (mode: string) => {
      try {
        await api.setMode(mode);
        patch({ mode });
      } catch (e) {
        setError(e);
      }
    },
    [patch, setError],
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

  const connectToPeer = useCallback(
    async (deviceId: string) => {
      patch({ busy: true, error: null });
      try {
        await api.connectToPeer(deviceId);
        patch({ busy: false });
      } catch (e) {
        setError(e);
      }
    },
    [patch, setError],
  );

  const disconnectPeer = useCallback(
    async (deviceId: string) => {
      try {
        await api.disconnectPeer(deviceId);
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
  };
}

export type KanamiApi = ReturnType<typeof useKanami>;
