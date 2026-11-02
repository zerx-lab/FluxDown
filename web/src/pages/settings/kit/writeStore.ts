// 设置写回泵：乐观本地覆盖 + 250ms 防抖合并 + daemon 修订冲突自动重试（对应 GPUI `crates/settings/src/store.rs`）。
//
// - daemon 键 → `daemon.config.patch({expectedRevision, values})`；落在云同步目录里的 daemon 键
//   改走 `agent.preferences.patch`（同步键名，JSON 值），与 GPUI `set_daemon` 一致。
// - agent 偏好 → `agent.preferences.patch`；云同步目录内的键默认同步，其余 `sync:false`（设备本地）。
// - 覆盖层只用于让控件立刻回显；写回成功后短暂保留（等快照事件追上）再丢弃，失败立即回滚。
// - 连接未就绪（只读）时拒绝写入并报「本地服务未连接」。

import { useSyncExternalStore } from 'react'
import { daemonConfigField, rpc, rpcStore, RpcError } from '../../../lib/rpc'
import type { JsonValue } from '../../../lib/rpc'
import { RPC_ERROR_KEYS, rpcErrorKind } from '../../../lib/rpcErrorText'
import type { RpcErrorKind } from '../../../lib/rpcErrorText'

const FLUSH_DEBOUNCE_MS = 250
const MAX_CONFLICT_RETRIES = 3
/** 写回成功后覆盖层保留时长：足够快照事件到达。 */
const OVERLAY_GRACE_MS = 2000

export type SettingsErrorKind = RpcErrorKind

export interface SettingsError {
  kind: SettingsErrorKind
  detail: string
}

/** 对应 assets/i18n 的既有键（同 GPUI `SettingsErrorKind::i18n_key`）。 */
export const SETTINGS_ERROR_KEYS = RPC_ERROR_KEYS

// ── 云同步目录（镜像 native/protocol/src/settings.rs SYNC_SETTING_SPECS）──

/** 偏好 / agent 所有、参与云同步的键。 */
export const SYNCED_PREF_KEYS: ReadonlySet<string> = new Set([
  'appearance.theme_mode',
  'appearance.dark_theme',
  'appearance.light_theme',
  'appearance.color_scheme',
  'appearance.custom_color',
  'appearance.custom_themes',
  'appearance.file_icon_pack',
  'general.locale',
  'general.update_channel',
  'general.auto_check_update',
  'general.clipboard_watch',
  'general.floating_ball_enabled',
  'general.floating_ball_active_only',
  'ui.show_sidebar_status',
  'ui.show_sidebar_queues',
  'ui.show_sidebar_category',
  'ui.show_sidebar_rss',
  'ui.show_activity_rss',
  'ui.show_activity_webhooks',
  'ui.show_activity_theme',
  'ui.show_activity_account',
  'ui.show_titlebar_pause_all',
  'ui.show_titlebar_resume_all',
  'ui.show_titlebar_settings',
  'ui.show_titlebar_theme',
  'download.remember_last_save_dir',
  'download.notify_on_complete',
  'download.silent_download',
  'download.keep_awake',
  'custom_categories',
])

/** daemon 存储键 → 云同步键（owner = Daemon 的条目）。 */
export const DAEMON_SYNC_KEYS: Readonly<Record<string, string>> = {
  max_concurrent_tasks: 'download.max_concurrent_tasks',
  default_segments: 'download.default_segments',
  auto_max_connections: 'download.auto_max_connections',
  cdn_multi_enabled: 'download.cdn_multi_enabled',
  cdn_max_nodes: 'download.cdn_max_nodes',
  speed_limit_bytes: 'download.speed_limit_bytes',
  max_auto_retries: 'download.max_auto_retries',
  auto_retry_delay_secs: 'download.auto_retry_delay_secs',
  auto_resume_on_start: 'download.auto_resume_on_start',
  use_server_time: 'download.use_server_time',
  global_user_agent: 'download.global_user_agent',
  bt_enabled: 'bt.enabled',
  bt_enable_dht: 'bt.enable_dht',
  bt_enable_upnp: 'bt.enable_upnp',
  bt_enable_utp: 'bt.enable_utp',
  bt_custom_trackers: 'bt.custom_trackers',
  bt_tracker_sub_enabled: 'bt.tracker_sub_enabled',
  bt_tracker_sub_urls: 'bt.tracker_sub_urls',
  bt_seed_ratio_limit: 'bt.seed_ratio_limit',
  bt_seed_post_ratio_limit: 'bt.seed_post_ratio_limit',
  bt_seed_time_limit_minutes: 'bt.seed_time_limit_minutes',
  bt_seed_inactive_time_limit_minutes: 'bt.seed_inactive_time_limit_minutes',
  bt_seed_limit_operator: 'bt.seed_limit_operator',
  bt_seed_then_action: 'bt.seed_then_action',
  bt_seed_max_active: 'bt.seed_max_active',
  bt_guard_enabled: 'bt.guard.enabled',
  bt_guard_rule_excessive: 'bt.guard.rule_excessive',
  bt_guard_rule_rerequest: 'bt.guard.rule_rerequest',
  bt_guard_rule_rewind: 'bt.guard.rule_rewind',
  bt_guard_rule_lag: 'bt.guard.rule_lag',
  bt_guard_rule_fingerprint: 'bt.guard.rule_fingerprint',
  bt_guard_cloud_mode: 'bt.guard.cloud_mode',
  bt_guard_excess_ratio_pct: 'bt.guard.excess_ratio_pct',
  bt_guard_min_torrent_mib: 'bt.guard.min_torrent_mib',
  bt_guard_rewind_tolerance_pct: 'bt.guard.rewind_tolerance_pct',
  bt_guard_throttle_kib: 'bt.guard.throttle_kib',
  bt_guard_ban_base_hours: 'bt.guard.ban_base_hours',
  bt_guard_ban_max_days: 'bt.guard.ban_max_days',
  bt_guard_downloading_max_action: 'bt.guard.downloading_max_action',
  bt_guard_skip_private: 'bt.guard.skip_private',
  bt_guard_skip_lan: 'bt.guard.skip_lan',
  bt_guard_ipv6_prefix: 'bt.guard.ipv6_prefix',
  bt_guard_filter_urls: 'bt.guard.filter_urls',
  bt_guard_cloud_enabled: 'bt.guard.cloud_enabled',
  ed2k_enable_kad: 'ed2k.enable_kad',
  ed2k_enable_upnp: 'ed2k.enable_upnp',
  ed2k_server_list: 'ed2k.server_list',
  ed2k_server_sub_enabled: 'ed2k.server_sub_enabled',
  ed2k_server_sub_urls: 'ed2k.server_sub_urls',
}

// ── 值规范化（镜像 normalize_daemon_config_value）──

/** 规范化 daemon 配置值；非法返回 Error（消息为可展示细节）。 */
export function normalizeDaemonValue(key: string, value: string): string | Error {
  const field = daemonConfigField(key)
  if (!field) return new Error(`unknown config key: ${key}`)
  switch (field.kind) {
    case 'readOnly':
      return new Error(`${key} is read-only`)
    case 'bool': {
      const v = value.trim()
      if (v === 'true' || v === '1') return 'true'
      if (v === 'false' || v === '0') return 'false'
      return new Error(`${key}: expected boolean`)
    }
    case 'integer': {
      const v = value.trim()
      if (!/^[+-]?\d+$/.test(v)) return new Error(`${key}: expected integer`)
      const parsed = Number.parseInt(v, 10)
      const min = field.min ?? Number.MIN_SAFE_INTEGER
      const max = field.max ?? Number.MAX_SAFE_INTEGER
      if (parsed < min || parsed > max) return new Error(`${key}: must be between ${min} and ${max}`)
      return String(parsed)
    }
    case 'float': {
      const parsed = Number.parseFloat(value.trim())
      if (!Number.isFinite(parsed) || parsed < (field.min ?? 0)) return new Error(`${key}: must be a finite number >= ${field.min ?? 0}`)
      return String(parsed)
    }
    case 'enum': {
      const v = value.trim()
      return field.options?.includes(v) ? v : new Error(`${key}: must be one of ${field.options?.join(', ')}`)
    }
    case 'text':
      return value.trim()
  }
}

function daemonWireToJson(specKey: string, wire: string): JsonValue {
  const field = daemonConfigField(Object.entries(DAEMON_SYNC_KEYS).find(([, spec]) => spec === specKey)?.[0] ?? '')
  switch (field?.kind) {
    case 'bool':
      return wire === 'true'
    case 'integer':
    case 'float': {
      const n = Number(wire)
      return Number.isFinite(n) ? n : null
    }
    default:
      return wire
  }
}

// ── 存储 ──

interface PendingPref {
  value: JsonValue
  synced: boolean
  /** 同步类 daemon 键经偏好通道写回时，对应的 daemon 存储键与规范化 wire 值（用于清理其覆盖层）。 */
  daemonKey?: string
  daemonWire?: string
}

let overlayDaemon = new Map<string, string>()
/** 写入时快照里该 daemon 键的值：快照之后偏离它且不等于覆盖值，说明被外部改动，覆盖层作废。 */
let overlayBaseline = new Map<string, string | undefined>()
let overlayPrefs = new Map<string, JsonValue>()
let pendingDaemon = new Map<string, string>()
let pendingPrefs = new Map<string, PendingPref>()

/** 覆盖层是否仍有效；快照被外部改成别的值时返回 undefined（回落到快照）。 */
export function resolveDaemonOverlay(
  overlay: string | undefined,
  baseline: string | undefined,
  snapshot: string | undefined,
): string | undefined {
  if (overlay === undefined) return undefined
  if (snapshot !== undefined && snapshot !== baseline && snapshot !== overlay) return undefined
  return overlay
}

function dropDaemonOverlay(key: string): boolean {
  overlayBaseline.delete(key)
  return overlayDaemon.delete(key)
}

function rollbackPref(key: string, pending: PendingPref): void {
  overlayPrefs.delete(key)
  if (pending.daemonKey !== undefined) dropDaemonOverlay(pending.daemonKey)
}
let error: SettingsError | null = null
let version = 0
let scheduled: ReturnType<typeof setTimeout> | null = null
let inflight = false
let conflictRetries = 0
/** 冲突时服务端回带的最新修订（快照事件可能稍晚）。 */
let revisionHint = 0
const listeners = new Set<() => void>()

function emit(): void {
  version += 1
  for (const listener of listeners) listener()
}

const subscribe = (listener: () => void): (() => void) => {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

function isReady(): boolean {
  return rpcStore.peek().connection.phase === 'ready'
}

function setError(kind: SettingsErrorKind, detail = ''): void {
  error = { kind, detail }
  emit()
}

export function clearSettingsError(): void {
  if (error === null) return
  error = null
  emit()
}

/** 最近一次写回失败；无则 null。 */
export function useSettingsError(): SettingsError | null {
  return useSyncExternalStore(subscribe, () => error, () => error)
}

function kindOf(err: unknown): SettingsErrorKind {
  return err instanceof RpcError ? rpcErrorKind(err) : 'failed'
}

function schedule(immediate: boolean): void {
  if (scheduled !== null) {
    if (!immediate) return
    clearTimeout(scheduled)
  }
  scheduled = setTimeout(
    () => {
      scheduled = null
      void flush()
    },
    immediate ? 0 : FLUSH_DEBOUNCE_MS,
  )
}

function dropOverlayLater(daemon: Map<string, string>, prefs: Map<string, PendingPref>): void {
  setTimeout(() => {
    let changed = false
    for (const [key, value] of daemon) {
      if (overlayDaemon.get(key) === value && !pendingDaemon.has(key)) {
        changed = dropDaemonOverlay(key) || changed
      }
    }
    for (const [key, pending] of prefs) {
      if (pending.daemonKey !== undefined) {
        if (overlayDaemon.get(pending.daemonKey) === pending.daemonWire && !pendingPrefs.has(key)) {
          changed = dropDaemonOverlay(pending.daemonKey) || changed
        }
      } else if (overlayPrefs.get(key) === pending.value && !pendingPrefs.has(key)) {
        overlayPrefs.delete(key)
        changed = true
      }
    }
    if (changed) emit()
  }, OVERLAY_GRACE_MS)
}

/** 立即把待写回的编辑发出去（页面卸载 / 关闭前）。 */
export async function flushSettings(): Promise<void> {
  if (scheduled !== null) {
    clearTimeout(scheduled)
    scheduled = null
  }
  await flush()
}

async function flush(): Promise<void> {
  if (inflight) return
  if (pendingDaemon.size === 0 && pendingPrefs.size === 0) return
  if (!isReady()) {
    // 只读：丢弃编辑并回滚覆盖层。
    for (const key of pendingDaemon.keys()) dropDaemonOverlay(key)
    for (const [key, pending] of pendingPrefs) rollbackPref(key, pending)
    pendingDaemon = new Map()
    pendingPrefs = new Map()
    setError('disconnected')
    return
  }
  inflight = true
  const daemonValues = pendingDaemon
  const prefValues = pendingPrefs
  pendingDaemon = new Map()
  pendingPrefs = new Map()

  const requeueDaemon = () => {
    for (const [key, value] of daemonValues) if (!pendingDaemon.has(key)) pendingDaemon.set(key, value)
  }
  const requeuePrefs = (entries: [string, PendingPref][]) => {
    for (const [key, pending] of entries) if (!pendingPrefs.has(key)) pendingPrefs.set(key, pending)
  }
  const synced = [...prefValues].filter(([, pending]) => pending.synced)
  const local = [...prefValues].filter(([, pending]) => !pending.synced)
  const toValues = (entries: [string, PendingPref][]) => Object.fromEntries(entries.map(([key, pending]) => [key, pending.value]))

  let retry = false
  let failure: unknown = null
  const outcomes: Promise<void>[] = []
  // 只对成功的子调用保留「稍后丢弃覆盖层」；失败的立即回滚，冲突重试的保持原样。
  const okDaemon = new Map<string, string>()
  const okPrefs = new Map<string, PendingPref>()

  if (daemonValues.size > 0) {
    const snapshotRevision = rpcStore.peek().snapshot?.daemon.config.revision ?? 0
    outcomes.push(
      rpc.daemon.config
        .patch({ expectedRevision: Math.max(snapshotRevision, revisionHint), values: Object.fromEntries(daemonValues) })
        .then(() => {
          for (const [key, value] of daemonValues) okDaemon.set(key, value)
        })
        .catch((err: unknown) => {
          if (err instanceof RpcError && err.appCode === 'conflict' && conflictRetries < MAX_CONFLICT_RETRIES) {
            if (err.revision !== undefined) revisionHint = err.revision
            requeueDaemon()
            retry = true
            return
          }
          failure ??= err
          for (const key of daemonValues.keys()) dropDaemonOverlay(key)
        }),
    )
  }
  const prefCall = (entries: [string, PendingPref][], sync: boolean) => {
    if (entries.length === 0) return
    outcomes.push(
      rpc.agent.preferences
        .patch(sync ? { values: toValues(entries) } : { values: toValues(entries), sync: false })
        .then(() => {
          for (const [key, pending] of entries) okPrefs.set(key, pending)
        })
        .catch((err: unknown) => {
          if (err instanceof RpcError && err.appCode === 'conflict' && conflictRetries < MAX_CONFLICT_RETRIES) {
            requeuePrefs(entries)
            retry = true
            return
          }
          failure ??= err
          for (const [key, pending] of entries) rollbackPref(key, pending)
        }),
    )
  }
  prefCall(synced, true)
  prefCall(local, false)

  await Promise.all(outcomes)
  inflight = false
  if (failure !== null) {
    conflictRetries = 0
    const kind = kindOf(failure)
    error = { kind, detail: kind === 'invalidArgument' && failure instanceof Error ? failure.message : '' }
  } else if (retry) {
    conflictRetries += 1
  } else {
    conflictRetries = 0
    if (error !== null && error.kind !== 'invalidArgument') error = null
  }
  dropOverlayLater(okDaemon, okPrefs)
  emit()
  if (pendingDaemon.size > 0 || pendingPrefs.size > 0) schedule(false)
}

if (typeof window !== 'undefined') {
  window.addEventListener('pagehide', () => void flushSettings())
}

// ── 写入 API ──

/** 写一个 daemon 配置键（wire 字符串）。非法值报 invalidArgument（不发请求）。 */
export function setDaemon(key: string, value: string): void {
  if (!isReady()) {
    setError('disconnected')
    return
  }
  const normalized = normalizeDaemonValue(key, value)
  if (normalized instanceof Error) {
    setError('invalidArgument', normalized.message)
    return
  }
  const snapshotValue = rpcStore.peek().snapshot?.daemon.config.values[key]
  const effective = resolveDaemonOverlay(overlayDaemon.get(key), overlayBaseline.get(key), snapshotValue)
  const current = effective ?? snapshotValue ?? daemonConfigField(key)?.default
  if (current === normalized) {
    // 与当前显示值相同：丢弃可能已陈旧的覆盖层，避免它遮住之后的真实值。
    if (overlayDaemon.has(key) && effective === undefined) {
      dropDaemonOverlay(key)
      emit()
    }
    return
  }
  overlayDaemon.set(key, normalized)
  overlayBaseline.set(key, snapshotValue)
  const specKey = DAEMON_SYNC_KEYS[key]
  if (specKey) {
    pendingPrefs.set(specKey, {
      value: daemonWireToJson(specKey, normalized),
      synced: true,
      daemonKey: key,
      daemonWire: normalized,
    })
  } else {
    pendingDaemon.set(key, normalized)
  }
  emit()
  schedule(false)
}

export const setDaemonBool = (key: string, value: boolean): void => setDaemon(key, String(value))
export const setDaemonNumber = (key: string, value: number): void => setDaemon(key, String(value))

/** 写一个 agent 偏好；`immediate` 跳过防抖（外观类需要立即生效）。 */
export function setPref(key: string, value: JsonValue, options?: { immediate?: boolean }): void {
  if (!isReady()) {
    setError('disconnected')
    return
  }
  const current = overlayPrefs.has(key)
    ? overlayPrefs.get(key)
    : (rpcStore.peek().snapshot?.preferences.values as Record<string, JsonValue> | undefined)?.[key]
  if (current === value) return
  overlayPrefs.set(key, value)
  pendingPrefs.set(key, { value, synced: SYNCED_PREF_KEYS.has(key) })
  emit()
  schedule(options?.immediate === true)
}

// ── 读取 hooks（覆盖层 ?? 快照 ?? 默认）──

/** daemon 配置值（wire 字符串）；缺省取目录默认值。 */
export function useDaemonValue(key: string): string {
  const overlay = useSyncExternalStore(subscribe, () => overlayDaemon.get(key), () => undefined)
  const baseline = useSyncExternalStore(subscribe, () => overlayBaseline.get(key), () => undefined)
  const snapshot = useSyncExternalStore(
    rpcStore.subscribe,
    () => rpcStore.getPublished().snapshot?.daemon.config.values[key],
    () => undefined,
  )
  return resolveDaemonOverlay(overlay, baseline, snapshot) ?? snapshot ?? daemonConfigField(key)?.default ?? ''
}

export function useDaemonBool(key: string): boolean {
  const raw = useDaemonValue(key)
  return raw === 'true' || raw === '1'
}

export function useDaemonNumber(key: string): number {
  const raw = useDaemonValue(key)
  const parsed = Number(raw.trim())
  if (raw.trim() !== '' && Number.isFinite(parsed)) return parsed
  const fallback = Number(daemonConfigField(key)?.default ?? '0')
  return Number.isFinite(fallback) ? fallback : 0
}

/** agent 偏好原值（未设置为 undefined）。 */
export function usePrefRaw(key: string): JsonValue | undefined {
  const overlay = useSyncExternalStore(subscribe, () => overlayPrefs.get(key), () => undefined)
  const snapshot = useSyncExternalStore(
    rpcStore.subscribe,
    () => (rpcStore.getPublished().snapshot?.preferences.values as Record<string, JsonValue> | undefined)?.[key],
    () => undefined,
  )
  return overlay !== undefined ? overlay : snapshot
}

export function usePrefBoolean(key: string, fallback: boolean): boolean {
  const value = usePrefRaw(key)
  return typeof value === 'boolean' ? value : fallback
}

export function usePrefString(key: string, fallback: string): string {
  const value = usePrefRaw(key)
  return typeof value === 'string' ? value : fallback
}

export function usePrefNumber(key: string, fallback: number): number {
  const value = usePrefRaw(key)
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback
}

/** 连接是否就绪；未就绪时设置页只读（写入会被拒绝）。 */
export function useSettingsReadOnly(): boolean {
  return useSyncExternalStore(
    rpcStore.subscribe,
    () => rpcStore.getPublished().connection.phase !== 'ready',
    () => true,
  )
}
