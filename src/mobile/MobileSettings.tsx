import { Card, List, Tag, Space, NoticeBar } from "antd-mobile";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 移动端设置页。
export default function MobileSettings({ k }: { k: KanamiApi }) {
  const p = k.platform;

  return (
    <Space direction="vertical" style={{ width: "100%", "--gap": "12px" } as never}>
      <Card title={S.settingsPlatform}>
        <List>
          <List.Item extra={p?.platform ?? "-"}>平台</List.Item>
          <List.Item extra={p?.version ?? "-"}>版本</List.Item>
          <List.Item
            extra={
              <Tag color={p?.system_audio_capture ? "success" : "default"}>
                {p?.system_audio_capture ? S.commonSupported : S.commonUnsupported}
              </Tag>
            }
          >
            系统声音采集
          </List.Item>
          <List.Item
            extra={
              <Tag color={p?.process_exclusion ? "success" : "warning"}>
                {p?.process_exclusion ? S.commonSupported : S.commonUnsupported}
              </Tag>
            }
          >
            排除本进程
          </List.Item>
          <List.Item
            extra={
              <Tag color={p?.background_playback ? "success" : "default"}>
                {p?.background_playback ? S.commonSupported : S.commonUnsupported}
              </Tag>
            }
          >
            后台播放
          </List.Item>
        </List>
      </Card>

      {p && p.notes.length > 0 && (
        <Card title={S.settingsNotes}>
          {p.notes.map((n, i) => (
            <NoticeBar key={i} content={n} color="info" wrap style={{ marginBottom: 8 }} />
          ))}
        </Card>
      )}

      <Card title="本机信息">
        <List>
          <List.Item extra={k.local?.name ?? "-"}>设备名</List.Item>
          <List.Item extra={<span style={{ fontSize: 11 }}>{k.local?.device_id?.slice(0, 8)}</span>}>
            设备 ID
          </List.Item>
          <List.Item extra={k.local?.addresses.join(", ") || "未检测到"}>局域网地址</List.Item>
          <List.Item extra={k.local?.audio_port || "未启动"}>音频端口</List.Item>
        </List>
      </Card>
    </Space>
  );
}
