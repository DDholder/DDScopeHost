export const SUPPORTED_LOCALES = ["zh-CN", "en"] as const;

export type UiLocale = (typeof SUPPORTED_LOCALES)[number];

export const LOCALE_STORAGE_KEY = "ddscope.locale";

export const EN_MESSAGES = {
  // Topbar & Shell
  "topbar.subtitle": "Phase 1 · MSO Oscilloscope Workbench",
  "topbar.host": "Host",
  "topbar.hostMock": "browser-mock",
  "topbar.hostTauri": "tauri",
  "topbar.detecting": "detecting",
  "topbar.core": "Core",
  "topbar.gpu": "GPU",
  "topbar.ddsf": "DDSF",
  "topbar.seq": "seq #{seq}",
  "topbar.glitchBtn": "⚡ Glitch +80V",
  "topbar.glitchTitle": "Inject a single-sample +80V (128V peak) overvoltage glitch on v_bus",
  "topbar.ram": "RAM",
  "topbar.pinned": "Pinned {size}",
  "topbar.releasePins": "Release Pins",
  "topbar.languageLabel": "Language",
  "topbar.initErrorTitle": "Workspace Initialization Error",
  "topbar.startingTitle": "Starting DDScopeHost High-Frequency Workbench...",
  "topbar.startingDesc":
    "Initializing DDSF v1 data bus, simulation worker, and WebGPU/WebGL2 stage.",

  // Channel Tree Sidebar
  "sidebar.title": "Sources & Channel Tree",
  "sidebar.newSource": "+ New Source",
  "sidebar.expandTitle": "Expand Sources & Channels",
  "sidebar.collapseTitle": "Collapse Sidebar",
  "sidebar.filterPlaceholder": "Filter <source>/<channel>...",
  "sidebar.state.running": "running",
  "sidebar.state.stopped": "stopped",
  "sidebar.state.completed": "completed",
  "sidebar.state.error": "error",
  "sidebar.stop": "Stop",
  "sidebar.start": "Start",
  "sidebar.recActive": "● REC",
  "sidebar.recIdle": "Rec .ddraw",
  "sidebar.recTitle": "Toggle Bypass .ddraw Raw Recorder",
  "sidebar.del": "Del",
  "sidebar.delTitle": "Remove Source",
  "sidebar.seekTitle": "Seek File Position",
  "sidebar.speedTitle": "Playback Speed",
  "sidebar.dragRowTitle": "Drag onto any MSO Oscilloscope pane or click + to bind",
  "sidebar.bindTitle": "Bind to Scope {id}",
  "sidebar.mod.ana": "ANA",
  "sidebar.mod.dig": "DIG",
  "sidebar.mod.fsm": "FSM",
  "sidebar.mod.fft": "FFT",
  "sidebar.valHigh": "HIGH (1)",
  "sidebar.valLow": "LOW (0)",

  // Dockview Stage
  "stage.emptyTitle": "No Active MSO Oscilloscope Panels",
  "stage.emptyDesc":
    "Create a new independent MSO oscilloscope viewport or drag channels from the sidebar.",
  "stage.addScope": "+ Add MSO Scope",
  "stage.unavailable": "Scope #{id} is unavailable.",
  "stage.splitStacked": "Split Stacked",
  "stage.splitSideBySide": "Split Side-by-Side",
  "stage.hint":
    "Drag tabs to split/dock · Scroll on waveform to zoom · Drag channel from tree to bind",

  // Scope Viewport Widget & HUD
  "scope.sweepModeAria": "Sweep Mode",
  "scope.sweep.roll": "Roll",
  "scope.sweep.auto": "Auto",
  "scope.sweep.normal": "Normal",
  "scope.sweep.single": "Single",
  "scope.status.roll": "ROLL",
  "scope.status.armed": "ARMED",
  "scope.status.auto": "AUTO",
  "scope.status.triggered": "TRIG'D",
  "scope.status.latched": "LATCHED · PINNED",
  "scope.status.idle": "IDLE",
  "scope.rearm": "Re-arm",
  "scope.pause": "Pause",
  "scope.resume": "Resume",
  "scope.zeroPan": "Zero Pan",
  "scope.zeroPanTitle": "Reset Pan Offset",
  "scope.overlay": "Overlay",
  "scope.stacked": "Stacked",
  "scope.cursors": "Cursors A/B",
  "scope.syncAria": "Sync Group",
  "scope.syncOff": "Sync: Off",
  "scope.syncA": "Sync: Group A",
  "scope.syncB": "Sync: Group B",
  "scope.spanAria": "Timebase Span",
  "scope.spanPrefix": "Span",
  "scope.trigLabel": "Trig:",
  "scope.trigChannelAria": "Trigger Channel",
  "scope.trigSlopeAria": "Trigger Slope",
  "scope.slopeRising": "↑ Rising",
  "scope.slopeFalling": "↓ Falling",
  "scope.slopeEither": "↕ Either",
  "scope.lvl": "Lvl",
  "scope.hys": "Hys",
  "scope.stateCode": "State",
  "scope.zoomYIn": "Zoom Y In",
  "scope.zoomYOut": "Zoom Y Out",
  "scope.hideTrack": "Hide Track",
  "scope.showTrack": "Show Track",
  "scope.unbindTrack": "Unbind Track",
  "scope.emptyTracksHint": "Drag channels from the left Channel Tree here to bind MSO tracks",
  "scope.dropBanner": "Drop Channel to Bind MSO Track",
  "scope.meas.mean": "Mean",
  "scope.meas.freq": "Freq",
  "scope.meas.duty": "Duty",
  "hud.span": "Span",
  "hud.trig": "Trig",

  // Downlink Bar
  "downlink.collapsedBtn": "▲ TX Downlink & Coalescer",
  "downlink.title": "Full-Duplex TX Downlink & Coalescer",
  "downlink.sliders": "Sliders",
  "downlink.dispatched": "Dispatched",
  "downlink.coalesced": "Coalesced",
  "downlink.queue": "Queue",
  "downlink.injectGlitch": "⚡ Inject +80V Glitch",
  "downlink.collapseTitle": "Collapse TX Downlink Bar",
  "downlink.target": "Target",
  "downlink.priority": "Priority",
  "downlink.prioNormal": "Normal (Coalesce Key / FIFO)",
  "downlink.prioHigh": "High (Preempt Immediate)",
  "downlink.burstBtn": "Burst 10x Drag Test",
  "downlink.burstTitle": "Send 10 rapid keyed updates to verify Latest-Value-Wins coalescing",
  "downlink.modeAscii": "ASCII",
  "downlink.modeHex": "HEX Bytes",
  "downlink.modeKv": "Key = Value",
  "downlink.asciiPlaceholder": "ASCII command...",
  "downlink.keyPlaceholder": "key",
  "downlink.valPlaceholder": "value",
  "downlink.sendTx": "Send TX",
  "downlink.kvNumericError": "KV value must be numeric or empty",
  "downlink.hint":
    "Coalescing key overwrites stale queued commands in-place under TxCoalescer (< 90 ms window).",
  "downlink.logEmpty": "Drag sliders or click Send TX to inspect coalescing & TX receipts.",
  "downlink.coalescedBadge": "COALESCED",

  // Source Wizard Modal
  "wizard.defaultName": "Source #{index}",
  "wizard.ariaLabel": "Create Data Source",
  "wizard.title": "Add Multi-Modal Data Source",
  "wizard.sourceId": "Source ID",
  "wizard.displayName": "Display Name",
  "wizard.transport": "Transport",
  "wizard.transport.sim": "SimSignalTransport (High-Speed Generator)",
  "wizard.transport.serial": "Serial UART / USB-CDC (up to 12 Mbps)",
  "wizard.transport.udp": "UDP Socket (Unicast / Multicast)",
  "wizard.transport.tcpClient": "TCP Client (Auto-Reconnect)",
  "wizard.transport.tcpServer": "TCP Server",
  "wizard.transport.replay": "RawFileReplay (.bin / .ddraw)",
  "wizard.clock": "Clock Engine",
  "wizard.clock.tier2": "Tier 2: 2nd-Order Software PLL",
  "wizard.clock.tier1": "Tier 1: Hardware Tick Drift Anchor",
  "wizard.clock.tier3": "Tier 3: Host Monotonic Clock",
  "wizard.simRate": "Sample Rate (Hz)",
  "wizard.simChannels": "Analog Channels",
  "wizard.simGlitch": "Inject Single-Sample Glitch",
  "wizard.serialPort": "Serial Port",
  "wizard.baudRate": "Baud Rate",
  "wizard.parityFlow": "Parity / Flow",
  "wizard.parityNone": "None",
  "wizard.parityOdd": "Odd",
  "wizard.parityEven": "Even",
  "wizard.stop1": "1 Stop",
  "wizard.stop2": "2 Stop",
  "wizard.flowNone": "No Flow",
  "wizard.udpBind": "UDP Bind Address",
  "wizard.tcpRemote": "Remote Host:Port",
  "wizard.tcpListen": "Listen Bind:Port",
  "wizard.replayPath": "Capture File Path",
  "wizard.replaySpeed": "Playback Speed",
  "wizard.replayFullSpeed": "Offline Full-Speed Mmap",
  "wizard.replayLoop": "Loop File",
  "wizard.codec": "Protocol Codec",
  "wizard.codec.sim": "SimPreset (Analog + Logic + State)",
  "wizard.codec.justFloat": "JustFloat (VOFA+ 00 00 80 7F)",
  "wizard.codec.fireWater": "FireWater (SIMD CSV)",
  "wizard.codec.fixedSchema": "FixedSchemaBinary (Mixed Struct)",
  "wizard.pllRate": "PLL Nominal Sample Rate (Hz)",
  "wizard.hwTick": "Hardware Tick Clock (Hz)",
  "wizard.channelNames": "Channel Names (comma separated)",
  "wizard.autoStart": "Auto-Start Acquisition Immediately",
  "wizard.errSourceId": "Source ID is required",
  "wizard.cancel": "Cancel",
  "wizard.creating": "Creating...",
  "wizard.create": "Create Source",
} as const;

export type MessageKey = keyof typeof EN_MESSAGES;

export const ZH_CN_MESSAGES: Record<MessageKey, string> = {
  // Topbar & Shell
  "topbar.subtitle": "Phase 1 · 多域示波器工作台",
  "topbar.host": "运行模式",
  "topbar.hostMock": "浏览器仿真",
  "topbar.hostTauri": "桌面端",
  "topbar.detecting": "检测中",
  "topbar.core": "内核",
  "topbar.gpu": "渲染",
  "topbar.ddsf": "数据帧",
  "topbar.seq": "帧 #{seq}",
  "topbar.glitchBtn": "⚡ 注入 +80V 尖峰",
  "topbar.glitchTitle": "向母线电压 v_bus 注入单采样点 +80V (峰值 128V) 过压尖峰",
  "topbar.ram": "内存配额",
  "topbar.pinned": "已锁定 {size}",
  "topbar.releasePins": "释放全部锁定",
  "topbar.languageLabel": "界面语言",
  "topbar.initErrorTitle": "工作台初始化失败",
  "topbar.startingTitle": "正在启动 DDScopeHost 高频多域示波工作台...",
  "topbar.startingDesc":
    "正在初始化 DDSF v1 二进制总线、仿真 Worker 与 WebGPU/WebGL2 渲染管线。",

  // Channel Tree Sidebar
  "sidebar.title": "数据源与通道树",
  "sidebar.newSource": "+ 新建数据源",
  "sidebar.expandTitle": "展开数据源与通道树",
  "sidebar.collapseTitle": "收起侧边栏",
  "sidebar.filterPlaceholder": "筛选 <数据源>/<通道>...",
  "sidebar.state.running": "运行中",
  "sidebar.state.stopped": "已停止",
  "sidebar.state.completed": "已完成",
  "sidebar.state.error": "异常",
  "sidebar.stop": "停止",
  "sidebar.start": "启动",
  "sidebar.recActive": "● 录制中",
  "sidebar.recIdle": "录制 .ddraw",
  "sidebar.recTitle": "开关旁路直录 .ddraw 原始数据文件",
  "sidebar.del": "删除",
  "sidebar.delTitle": "移除数据源",
  "sidebar.seekTitle": "定位文件回放进度",
  "sidebar.speedTitle": "回放倍速",
  "sidebar.dragRowTitle": "拖拽至任意 MSO 示波器窗格，或点击 + 快捷绑定",
  "sidebar.bindTitle": "绑定到示波器 {id}",
  "sidebar.mod.ana": "模拟",
  "sidebar.mod.dig": "数字",
  "sidebar.mod.fsm": "状态",
  "sidebar.mod.fft": "频谱",
  "sidebar.valHigh": "高 (1)",
  "sidebar.valLow": "低 (0)",

  // Dockview Stage
  "stage.emptyTitle": "当前无活动 MSO 示波器面板",
  "stage.emptyDesc": "点击下方按钮新建独立 MSO 示波器视口，或从左侧通道树拖拽通道。",
  "stage.addScope": "+ 新建示波器",
  "stage.unavailable": "示波器 #{id} 不可用。",
  "stage.splitStacked": "上下分屏",
  "stage.splitSideBySide": "左右分屏",
  "stage.hint": "拖拽标签可分屏停靠 · 波形区滚轮缩放 · 从左侧拖入通道即可绑定",

  // Scope Viewport Widget & HUD
  "scope.sweepModeAria": "扫描触发模式",
  "scope.sweep.roll": "滚动",
  "scope.sweep.auto": "自动",
  "scope.sweep.normal": "普通",
  "scope.sweep.single": "单次",
  "scope.status.roll": "滚动采集",
  "scope.status.armed": "已武装待触发",
  "scope.status.auto": "自动自由扫描",
  "scope.status.triggered": "已触发",
  "scope.status.latched": "已锁存 · 内存固定",
  "scope.status.idle": "空闲",
  "scope.rearm": "重新武装",
  "scope.pause": "暂停",
  "scope.resume": "恢复",
  "scope.zeroPan": "平移归零",
  "scope.zeroPanTitle": "重置水平平移偏移",
  "scope.overlay": "叠加视图",
  "scope.stacked": "分栏视图",
  "scope.cursors": "测量光标 A/B",
  "scope.syncAria": "同步分组",
  "scope.syncOff": "同步: 关闭",
  "scope.syncA": "同步: A 组",
  "scope.syncB": "同步: B 组",
  "scope.spanAria": "时基窗宽",
  "scope.spanPrefix": "窗宽",
  "scope.trigLabel": "触发:",
  "scope.trigChannelAria": "触发通道",
  "scope.trigSlopeAria": "触发边沿",
  "scope.slopeRising": "↑ 上升沿",
  "scope.slopeFalling": "↓ 下降沿",
  "scope.slopeEither": "↕ 双边沿",
  "scope.lvl": "电平",
  "scope.hys": "滞回",
  "scope.stateCode": "状态码",
  "scope.zoomYIn": "放大垂直标尺",
  "scope.zoomYOut": "缩小垂直标尺",
  "scope.hideTrack": "隐藏通道",
  "scope.showTrack": "显示通道",
  "scope.unbindTrack": "解绑通道",
  "scope.emptyTracksHint": "请从左侧通道树拖拽通道至此处以绑定 MSO 波形轨道",
  "scope.dropBanner": "释放通道以绑定到当前示波器",
  "scope.meas.mean": "均值",
  "scope.meas.freq": "频率",
  "scope.meas.duty": "占空比",
  "hud.span": "窗宽",
  "hud.trig": "触发电平",

  // Downlink Bar
  "downlink.collapsedBtn": "▲ 全双工下行指令与并合器",
  "downlink.title": "全双工下行发送 & 原位并合调试栏",
  "downlink.sliders": "滑块事件",
  "downlink.dispatched": "已下发",
  "downlink.coalesced": "已并合",
  "downlink.queue": "队列深度",
  "downlink.injectGlitch": "⚡ 注入 +80V 尖峰",
  "downlink.collapseTitle": "收起下行调试栏",
  "downlink.target": "目标源",
  "downlink.priority": "优先级",
  "downlink.prioNormal": "普通 (按 Key 并合 / FIFO)",
  "downlink.prioHigh": "高优先级 (立即抢占下发)",
  "downlink.burstBtn": "10连发并合测试",
  "downlink.burstTitle": "连续发送 10 次同 Key 参数更新以验证 TxCoalescer 最新值覆盖并合",
  "downlink.modeAscii": "ASCII 文本",
  "downlink.modeHex": "HEX 字节流",
  "downlink.modeKv": "键值对",
  "downlink.asciiPlaceholder": "输入 ASCII 控制指令...",
  "downlink.keyPlaceholder": "参数名",
  "downlink.valPlaceholder": "数值",
  "downlink.sendTx": "发送指令",
  "downlink.kvNumericError": "键值必须为有效数字或留空",
  "downlink.hint": "相同 Coalescing Key 在 < 90 ms 调度窗口内会由 TxCoalescer 原位覆盖过时指令。",
  "downlink.logEmpty": "拖动滑块或点击发送指令，以实时观测指令并合计数与回执日志。",
  "downlink.coalescedBadge": "已并合",

  // Source Wizard Modal
  "wizard.defaultName": "数据源 #{index}",
  "wizard.ariaLabel": "新建数据源向导",
  "wizard.title": "新建多模态数据源",
  "wizard.sourceId": "数据源 ID",
  "wizard.displayName": "显示名称",
  "wizard.transport": "传输通道",
  "wizard.transport.sim": "SimSignalTransport (高速仿真信号发生器)",
  "wizard.transport.serial": "串口 UART / USB-CDC (最高 12 Mbps)",
  "wizard.transport.udp": "UDP 套接字 (单播 / 组播)",
  "wizard.transport.tcpClient": "TCP 客户端 (自动重连)",
  "wizard.transport.tcpServer": "TCP 服务端监听",
  "wizard.transport.replay": "RawFileReplay 离线回放 (.bin / .ddraw)",
  "wizard.clock": "时钟同步引擎",
  "wizard.clock.tier2": "Tier 2: 二阶软件锁相环 (PLL)",
  "wizard.clock.tier1": "Tier 1: 硬件计数器漂移锚定",
  "wizard.clock.tier3": "Tier 3: 宿主机单调到达时间戳",
  "wizard.simRate": "采样率 (Hz)",
  "wizard.simChannels": "模拟通道数",
  "wizard.simGlitch": "注入单采样点过压尖峰",
  "wizard.serialPort": "串口设备名",
  "wizard.baudRate": "波特率",
  "wizard.parityFlow": "校验 / 停止位 / 流控",
  "wizard.parityNone": "无校验",
  "wizard.parityOdd": "奇校验",
  "wizard.parityEven": "偶校验",
  "wizard.stop1": "1 停止位",
  "wizard.stop2": "2 停止位",
  "wizard.flowNone": "无流控",
  "wizard.udpBind": "UDP 绑定地址",
  "wizard.tcpRemote": "远端主机:端口",
  "wizard.tcpListen": "本地监听地址:端口",
  "wizard.replayPath": "录制文件路径",
  "wizard.replaySpeed": "回放倍速",
  "wizard.replayFullSpeed": "全速离线 Mmap 回放",
  "wizard.replayLoop": "循环回放文件",
  "wizard.codec": "协议编解码器",
  "wizard.codec.sim": "SimPreset (模拟 + 数字 + 状态机预设)",
  "wizard.codec.justFloat": "JustFloat (VOFA+ 00 00 80 7F)",
  "wizard.codec.fireWater": "FireWater (SIMD 高速 CSV)",
  "wizard.codec.fixedSchema": "FixedSchemaBinary (混合结构体二进制)",
  "wizard.pllRate": "PLL 标称采样率 (Hz)",
  "wizard.hwTick": "硬件计数时钟频率 (Hz)",
  "wizard.channelNames": "通道名称列表 (逗号分隔)",
  "wizard.autoStart": "创建后立即启动采集",
  "wizard.errSourceId": "请输入数据源 ID",
  "wizard.cancel": "取消",
  "wizard.creating": "正在创建...",
  "wizard.create": "创建数据源",
};

const DICTIONARIES: Record<UiLocale, Record<MessageKey, string>> = {
  "zh-CN": ZH_CN_MESSAGES,
  en: EN_MESSAGES,
};

export function isSupportedLocale(raw: unknown): raw is UiLocale {
  return raw === "zh-CN" || raw === "en";
}

export function detectInitialLocale(): UiLocale {
  if (typeof window !== "undefined") {
    try {
      const saved = window.localStorage.getItem(LOCALE_STORAGE_KEY);
      if (isSupportedLocale(saved)) {
        return saved;
      }
    } catch {
      // Ignore storage access restriction
    }
  }
  if (typeof navigator !== "undefined" && typeof navigator.language === "string") {
    if (navigator.language.toLowerCase().startsWith("zh")) {
      return "zh-CN";
    }
  }
  return "en";
}

export function persistLocale(locale: UiLocale): void {
  if (typeof window !== "undefined") {
    try {
      window.localStorage.setItem(LOCALE_STORAGE_KEY, locale);
    } catch {
      // Ignore storage access restriction
    }
  }
  if (typeof document !== "undefined" && document.documentElement) {
    document.documentElement.lang = locale;
  }
}

export function translate(
  locale: UiLocale,
  key: MessageKey,
  params?: Record<string, string | number>,
): string {
  const dict = DICTIONARIES[locale] ?? EN_MESSAGES;
  let template = dict[key] ?? EN_MESSAGES[key];
  if (!params) return template;
  for (const [k, v] of Object.entries(params)) {
    template = template.replaceAll(`{${k}}`, String(v));
  }
  return template;
}

const DEFAULT_SCOPE_TITLES: Record<"scope1" | "scope2", Record<UiLocale, string>> = {
  scope1: {
    en: "MSO Scope 1 · Motor Live (Roll)",
    "zh-CN": "MSO 示波器 1 · 电机实时监测 (滚动)",
  },
  scope2: {
    en: "MSO Scope 2 · Glitch Capture (Single)",
    "zh-CN": "MSO 示波器 2 · 母线尖峰捕捉 (单次)",
  },
};

export function getDefaultScopeTitle(
  kind: "scope1" | "scope2" | "generic",
  locale: UiLocale,
  scopeId?: number,
): string {
  if (kind === "scope1" || kind === "scope2") {
    return DEFAULT_SCOPE_TITLES[kind][locale];
  }
  const idNum = scopeId ?? 1;
  return locale === "zh-CN" ? `MSO 示波器 ${idNum}` : `MSO Scope ${idNum}`;
}

export function translateDefaultScopeTitle(title: string, targetLocale: UiLocale): string {
  if (
    title === DEFAULT_SCOPE_TITLES.scope1.en ||
    title === DEFAULT_SCOPE_TITLES.scope1["zh-CN"]
  ) {
    return DEFAULT_SCOPE_TITLES.scope1[targetLocale];
  }
  if (
    title === DEFAULT_SCOPE_TITLES.scope2.en ||
    title === DEFAULT_SCOPE_TITLES.scope2["zh-CN"]
  ) {
    return DEFAULT_SCOPE_TITLES.scope2[targetLocale];
  }
  const enMatch = /^MSO Scope (\d+)$/.exec(title);
  if (enMatch) {
    return getDefaultScopeTitle("generic", targetLocale, Number(enMatch[1]));
  }
  const zhMatch = /^MSO 示波器 (\d+)$/.exec(title);
  if (zhMatch) {
    return getDefaultScopeTitle("generic", targetLocale, Number(zhMatch[1]));
  }
  return title;
}
