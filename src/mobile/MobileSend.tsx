import { Card, Button, Selector, List, Radio, Space, Tag, NoticeBar } from "antd-mobile";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 移动端发送页。
///
/// 移动端能发的只有麦克风（Android 的系统声音采集需要 REMOTE_SUBMIX，
/// 实测目标设备不具备；iOS 根本拿不到系统输出）。
export default function MobileSend({ k }: { k: KanamiApi }) {
  const running = k.status.sending;

  return (
    <Space direction="vertical" style={{ width: "100%", "--gap": "12px" } as never}>
      {k.platform && !k.platform.process_exclusion && (
        <NoticeBar
          content="本机同时收发时可能产生回授，建议戴耳机"
          color="alert"
          wrap
        />
      )}

      <Card title={S.modeLabel}>
        <Selector
          options={[
            { label: S.modeStandard, value: "标准" },
            { label: S.modeLowLatency, value: "低延迟" },
            { label: S.modeHighQuality, value: "高音质" },
          ]}
          value={[k.mode]}
          onChange={(v) => v[0] && k.setMode(String(v[0]))}
        />
      </Card>

      <Card title={S.sendTitle}>
        <List>
          <List.Item>
            <div style={{ width: "100%" }}>
              <div style={{ fontSize: 13, color: "#86909c", marginBottom: 8 }}>
                {S.sendSource}
              </div>
              <Radio.Group
                value={k.selectedSource}
                onChange={(v) => k.setSelectedSource(String(v))}
              >
                <Space direction="vertical">
                  {k.sources.map((s) => (
                    <Radio key={s.id} value={s.id}>
                      {s.name}
                    </Radio>
                  ))}
                </Space>
              </Radio.Group>
            </div>
          </List.Item>
          {running && (
            <List.Item extra={k.status.outgoing_frames}>已发送帧</List.Item>
          )}
        </List>

        <div style={{ padding: 12 }}>
          <Button
            block
            color={running ? "danger" : "primary"}
            loading={k.busy}
            onClick={running ? k.stopSending : k.startSending}
          >
            {running ? S.sendStop : S.sendStart}
          </Button>
        </div>
      </Card>

      <Card title={S.sendTargets}>
        {k.peers.filter((p) => p.role === "Player" || p.role === "Both").length === 0 ? (
          <div className="empty-hint">{S.sendTargetsEmpty}</div>
        ) : (
          <List>
            {k.peers
              .filter((p) => p.role === "Player" || p.role === "Both")
              .map((p) => (
                <List.Item key={p.device_id} extra={<Tag color="primary">{p.mode}</Tag>}>
                  {p.name}
                </List.Item>
              ))}
          </List>
        )}
      </Card>
    </Space>
  );
}
