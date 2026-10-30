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
                    {s.mode} · 缓冲 <span className="latency-value">{s.buffer_ms}ms</span> ·
                    包 {s.packets} · 丢 {s.lost}
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
