import { useMemo } from "react";
import {
  Button,
  Card,
  Empty,
  List,
  Select,
  Space,
  Tag,
  Typography,
  Alert,
  Statistic,
  Row,
  Col,
} from "antd";
import { PlayCircleOutlined, PauseCircleOutlined } from "@ant-design/icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";
import ModePicker from "./ModePicker";

/// 发送面板：选音源、起停、看推给了谁。
export default function SendPanel({ k }: { k: KanamiApi }) {
  const running = k.status.sending;

  // 能推给谁：只有扮演播放端的设备才出现在这里
  const targets = useMemo(
    () => k.peers.filter((p) => p.role === "Player" || p.role === "Both"),
    [k.peers],
  );

  return (
    <Space direction="vertical" size={16} style={{ width: "100%" }}>
      <Card
        title={S.sendTitle}
        extra={<ModePicker k={k} />}
        actions={[
          running ? (
            <Button
              key="stop"
              type="primary"
              danger
              icon={<PauseCircleOutlined />}
              loading={k.busy}
              onClick={k.stopSending}
            >
              {S.sendStop}
            </Button>
          ) : (
            <Button
              key="start"
              type="primary"
              icon={<PlayCircleOutlined />}
              loading={k.busy}
              disabled={!k.selectedSource}
              onClick={k.startSending}
            >
              {S.sendStart}
            </Button>
          ),
        ]}
      >
        <Space direction="vertical" size={12} style={{ width: "100%" }}>
          <div>
            <Typography.Text type="secondary">{S.sendSourceHint}</Typography.Text>
          </div>
          <Select
            value={k.selectedSource}
            onChange={k.setSelectedSource}
            disabled={running}
            style={{ width: "100%" }}
            options={k.sources.map((s) => ({
              value: s.id,
              label: `${s.name} · ${s.sample_rate}Hz ${s.channels}ch`,
            }))}
            placeholder={S.sendSource}
            notFoundContent={<Empty description="没有可用音源" />}
          />

          {k.platform && !k.platform.process_exclusion && (
            <Alert
              type="warning"
              showIcon
              message="本机同时收发时可能产生回授"
              description="当前平台不支持采集时排除本进程。建议戴耳机，或关闭本机的接收功能。"
            />
          )}

          {running && (
            <Row gutter={16}>
              <Col span={12}>
                <Statistic title="已发送帧" value={k.status.outgoing_frames} />
              </Col>
              <Col span={12}>
                <Statistic title="音频端口" value={k.status.audio_port} />
              </Col>
            </Row>
          )}
        </Space>
      </Card>

      <Card title={S.sendTargets} size="small">
        <Typography.Paragraph type="secondary" style={{ marginTop: -4 }}>
          {S.sendTargetsHint}
        </Typography.Paragraph>
        {targets.length === 0 ? (
          <Empty description={S.sendTargetsEmpty} image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : (
          <List
            size="small"
            dataSource={targets}
            renderItem={(p) => (
              <List.Item>
                <Space>
                  <Typography.Text>{p.name}</Typography.Text>
                  <Tag>{p.mode}</Tag>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {p.addr}
                  </Typography.Text>
                </Space>
              </List.Item>
            )}
          />
        )}
      </Card>
    </Space>
  );
}
