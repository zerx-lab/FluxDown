// BT 防吸血的纯逻辑（GPUI `crates/settings` / Android / iOS 的同名判定逐条对齐）：
// CIDR 校验、时长预设、剩余时长、证据摘要、列表查询、批量分块、上报与云名单状态判定、Peer 处置状态。

import type {
  BtGuardCloudStatusDto,
  BtGuardEntriesAddParams,
  BtGuardEntriesListParams,
  BtGuardEntryDto,
  BtGuardEventsListParams,
  BtGuardLayer,
  BtGuardRule,
  BtPeerDto,
  EventFrame,
  JsonValue,
} from './rpc'
import { BT_GUARD_LAYERS, BT_GUARD_LIST_DEFAULT_LIMIT, BT_GUARD_REMOVE_MAX_IDS, BT_GUARD_RULES } from './rpc'

// ── CIDR / IP 组校验 ──

export type CidrIssue = 'empty' | 'address' | 'prefix'
export type CidrCheck = { ok: true; family: 4 | 6; cidr: string; prefix: number | null } | { ok: false; issue: CidrIssue }

function isIpv4(text: string): boolean {
  const parts = text.split('.')
  if (parts.length !== 4) return false
  return parts.every((part) => /^\d{1,3}$/.test(part) && (part === '0' || !part.startsWith('0')) && Number(part) <= 255)
}

function isIpv6(text: string): boolean {
  if (!text.includes(':') || text.includes('%')) return false
  let body = text
  if (text.includes('.')) {
    const cut = text.lastIndexOf(':')
    if (!isIpv4(text.slice(cut + 1))) return false
    // 内嵌 IPv4 占两个 16 位组。
    body = `${text.slice(0, cut + 1)}0:0`
  }
  const halves = body.split('::')
  if (halves.length > 2) return false
  const groups = halves.map((half) => (half === '' ? [] : half.split(':')))
  if (!groups.every((half) => half.every((group) => /^[0-9a-fA-F]{1,4}$/.test(group)))) return false
  const total = groups.reduce((sum, half) => sum + half.length, 0)
  return halves.length === 1 ? total === 8 : total <= 7
}

/** 校验 `1.2.3.4` / `1.2.3.0/24` / `2001:db8::/64`（裸地址等价于单地址）；返回去首尾空白后的原文。 */
export function parseCidr(input: string): CidrCheck {
  const text = input.trim()
  if (text === '') return { ok: false, issue: 'empty' }
  const slash = text.indexOf('/')
  const address = slash < 0 ? text : text.slice(0, slash)
  const prefixText = slash < 0 ? null : text.slice(slash + 1)
  const family = isIpv4(address) ? 4 : isIpv6(address) ? 6 : null
  if (family === null) return { ok: false, issue: 'address' }
  if (prefixText === null) return { ok: true, family, cidr: text, prefix: null }
  if (!/^\d{1,3}$/.test(prefixText)) return { ok: false, issue: 'prefix' }
  const prefix = Number(prefixText)
  if (prefix > (family === 4 ? 32 : 128)) return { ok: false, issue: 'prefix' }
  return { ok: true, family, cidr: text, prefix }
}

/** IP 组文本（`1.2.3.4` / `2001:db8::/64`）→ 其网络地址部分，用于 `entries.list` 的 `containsIp`。 */
export function groupAddress(group: string): string {
  const text = group.trim()
  const slash = text.indexOf('/')
  return slash < 0 ? text : text.slice(0, slash)
}

// ── 时长预设 / 剩余时长 ──

export const GUARD_DURATION_IDS = ['1h', '1d', '7d', 'forever'] as const
export type GuardDurationId = (typeof GUARD_DURATION_IDS)[number]

const DURATION_SECS: Record<GuardDurationId, number | undefined> = { '1h': 3600, '1d': 86_400, '7d': 604_800, forever: undefined }

/** 预设 → `durationSecs`；永久为 `undefined`（协议缺省 = 永久）。 */
export function durationSecs(id: GuardDurationId): number | undefined {
  return DURATION_SECS[id]
}

export type RemainingUnit = 's' | 'm' | 'h' | 'd'

/** 剩余秒数 → 最大整单位；永久（缺省）→ `null`；负值 / 非有限按 0。 */
/** 取回时刻 `remainingSecs` 在 `nowMs` 时的剩余秒数（本地倒计时，≥ 0）；永久缺省。 */
export function remainingNow(remainingSecs: number | undefined, fetchedAtMs: number, nowMs: number): number | undefined {
  if (remainingSecs === undefined) return undefined
  const elapsed = Math.max(0, nowMs - fetchedAtMs) / 1000
  return Math.max(0, Math.floor(remainingSecs - elapsed))
}

export function remainingParts(secs: number | undefined): { unit: RemainingUnit; n: number } | null {
  if (secs === undefined) return null
  const s = Math.max(0, Math.floor(Number.isFinite(secs) ? secs : 0))
  if (s < 60) return { unit: 's', n: s }
  if (s < 3600) return { unit: 'm', n: Math.floor(s / 60) }
  if (s < 86_400) return { unit: 'h', n: Math.floor(s / 3600) }
  return { unit: 'd', n: Math.floor(s / 86_400) }
}

type Translate = (key: string, params?: Record<string, string | number>) => string

/** 单位时长文案（`btGuardDuration{D,H,M,S}`）。 */
export function formatDurationParts(t: Translate, parts: { unit: RemainingUnit; n: number }): string {
  return t(`btGuardDuration${parts.unit.toUpperCase()}`, { n: parts.n })
}

/** 剩余时长文案：永久 / 「x 剩余」。 */
export function formatRemaining(t: Translate, secs: number | undefined): string {
  const parts = remainingParts(secs)
  if (parts === null) return t('btGuardPermanent')
  return t('btGuardRemaining', { time: formatDurationParts(t, parts) })
}

// ── 证据摘要 ──

export type EvidenceFactId =
  | 'detail'
  | 'torrentSize'
  | 'deliveredBytes'
  | 'dupBytes'
  | 'ratio'
  | 'rerequests'
  | 'rewind'
  | 'strikes'
  | 'category'

/** 证据事实 → 文案键（`btGuardEv*`）。 */
export const EVIDENCE_FACT_KEYS: Record<EvidenceFactId, string> = {
  detail: 'btGuardEvDetail',
  torrentSize: 'btGuardEvTorrentSize',
  deliveredBytes: 'btGuardEvDelivered',
  dupBytes: 'btGuardEvDup',
  ratio: 'btGuardEvRatio',
  rerequests: 'btGuardEvRerequests',
  rewind: 'btGuardEvRewind',
  strikes: 'btGuardEvStrikes',
  category: 'btGuardEvCategory',
}

export interface EvidenceFact {
  id: EvidenceFactId
  value: string
}

function nonNegative(value: JsonValue | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null
}

function nonEmptyText(value: JsonValue | undefined): string | null {
  return typeof value === 'string' && value.trim() !== '' ? value.trim() : null
}

/** 把倍率千分数格式化为 `3.70×`。 */
export function formatRatioMilli(milli: number): string {
  return `${(milli / 1000).toFixed(2)}×`
}

/** 千分比 → `7.5%`。 */
export function formatPermille(permille: number): string {
  return `${(permille / 10).toFixed(1)}%`
}

/**
 * 条目 / 事件的证据 JSON → 有序事实列表（零值与缺省字段省略）。`evidence` 可以是已解析的对象，
 * 也可以是 JSON 文本（事件的 `detail`）；无法解析的文本返回空列表。
 */
export function evidenceFacts(evidence: JsonValue | undefined, formatBytes: (bytes: number) => string): EvidenceFact[] {
  let value: JsonValue | undefined = evidence
  if (typeof value === 'string') {
    try {
      value = JSON.parse(value) as JsonValue
    } catch {
      return []
    }
  }
  if (value === null || value === undefined || typeof value !== 'object' || Array.isArray(value)) return []
  const facts: EvidenceFact[] = []
  const push = (id: EvidenceFactId, text: string | null) => {
    if (text !== null) facts.push({ id, value: text })
  }
  const positive = (key: string, format: (n: number) => string): string | null => {
    const n = nonNegative(value[key])
    return n !== null && n > 0 ? format(n) : null
  }
  push('detail', nonEmptyText(value.detail))
  push('torrentSize', positive('torrentSize', formatBytes))
  push('deliveredBytes', positive('deliveredBytes', formatBytes))
  push('dupBytes', positive('dupBytes', formatBytes))
  push('ratio', positive('ratioMilli', formatRatioMilli))
  push('rerequests', positive('rerequests', String))
  push('rewind', positive('rewindPermille', formatPermille))
  push('strikes', positive('strikes', String))
  push('category', nonEmptyText(value.category))
  return facts
}

// ── 列表查询 / 批量 ──

export const GUARD_PAGE_SIZE = BT_GUARD_LIST_DEFAULT_LIMIT

/** 筛选下拉的取值（首项 `all` = 不限）。 */
export const BT_GUARD_LAYERS_FILTER = ['all', ...BT_GUARD_LAYERS] as const
export const BT_GUARD_RULE_FILTER = ['all', ...BT_GUARD_RULES] as const

/** 事件日志的 kind 筛选（daemon 实际写入的取值；其余未知 kind 只显示原文）。 */
export const BT_GUARD_EVENT_KIND_FILTER = [
  'all',
  'observed',
  'enforced',
  'probe',
  'probe_cleared',
  'entry_added',
  'entry_removed',
  'cloud_applied',
  'cloud_rejected',
  'filter_applied',
] as const

export interface EntryFilter {
  layer: BtGuardLayer | 'all'
  rule: BtGuardRule | 'all'
  query: string
}

export const DEFAULT_ENTRY_FILTER: EntryFilter = { layer: 'all', rule: 'all', query: '' }

/** 筛选 + 页码 → `entries.list` 参数（`all` 与空白搜索不带字段）。 */
export function entriesQuery(filter: EntryFilter, page: number, limit: number = GUARD_PAGE_SIZE): BtGuardEntriesListParams {
  const query = filter.query.trim()
  return {
    ...(filter.layer === 'all' ? {} : { layer: filter.layer }),
    ...(filter.rule === 'all' ? {} : { rule: filter.rule }),
    ...(query === '' ? {} : { query }),
    offset: Math.max(0, page) * limit,
    limit,
  }
}

export interface EventFilter {
  rule: BtGuardRule | 'all'
  kind: string
}

export const DEFAULT_EVENT_FILTER: EventFilter = { rule: 'all', kind: 'all' }

export function eventsQuery(filter: EventFilter, page: number, limit: number = GUARD_PAGE_SIZE): BtGuardEventsListParams {
  return {
    offset: Math.max(0, page) * limit,
    limit,
    ...(filter.rule === 'all' ? {} : { rule: filter.rule }),
    ...(filter.kind === 'all' ? {} : { kind: filter.kind }),
  }
}

/** 总数 → 页数（至少 1 页）。 */
export function pageCount(total: number, limit: number = GUARD_PAGE_SIZE): number {
  return Math.max(1, Math.ceil(Math.max(0, total) / limit))
}

/** 删除 / 末页清空后把页码夹回有效范围。 */
export function clampPage(page: number, total: number, limit: number = GUARD_PAGE_SIZE): number {
  return Math.min(Math.max(0, page), pageCount(total, limit) - 1)
}

/** 批量解除按协议单次上限分块。 */
export function chunkIds(ids: readonly number[], size: number = BT_GUARD_REMOVE_MAX_IDS): number[][] {
  const chunks: number[][] = []
  for (let i = 0; i < ids.length; i += size) chunks.push(ids.slice(i, i + size))
  return chunks
}

/** Peer 的 IP 组 → 证据查询（服务端过滤）：覆盖该地址的条目（`containsIp`）+ 该组的最近事件（`ipGroup`）。 */
export function evidenceQueries(group: string, eventRows: number): { entries: BtGuardEntriesListParams; events: BtGuardEventsListParams } {
  return {
    entries: { containsIp: groupAddress(group), offset: 0, limit: GUARD_PAGE_SIZE },
    events: { ipGroup: group, offset: 0, limit: eventRows },
  }
}

/** 云层条目禁止直接删除（daemon 返回 invalidArgument），只能加白覆盖。 */
export function canRemoveEntry(entry: Pick<BtGuardEntryDto, 'layer'>): boolean {
  return entry.layer !== 'cloud'
}

/** 白名单层本身无需再加白。 */
export function canWhitelistEntry(entry: Pick<BtGuardEntryDto, 'layer'>): boolean {
  return entry.layer !== 'allow'
}

/** 选中项中可批量解除的 id（云层条目跳过）。 */
export function removableIds(entries: readonly BtGuardEntryDto[], selected: ReadonlySet<number>): number[] {
  return entries.filter((entry) => selected.has(entry.id) && canRemoveEntry(entry)).map((entry) => entry.id)
}

// ── 云名单 / 上报状态 ──

export type CloudStatusKind = 'disabled' | 'none' | 'stale' | 'rejected' | 'error' | 'ok'

/** 云名单状态卡的主状态（优先级：未启用 > 无文档 > 已过期 > 被拒 > 拉取失败 > 正常）。 */
export function cloudStatusKind(cloud: BtGuardCloudStatusDto, enabled: boolean): CloudStatusKind {
  if (!enabled) return 'disabled'
  if (cloud.sequence === 0) return cloud.rejectedReason ? 'rejected' : cloud.lastError ? 'error' : 'none'
  if (cloud.stale) return 'stale'
  if (cloud.rejectedReason) return 'rejected'
  if (cloud.lastError) return 'error'
  return 'ok'
}

export type ReportGate = 'ready' | 'needLogin'

/** 证据上报仅登录用户可用：开关可以开，但未登录时不会实际上报。 */
export function reportGate(loggedIn: boolean): ReportGate {
  return loggedIn ? 'ready' : 'needLogin'
}

/** 实际是否在上报：开关打开且已登录。 */
export function reportActive(enabled: boolean, loggedIn: boolean): boolean {
  return enabled && loggedIn
}

// ── Peer 嫌疑 / 处置 ──

export type PeerSuspicion = 'none' | 'weak' | 'medium' | 'strong'
export type PeerGuardAction = 'allow' | 'throttle' | 'choke' | 'banned'

export interface PeerGuardView {
  ipGroup: string
  groupDeliveredBytes: number
  suspicion: PeerSuspicion
  rules: string[]
  action: PeerGuardAction
  observeOnly: boolean
}

const SUSPICIONS: readonly string[] = ['none', 'weak', 'medium', 'strong']
const PEER_ACTIONS: readonly string[] = ['allow', 'throttle', 'choke', 'banned']

/** 归一化 peer 的防吸血字段：旧 daemon / 未知取值回落到安全默认（无嫌疑、放行）。 */
export function peerGuardView(peer: Pick<BtPeerDto, 'ipGroup' | 'groupDeliveredBytes' | 'suspicion' | 'rules' | 'action' | 'observeOnly'>): PeerGuardView {
  const suspicion = peer.suspicion !== undefined && SUSPICIONS.includes(peer.suspicion) ? peer.suspicion : 'none'
  const action = peer.action !== undefined && PEER_ACTIONS.includes(peer.action) ? peer.action : 'allow'
  const delivered = peer.groupDeliveredBytes
  return {
    ipGroup: (peer.ipGroup ?? '').trim(),
    groupDeliveredBytes: typeof delivered === 'number' && Number.isFinite(delivered) && delivered > 0 ? delivered : 0,
    suspicion,
    rules: (peer.rules ?? []).filter((rule) => rule !== ''),
    action,
    observeOnly: peer.observeOnly === true,
  }
}

/** 是否有值得展示的防吸血标记（命中规则、非放行处置或非 none 嫌疑）。 */
export function peerFlagged(view: PeerGuardView): boolean {
  return view.suspicion !== 'none' || view.rules.length > 0 || view.action !== 'allow'
}

// ── Peer 右键处置 ──

/** 限速处置的固定时长（秒）：1 天，与桌面端一致。 */
export const PEER_THROTTLE_SECS = 86_400

export type PeerGuardCommand = { kind: 'ban'; duration: GuardDurationId } | { kind: 'throttle' } | { kind: 'allow' }

/** Peer 右键处置 → `entries.add` 参数（封禁按预设时长，限速 1 天，加白永久）。 */
export function peerCommandParams(group: string, command: PeerGuardCommand): BtGuardEntriesAddParams {
  switch (command.kind) {
    case 'ban': {
      const secs = durationSecs(command.duration)
      return { cidr: group, action: 'ban', ...(secs === undefined ? {} : { durationSecs: secs }), reason: '' }
    }
    case 'throttle':
      return { cidr: group, action: 'throttle', durationSecs: PEER_THROTTLE_SECS, reason: '' }
    case 'allow':
      return { cidr: group, action: 'allow', reason: '' }
  }
}

// ── 事件 ──

/** 帧是否为防吸血变化事件：daemon 直达（`service: 'daemon'`）或经 agent 转发（`{type:'daemon'}`）两种形态。 */
export function isGuardChangedFrame(frame: EventFrame): boolean {
  const outer = frame.event
  const daemonEvent = outer.service === 'daemon' ? outer.event : outer.event.type === 'daemon' ? outer.event.data : null
  return daemonEvent?.type === 'btGuardChanged'
}

// ── Peers 表格版式 ──

/** 连接表并排防吸血三列所需的最小内容宽度（px，对齐 GPUI `GUARD_COLUMNS_MIN_WIDTH`）。 */
export const GUARD_COLUMNS_MIN_WIDTH = 1180

// ── 剩余时长本地倒计时 ──

/**
 * 条目剩余时长的本地倒计时：记录每个条目「观察到某个 `remainingSecs`」的本地时刻（避免依赖服务端时钟），
 * 之后按本地流逝时间递减；条目重新取回（`remainingSecs` 变化）时重新对时，不在列表中的条目自动丢弃。
 */
export class RemainingClock {
  private readonly seen = new Map<number, { secs: number | undefined; at: number }>()

  observe(items: readonly Pick<BtGuardEntryDto, 'id' | 'remainingSecs'>[], nowMs: number): void {
    const alive = new Set<number>()
    for (const item of items) {
      alive.add(item.id)
      const prev = this.seen.get(item.id)
      if (prev === undefined || prev.secs !== item.remainingSecs) this.seen.set(item.id, { secs: item.remainingSecs, at: nowMs })
    }
    for (const id of this.seen.keys()) if (!alive.has(id)) this.seen.delete(id)
  }

  /** 当前剩余秒数；永久缺省；未观察过的条目按原值。 */
  secs(item: Pick<BtGuardEntryDto, 'id' | 'remainingSecs'>, nowMs: number): number | undefined {
    const stamp = this.seen.get(item.id)
    return stamp === undefined ? item.remainingSecs : remainingNow(item.remainingSecs, stamp.at, nowMs)
  }
}

/** 下一次刷新倒计时的间隔：有 2 分钟内到期的条目按秒，否则 15 秒；没有限时条目返回 `null`（无需定时）。 */
export function countdownIntervalMs(secs: readonly (number | undefined)[]): number | null {
  let min: number | null = null
  for (const value of secs) if (value !== undefined) min = min === null ? value : Math.min(min, value)
  if (min === null) return null
  return min < 120 ? 1000 : 15_000
}

/** 上报开关是否置灰：只在「未开启」时受总开关 / 登录限制；已开启的上报始终可以关闭。 */
export function reportSwitchDisabled(state: { enabled: boolean; loggedIn: boolean; guardDisabled: boolean }): boolean {
  return !state.enabled && (state.guardDisabled || !state.loggedIn)
}
