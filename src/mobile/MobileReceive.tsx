import { useState } from "react";
import {
  Card,
  List,
  Button,
  Slider,
  Switch,
  Tag,
  Space,
  Input,
  Toast,
} from "antd-mobile";
import { AddOutline, RedoOutline } from "antd-mobile-icons";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 移动端接收页：设备列表 + 混音台。
export default function MobileReceive({ k }: { k: KanamiApi }) {
  const [addr, setAddr] = useState("");
  const [showManual, setShowManual] = useState(false);

  const sources = k.status.sources;

  return (
    <Space direction="vertical" style={{ width: "100%", "--gap": "12px" } as never}>
      <Card
        title={S.devicesTitle}
        extra={
          <Space>
            <Button size="mini" onClick={() => setShowManual(!showManual)}>
              <AddOutline />
            </Button>
            <Button size="mini" loading={k.scanning} onClick={k.scan}>
              <RedoOutline />
            </Button>
          </Space>
        }
      >
        {showManual && (
          <div style={{ padding: "0 0 12px" }}>
            <Space>
              <Input
                placeholder={S.devicesManualPlaceholder}
                value={addr}
                onChange={setAddr}
                style={{ "--text-align": "left", width: 180 } as never}
              />
              <Button
                size="small"
                color="primary"
                onClick={() => {
                  if (!addr.trim()) {
                    Toast.show("请填写地址");
                    return;
                  }
                  k.addPeerManual(addr.trim());
                  setAddr("");
                  setShowManual(false);
                }}
              >
                添加
              </Button>
            </Space>
          </div>
        )}

        {k.peers.length === 0 ? (
          <div className="empty-hint">{S.devicesEmpty}</div>
        ) : (
          <List>
            {k.peers.map((p) => {
              const connected = sources.some((s) => s.device_id === p.device_id);
              return (
                <List.Item
                  key={p.device_id}
                  description={`${p.via} · ${p.addr}`}
                  extra={
                    <Button
                      size="mini"
                      color={connected ? "danger" : "primary"}
                      onClick={() =>
                        connected ? k.disconnectPeer(p.device_id) : k.connectToPeer(p.device_id)
                      }
                    >
                      {connected ? S.devicesDisconnect : S.devicesConnect}
                    </Button>
                  }
                >
                  <Space>
                    {p.name}
                    <Tag color="primary">{p.mode}</Tag>
                  </Space>
                </List.Item>
              );
            })}
          </List>
        )}
      </Card>

      <Card
        title={S.receiveMixer}
        extra={<Tag color={sources.length > 0 ? "success" : "default"}>{sources.length}/8</Tag>}
      >
        {sources.length === 0 ? (
          <div className="empty-hint">{S.receiveEmptyHint}</div>
        ) : (
          <List>
            {sources.map((s) => (
              <List.Item key={s.stream_id} description={`${s.mode} · 缓冲 ${s.buffer_ms}ms`}>
                <div style={{ width: "100%" }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <span className={s.active ? "source-active" : "source-idle"} />
                    <span>{s.name}</span>
                  </div>
                  <div style={{ display: "flex", alignItems: "center", gap: 12, marginTop: 8 }}>
                    <Slider
                      style={{ flex: 1 }}
                      min={0}
                      max={200}
                      defaultValue={100}
                      onChange={(v) => k.setSourceVolume(s.stream_id, (v as number) / 100)}
                    />
                    <Switch
                      onChange={(muted) => k.setSourceMuted(s.stream_id, muted)}
                      style={{ "--height": "20px", "--width": "36px" } as never}
                    />
                  </div>
                </div>
              </List.Item>
            ))}
          </List>
        )}
      </Card>
    </Space>
  );
}
