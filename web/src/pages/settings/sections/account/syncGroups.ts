// 配置同步「在此设备同步的范围」：按同步目录键前缀分组，逐组开关 → `agent.sync.setLocalOnly`。
// 键表镜像 `native/protocol/src/settings.rs::SYNC_SETTING_SPECS`（`syncGroups.test.ts` 会与 Rust 源文件核对）。

export type SyncGroupId = 'appearance' | 'general' | 'ui' | 'download' | 'bt' | 'ed2k' | 'categories'

export interface SyncGroup {
  id: SyncGroupId
  /** 分组标题 i18n 键。 */
  labelKey: string
  keys: readonly string[]
}

export const SYNC_GROUPS: readonly SyncGroup[] = [
  {
    id: 'appearance',
    labelKey: 'syncScopeAppearance',
    keys: [
      'appearance.theme_mode',
      'appearance.dark_theme',
      'appearance.light_theme',
      'appearance.color_scheme',
      'appearance.custom_color',
      'appearance.custom_themes',
      'appearance.file_icon_pack',
    ],
  },
  {
    id: 'general',
    labelKey: 'syncScopeGeneral',
    keys: [
      'general.locale',
      'general.update_channel',
      'general.auto_check_update',
      'general.clipboard_watch',
      'general.floating_ball_enabled',
      'general.floating_ball_active_only',
    ],
  },
  {
    id: 'ui',
    labelKey: 'syncScopeUi',
    keys: [
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
    ],
  },
  {
    id: 'download',
    labelKey: 'syncScopeDownload',
    keys: [
      'download.max_concurrent_tasks',
      'download.default_segments',
      'download.auto_max_connections',
      'download.cdn_multi_enabled',
      'download.cdn_max_nodes',
      'download.speed_limit_bytes',
      'download.max_auto_retries',
      'download.auto_retry_delay_secs',
      'download.auto_resume_on_start',
      'download.remember_last_save_dir',
      'download.use_server_time',
      'download.global_user_agent',
      'download.notify_on_complete',
      'download.silent_download',
      'download.keep_awake',
    ],
  },
  {
    id: 'bt',
    labelKey: 'syncScopeBt',
    keys: [
      'bt.enabled',
      'bt.enable_dht',
      'bt.enable_upnp',
      'bt.enable_utp',
      'bt.custom_trackers',
      'bt.tracker_sub_enabled',
      'bt.tracker_sub_urls',
      'bt.seed_ratio_limit',
      'bt.seed_post_ratio_limit',
      'bt.seed_time_limit_minutes',
      'bt.seed_inactive_time_limit_minutes',
      'bt.seed_limit_operator',
      'bt.seed_then_action',
      'bt.seed_max_active',
      'bt.guard.enabled',
      'bt.guard.rule_excessive',
      'bt.guard.rule_rerequest',
      'bt.guard.rule_rewind',
      'bt.guard.rule_lag',
      'bt.guard.rule_fingerprint',
      'bt.guard.cloud_mode',
      'bt.guard.excess_ratio_pct',
      'bt.guard.min_torrent_mib',
      'bt.guard.rewind_tolerance_pct',
      'bt.guard.throttle_kib',
      'bt.guard.ban_base_hours',
      'bt.guard.ban_max_days',
      'bt.guard.downloading_max_action',
      'bt.guard.skip_private',
      'bt.guard.skip_lan',
      'bt.guard.ipv6_prefix',
      'bt.guard.filter_urls',
      'bt.guard.cloud_enabled',
    ],
  },
  {
    id: 'ed2k',
    labelKey: 'syncScopeEd2k',
    keys: ['ed2k.enable_kad', 'ed2k.enable_upnp', 'ed2k.server_list', 'ed2k.server_sub_enabled', 'ed2k.server_sub_urls'],
  },
  { id: 'categories', labelKey: 'syncScopeCategories', keys: ['custom_categories'] },
]

export type SyncGroupState = 'sync' | 'local' | 'mixed'

/** 分组当前状态：全部参与同步 / 全部本设备专属 / 混合（其他客户端逐键设置过）。 */
export function syncGroupState(group: SyncGroup, localOnlyKeys: readonly string[]): SyncGroupState {
  const local = new Set(localOnlyKeys)
  const localCount = group.keys.filter((key) => local.has(key)).length
  if (localCount === 0) return 'sync'
  return localCount === group.keys.length ? 'local' : 'mixed'
}

/**
 * 点击分组开关后应发送的 `setLocalOnly` 参数：仅「全部参与同步」时切为本设备专属，
 * 其余（本设备专属 / 混合）一律恢复同步。
 */
export function toggleGroupParams(group: SyncGroup, state: SyncGroupState): { keys: string[]; localOnly: boolean } {
  return { keys: [...group.keys], localOnly: state === 'sync' }
}

export type SyncPhase = 'off' | 'halted' | 'error' | 'connecting' | 'syncing' | 'synced'

/** 状态优先级：未启用 → 暂停(halted) → 失败 → 连接中 → 同步中(有脏键) → 已同步。 */
export function syncPhase(sync: {
  enabled: boolean
  halted?: boolean
  connected?: boolean
  lastError: string | null
  lastErrorReason?: string | null
  dirtyKeys: readonly string[]
}): SyncPhase {
  if (!sync.enabled) return 'off'
  if (sync.halted) return 'halted'
  if (sync.lastError || sync.lastErrorReason) return 'error'
  if (sync.connected === false) return 'connecting'
  return sync.dirtyKeys.length > 0 ? 'syncing' : 'synced'
}
