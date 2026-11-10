// 扩展设置的纯逻辑（无 React / 别名依赖，可被 bun test 直接加载）。
// 语义与 crates/extensions（plugin_settings.rs / plugins.rs / plugin_auth.rs / managed_components.rs）逐条对齐。

import type { MarketEntryDto, PluginAuthResponse, PluginDto, PluginRisk, SettingFieldDto } from '../../../../lib/rpc/protocol'

// ── 插件设置校验 ────────────────────────────────────────────

/** 前置校验失败原因；文案映射留给 UI 层。 */
export type FieldError =
  | { kind: 'required' }
  | { kind: 'number' }
  | { kind: 'min'; min: string }
  | { kind: 'max'; max: string }
  | { kind: 'select' }
  /** daemon 校验失败并指明了字段（如 `pattern` 不匹配）；文案已本地化。 */
  | { kind: 'server'; message: string }

/** 十进制浮点（与 Rust `f64::from_str` 对常规输入的接受范围一致，不含 hex / inf / NaN）。 */
const DECIMAL = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/

/** 单条设置项前置校验：required → number/min/max → select 成员。 */
export function validateField(field: SettingFieldDto, raw: string): FieldError | null {
  const value = raw.trim()
  if (field.required && value === '') return { kind: 'required' }
  if (value === '') return null
  if (field.type === 'number') {
    if (!DECIMAL.test(value)) return { kind: 'number' }
    const number = Number(value)
    if (!Number.isFinite(number)) return { kind: 'number' }
    if (field.min !== null && number < field.min) return { kind: 'min', min: String(field.min) }
    if (field.max !== null && number > field.max) return { kind: 'max', max: String(field.max) }
  }
  if (field.widget === 'select' && field.options.length > 0 && !field.options.some((option) => option.value === value)) {
    return { kind: 'select' }
  }
  return null
}

/** 字段初始值：已保存值优先，否则 manifest 默认值（toggle 默认 `false`）。 */
export function initialFieldValue(field: SettingFieldDto, saved: Readonly<Record<string, string>>): string {
  const stored = saved[field.key]
  if (stored !== undefined) return stored
  if (field.default) return field.default
  return field.widget === 'toggle' ? 'false' : ''
}

/** 数字字段占位提示：`1 – 10` / `≥ 1` / `≤ 10`。 */
export function rangeHint(field: SettingFieldDto): string | null {
  const { min, max } = field
  if (min !== null && max !== null) return `${String(min)} – ${String(max)}`
  if (min !== null) return `≥ ${String(min)}`
  if (max !== null) return `≤ ${String(max)}`
  return null
}

// ── 市场 ────────────────────────────────────────────────────

/** 市场列表每次展开的条数。 */
export const MARKET_PAGE_SIZE = 50

/** 目录最长可信时长：超过后再次进入插件页会重新拉取，保证「更新」徽标不陈旧（对齐 GPUI `MARKET_MAX_AGE`）。 */
export const MARKET_MAX_AGE_MS = 10 * 60 * 1000

export interface MarketLoadState {
  /** 是否发起过加载。 */
  requested: boolean
  /** 上次失败是断连类：连接恢复后自动重试一次。 */
  retryOnReconnect: boolean
  /** 当前展示的是失败态。 */
  failed: boolean
  /** 上次成功加载完成的时刻（毫秒时间戳）；从未成功为 null。 */
  loadedAt: number | null
}

/**
 * 目录是否需要（重新）加载（对齐 GPUI `market_needs_load`）：从未请求、断连失败待重试、或上次成功加载已过期。
 * 失败且非断连类时不自动重试——由手动刷新恢复。
 */
export function marketNeedsLoad(state: MarketLoadState, now: number): boolean {
  if (!state.requested || state.retryOnReconnect) return true
  return !state.failed && state.loadedAt !== null && now - state.loadedAt >= MARKET_MAX_AGE_MS
}

/** 市场条目关键字过滤：名称 / id / 描述 / 作者 / 标签任一命中（大小写不敏感）。 */
export function filterMarket(entries: readonly MarketEntryDto[], query: string): MarketEntryDto[] {
  const needle = query.trim().toLowerCase()
  if (needle === '') return [...entries]
  const hit = (value: string) => value.toLowerCase().includes(needle)
  return entries.filter(
    (entry) =>
      hit(entry.name) || hit(entry.pluginId) || hit(entry.description) || hit(entry.author) || entry.tags.some(hit),
  )
}

/**
 * 分页切片（对齐 GPUI `market::page_of`）：前 `limit` 条可见，`remaining` = 其后还没展开的条数
 * （0 = 不显示「显示更多」）。公开市场与专属插件区共用。
 */
export function pageOf<T>(items: readonly T[], limit: number): { visible: T[]; remaining: number } {
  const shown = Math.min(Math.max(0, limit), items.length)
  return { visible: items.slice(0, shown), remaining: items.length - shown }
}

/** 专属插件区条目数达到该值才出现搜索框（对齐 GPUI `PRIVATE_SEARCH_MIN_ENTRIES`）。 */
export const PRIVATE_SEARCH_MIN_ENTRIES = 8

/** 专属插件区是否显示搜索框（`total` = 可装条目数，未过滤）。 */
export function privateSearchVisible(total: number): boolean {
  return total >= PRIVATE_SEARCH_MIN_ENTRIES
}

/** 专属插件区过滤（对齐 GPUI `market::filter_private`）：搜索框不可见时忽略 `query`，可见时同公开市场规则。 */
export function filterPrivate(entries: readonly MarketEntryDto[], query: string): MarketEntryDto[] {
  return privateSearchVisible(entries.length) ? filterMarket(entries, query) : [...entries]
}

/** 引擎只安装 `yanked === 'none'` 的条目。 */
function installable(entry: MarketEntryDto): boolean {
  return entry.yanked === 'none'
}

/**
 * 每个插件一条（镜像 crates/extensions/src/market.rs `latest_per_plugin`）：最新可安装版本；
 * 全部撤回（下架）的插件不出现。顺序保持各插件在索引中首次出现的位置。
 */
export function latestPerPlugin(entries: readonly MarketEntryDto[]): MarketEntryDto[] {
  const best = new Map<string, MarketEntryDto>()
  for (const entry of entries) {
    const current = best.get(entry.pluginId)
    if (current === undefined) {
      best.set(entry.pluginId, entry)
      continue
    }
    const a = installable(entry)
    const b = installable(current)
    const better = a !== b ? a : entry.sequence > current.sequence
    if (better) best.set(entry.pluginId, entry)
  }
  return [...best.values()].filter(installable)
}

/** `MAJOR.MINOR.PATCH`（可带前导 `v`，忽略预发布 / 构建后缀）；无法解析返回 null。 */
function parseSemver(version: string): [number, number, number] | null {
  const trimmed = version.trim()
  const core = (trimmed.startsWith('v') ? trimmed.slice(1) : trimmed).split(/[-+]/)[0] ?? ''
  const parts = core.split('.')
  if (parts.length !== 3 || parts.some((part) => !/^\d+$/.test(part))) return null
  return [Number(parts[0]), Number(parts[1]), Number(parts[2])]
}

/** `candidate` 严格新于 `current` 时为真；任一侧无法解析视为不可比较。 */
export function versionNewer(candidate: string, current: string): boolean {
  const a = parseSemver(candidate)
  const b = parseSemver(current)
  if (a === null || b === null) return false
  for (let i = 0; i < 3; i += 1) {
    const x = a[i] as number
    const y = b[i] as number
    if (x !== y) return x > y
  }
  return false
}

export type MarketAction = 'install' | 'update' | 'installed' | 'unavailable'

/** 私有商店条目（`agent.privatePlugin.list`）；公开市场条目无 `channel`。 */
export function isPrivateEntry(entry: MarketEntryDto): boolean {
  return entry.channel === 'private'
}

/**
 * 私有条目的安装将替换本机同 id 的非私有插件（公开市场 / 本机包安装的；dev 副本不算）：
 * 权限确认弹窗据此追加替换警示。与 Rust `market::replaces_local_plugin` 逐条对齐。
 */
export function replacesLocalPlugin(entry: MarketEntryDto, installed: PluginDto | undefined): boolean {
  return isPrivateEntry(entry) && installed !== undefined && !installed.devMode && installed.source !== 'private'
}

/**
 * 市场条目相对本机安装状态的动作；开发模式插件不被市场覆盖。
 * 私有隔离（镜像 crates/extensions/src/market.rs `market_action`）：`source === 'private'` 的已装插件
 * 只受私有条目更新，公开条目对它一律按「已安装」处理（不给公开 Update / Install）。
 */
export function marketAction(entry: MarketEntryDto, installed: PluginDto | undefined): MarketAction {
  if (installed === undefined) return installable(entry) ? 'install' : 'unavailable'
  if (installed.source === 'private' && !isPrivateEntry(entry)) return 'installed'
  if (!installed.devMode && installable(entry) && versionNewer(entry.version, installed.version)) return 'update'
  return 'installed'
}

/** 需要用户确认的权限：新装取条目全部权限，更新只取已安装版本没有的新增权限。 */
export function permissionsToConfirm(entry: MarketEntryDto, installed: PluginDto | undefined): string[] {
  const granted = installed?.permissions ?? []
  return entry.permissions.filter((permission) => !granted.includes(permission))
}

/**
 * 已安装版本在目录中的撤回标记；未撤回或目录无此版本返回 null。
 * 私有隔离：`source === 'private'` 的插件只认私有条目，其余插件只认公开条目。
 */
export function installedVersionYanked(entries: readonly MarketEntryDto[], plugin: PluginDto): string | null {
  if (plugin.devMode) return null
  const privateSource = plugin.source === 'private'
  const hit = entries.find(
    (entry) => isPrivateEntry(entry) === privateSource && entry.pluginId === plugin.identity && entry.version === plugin.version,
  )
  if (hit === undefined || hit.yanked === '' || hit.yanked === 'none') return null
  return hit.yanked
}

/** 私有条目授权到期时间的展示串：缺省 / null / 空 = 永久（null）；无法解析原样返回，解析成功交给 `format`。 */
export function privateExpiryText(expiresAt: string | null | undefined, format: (date: Date) => string): string | null {
  if (expiresAt === undefined || expiresAt === null || expiresAt.trim() === '') return null
  const ms = Date.parse(expiresAt)
  return Number.isNaN(ms) ? expiresAt : format(new Date(ms))
}

/** 私有目录加载状态：`signedIn` 为 null = 尚未拿到过应答。 */
export interface PrivateCatalogState {
  loading: boolean
  /** 拉取失败（只影响专属区）。 */
  failed: boolean
  signedIn: boolean | null
  entries: readonly MarketEntryDto[]
}

/** 专属区呈现：hidden 整区不展示（未请求 / 已登录无授权，防存在性暗示）。 */
export type PrivateSectionView = 'hidden' | 'loading' | 'error' | 'signIn' | 'list'

/** 专属区呈现判定（镜像 crates/extensions/src/market.rs `private_section_view`）。 */
export function privateSectionView(state: PrivateCatalogState): PrivateSectionView {
  if (state.failed) return 'error'
  if (state.signedIn === null) return state.loading ? 'loading' : 'hidden'
  if (!state.signedIn) return 'signIn'
  return latestPerPlugin(state.entries).length === 0 ? 'hidden' : 'list'
}

/** 私有目录的初始状态：尚未请求。 */
export const INITIAL_PRIVATE_CATALOG: PrivateCatalogState = { loading: false, failed: false, signedIn: null, entries: [] }

/**
 * 发起一次拉取后的状态（镜像 GPUI `PrivateUi::begin_load`）。
 * `reset`：丢弃已有目录回到加载态（首次 / 重试 / 登录态变化）。
 * 否则（刷新 / 安装时发现版本已变）：已加载的目录保持显示，完成后整体替换，不闪空；
 * 失败态没有可展示的条目，回到加载态。
 */
export function beginPrivateLoad(current: PrivateCatalogState, reset: boolean): PrivateCatalogState {
  if (reset || current.failed) return { ...INITIAL_PRIVATE_CATALOG, loading: true }
  return { ...current, loading: true }
}

/** 拉取失败后的状态：只影响专属区，错误条取代列表。 */
export function failedPrivateLoad(current: PrivateCatalogState): PrivateCatalogState {
  return { ...current, loading: false, failed: true }
}

/** 市场 `yanked` 标记 → i18n 键；空 / 未知值不展示。 */
export function yankedLabelKey(yanked: string): string | null {
  switch (yanked) {
    case 'deprecated':
      return 'marketYankedDeprecated'
    case 'vulnerable':
      return 'marketYankedVulnerable'
    case 'malicious':
      return 'marketYankedMalicious'
    default:
      return null
  }
}

/** 权限 → i18n 键（名称、说明）；未知权限返回 null（调用侧回退展示原始名 + 未知说明）。 */
export function permissionKeys(permission: string): { name: string; desc: string } | null {
  switch (permission) {
    case 'ffmpeg':
      return { name: 'pluginPermFfmpegName', desc: 'pluginPermFfmpegDesc' }
    case 'ytdlp':
      return { name: 'pluginPermYtdlpName', desc: 'pluginPermYtdlpDesc' }
    case 'auth':
      return { name: 'pluginPermAuthName', desc: 'pluginPermAuthDesc' }
    case 'tasks':
      return { name: 'pluginPermTasksName', desc: 'pluginPermTasksDesc' }
    case 'browserRules':
      return { name: 'pluginPermBrowserRulesName', desc: 'pluginPermBrowserRulesDesc' }
    case 'desktop':
      return { name: 'pluginPermDesktopName', desc: 'pluginPermDesktopDesc' }
    case 'background':
      return { name: 'pluginPermBackgroundName', desc: 'pluginPermBackgroundDesc' }
    case 'intercept':
      return { name: 'pluginPermInterceptName', desc: 'pluginPermInterceptDesc' }
    case 'pageScripts':
      return { name: 'pluginPermPageScriptsName', desc: 'pluginPermPageScriptsDesc' }
    case 'archive':
      return { name: 'pluginPermArchiveName', desc: 'pluginPermArchiveDesc' }
    default:
      return null
  }
}

// ── 授权与风险（镜像 crates/extensions market.rs；夹具 crates/extensions/tests/fixtures/consent_cases.json）──

const ELEVATED_PERMISSIONS: readonly string[] = [
  'ffmpeg',
  'ytdlp',
  'auth',
  'tasks',
  'browserRules',
  'background',
  'intercept',
  'archive',
]

/** 风险等级：含 `desktop` / `pageScripts` → full；含任一提权权限 → elevated；否则 standard。 */
export function riskOf(permissions: readonly string[]): PluginRisk {
  if (permissions.includes('desktop') || permissions.includes('pageScripts')) return 'full'
  if (permissions.some((permission) => ELEVATED_PERMISSIONS.includes(permission))) return 'elevated'
  return 'standard'
}

/** 风险等级 → 徽标 / 提示的 i18n 键。 */
export function riskKeys(risk: string): { label: string; tooltip: string } {
  switch (risk) {
    case 'full':
      return { label: 'pluginRiskFull', tooltip: 'pluginRiskTooltipFull' }
    case 'elevated':
      return { label: 'pluginRiskElevated', tooltip: 'pluginRiskTooltipElevated' }
    default:
      return { label: 'pluginRiskStandard', tooltip: 'pluginRiskTooltipStandard' }
  }
}

export interface ConsentSet {
  permissions: readonly string[]
  networkHosts: readonly string[]
}

function normalizedSet(values: readonly string[]): string[] {
  return [...new Set(values)].sort()
}

/** 用户已确认的集合与插件待授权集合排序去重后完全相等 → 免弹窗直接授权。 */
export function consentMatches(confirmed: ConsentSet, pending: ConsentSet): boolean {
  const same = (a: readonly string[], b: readonly string[]) => {
    const left = normalizedSet(a)
    const right = normalizedSet(b)
    return left.length === right.length && left.every((value, index) => value === right[index])
  }
  return same(confirmed.permissions, pending.permissions) && same(confirmed.networkHosts, pending.networkHosts)
}

/** 市场条目中尚未授权的网络主机：新装取全部，更新排除已授权（已装 networkHosts 去掉 pending）。 */
export function hostsToConfirm(entry: MarketEntryDto, installed: PluginDto | undefined): string[] {
  const pending = installed?.consentPendingHosts ?? []
  const granted = (installed?.networkHosts ?? []).filter((host) => !pending.includes(host))
  return (entry.networkHosts ?? []).filter((host) => !granted.includes(host))
}

/** 安装 / 更新成功后的授权处理：无待授权 → none；市场预确认与 pending 吻合 → grant；其余 → dialog。 */
export type PostInstallConsent = 'none' | 'grant' | 'dialog'

export function postInstallConsent(plugin: PluginDto, confirmed: ConsentSet | null): PostInstallConsent {
  const pending: ConsentSet = { permissions: plugin.consentPending, networkHosts: plugin.consentPendingHosts }
  if (pending.permissions.length === 0 && pending.networkHosts.length === 0) return 'none'
  if (confirmed !== null && consentMatches(confirmed, pending)) return 'grant'
  return 'dialog'
}

/** 各对话框当前指向的插件 identity（未打开为 null）。 */
export interface DialogIdentities {
  detail: string | null
  settings: string | null
  auth: string | null
  consent: string | null
}

/** 对话框对应的最新插件快照；插件已卸载为 null。授权框另需仍有待授权项（别处已授权则为 null → 关闭）。 */
export type LiveDialogPlugins = { [K in keyof DialogIdentities]: PluginDto | null }

export function liveDialogPlugins(plugins: readonly PluginDto[], open: DialogIdentities): LiveDialogPlugins {
  const find = (identity: string | null) => (identity === null ? null : (plugins.find((plugin) => plugin.identity === identity) ?? null))
  const consent = find(open.consent)
  return {
    detail: find(open.detail),
    settings: find(open.settings),
    auth: find(open.auth),
    consent: consent !== null && postInstallConsent(consent, null) === 'none' ? null : consent,
  }
}

// ── 插件登录挑战 ────────────────────────────────────────────

export interface PluginAuthState {
  status: string
  sessionId: string
  authRef: string
  challenge: string | null
  challengeType: string | null
  message: string | null
}

export function isQrcodeChallenge(type: string | null): boolean {
  return type?.toLowerCase() === 'qrcode'
}

/** pending poll 可以省略挑战；终态不沿用旧挑战，logout 始终清空本地登录态。 */
export function applyPluginAuthResponse(
  previous: PluginAuthState,
  response: PluginAuthResponse,
  wasLogout: boolean,
): PluginAuthState {
  const pending = response.status === 'pending'
  const next: PluginAuthState = {
    status: response.status,
    sessionId: response.sessionId,
    authRef: response.authRef ?? '',
    challenge: pending ? (response.challenge ?? previous.challenge) : (response.challenge ?? null),
    challengeType: pending ? (response.challengeType ?? previous.challengeType) : (response.challengeType ?? null),
    message: response.message || null,
  }
  return wasLogout ? { ...next, authRef: '', sessionId: '', challenge: null, challengeType: null } : next
}

/** 挑战文本超过该长度即截断显示（复制按钮始终给出完整原文）。 */
export const CHALLENGE_TEXT_LIMIT = 512

/** `data:image/...;base64,` 挑战超过该长度直接放弃图片渲染，退化为文本 + 复制按钮。 */
export const MAX_CHALLENGE_DATA_URL_LEN = 256 * 1024

const IMAGE_MIMES: Record<string, true> = {
  'image/png': true,
  'image/jpeg': true,
  'image/gif': true,
  'image/webp': true,
  'image/bmp': true,
  'image/svg+xml': true,
}
const BASE64_BODY = /^[A-Za-z0-9+/]*={0,2}$/

/**
 * 插件挑战不可信：只有形如 `data:image/<mime>[;..];base64,<payload>` 且 mime 已知、载荷是合法 base64、
 * 长度未超限的值才当图片渲染；其余返回 null（调用侧退化为截断文本 + 复制）。
 * 返回值可直接作 `<img src>`。
 */
export function dataImageChallengeSrc(value: string): string | null {
  if (value.length > MAX_CHALLENGE_DATA_URL_LEN) return null
  if (!value.toLowerCase().startsWith('data:')) return null
  const comma = value.indexOf(',')
  if (comma < 0) return null
  const [mimeRaw = '', ...params] = value.slice('data:'.length, comma).split(';')
  const mime = mimeRaw.toLowerCase()
  if (!params.some((param) => param.toLowerCase() === 'base64')) return null
  if (!Object.hasOwn(IMAGE_MIMES, mime)) return null
  const payload = value.slice(comma + 1).replace(/\s+/g, '')
  if (payload === '' || payload.length % 4 === 1 || !BASE64_BODY.test(payload)) return null
  return `data:${mime};base64,${payload}`
}

/** 挑战文本超过 {@link CHALLENGE_TEXT_LIMIT} 个字符（按码点计）时截断并追加省略号。 */
export function truncateChallengeText(value: string): string {
  const chars = Array.from(value)
  if (chars.length <= CHALLENGE_TEXT_LIMIT) return value
  return `${chars.slice(0, CHALLENGE_TEXT_LIMIT).join('')}…`
}

/** 仅 http(s) 链接可点击（挑战 / 主页均不可信，拒绝 `javascript:` 等）。 */
export function safeHttpUrl(value: string): string | null {
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null
  } catch {
    return null
  }
}

// ── 组件 ────────────────────────────────────────────────────

/** 1024 进制的人类可读字节数（`0 B` / `1.5 KB` / `12.3 MB` / `1.0 GB`）。 */
export function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB']
  const clamped = Math.max(0, bytes)
  let value = clamped
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return unit === 0 ? `${Math.trunc(clamped)} B` : `${value.toFixed(1)} ${units[unit]}`
}

export type MarketSort = 'default' | 'newest' | 'favorites'

/** 发布时间降序比较（ISO-8601 字符串比较），空串排最后。 */
function comparePublishDesc(a: string, b: string): number {
  if (a === b) return 0
  if (a === '') return 1
  if (b === '') return -1
  return a < b ? 1 : -1
}

/**
 * 市场排序 / 收藏筛选（镜像 crates/extensions/src/market.rs `arrange_market`）：
 * 先按 `favoritesOnly` 过滤；default 保持输入顺序，newest 按发布时间降序，
 * favorites 按收藏数降序（缺省 0）、同数按发布时间降序；均稳定。
 */
export function arrangeMarket(
  entries: readonly MarketEntryDto[],
  sort: MarketSort,
  counts: Readonly<Record<string, number>>,
  mine: ReadonlySet<string>,
  favoritesOnly: boolean,
): MarketEntryDto[] {
  const kept = favoritesOnly ? entries.filter((entry) => mine.has(entry.pluginId)) : [...entries]
  if (sort === 'default') return kept
  const count = (entry: MarketEntryDto) => counts[entry.pluginId] ?? 0
  const indexed = kept.map((entry, index) => ({ entry, index }))
  indexed.sort((x, y) => {
    if (sort === 'favorites') {
      const diff = count(y.entry) - count(x.entry)
      if (diff !== 0) return diff
    }
    return comparePublishDesc(x.entry.publishTime, y.entry.publishTime) || x.index - y.index
  })
  return indexed.map((item) => item.entry)
}
