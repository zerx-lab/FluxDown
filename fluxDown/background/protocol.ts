/**
 * 扩展内部消息契约（计划 C6）：background ↔ sensor / panel / popup / options / sidepanel。
 *
 * 纯类型 + 常量模块，零运行时依赖；UI、content script 与 background 共同 import。
 * 路由实现在 `background/messages.ts`，端口实现在 `background/ports.ts`。
 */
import type { SourceBufferBinding, PageConfig } from "@/page/bridge";
import type {
  ResponseDecision,
  ResponseInput,
  RuleRef,
  RuleStatus,
} from "@/rules/engine";
import type { RulePack, RevocationDoc, SniffRuleV1 } from "@/rules/schema";
import type { RulesListItem } from "./rule-view";
import type { DashManifest } from "@/utils/dash-manifest";
import type { HlsPlaylistInfo } from "@/utils/hls-playlist";
import type { MediaCandidate } from "@/utils/media-candidates";
import type { ResourceDebugLog } from "@/utils/resource-debug-log";
import type {
  DetectedResource,
  ResourceMessagePayload,
  ResourceType,
} from "@/utils/resource-types";
import type { SiteProfileFields } from "@/utils/site-profiles";
import type { PageScriptSourceResult, PageScriptsActionResult, PageScriptsStatus } from "./page-scripts/model";

// ===== 媒体元素 =====

/** sensor 上报的媒体元素状态；frameId 由 background 用 sender.frameId 填写。 */
export interface ElementState {
  /** `${sid}:${n}`，sid = sensor 会话 id（crypto.randomUUID().slice(0, 8)）。 */
  key: string;
  frameId: number;
  tag: "video" | "audio";
  playing: boolean;
  visibleRatio: number;
  rect: { w: number; h: number };
  videoWidth: number;
  videoHeight: number;
  /** 仅 http(s)，否则 ""。 */
  currentSrc: string;
  lastPlayedAt: number;
  lastInteractionAt: number;
  bindings: SourceBufferBinding[];
  /** `bindings` 所属的 MediaSource 会话（钩子实例 hid + msId）；没有 MSE 归因时缺省。 */
  msId?: number;
  hid?: string;
  /** 元素对应内容的标题（page 规则 / Media Session / 元素属性）；空串表示未知，沿用页面标题。 */
  title: string;
  /** 元素当前来源受 DRM 保护（EME `encrypted` 事件或已挂 MediaKeys）。 */
  encrypted: boolean;
  tracks: { kind: string; srclang: string; label: string; src: string }[];
  /**
   * background 归档的历史播放会话（元素换了来源或被移除）：值为会话键。
   * 归档项不是真实元素，永远不会成为当前播放元素，只承载该会话的来源证据与当时的标题。
   */
  retiredSession?: string;
}

/** MAIN 钩子对 sensor 握手的应答（协议 0 的旧钩子不应答，此时为 null）。 */
export interface HooksInfo {
  protocol: number;
  hid: string;
  /** 钩子在文档加载完成后才装上（扩展安装 / 更新后补注入），已建立的 MSE 关联看不到。 */
  late: boolean;
}

/**
 * 页面探测状态：
 * - live：顶层帧有存活的 sensor；
 * - recovering：正在给缺少 sensor 的页面补注入；
 * - degraded：钩子补注入前页面已开始播放，当前播放元素拿不到来源证据（刷新可恢复）；
 * - unavailable：页面不允许扩展注入（受限页 / 注入失败）；
 * - unknown：尚无结论（页面加载中、非 http 页面等）。
 */
export type ProbeState = "live" | "recovering" | "degraded" | "unavailable" | "unknown";

// ===== 视图模型 =====

export interface CandidateRow {
  candidate: MediaCandidate;
  elementKey: string | null;
  playingVariantId: string | null;
  binding: "exact" | "temporal" | "declared" | "inferred" | "none";
  ruleIds: string[];
}

export interface ResourceRow {
  resource: DetectedResource;
  ruleIds: string[];
}

export type HandoffState =
  | { kind: "choose-target" }
  | { kind: "unavailable"; reason: "disconnected" | "remote-mode" | "old-app" | "plugins-unsupported" }
  | { kind: "no-match"; installed: number }
  | { kind: "disabled"; identity: string; name: string }
  | { kind: "match"; identity: string; name: string };

export interface TabView {
  tabId: number;
  pageUrl: string;
  pageTitle: string;
  version: number;
  activeElementKey: string | null;
  playing: CandidateRow[];
  otherElements: CandidateRow[];
  unbound: CandidateRow[];
  files: ResourceRow[];
  hiddenLowConfidence: number;
  counts: Partial<Record<ResourceType | "all", number>>;
  handoff: HandoffState;
  site: { host: string; profileKey: string | null; profile: SiteProfileFields };
  probe: ProbeState;
}

export interface ElementCandidates {
  rows: CandidateRow[];
}

// ===== 宿主 / 规则同步 =====

export interface ResolverInfo {
  identity: string;
  name: string;
  enabled: boolean;
  loaded: boolean;
  urls: string[];
}

export interface HostCapabilities {
  protocol: 1 | 2;
  pluginsSupported: boolean;
  resolvers: ResolverInfo[];
  connected: boolean;
  /** 桌面端插件规则包当前序号；从未有过插件规则为 0。 */
  pluginRulesSequence: number;
  /** 桌面端 plugin 层实例标识（数据库重置后改变）；旧桌面端 / 无插件时缺省或 null。 */
  pluginRulesEpoch?: string | null;
  /** 桌面端支持的 NMH 动作（`capabilities.actions`）；旧桌面端为空。含 `page_scripts` 才支持页面脚本。 */
  actions: string[];
  /** 页面脚本索引序号；桌面端 daemon 不支持 / 查询失败 / epoch 为空时为 null。 */
  pageScriptsSequence: number | null;
  /** 页面脚本索引 epoch（数据库重置后改变）；与序号同时为 null 或同时有值。 */
  pageScriptsEpoch: string | null;
}

/** 本地已存的规则包（storage.local `fluxdown_rule_packs`）；只放包本身，同步元信息见 {@link StoredSyncMeta}。 */
export interface StoredRulePacks {
  official?: RulePack;
  community?: RulePack;
  plugin?: RulePack;
  /** `plugin` 包所属的桌面端 epoch；变化说明桌面端数据库被重置，plugin 层须整体替换。 */
  pluginEpoch?: string;
  revocations?: RevocationDoc;
}

/** 同步问题代码（稳定，UI 本地化；`detail` 仅作补充）。 */
export type RuleSyncProblemCode =
  /** 桌面端没有应答规则请求。 */
  | "unavailable"
  /** 收到的包 sequence 低于本地已有（拒收）。 */
  | "rollback"
  /** 整个包未通过校验（拒收）。 */
  | "pack_invalid"
  /** 包内部分规则未通过校验（`count` 条被剔除，其余生效）。 */
  | "rules_rejected"
  /** 吊销列表未通过校验（拒收）。 */
  | "revocations_invalid";

export interface RuleSyncProblem {
  code: RuleSyncProblemCode;
  tier?: "official" | "community" | "plugin" | "revocations";
  count?: number;
  detail?: string;
}

/** 桌面端（agent）自己的拉取状态：与本次同步的结论无关，单独展示。 */
export interface HostSyncStatus {
  lastFetchUnix: number | null;
  lastAttemptUnix: number | null;
  /** 各文档最近一次拉取错误的汇总串。 */
  lastError: string | null;
  /** 逐文档错误（null / 缺省 = 无错误）。 */
  errors: Partial<Record<"official" | "community" | "revocations", string | null>>;
  /** 桌面端正在后台拉取。 */
  refreshing: boolean;
}

/** 同步元信息（storage.local `fluxdown_rule_sync_meta`）：独立于规则包，写入不触发引擎重编译。 */
export interface StoredSyncMeta {
  /** 最近一次与桌面端成功完成规则交换的时间。 */
  lastSyncAt?: number;
  /** 最近一次同步尝试（含未同步）的时间与结论。 */
  lastAttemptAt?: number;
  lastOutcome?: RuleSyncOutcome;
  problems?: RuleSyncProblem[];
  host?: HostSyncStatus;
}

export interface RuleDocSummary {
  sequence: number;
  issuedAt: string;
  /** 包内条目数：规则包为规则条数，吊销列表为吊销条目数。 */
  count: number;
  expiresAt?: string;
}

export interface RuleSyncStatus {
  enabled: boolean;
  communityEnabled: boolean;
  official: RuleDocSummary | null;
  community: RuleDocSummary | null;
  plugin: RuleDocSummary | null;
  revocations: RuleDocSummary | null;
  lastSyncAt: number | null;
  lastAttemptAt: number | null;
  lastOutcome: RuleSyncOutcome | null;
  /** 最近一次同步（扩展侧）发现的问题；桌面端自己的拉取错误见 `host`。 */
  problems: RuleSyncProblem[];
  /** 引擎最近一次编译失败的原因（成功后清空）。 */
  compileError: string | null;
  host: HostSyncStatus | null;
}

/**
 * 一次同步的结果：`disabled` 远程同步已关闭（仅同步了本机 plugin 层）、`offline` 桌面端未连接或版本过旧、
 * `unavailable` 桌面端未应答规则请求、`partial` 有包被拒 / 回滚、`fetch-failed` 桌面端应答了但拉取远程出错、
 * `ok` 完成。
 */
export type RuleSyncOutcome = "ok" | "partial" | "fetch-failed" | "disabled" | "offline" | "unavailable";

export interface RuleSyncResult {
  outcome: RuleSyncOutcome;
  status: RuleSyncStatus;
}

export interface HostStatus {
  connected: boolean;
  protocol: 1 | 2;
  capabilities: HostCapabilities;
  ruleSync: RuleSyncStatus;
}

// ===== 消息 =====

export type SendChannel = "local" | "remote";

export interface SendResult {
  success: boolean;
  message?: string;
  channel?: SendChannel;
  /** 桌面端直接建好的任务 ID；交给桌面确认框（尚未建任务）或批量发送时为空。 */
  taskId?: string;
}

export interface PageLink {
  url: string;
  /** ≤ 120 字。 */
  text: string;
  tag: string;
}

/** background 用用户实际生效的规则分类后的页面链接（LinkGrabber 分组用）。 */
export interface ClassifiedLink extends PageLink {
  type: ResourceType;
}

export type RulesListEntry = RulesListItem;

/** 规则测试台里一条命中的规则（按求值顺序）；`winner` = 实际生效的那条，其余被它遮蔽。 */
export interface RulesTestMatch {
  ref: RuleRef;
  decision: "resource" | "ignore";
  winner: boolean;
}

export interface RulesTestResult {
  response: ResponseDecision;
  track: "video" | "audio" | null;
  dedupeKey: string;
  fileExtension: string;
  matches: RulesTestMatch[];
  /** 全局嗅探开关与站点配置是否允许该页面嗅探。 */
  sniffAllowed: boolean;
  /** 真实管线会不会收录这条响应（sniffAllowed 且未被忽略，且命中资源规则或带 attachment）。 */
  wouldIngest: boolean;
  /** 收录时的最终资源类型；不收录为 null。 */
  finalType: ResourceType | null;
  /** 被哪条 ignore 规则丢弃。 */
  ignoredBy: RuleRef | null;
  /** 带草稿测试时，草稿规则自身的编译状态。 */
  draft?: { status: RuleStatus; detail?: string; code?: string; param?: string };
}

/** `rulesTest` 请求：响应输入 + 可选草稿规则（编辑器里尚未保存的规则）。 */
export type RulesTestRequest = ResponseInput & { draft?: SniffRuleV1 };

/** runtime.sendMessage 的请求（action 判别）与对应返回值。 */
export interface MessageMap {
  /** 拉取页面配置；sensor 同时带上 sid 登记本帧的 sensor 会话（页内面板不带）。 */
  getPageConfig: { req: { pageUrl: string; sid?: string }; res: PageConfig };
  sensorReport: { req: { resources: ResourceMessagePayload[] }; res: { added: number } };
  manifestObserved: {
    req: { manifest: DashManifest; manifestUrl: string; pageUrl: string };
    res: { ok: boolean };
  };
  hlsObserved: {
    req: { url: string; playlist: HlsPlaylistInfo; pageUrl: string };
    res: { ok: boolean };
  };
  bodyObserved: {
    req: { url: string; pageUrl: string; ruleIds: string[]; text: string };
    res: { emitted: number };
  };
  /** sensor 执行 PageConfig.dom 规则的命中；background 用 engine.renderDomEmit 渲染后入库。 */
  domObserved: {
    req: { pageUrl: string; hits: { ruleId: string; value: string; text: string; elementKey?: string }[] };
    res: { emitted: number };
  };
  /** 侧边栏诊断「导出资源调试日志」：按 tab 生成已脱敏的嗅探状态快照。 */
  getDebugLog: { req: { tabId: number }; res: ResourceDebugLog };
  preemptDownload: { req: { url: string }; res: { ok: boolean } };
  /** 帧所在源注册的 Service Worker 作用域；background 校验同源后登记。 */
  serviceWorkerScope: { req: { scope: string }; res: { ok: boolean } };
  pageUrlChanged: { req: { pageUrl: string; kind: "push" | "replace" | "pop" }; res: { ok: boolean } };
  /**
   * 元素状态增量；`full` = 本 sid 的全量快照（不在其中的本 sid 元素一律视为已移除）。
   * `resync`：background 不认识这个 sid（SW 重启后登记表丢失等），要求 sensor 再发一次全量。
   */
  mediaElementState: {
    req: { sid: string; elements: Omit<ElementState, "frameId">[]; removed: string[]; full?: boolean };
    res: { ok: boolean; resync?: boolean };
  };
  /** sensor 收到（或超时未收到）钩子握手应答后上报钩子状态。 */
  sensorStatus: { req: { sid: string; hooks: HooksInfo | null }; res: { ok: boolean } };
  /** sensor 上报跳过接管修饰键的按下 / 松开（失焦即松开）；全局状态，见 breaker.setBypassKeyHeld。 */
  bypassKey: { req: { held: boolean }; res: { ok: boolean } };
  /** sensor 上报带跳过接管修饰键的真实点击；`href` = 点中的链接（按钮等非链接为空）。见 breaker.armBypassClick。 */
  bypassClick: { req: { href?: string }; res: { ok: boolean } };
  /** sensor 上报一次真实链接按下（响应头到达前）；像下载时 background 预热 NMH。见 messages.linkPress。 */
  linkPress: { req: { href: string; download: boolean }; res: { ok: boolean } };
  /** 页面探测降级时由 UI 触发：刷新该 tab（内容脚本内的面板没有 tabs API）。 */
  reloadTab: { req: { tabId?: number }; res: { ok: boolean } };
  contextTarget: { req: { elementKey: string | null }; res: { ok: boolean } };
  getElementCandidates: { req: { elementKey: string }; res: ElementCandidates };
  getTabView: { req: { tabId?: number }; res: TabView | null };
  downloadCandidate: {
    req: { tabId?: number; candidateId: string; variantId?: string; channel?: SendChannel };
    res: SendResult;
  };
  downloadResources: {
    req: { tabId?: number; resourceIds: string[]; channel?: SendChannel };
    res: SendResult;
  };
  downloadUrls: {
    req: { urls: string[]; referrer?: string; tabId?: number; channel?: SendChannel };
    res: SendResult;
  };
  interceptMagnet: { req: { url: string; referrer: string }; res: SendResult };
  handoffPage: { req: { tabId: number }; res: SendResult };
  clearResources: { req: { tabId?: number }; res: { ok: boolean } };
  collectLinks: { req: { tabId: number }; res: { links: ClassifiedLink[] } };
  openPanel: { req: Record<never, never>; res: { ok: boolean } };
  rulesTest: { req: RulesTestRequest; res: RulesTestResult };
  rulesSyncNow: { req: Record<never, never>; res: RuleSyncResult };
  getHostStatus: { req: Record<never, never>; res: HostStatus };
  rulesList: { req: Record<never, never>; res: RulesListEntry[] };
  /** 规则全文（列表只带精简字段）；不存在或被隐藏（发布构建的内置规则）时为 null。 */
  ruleGet: { req: { id: string }; res: SniffRuleV1 | null };
  rulesSyncStatus: { req: Record<never, never>; res: RuleSyncStatus };
  /**
   * 页面脚本状态。`refresh`：none = 只读缓存；stale = 缓存超过 1 分钟才同步；force = 立即同步（选项页打开 / 手动重新检查）。
   * 只接受扩展页发来的消息。
   */
  pageScriptsStatus: { req: { refresh?: "none" | "stale" | "force" }; res: PageScriptsStatus };
  /** 首次启用某插件的页面脚本（选项页确认对话框 + 已请求到 `userScripts` 权限之后调用）。 */
  pageScriptsApprove: { req: { identity: string }; res: PageScriptsActionResult };
  /** 停用并移除：注销全部脚本、删令牌与批准记录。 */
  pageScriptsRevoke: { req: { identity: string }; res: PageScriptsActionResult };
  /** 「查看源码」：按需从桌面端拉取并校验 sha256，只读展示，不落 storage。 */
  pageScriptsSource: { req: { identity: string; id: string }; res: PageScriptSourceResult };
  /** 页面内「浏览器下载」提示卡的「在文件夹中显示」；只放行该标签页收到过提示的下载。 */
  showBrowserDownload: { req: { downloadId: number }; res: { ok: boolean } };
}

export type MessageAction = keyof MessageMap;
export type MessageRequest<A extends MessageAction> = { action: A } & MessageMap[A]["req"];
export type MessageResponse<A extends MessageAction> = MessageMap[A]["res"];

/** background → content（tabs.sendMessage）。 */
export type ContentMessage =
  | { action: "pageConfigChanged" }
  | { action: "openPanel" }
  | { action: "getPageCharset" }
  | { action: "collectLinks" }
  /** 探测顶层帧 sensor 是否存活；应答 {@link ProbeReply} 并随后全量重发元素状态。 */
  | { action: "probe" }
  | BrowserDownloadNotice;

/**
 * 浏览器自己下载的文件的页内提示（下载 UI 被本扩展隐藏时）：started → done / failed。
 * 只发给顶层帧；应答 `{ ok: true }` 表示已显示，否则 background 退回系统通知。
 */
export interface BrowserDownloadNotice {
  action: "browserDownload";
  phase: "started" | "done" | "failed";
  downloadId: number;
  /** 文件名（failed 时为空）。 */
  name: string;
}

export interface ProbeReply {
  sid: string;
  hooks: HooksInfo | null;
}

// ===== 端口 =====

export const TAB_VIEW_PORT = "tab-view";

/** UI → background：连接后先发订阅。"sender" = 端口所在 tab（content UI 用）。 */
export interface TabViewSubscribe {
  subscribe: number | "sender";
}

/** background → UI：订阅时与视图版本变化时推送；同一 tab 150 ms 内合并。 */
export interface TabViewPush {
  type: "tabView";
  view: TabView | null;
}

export type { RuleRef };
