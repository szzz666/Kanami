import { Card, Descriptions, Tag, Typography, Space, List } from "antd";
import { CheckCircleOutlined, CloseCircleOutlined } from "@ant-design/icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

function Cap({ ok, label }: { ok: boolean; label: string }) {
  return (
    <Space>
      {ok ? (
        <CheckCircleOutlined style={{ color: "var(--kanami-ok)" }} />
      ) : (
        <CloseCircleOutlined style={{ color: "var(--kanami-err)" }} />
      )}
      <span>{label}</span>
      <Tag color={ok ? "green" : "default"}>{ok ? S.commonSupported : S.commonUnsupported}</Tag>
    </Space>
  );
}

/// 设置面板：输出设备、平台能力、本机信息。
export default function SettingsPanel({ k }: { k: KanamiApi }) {
  const p = k.platform;

  return (
    <Space direction="vertical" size={16} style={{ width: "100%" }}>
      <Card title={S.settingsPlatform}>
        {p ? (
          <Descriptions column={1} size="small">
            <Descriptions.Item label="平台">{p.platform}</Descriptions.Item>
            <Descriptions.Item label="版本">{p.version}</Descriptions.Item>
            <Descriptions.Item label="系统声音采集">
              <Cap ok={p.system_audio_capture} label="能否抓取系统输出" />
            </Descriptions.Item>
            <Descriptions.Item label="排除本进程">
              <Cap ok={p.process_exclusion} label="能否避免本机回授" />
            </Descriptions.Item>
            <Descriptions.Item label="后台播放">
              <Cap ok={p.background_playback} label="锁屏/切后台能否继续" />
            </Descriptions.Item>
          </Descriptions>
        ) : (
          <Typography.Text type="secondary">{S.commonLoading}</Typography.Text>
        )}
      </Card>

      {p && p.notes.length > 0 && (
        <Card title={S.settingsNotes} size="small">
          <List
            size="small"
            dataSource={p.notes}
            renderItem={(n) => (
              <List.Item>
                <Typography.Text type="secondary">· {n}</Typography.Text>
              </List.Item>
            )}
          />
        </Card>
      )}

      <Card title="本机信息" size="small">
        {k.local ? (
          <Descriptions column={1} size="small">
            <Descriptions.Item label="设备名">{k.local.name}</Descriptions.Item>
            <Descriptions.Item label="设备 ID">
              <Typography.Text copyable style={{ fontSize: 12 }}>
                {k.local.device_id}
              </Typography.Text>
            </Descriptions.Item>
            <Descriptions.Item label="局域网地址">
              {k.local.addresses.length > 0 ? (
                <Space wrap>
                  {k.local.addresses.map((a) => (
                    <Typography.Text key={a} copyable code>
                      {a}
                    </Typography.Text>
                  ))}
                </Space>
              ) : (
                <Typography.Text type="secondary">未检测到</Typography.Text>
              )}
            </Descriptions.Item>
            <Descriptions.Item label="音频端口">
              {k.local.audio_port || "未启动"}
            </Descriptions.Item>
            <Descriptions.Item label="信令端口">
              {k.local.signal_port || "未启动"}
            </Descriptions.Item>
          </Descriptions>
        ) : (
          <Typography.Text type="secondary">{S.commonLoading}</Typography.Text>
        )}
      </Card>
    </Space>
  );
}
