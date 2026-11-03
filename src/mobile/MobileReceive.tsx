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
  NoticeBar,
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
      {/* 播放设备出错必须显眼提示。
          cpal 的 error callback 会把设备被拔出、格式不支持记到
          OutputHealth.failed——但之前没有任何地方读它，
          用户只看到「界面一切正常，就是没声音」。 */}
      {sources.length > 0 && k.status.playback_failed && (
        <NoticeBar
          content="播放设备出错，本机听不到声音。请检查音频输出设备是否可用（是否被拔出或被其它程序占用）。"
          color="error"
          wrap
        />
      )}

      {/* 待确认的接入请求。
          这一块必须有：播放端连上来时服务端会回 Reject(NotTrusted)，
          等本机批准后才放行。没有这个入口，用户看到的就是
          「对方一直连不上」，而原因藏在日志里，界面上毫无线索。 */}
      {k.pending.length > 0 && (
        <Card title={S.pendingTitle}>
          <List>
            {k.pending.map((p) => (
              <List.Item
                key={p.device_id}
                description={p.addr}
                extra={
                  <Space>
                    <Button
                      size="mini"
                      color="primary"
                      onClick={() => k.approvePeer(p.device_id)}
                    >
                      {S.pendingApprove}
                    </Button>
                    <Button
                      size="mini"
                      onClick={() => k.rejectPeer(p.device_id)}
                    >
                      {S.pendingReject}
                    </Button>
                  </Space>
                }
              >
                {p.name}
              </List.Item>
            ))}
          </List>
          <div style={{ fontSize: 12, color: "#86909c", paddingTop: 8 }}>
            {S.pendingHint}
          </div>
        </Card>
      )}

      {/* 已授权设备管理。
          之前只有「允许」没有「撤销」：一旦同意过，那台设备就永久
          有访问权，用户没有反悔的途径——只能卸载应用或手删配置文件。 */}
      {k.trusted.length > 0 && (
        <Card title={S.trustedTitle}>
          <List>
            {k.trusted.map((id) => (
              <List.Item
                key={id}
                extra={
                  <Button
                    size="mini"
                    color="danger"
                    onClick={() => k.revokePeer(id)}
                  >
                    {S.trustedRevoke}
                  </Button>
                }
              >
                <span style={{ fontSize: 12 }}>{id}</span>
              </List.Item>
            ))}
          </List>
          <div style={{ fontSize: 12, color: "#86909c", paddingTop: 8 }}>
            {S.trustedHint}
          </div>
        </Card>
      )}

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
              <List.Item
                key={s.stream_id}
                description={
                  `${s.mode} · 缓冲 ${s.buffer_ms}ms · 延迟 ${s.synced ? `${s.latency_ms}ms` : "同步中"}` +
                  // 欠载 = 听到的断续。低延迟档缓冲固定 20ms，网络一抖就欠载，
                  // 不显示的话用户只会觉得「这软件断断续续」，无从判断原因。
                  (s.underruns > 0 ? ` · 断续 ${s.underruns} 次` : "")
                }
              >
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
                      value={Math.round(s.gain * 100)}
                      onChange={(v) => k.setSourceVolume(s.stream_id, (v as number) / 100)}
                    />
                    <Switch
                      checked={s.muted}
                      onChange={(muted) => k.setSourceMuted(s.stream_id, muted)}
                      style={{ "--height": "20px", "--width": "36px" } as never}
                    />
                    {/* 独奏按钮。
                        移动端原来没有这个功能——不是不想做，而是后端
                        status 不回传 solo，做出来也没法显示当前状态。
                        现在补上，按下变蓝表示独奏中。 */}
                    <Button
                      size="mini"
                      color={s.solo ? "primary" : "default"}
                      onClick={() => k.setSourceSolo(s.stream_id, !s.solo)}
                      style={{ minWidth: 44 }}
                    >
                      {s.solo ? "独奏中" : "独奏"}
                    </Button>
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