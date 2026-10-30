import { Space, Tag, Typography, Tooltip } from "antd";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 常驻状态条：一眼看到发送与接收的全局状态。
export default function StatusBar({ k }: { k: KanamiApi }) {
  const sending = k.status.sending;
  const receiving = k.status.receiving;
  const sources = k.status.source_count;

  return (
    <div className="status-bar">
      <Tooltip title="本机设备名">
        <Typography.Text strong>{k.status.self_name || S.commonLoading}</Typography.Text>
      </Tooltip>

      <Space size={4}>
        <span className={`status-dot ${sending ? "on" : "off"}`} />
        <Typography.Text type={sending ? undefined : "secondary"}>
          {sending ? S.sendRunning : S.sendIdle}
        </Typography.Text>
      </Space>

      <Space size={4}>
        <span className={`status-dot ${receiving ? "on" : "off"}`} />
        <Typography.Text type={receiving ? undefined : "secondary"}>
          {receiving ? `${S.receiveRunning} · ${sources} 路` : S.receiveIdle}
        </Typography.Text>
      </Space>

      <Tag color="blue">{k.status.mode}</Tag>

      {k.status.audio_port > 0 && (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          音频端口 {k.status.audio_port}
        </Typography.Text>
      )}

      <div style={{ flex: 1 }} />

      {k.platform && (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          {k.platform.platform} · v{k.platform.version}
        </Typography.Text>
      )}
    </div>
  );
}
