import { useState } from "react";
import {
  Button,
  Input,
  List,
  Space,
  Tag,
  Typography,
  Popconfirm,
  Empty,
  Tooltip,
} from "antd";
import {
  ReloadOutlined,
  PlusOutlined,
  DeleteOutlined,
  LinkOutlined,
  DisconnectOutlined,
} from "@ant-design/icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

function Toolbar({ k }: { k: KanamiApi }) {
  const [addr, setAddr] = useState("");
  const [adding, setAdding] = useState(false);

  return (
    <Space.Compact>
      {adding && (
        <Input
          placeholder={S.devicesManualPlaceholder}
          value={addr}
          onChange={(e) => setAddr(e.target.value)}
          onPressEnter={() => {
            if (addr.trim()) {
              k.addPeerManual(addr.trim());
              setAddr("");
              setAdding(false);
            }
          }}
          style={{ width: 220 }}
        />
      )}
      <Button
        icon={<PlusOutlined />}
        onClick={() => {
          if (adding && addr.trim()) {
            k.addPeerManual(addr.trim());
            setAddr("");
          }
          setAdding(!adding);
        }}
      >
        {S.devicesAddManual}
      </Button>
      <Button icon={<ReloadOutlined />} loading={k.scanning} onClick={k.scan}>
        {k.scanning ? S.devicesScanning : S.devicesScan}
      </Button>
    </Space.Compact>
  );
}

function PeerListBody({ k }: { k: KanamiApi }) {
  if (k.peers.length === 0) {
    return <Empty description={S.devicesEmpty} image={Empty.PRESENTED_IMAGE_SIMPLE} />;
  }

  return (
    <List
      dataSource={k.peers}
      renderItem={(p) => {
        const connected = k.status.sources.some((s) => s.device_id === p.device_id);
        return (
          <List.Item
            actions={[
              connected ? (
                <Button
                  key="disconnect"
                  icon={<DisconnectOutlined />}
                  onClick={() => k.disconnectPeer(p.device_id)}
                >
                  {S.devicesDisconnect}
                </Button>
              ) : (
                <Button
                  key="connect"
                  type="primary"
                  icon={<LinkOutlined />}
                  loading={k.busy}
                  onClick={() => k.connectToPeer(p.device_id)}
                >
                  {S.devicesConnect}
                </Button>
              ),
              <Popconfirm
                key="del"
                title="移除这台设备？"
                onConfirm={() => k.removePeer(p.device_id)}
              >
                <Button icon={<DeleteOutlined />} danger />
              </Popconfirm>,
            ]}
          >
            <List.Item.Meta
              title={
                <Space>
                  <Typography.Text>{p.name}</Typography.Text>
                  <Tag color="blue">{p.mode}</Tag>
                  {connected && <Tag color="green">已连接</Tag>}
                </Space>
              }
              description={
                <Space size={12}>
                  <Tooltip title="发现方式">
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      {p.via}
                    </Typography.Text>
                  </Tooltip>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {p.addr}
                  </Typography.Text>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    音频 {p.audio_port}
                  </Typography.Text>
                </Space>
              }
            />
          </List.Item>
        );
      }}
    />
  );
}

const PeerList = Object.assign(PeerListBody, { Toolbar });
export default PeerList;
