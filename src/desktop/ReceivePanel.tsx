import {
  Button,
  Card,
  Empty,
  List,
  Space,
  Tag,
  Typography,
  Slider,
  Switch,
} from "antd";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";
import PeerList from "./PeerList";

/// 接收面板：设备列表 + 混音台。
///
/// 混音台是「一个播放设备接入多个音源」的落点：
/// 每路独立音量/静音/独奏，某一路断流不影响其他路。
export default function ReceivePanel({ k }: { k: KanamiApi }) {
  const sources = k.status.sources;

  return (
    <Space direction="vertical" size={16} style={{ width: "100%" }}>
      {/* 待确认的接入请求。
          播放端连上来时服务端回 Reject(NotTrusted)，必须本机批准才放行。
          没有这个入口，用户看到的就是「对方一直连不上」，
          原因藏在日志里，界面上毫无线索。 */}
      {k.pending.length > 0 && (
        <Card title={S.pendingTitle}>
          <List
            dataSource={k.pending}
            renderItem={(p) => (
              <List.Item
                actions={[
                  <Button
                    key="ok"
                    type="primary"
                    size="small"
                    onClick={() => k.approvePeer(p.device_id)}
                  >
                    {S.pendingApprove}
                  </Button>,
                  <Button key="no" size="small" onClick={() => k.rejectPeer(p.device_id)}>
                    {S.pendingReject}
                  </Button>,
                ]}
              >
                <List.Item.Meta title={p.name} description={p.addr} />
              </List.Item>
            )}
          />
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            {S.pendingHint}
          </Typography.Text>
        </Card>
      )}

      <Card title={S.devicesTitle} extra={<PeerList.Toolbar k={k} />}>
        <PeerList k={k} />
      </Card>

      <Card
        title={S.receiveMixer}
        extra={
          <Tag color={sources.length > 0 ? "green" : "default"}>
            {sources.length} / 8 路
          </Tag>
        }
      >
        {sources.length === 0 ? (
          <Empty description={S.receiveEmpty} image={Empty.PRESENTED_IMAGE_SIMPLE}>
            <Typography.Text type="secondary">{S.receiveEmptyHint}</Typography.Text>
          </Empty>
        ) : (
          <List
            dataSource={sources}
            renderItem={(s) => (
              <div className="mixer-row">
                <span className={s.active ? "source-active" : "source-idle"} />
                <div className="mixer-name">
                  <div>{s.name}</div>
                  <div className="mixer-meta">
                    {s.mode} · 缓冲 <span className="latency-value">{s.buffer_ms}ms</span>
                    {" · "}
                    {/* 端到端延迟要时钟同步之后才有意义，没同步时如实显示 */}
                    延迟{" "}
                    {s.synced ? (
                      <span className="latency-value">{s.latency_ms}ms</span>
                    ) : (
                      <span style={{ color: "#86909c" }}>同步中</span>
                    )}
                    {" · "}包 {s.packets} · 丢 {s.lost}
                  </div>
                </div>
                <Slider
                  style={{ width: 120 }}
                  min={0}
                  max={200}
                  defaultValue={100}
                  tooltip={{ formatter: (v) => `${v}%` }}
                  onChange={(v) => k.setSourceVolume(s.stream_id, v / 100)}
                />
                <Switch
                  size="small"
                  checkedChildren="静音"
                  unCheckedChildren="发声"
                  onChange={(muted) => k.setSourceMuted(s.stream_id, muted)}
                />
                <Button
                  size="small"
                  onClick={() => k.setSourceSolo(s.stream_id, true)}
                  title="独奏"
                >
                  独奏
                </Button>
              </div>
            )}
          />
        )}
      </Card>
    </Space>
  );
}