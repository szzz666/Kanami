import { Segmented, Tooltip } from "antd";
import type { KanamiApi } from "../useKanami";
import { S } from "../strings";

/// 三档模式选择。
///
/// 模式由播放端决定（这是共识），音源端这里切换的是
/// 「作为音源时对新建连接采用的模式」以及本机接收时的解码档位。
export default function ModePicker({ k }: { k: KanamiApi }) {
  const options = [
    { label: S.modeStandard, value: "标准" },
    { label: S.modeLowLatency, value: "低延迟" },
    { label: S.modeHighQuality, value: "高音质" },
  ];

  return (
    <Tooltip title={S.modeHint}>
      <Segmented
        value={k.mode}
        options={options}
        onChange={(v) => k.setMode(String(v))}
      />
    </Tooltip>
  );
}
