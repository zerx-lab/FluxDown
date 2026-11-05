// agent 自有 DTO（native/protocol/src/agent.rs）：FluxCloud 账号、网关、同步、跨设备任务、捕获、诊断等。
// 全部为服务端输出，字段均必带（`Option<T>` → `T | null`）。

import type { JsonValue } from './common';
import type { ErrorReason } from './error';
import type { MarketEntryDto } from './plugin';
import type { CreateGroupRequest } from './queue';
import type { CreateTaskRequest } from './task';

// ── 账号 ──

export type CloudConnectionState = 'disconnected' | 'connecting' | 'connected' | 'reconnecting';

/** 任务 SSE + 在线租约 + 最新设备名册均成功才 connected；独立于配置同步连接。 */
export interface CloudConnectionDto {
  state: CloudConnectionState;
  lastError?: string;
  lastErrorReason?: ErrorReason;
}

export type CloudUserStatus = 'active' | 'disabled' | 'pending' | 'unknown';

/** FluxCloud 用户公开资料。 */
export interface CloudUser {
  id: string;
  email: string;
  nickname: string;
  plan: string;
  status: CloudUserStatus;
  createdAt: string;
  lastLoginAt: string | null;
  /** Origin ID（数字身份号）。 */
  originId: number | null;
  originIdChanged: boolean;
  membershipOrdinal: number | null;
  /** 是否已设置登录密码；旧版云端不下发时为 `null`（未知）。 */
  hasPassword: boolean | null;
}

/** 套餐权益集合：前向兼容，未知键原样保留。 */
export type Entitlements = Record<string, JsonValue>;

export interface CloudPlanCampaignStage {
  label: string;
  priceMinor: number;
  quota: number | null;
}

export interface CloudPlanCampaign {
  name: string;
  endAt: string | null;
  stages: CloudPlanCampaignStage[];
  soldTotal: number;
  stageSold: number[];
  currentStageIndex: number;
  effectivePriceMinor: number;
}

/** 套餐（`agent.plan.list` 元素）。金额单位为「分」（`*Minor`）。 */
export interface CloudPlan {
  code: string;
  name: string;
  description: string;
  badge: string | null;
  icon: string;
  color: string;
  badgeStyle: string;
  badgeColor: string;
  badgeNumbered: boolean;
  badgeNumberDigits: number;
  priceMinor: number;
  currency: string;
  highlights: string[];
  /** wire 键名为 `entitlements`。 */
  entitlements: Record<string, JsonValue>;
  sort: number;
  campaign: CloudPlanCampaign | null;
  purchasable: boolean;
  /** 周期会员卡周期天数；null / 缺失 = 永久套餐。 */
  billingPeriodDays?: number | null;
}

/** 当前账户资料：用户字段平铺在顶层，与 `entitlements` / `currentPlan` 同级。 */
export type CloudProfile = CloudUser & {
  entitlements: Entitlements;
  currentPlan: CloudPlan | null;
  purchaseCreditMinor: number;
  activeSubscription?: CloudSubscription | null;
};

/** 生效中的周期会员卡。 */
export interface CloudSubscription {
  planCode: string;
  planName: string;
  startsAt: string;
  endsAt: string;
}

export interface OriginIdCheckResult {
  available: boolean;
  reason: string | null;
}

/** 受信任设备公开投影。 */
export interface CloudDevice {
  id: string;
  deviceId: string;
  name: string;
  platform: string | null;
  createdAt: string;
  lastSeenAt: string;
  lastIp: string | null;
  appVersion: string | null;
  isOnline: boolean;
  isCurrent: boolean;
  /** 设备自报的默认下载目录（目标设备本地路径）。 */
  defaultSaveDir?: string | null;
  /** 设备自报的路径风格；缺失时按 platform 推断（见 `effectivePathStyle`）。 */
  pathStyle?: PathStyle | null;
}

/** 设备本地文件路径的书写风格。 */
export type PathStyle = 'windows' | 'posix' | 'unknown';

/** 按平台名推断路径风格（对应 Rust `PathStyle::from_platform`）。 */
export function pathStyleFromPlatform(platform: string | null | undefined): PathStyle | null {
  switch ((platform ?? '').trim().toLowerCase()) {
    case 'windows':
    case 'win32':
      return 'windows';
    case 'macos':
    case 'darwin':
    case 'linux':
    case 'android':
    case 'ios':
    case 'freebsd':
    case 'openbsd':
    case 'netbsd':
      return 'posix';
    default:
      return null;
  }
}

/** 设备自报风格，未上报（或 unknown）时按平台推断（对应 Rust `effective_path_style`）。 */
export function effectivePathStyle(device: {
  pathStyle?: PathStyle | null;
  platform?: string | null;
}): PathStyle | null {
  if (device.pathStyle && device.pathStyle !== 'unknown') return device.pathStyle;
  return pathStyleFromPlatform(device.platform);
}

/** `path` 是否为该风格下的绝对路径（对应 Rust `PathStyle::is_absolute`）。 */
export function isAbsolutePath(style: PathStyle, path: string): boolean {
  const p = path.trim();
  switch (style) {
    case 'windows':
      return /^[A-Za-z]:[\\/]/.test(p) || p.startsWith('\\\\');
    case 'posix':
      return p.startsWith('/');
    default:
      return false;
  }
}

/** 无 token 的本地会话视图。 */
export interface AgentSessionDto {
  user: CloudUser;
  entitlements: Entitlements;
  currentPlan: CloudPlan | null;
  activeSubscription?: CloudSubscription | null;
  device: CloudDevice;
}

/**
 * 登录/注册类方法的结果（`status` 内部标记）。
 * `agent.auth.register` / `sendCode` 路径没有 token 时同样返回 `deviceVerificationRequired`
 * （此时 `ttlSeconds` 是验证码有效期）。
 */
export type AgentLoginResult =
  | { status: 'ok'; session: AgentSessionDto }
  | {
      status: 'deviceVerificationRequired';
      ttlSeconds: number;
      willReplaceDevices: boolean;
    };

// ── 订单 / 推介 ──

export interface CloudOrder {
  orderNo: string;
  planCode: string;
  planName: string;
  /** 默认 `pending`。 */
  status: string;
  amountMinor: number;
  listPriceMinor: number;
  creditMinor: number;
  upgradeFromPlan: string | null;
  currency: string;
  campaignName: string | null;
  stageLabel: string | null;
  referralCode: string | null;
  referralDiscountMinor: number;
  /** 微信 Native 支付二维码链接。 */
  codeUrl: string | null;
  createdAt: string;
  paidAt: string | null;
  expiresAt: string;
}

export interface CloudReferralRule {
  planCode: string;
  planName: string;
  priceMinor: number;
  discountMinor: number;
  rewardPercent: number;
}

export interface CloudReferralSummary {
  enabled: boolean;
  description: string;
  rewardEnabled: boolean;
  contact: string;
  invitedCount: number;
  pendingRewardMinor: number;
  paidRewardMinor: number;
  totalRewardMinor: number;
  rules: CloudReferralRule[];
}

export interface CloudReferralCode {
  id: string;
  code: string;
  paidOrderCount: number;
  rewardMinor: number;
  createdAt: string;
}

export interface CloudReferralCodesResult {
  total: number;
  items: CloudReferralCode[];
}

export interface CloudReferralRecord {
  id: string;
  buyerLabel: string;
  orderAmountMinor: number;
  rewardMinor: number;
  rewardPercent: number;
  status: string;
  createdAt: string;
  paidAt: string | null;
  referralCode: string | null;
}

export interface CloudReferralRecordsResult {
  total: number;
  items: CloudReferralRecord[];
}

export interface CloudReferralValidateResult {
  valid: boolean;
  discountMinor: number;
  reason: string | null;
}

/** 插件市场收藏概览：`counts` 仅含收藏数大于 0 的插件；未登录时 `mine` 为空。 */
export interface PluginFavoritesDto {
  counts: Record<string, number>;
  mine: string[];
  signedIn: boolean;
}

export interface PluginFavoriteSetRequest {
  pluginId: string;
  favorited: boolean;
}

/** 私有插件目录：未登录 `signedIn=false` 且 `entries` 为空；条目 `channel === 'private'`，`mirrors` 恒空。 */
export interface PrivatePluginListDto {
  signedIn: boolean;
  entries: MarketEntryDto[];
}

/** `version` = 用户确认权限时看到的版本；云端当前可装版本不一致时以 `marketVersionChanged` 拒绝。 */
export interface PrivatePluginInstallRequest {
  pluginId: string;
  version: string;
}

export interface PluginFavoriteResultDto {
  pluginId: string;
  favorited: boolean;
  count: number;
}

// ── 跨设备任务 ──

/** 远端任务状态；未识别的值 Rust 端解析为 `unknown`（不接单、不控制）。 */
export type RemoteTaskStatus =
  | 'accepted'
  | 'downloading'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'canceled'
  | 'pending'
  | 'unknown';

export interface RemoteTaskDto {
  id: string;
  fromDevice: string;
  toDevice: string;
  url: string;
  saveDir: string | null;
  fileName: string;
  status: RemoteTaskStatus;
  totalBytes: number | null;
  downloadedBytes: number;
  speed: number;
  /** 0..1。 */
  progress: number;
  error: string | null;
  createdAt: string;
  updatedAt: string;
}

/** 已配对设备（P2P 互联）对外视图。`platform` 缺失时字段省略。 */
export interface LinkDeviceInfo {
  fingerprint: string;
  name: string;
  platform?: string;
  online: boolean;
  pairedAt: number;
  lastSeenAt: number;
  /** 对端自报的默认下载目录；旧版对端省略。 */
  defaultSaveDir?: string;
  pathStyle?: PathStyle;
  /** 本机备注名；`name` 已是生效名（备注名优先）。 */
  customName?: string;
  appVersion?: string;
  /** 对端登录云账号时自报的云设备 id（用于与云设备合并）。 */
  cloudDeviceId?: string;
  /** 该对端可控制本机（本机批准过其「请求控制」配对）。 */
  grantsControl?: boolean;
  /** 本机可控制该对端。 */
  controllable?: boolean;
  /** 对端已解除配对。 */
  revoked: boolean;
}

/** 局域网发现的对端（`agent.link.probe` 结果 / `linkDiscoveredChanged`）。 */
export interface LinkDiscoveredPeer {
  fingerprint?: string;
  name: string;
  platform?: string;
  host: string;
  port: number;
  appVersion?: string;
  /** `mdns` | `manual`。 */
  source: string;
}

/** 等待本机确认的入站局域网配对请求。 */
export interface LinkPairingRequestDto {
  sessionId: string;
  peerName: string;
  peerFingerprint: string;
  peerPlatform?: string | null;
  /** 双方肉眼核对的短认证串。 */
  sas: string;
  /** 对端请求「完全控制」本机（批准后其可远程控制本机）；旧版省略。 */
  requestsControl?: boolean;
  expiresAtUnixMs: number;
  /** 生成 DTO 时刻的剩余毫秒；旧主机省略。 */
  ttlMs?: number;
}

/** 本机当前展示的局域网配对码。 */
export interface LinkPairingCodeDto {
  code: string;
  expiresAtUnixMs: number;
  /** 生成 DTO 时刻的剩余毫秒；客户端以收到时刻 + ttlMs 倒计时（免疫时钟偏差），旧主机省略。 */
  ttlMs?: number;
  /** 空数组 = 网关仅监听 127.0.0.1，需开启局域网访问。 */
  addresses: string[];
  fingerprint: string;
  deviceName: string;
}

export interface LinkPairBeginResponse {
  token: string;
  sas: string;
  peerName: string;
  peerFingerprint: string;
}

export interface LinkPairFinishResponse {
  paired: boolean;
  device?: LinkDeviceInfo;
}

export interface LinkControlCredentialDto {
  token: string;
}

// ── 网关 / 外壳 / 电源 / 同步 / 偏好 ──

/** UI Gateway 运行状态；永远不携带 token 文本（用 revealToken 获取）。 */
export interface GatewayStatusDto {
  takeoverEnabled: boolean;
  jsonrpcEnabled: boolean;
  apiEnabled: boolean;
  mcpEnabled: boolean;
  corsEnabled: boolean;
  userTokenConfigured: boolean;
  /** 当前已验证可用的实际监听端口；修改失败时保持原值。 */
  port: number;
  /** server 模式或环境固定监听地址时为 false。 */
  portEditable: boolean;
  /** 是否对局域网开放兼容 API；修改后下次 agent 启动生效。 */
  lanEnabled: boolean;
  /** 本次运行网关是否实际监听非回环地址。 */
  lanActive: boolean;
}

export type TrayUnavailableReason = 'notBuilt' | 'noDisplay' | 'noHost' | 'initFailed';

/** agent 全局快捷键后端（Rust `GlobalShortcutBackend`）。 */
export type GlobalShortcutBackend = 'unavailable' | 'windows' | 'macOs' | 'x11' | 'waylandPortal';

/** 单个全局快捷键的注册结果（Rust `GlobalShortcutState`）。 */
export type GlobalShortcutState = 'active' | 'inUse' | 'unsupported' | 'permissionRequired' | 'failed';

export interface GlobalShortcutStatusDto {
  action: string;
  /** 规范触发器文本，如 `ctrl-shift-d` / `double:ctrl`。 */
  trigger: string;
  state: GlobalShortcutState;
  detail?: string | null;
}

export interface GlobalShortcutsStatusDto {
  backend: GlobalShortcutBackend;
  entries: GlobalShortcutStatusDto[];
}

/** agent 系统外壳状态。 */
export interface ShellStatusDto {
  trayAvailable: boolean;
  trayUnavailableReason: TrayUnavailableReason | null;
  /** true：关闭全部 UI 后 agent + daemon 继续驻留。 */
  resident: boolean;
  /** 桌面 agent 的全局快捷键状态（Web 不展示；旧 agent 缺省）。 */
  globalShortcuts?: GlobalShortcutsStatusDto;
}

/** 完成后关机状态。 */
export interface PowerStatusDto {
  /** 已排定的延迟（秒）；null = 未启用。 */
  armedDelaySecs: number | null;
  /** 全部任务结束、开始倒计时后的剩余秒数；等待任务完成阶段为 null。 */
  countdownRemainingSecs: number | null;
}

export interface SyncStatusDto {
  enabled: boolean;
  revision: number;
  dirtyKeys: string[];
  lastError: string | null;
  lastErrorReason?: ErrorReason | null;
  /** 同步事件流已连通。 */
  connected?: boolean;
  /** 不可自动恢复的错误（设备超限/未受信任）暂停重试。 */
  halted?: boolean;
  lastSyncedAtUnixMs?: number | null;
  /** 本设备不参与云同步的同步目录键。 */
  localOnlyKeys?: string[];
}

/**
 * agent 自有偏好及其原子版本。`values` 的值类型随键而异（布尔/整数/浮点/字符串，
 * `custom_categories` 可为 JSON 字符串或数组）。
 */
export interface AgentPreferencesDto {
  revision: number;
  values: Record<string, JsonValue>;
}

/** 偏好键：自定义分类列表。 */
export const CUSTOM_CATEGORIES_PREF_KEY = 'custom_categories';

/** 自定义分类（沿用 `custom_categories` 持久化 JSON 形状）。 */
export interface CustomCategoryDto {
  id: string;
  name: string;
  icon: string;
  /** `extension` | `regex` | `url`。 */
  matchMode: string;
  extensions: string[];
  regexPattern: string;
  /** `url` 模式的通配符模式（`*` 任意串、`?` 单字符、不分大小写、完整匹配整个网址）。 */
  urlPattern: string;
  position: number;
  visible: boolean;
  isBuiltin: boolean;
  builtinType: string | null;
  saveDir: string;
}

/** 分类目录路由（`CategoryRouteDto`）：外部捕获未指定保存目录时随 `daemon.task.create` 交给 daemon。 */
export interface CategoryRouteDto {
  /** 可见普通分类（按 `position`），只保留影响落点的条目。 */
  rules: CategoryRuleDto[];
  /** 「其他」分类的目录；缺省 = 不兜底。 */
  otherDir?: string;
}

/** `CategoryRouteDto.rules` 的单条普通分类。 */
export interface CategoryRuleDto {
  /** `extension` | `regex` | `url`。 */
  matchMode: string;
  extensions?: string[];
  regexPattern?: string;
  /** `url` 模式的通配符模式。 */
  urlPattern?: string;
  /** 缺省 = 不改变落点，只阻止「其他」兜底。 */
  saveDir?: string;
}

// ── 外部捕获 ──

/** 等待官方 UI 确认的外部捕获（不含 cookie/header 原文，仅摘要）。 */
export interface PendingCaptureDto {
  transactionId: string;
  url: string;
  fileName: string;
  createdAtUnixMs: number;
  /** 捕获方声明的文件大小（字节，0 = 未知）。 */
  fileSize: number;
  referrer: string;
  /** 空 = 未指定。 */
  saveDir: string;
  hasCookies: boolean;
  /** 携带的请求头名（不含值）。 */
  headerNames: string[];
}

/** 只读捕获预解析；不消费事务，原 URL/method/body/audioUrl 不可被表单覆盖。 */
export interface CapturePreviewParams {
  transactionId: string;
  request: CreateTaskRequest;
}

/** 最终清单选择建组；成功后消费事务，sourceUrl 恒取捕获原 URL。 */
export interface CaptureCreateGroupParams {
  transactionId: string;
  request: CreateGroupRequest;
  /** 原表单 HTTP Basic 凭据，非空用户名覆盖浏览器 Authorization。 */
  context: CreateTaskRequest;
}

/**
 * `agent.capture.submitTorrentFile` 参数。`silent=true` 全选文件直接建任务；`silent=false`
 * 由 daemon 发 BT 文件选择请求。其余字段缺省时维持 daemon 默认目录 / 队列 / 立即开始。
 */
export interface CaptureSubmitTorrentFileParams {
  path: string;
  silent?: boolean;
  saveDir?: string;
  queueId?: string;
  startPaused?: boolean;
}

// ── 诊断 / 更新 ──

export type DiagnosticLevel = 'info' | 'ok' | 'warn' | 'error';

export interface DiagnosticRepairParams {
  /** `reregister` / `use_this_install` / `enable_service` / `register` / `open_log_dir` / `refreshTrackers` / `refreshEd2kServers`。 */
  action: string;
  target?: string;
}

export interface DiagnosticCheckDto {
  /** 稳定 ID，UI 据此选择标题文案与修复动作。 */
  id: string;
  target: string;
  level: DiagnosticLevel;
  detail: string;
  /** 既有 hint code（如 `HINT_CHECK_DISK`），无则为空。 */
  hint: string;
  /** 可用的就地修复动作；无则为 null。 */
  repair: DiagnosticRepairParams | null;
}

export interface DiagnosticsReportDto {
  generatedAtUnixMs: number;
  appVersion: string;
  platform: string;
  agentDataDir: string;
  daemonConnected: boolean;
  checks: DiagnosticCheckDto[];
}

export interface LogPathsDto {
  agentLogDir: string;
  /** daemon 不可达时为空串。 */
  daemonLogDir: string;
}

export interface ReleaseNoteDto {
  version: string;
  publishedAt: string;
  body: string;
}

export type UpdatePhase =
  | 'idle'
  | 'checking'
  | 'upToDate'
  | 'available'
  | 'downloading'
  | 'ready'
  | 'installing'
  | 'failed';

export type UpdateInstallKind =
  | 'windowsSetup'
  | 'windowsPortable'
  | 'macosApp'
  | 'linuxAppImage'
  | 'linuxDeb'
  | 'linuxArch'
  | 'linuxPortable'
  | 'serverBinary'
  | 'docker'
  | 'synology'
  | 'qnap'
  | 'openwrt'
  | 'fnos'
  | 'androidApk'
  | 'unknown';

export type UpdateManualReason =
  | 'managedPackage'
  | 'notWritable'
  | 'noAsset'
  | 'elevationUnavailable'
  | 'readOnlyLocation'
  | 'unofficialBuild'
  | 'unsupported'
  | 'unknown';

export type UpdateFailure =
  | 'network'
  | 'verify'
  | 'storage'
  | 'install'
  | 'elevationCancelled'
  | 'installIncomplete'
  | 'unknown';

/** 自更新状态（native/protocol `UpdateStatusDto`）；`agent.update.*` 返回它，也是 `AgentSnapshot.update`。 */
export interface UpdateStatusDto {
  phase: UpdatePhase;
  currentVersion: string;
  /** 最近一次检查使用的渠道（`stable` | `frontier`）。 */
  channel: string;
  latestVersion: string;
  hasUpdate: boolean;
  installKind: UpdateInstallKind;
  /** 为空表示可一键「更新并重启」。 */
  manualReason?: UpdateManualReason | null;
  assetName: string;
  assetSize: number;
  downloadedBytes: number;
  /** 下载完成后立即安装（用户已点「更新并重启」，包仍在下载）。 */
  installPending: boolean;
  /** 手动升级用的资产直链；无可用资产时为空串。 */
  downloadUrl: string;
  releasePageUrl: string;
  notes: ReleaseNoteDto[];
  failure?: UpdateFailure | null;
  /** 失败的技术细节（英文）。 */
  errorDetail: string;
  /** 最近一次成功检查的 Unix 毫秒时间戳；0 = 尚未检查成功。 */
  checkedAtMs: number;
}
