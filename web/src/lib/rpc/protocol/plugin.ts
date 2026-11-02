// 插件与托管组件 DTO。

export interface SettingOptionDto {
  value: string;
  label: string;
}

/** 声明式设置项。 */
export interface SettingFieldDto {
  key: string;
  title: string;
  description: string;
  /** `string` / `number` / `boolean`（wire 字段名为 `type`）。 */
  type: string;
  /** `text` / `password` / `textarea` / `select` / `toggle` / `number` / `folder`。 */
  widget: string;
  options: SettingOptionDto[];
  default: string | null;
  required: boolean;
  min: number | null;
  max: number | null;
  pattern: string | null;
  /** 非空时 UI 在字段旁渲染复制按钮（仅复制文本，绝不执行）。 */
  helperScript: string | null;
  helperLabel: string | null;
}

export type PluginRisk = "standard" | "elevated" | "full";

/** 视图的桌面增强入口：仅 GPUI 桌面端经 GPUI Shell 运行，Web 不执行。 */
export interface PluginDesktopEntryDto {
  entry: string;
  width?: number;
  height?: number;
  networkHosts: string[];
}

/** 视图贡献声明。 */
export interface PluginViewDeclDto {
  id: string;
  title: string;
  /** `activity` / `settings` / `taskDetail` / `window`。 */
  location: string;
  /** 是否有可移植实现：无 `desktop` 的视图恒为 true；带 `desktop` 的仅在 manifest 显式 `portable: true` 时为 true。 */
  portable: boolean;
  desktop?: PluginDesktopEntryDto;
  /** 视图图标 SVG 文本（daemon 加载期读入并校验）；只当单色蒙版按主题着色。未声明时缺省，用默认图标。 */
  iconSvg?: string;
}

/** 命令贡献声明。 */
export interface PluginCommandDeclDto {
  id: string;
  title: string;
  palette: boolean;
  taskMenu: boolean;
}

/** 插件贡献点。 */
export interface PluginContributionsDto {
  views: PluginViewDeclDto[];
  commands: PluginCommandDeclDto[];
  /** 是否贡献浏览器扩展嗅探规则。 */
  sniffRules: boolean;
  /** Whether the plugin contributes signed archive classification rules. */
  archiveRules: boolean;
  /** 浏览器页面脚本声明（授权 / 详情界面展示将在哪些网站运行脚本）。 */
  pageScripts: PluginPageScriptDeclDto[];
}

/** 页面脚本贡献声明（manifest `contributes.pageScripts[]` 的展示字段）。 */
export interface PluginPageScriptDeclDto {
  id: string;
  title: string;
  matches: string[];
  /** `isolated` / `main`。 */
  world: string;
}

/** 站点默认插件条目（`daemon.plugin.siteDefaults`，按 site 排序）。 */
/** `daemon.plugin.archiveRules` parameters. */
export interface PluginArchiveRulesParams {
  identity: string;
  refresh?: boolean;
}

/** Status of the publisher-signed classification pack; never includes rule contents. */
export interface PluginArchiveRulesDto {
  sequence: number;
  ruleCount: number;
  lastCheckedAt: number | null;
  lastUpdatedAt: number | null;
  lastError: string | null;
  publicKey: string;
  updateUrl: string | null;
  automaticUpdates: boolean;
}
export interface PluginSiteDefaultDto {
  site: string;
  /** 当前默认插件 identity。 */
  identity: string;
  /** 可选插件 identity 集合。 */
  candidates: string[];
  /** true = 无人值守下载自动选定、尚未确认（普通下载仍会询问）；false = 用户已确认。 */
  automatic: boolean;
}

/**
 * plugin 层嗅探规则包；`pack` 为 null 表示从未有过插件规则（sequence 为 0）。
 * `epoch` 是随序号持久化的实例标识，数据库重置后必变。
 */
export interface PluginSniffRulesDto {
  sequence: number;
  epoch: string;
  pack: Record<string, unknown> | null;
}

/**
 * 插件页面脚本索引（`daemon.plugin.pageScripts`）；sequence / epoch 语义同嗅探规则包。
 * 源码不随索引下发，经 `daemon.plugin.pageScriptSource` 逐条拉取。
 */
export interface PluginPageScriptsDto {
  sequence: number;
  epoch: string;
  scripts: PluginPageScriptDto[];
}

/** 页面脚本索引条目。 */
export interface PluginPageScriptDto {
  identity: string;
  pluginName: string;
  pluginVersion: string;
  id: string;
  title: string;
  matches: string[];
  excludeMatches: string[];
  /** `document_start` / `document_end` / `document_idle`。 */
  runAt: string;
  /** `isolated` / `main`。 */
  world: string;
  allFrames: boolean;
  /** 拼接后源码的 SHA-256 小写十六进制。 */
  sha256: string;
  bytes: number;
}

/** `daemon.plugin.pageScriptSource` 参数。 */
export interface PluginPageScriptSourceParams {
  identity: string;
  id: string;
}

/** 单条页面脚本源码（manifest `js` 各文件以 `"\n;\n"` 拼接）。 */
export interface PluginPageScriptSourceDto {
  identity: string;
  id: string;
  sha256: string;
  code: string;
}

/** 已安装插件视图。 */
export interface PluginDto {
  identity: string;
  name: string;
  version: string;
  description: string;
  homepage: string;
  enabled: boolean;
  devMode: boolean;
  /** `None` / `Manual` / `CircuitBreaker` / `Revoked`。 */
  disabledReason: string;
  settings: SettingFieldDto[];
  /** 当前设置值（key → 字符串）。 */
  settingsValues: Record<string, string>;
  /** manifest 声明的能力权限（如 `["ffmpeg"]`）。 */
  permissions: string[];
  /** 是否声明平台登录入口。 */
  authSupported: boolean;
  subscriptionProviderIds: string[];
  /** manifest resolvers[0].match.urls（glob，供扩展判断页面是否可由插件解析）。 */
  resolverUrls: string[];
  /** `Loaded` / `Failed`；与 `enabled` 独立。 */
  loadStatus: string;
  /** 加载失败原因；成功为空。 */
  loadError: string;
  /** manifest `contributes`：视图 / 命令 / 嗅探规则。 */
  contributions: PluginContributionsDto;
  /** 风险等级（由权限推导）。 */
  risk: PluginRisk;
  /** 尚未授权的权限。 */
  consentPending: string[];
  /** 尚未授权的网络主机。 */
  consentPendingHosts: string[];
  /** 全部桌面视图声明的网络主机。 */
  networkHosts: string[];
  /** 插件包在 daemon 主机上的绝对目录（仅已加载插件有值）。 */
  packageDir?: string;
  /** 插件包修订号：包代码被替换（安装 / 升级 / dev 安装 / dev 重载）后变化，只做相等比较；旧 daemon 不下发。 */
  packageRevision?: number;
}

/** 插件平台登录请求。 */
export interface PluginAuthRequest {
  identity?: string;
  /** `begin` / `poll` / `cancel` / `logout` / `status`。 */
  action: string;
  site?: string;
  authRef?: string;
  sessionId?: string;
  /** 账号、验证码等额外输入。 */
  input?: string;
}

export interface PluginAuthResponse {
  /** `pending` / `success` / `error`。 */
  status: string;
  sessionId: string;
  /** 二维码文本、data URL 或其他挑战内容。 */
  challenge: string | null;
  challengeType: string | null;
  message: string;
  authRef: string | null;
}

/** 安装成功结果；`missingComponents` 是所需但未安装的基础组件（提醒式，不阻断）。 */
export interface InstalledPlugin {
  identity: string;
  missingComponents: string[];
}

/** 市场索引条目。 */
export interface MarketEntryDto {
  pluginId: string;
  version: string;
  sequence: number;
  contentHash: string;
  minAppVersion: string;
  name: string;
  description: string;
  author: string;
  homepage: string;
  mirrors: string[];
  publishTime: string;
  /** 非空 = 已被发布者撤回。 */
  yanked: string;
  tags: string[];
  permissions: string[];
  /** 插件桌面视图声明的网络主机。 */
  networkHosts: string[];
}

/** daemon 托管组件标识。 */
export type ComponentKind = 'ffmpeg' | 'ytdlp';

/** ffmpeg / yt-dlp 组件状态（两者字段相同）。 */
export interface ComponentStatus {
  /** 生效路径来源：`manual` / `managed` / `system` / `none`。 */
  source: string;
  /** 生效的可执行文件路径（`none` 时为空）。 */
  path: string;
  /** 探测到的版本串（失败/未找到为空）。 */
  version: string;
  /** 托管安装记录的版本（空 = 未托管安装）。 */
  managedVersion: string;
  /** 系统 PATH 中探测到的路径（空 = 无）。 */
  systemPath: string;
  /** 当前平台是否提供托管安装。 */
  managedSupported: boolean;
}
export type ComponentFfmpegStatus = ComponentStatus;
export type ComponentYtdlpStatus = ComponentStatus;

/** 受管组件类型化状态（`{component, status}` 相邻标记）。 */
export type ComponentStatusDto =
  | { component: 'ffmpeg'; status: ComponentFfmpegStatus }
  | { component: 'ytdlp'; status: ComponentYtdlpStatus };

/** 组件可安装版本。 */
export interface ComponentVersions {
  /** 降序排列的稳定版本号。 */
  versions: string[];
  /** 最新稳定版（空 = 解析失败）。 */
  latestStable: string;
}

/** `daemon.plugin.consent` 参数：集合须与插件当前请求的完全相等。 */
export interface PluginConsentParams {
  identity: string;
  permissions: string[];
  networkHosts: string[];
}

/** `daemon.plugin.audit.list` 参数。 */
export interface PluginAuditListParams {
  identity?: string;
  beforeId?: number;
  /** 缺省 200，上限 1000。 */
  limit?: number;
}

/** 一条插件审计记录。 */
export interface PluginAuditEntryDto {
  id: number;
  atMs: number;
  identity: string;
  /** `daemon` | `desktop`。 */
  origin: string;
  action: string;
  target: string;
  /** `ok` | `denied` | `error`。 */
  outcome: string;
  detail: string;
}

export interface PluginAuditListDto {
  entries: PluginAuditEntryDto[];
  /** 还有更早的记录时给出下一页游标。 */
  nextBeforeId?: number;
}

export interface PluginAuditClearParams {
  identity?: string;
}

/** 客户端上报的桌面脚本审计记录（origin 由 daemon 强制为 desktop）。 */
export interface PluginAuditRecordEntry {
  identity: string;
  /** 必须以 `desktop.` 开头。 */
  action: string;
  target: string;
  outcome: string;
  detail: string;
}

export interface PluginAuditRecordParams {
  entries: PluginAuditRecordEntry[];
}
