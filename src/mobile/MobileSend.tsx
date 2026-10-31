import { Card, Button, Selector, List, Radio, Space, Tag, NoticeBar } from "antd-mobile";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 移动端发送页。
///
/// 移动端发送页。
///
/// 音源列表由后端按平台能力给出：Android 上如果设备支持
/// AudioPlaybackCapture，会多出一个「系统声音」选项；
/// iOS 只可能有麦克风（系统不开放输出采集）。
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

      {/* 平台限制说明。后端早就生成了这些文案
          （macOS 的屏幕录制授权、Android 的采集限制等），
          但一直没渲染——用户遇到权限问题只能看着「正在发送」却没声音发呆。 */}
      {k.platform && k.platform.notes.length > 0 && (
        <NoticeBar
          content={
            <div style={{ fontSize: 12, lineHeight: 1.6 }}>
              {k.platform.notes.map((n, i) => (
                <div key={i}>· {n}</div>
              ))}
            </div>
          }
          color="info"
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

              {/* 选了「系统声音」时给出明确的操作步骤。
                  这一条不加，用户几乎必然踩坑：
                  系统授权对话框默认是「共享一个应用」，
                  选它只能抓那一个应用的音频，而且采集者自己（本应用）
                  始终被排除（防回授）。结果是电平恒为 0、
                  界面显示「正在发送」却没声音——完全看不出哪里错了。
                  实测时我自己的排查也卡在这里很久。 */}
              {k.selectedSource === "system" && (
                <div style={{ marginTop: 10 }}>
                  <NoticeBar
                    content={S.systemAudioHint}
                    color="info"
                    wrap
                  />
                </div>
              )}
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
          {k.busy && k.selectedSource === "system" && !running && (
            <div style={{ marginTop: 8, fontSize: 12, color: "#86909c", textAlign: "center" }}>
              {S.waitingAuth}
            </div>
          )}
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
