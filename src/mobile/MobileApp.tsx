import { useState } from "react";
import { TabBar, Toast } from "antd-mobile";
import { SoundOutline, AudioOutline, SetOutline } from "antd-mobile-icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";
import MobileSend from "./MobileSend";
import MobileReceive from "./MobileReceive";
import MobileSettings from "./MobileSettings";

/// 移动端主界面。
///
/// 用 antd-mobile 而不是 antd：触控目标、滚动物理、安全区都是为手指设计的。
/// 业务状态与桌面端完全共用（同一个 useKanami）。
export default function MobileApp({ k }: { k: KanamiApi }) {
  const [tab, setTab] = useState("send");

  if (k.error) {
    Toast.show({ icon: "fail", content: k.error });
    k.clearError();
  }

  return (
    <div className="mobile-shell safe-top">
      <div className="mobile-header">
        <div style={{ display: "flex", alignItems: "baseline", gap: 8 }}>
          <span style={{ fontSize: 20, fontWeight: 600 }}>{S.appName}</span>
          <span style={{ fontSize: 12, color: "#86909c" }}>{k.status.self_name}</span>
        </div>
        <div style={{ marginTop: 6, display: "flex", gap: 12, fontSize: 12 }}>
          <span style={{ color: k.status.sending ? "var(--kanami-ok)" : "#86909c" }}>
            ● {k.status.sending ? S.sendRunning : S.sendIdle}
          </span>
          <span style={{ color: k.status.receiving ? "var(--kanami-ok)" : "#86909c" }}>
            ● {k.status.receiving ? `${S.receiveRunning} ${k.status.source_count} 路` : S.receiveIdle}
          </span>
        </div>
      </div>

      <div className="mobile-content">
        {tab === "send" && <MobileSend k={k} />}
        {tab === "receive" && <MobileReceive k={k} />}
        {tab === "settings" && <MobileSettings k={k} />}
      </div>

      <div className="mobile-tabbar">
        <TabBar activeKey={tab} onChange={setTab}>
          <TabBar.Item key="send" icon={<SoundOutline />} title={S.navSend} />
          <TabBar.Item key="receive" icon={<AudioOutline />} title={S.navReceive} />
          <TabBar.Item key="settings" icon={<SetOutline />} title={S.navSettings} />
        </TabBar>
      </div>
    </div>
  );
}
