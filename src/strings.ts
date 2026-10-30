// 所有界面文案集中在这里。
//
// 现在只有中文。以后要加英文，只需要在旁边加一个 en 映射，
// 不用回头去改组件——这是当初定的「文案集中管理」。

export const S = {
  appName: "Kanami",
  tagline: "把本机声音送到局域网的任何设备",

  // 导航
  navSend: "发送",
  navReceive: "接收",
  navSettings: "设置",

  // 发送
  sendTitle: "发送声音",
  sendIdle: "未在发送",
  sendRunning: "正在发送",
  sendStart: "开始发送",
  sendStop: "停止发送",
  sendSource: "音源",
  sendSourceHint: "选择要从哪里抓声音",
  sendTargets: "推送到",
  sendTargetsEmpty: "还没有播放设备接入",
  sendTargetsHint: "播放设备主动连上来之后会出现在这里",

  // 接收
  receiveTitle: "接收声音",
  receiveIdle: "未在接收",
  receiveRunning: "正在接收",
  receiveEmpty: "还没有接入任何音源",
  receiveEmptyHint: "从「设备」里挑一台音源设备连上来",
  receiveMixer: "混音台",

  // 模式
  modeStandard: "标准",
  modeLowLatency: "低延迟",
  modeHighQuality: "高音质",
  modeLabel: "音质模式",
  modeHint: "模式由播放端决定，音源端可以降级",

  // 设备
  devicesTitle: "设备",
  devicesScan: "扫描",
  devicesScanning: "扫描中",
  devicesEmpty: "没有发现设备",
  devicesAddManual: "手动添加",
  devicesManualPlaceholder: "192.168.1.10:47822",
  devicesConnect: "连接",
  devicesDisconnect: "断开",
  devicesVia: "发现方式",

  // 授权
  pendingTitle: "有设备请求接入",
  pendingApprove: "允许",
  pendingReject: "拒绝",
  pendingHint: "允许后会记住这台设备，下次不再询问",

  // 统计
  statsLatency: "延迟",
  statsBuffer: "缓冲",
  statsLost: "丢包",
  statsSources: "音源数",

  // 设置
  settingsTitle: "设置",
  settingsOutput: "播放设备",
  settingsOutputHint: "选择从哪个喇叭出声",
  settingsPlatform: "平台能力",
  settingsNotes: "平台说明",
  settingsVersion: "版本",

  // 通用
  commonCancel: "取消",
  commonConfirm: "确定",
  commonUnknown: "未知",
  commonUnsupported: "不支持",
  commonSupported: "支持",
  commonLoading: "加载中",
  commonError: "出错了",
  commonRetry: "重试",
} as const;

export type StringKey = keyof typeof S;
