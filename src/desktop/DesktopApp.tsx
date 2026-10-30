import { useState } from "react";
import { Layout, Menu, Alert, Typography } from "antd";
import {
  SoundOutlined,
  CustomerServiceOutlined,
  SettingOutlined,
} from "@ant-design/icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";
import StatusBar from "./StatusBar";
import SendPanel from "./SendPanel";
import ReceivePanel from "./ReceivePanel";
import SettingsPanel from "./SettingsPanel";
import PendingModal from "./PendingModal";

const { Sider, Content } = Layout;

/// 桌面端主界面。
///
/// 结构按共识：三个 Tab + 顶部常驻状态条。
/// 状态条让用户随时看到「我正在发什么、正在收什么」，
/// 不用来回切页才知道全局状态。
export default function DesktopApp({ k }: { k: KanamiApi }) {
  const [tab, setTab] = useState("send");

  return (
    <Layout style={{ height: "100%" }}>
      <Sider width={200} theme="light" style={{ borderRight: "1px solid var(--kanami-border)" }}>
        <div style={{ padding: "20px 20px 12px" }}>
          <Typography.Title level={4} style={{ margin: 0 }}>
            {S.appName}
          </Typography.Title>
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            {S.tagline}
          </Typography.Text>
        </div>
        <Menu
          mode="inline"
          selectedKeys={[tab]}
          onClick={(e) => setTab(e.key)}
          style={{ borderInlineEnd: "none" }}
          items={[
            { key: "send", icon: <SoundOutlined />, label: S.navSend },
            { key: "receive", icon: <CustomerServiceOutlined />, label: S.navReceive },
            { key: "settings", icon: <SettingOutlined />, label: S.navSettings },
          ]}
        />
      </Sider>

      <Layout>
        <StatusBar k={k} />
        <Content className="desktop-content">
          {k.error && (
            <Alert
              type="warning"
              showIcon
              closable
              onClose={k.clearError}
              message={k.error}
              style={{ marginBottom: 16 }}
            />
          )}

          {tab === "send" && <SendPanel k={k} />}
          {tab === "receive" && <ReceivePanel k={k} />}
          {tab === "settings" && <SettingsPanel k={k} />}
        </Content>
      </Layout>

      <PendingModal k={k} />
    </Layout>
  );
}
