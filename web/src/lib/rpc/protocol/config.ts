// daemon 配置、站点凭据、运行时统计与目录列举（daemon.rs / daemon_config.rs）。

/**
 * 原子配置投影。`values` 全部是字符串（布尔 `"true"/"false"`、数字十进制串），
 * 未持久化的键不在其中，需回退到 {@link DAEMON_CONFIG_FIELDS} 的默认值。
 */
export interface DaemonConfigSnapshot {
  revision: number;
  values: Record<string, string>;
}

/** `daemon.config.patch` 参数；版本落后时返回 conflict，`error.data.revision` 为当前版本。 */
export interface DaemonConfigPatch {
  expectedRevision: number;
  values: Record<string, string>;
}

/** daemon 运行状态投影。 */
export interface DaemonRuntimeStatsDto {
  activeTasks: number;
  pendingTasks: number;
  totalDownloadBps: number;
  totalUploadBps: number;
  diskFreeBytes: number | null;
  saveDir: string;
  /** 「等待自动重试」的任务数（自动重试 / 备用链路 / 插件重试已排程、尚未重新派发）；旧 daemon 不返回。 */
  retryPendingTasks?: number;
}

export interface ProxyTestRequest {
  /** `http` / `https` / `socks4` / `socks5`。 */
  proxyType: string;
  host: string;
  /** 注意：端口是字符串。 */
  port: string;
  username?: string;
  password?: string;
}

export interface ProxyTestResponse {
  latencyMs: number;
}

/** `daemon.config.proxyRoute` 参数：`rules` 缺省 = 用已保存的规则；`url` 为空只做校验。 */
export interface ProxyRouteRequest {
  url?: string;
  rules?: string;
}

export interface ProxyRuleIssue {
  /** 规则在数组中的下标（从 0 起）。 */
  index: number;
  /** `''` / `id` / `name` / `match` / `pattern` / `action` / `proxy`。 */
  field: string;
  code: string;
  detail: string;
}

export interface ProxyRouteMatch {
  index: number;
  id: string;
  name: string;
}

export interface ProxyRouteResponse {
  issues: ProxyRuleIssue[];
  matched?: ProxyRouteMatch | null;
  /** `''` = 未给出 url。 */
  reason: string;
  endpoint: string;
}

/** `daemon.link.probe` 参数：只读 HEAD 探测下载链接。 */
export interface LinkProbeParams {
  url: string;
}

export type LinkProbeVerdict = 'resource' | 'notResource' | 'unknown';

export interface LinkProbeResult {
  verdict: LinkProbeVerdict;
  /** 跟随重定向后的地址；未拿到响应时等于原 url。 */
  finalUrl: string;
  fileName: string;
  /** 小写 MIME essence，无则空。 */
  mime: string;
  /** 0 = 未知。 */
  totalBytes: number;
}

/** 按域连接上限摘要。 */
export interface ConnPolicySummaryDto {
  domainCount: number;
}

/** 系统代理检测结果；未检测到时 `detected=false` 且其余为空/0。 */
export interface SystemProxyDto {
  detected: boolean;
  proxyType: string;
  host: string;
  port: number;
  /** 逗号分隔的排除列表。 */
  noList: string;
}

/** 已保存站点凭据（不含密码）。 */
export interface SiteAuthEntryDto {
  /** `host` 或 `host:port`。 */
  site: string;
  user: string;
}

/** 站点凭据详情，含明文密码；仅供编辑/认证表单回填，勿在列表或日志回显。 */
export interface SiteAuthCredentialDto {
  site: string;
  user: string;
  pass: string;
}

/** 目录项。 */
export interface FsEntry {
  name: string;
  path: string;
}

/** 服务端目录列举（保存目录选择器）。 */
export interface FsListResponse {
  /** 实际列举的目录（绝对路径）。 */
  path: string;
  /** 上级目录，根目录时为 null。 */
  parent: string | null;
  /** 子目录（不含文件、不含隐藏目录）。 */
  dirs: FsEntry[];
  /** 无读取权限：此时 `dirs` 必为空，语义是「看不到」而非「没有」。 */
  denied: boolean;
}

/** Tracker 订阅刷新结果。 */
export interface TrackerSubRefreshResponse {
  success: boolean;
  trackerCount: number;
  okSources: number;
  totalSources: number;
  /** 缓存更新时间（Unix 秒）。 */
  updatedAt: number;
  /** 全部源失败时的错误摘要（成功为空）。 */
  error: string;
}

/** ED2K 服务器订阅刷新结果。 */
export interface Ed2kServerSubRefreshResponse {
  success: boolean;
  serverCount: number;
  okSources: number;
  totalSources: number;
  updatedAt: number;
  error: string;
}

// ── 配置键目录（镜像 daemon_config.rs 的 DAEMON_CONFIG_FIELDS）──

export const BT_SEED_TIME_UNITS = ['minutes', 'hours', 'days'] as const;
export const FILE_EXISTS_BEHAVIORS = ['rename', 'overwrite', 'skip', 'ask'] as const;
export const FILE_MISSING_ACTIONS = ['keep', 'delete'] as const;
export const BT_SEED_LIMIT_OPERATORS = ['or', 'and'] as const;
export const BT_SEED_THEN_ACTIONS = ['stop', 'delete', 'delete_files'] as const;
export const BT_MSE_MODES = ['disabled', 'enabled', 'forced'] as const;
/** 防吸血规则 / 云名单模式。 */
export const BT_GUARD_MODES = ['off', 'observe', 'enforce'] as const;
/** 防吸血：下载中任务的动作上限。 */
export const BT_GUARD_DOWNLOADING_ACTIONS = ['observe', 'throttle', 'choke', 'ban'] as const;
export const PROXY_MODES = ['none', 'system', 'manual', 'auto'] as const;
export const PROXY_TYPES = ['http', 'https', 'socks4', 'socks5'] as const;

export type DaemonConfigKind = 'bool' | 'integer' | 'float' | 'enum' | 'text' | 'readOnly';

/** 单个配置键的值域描述；值在 wire 上始终是字符串。 */
export interface DaemonConfigField {
  key: string;
  kind: DaemonConfigKind;
  /** 未持久化时的有效默认值（字符串形式）。 */
  default: string;
  /** integer 的下界 / float 的下界。 */
  min?: number;
  /** integer 的上界；缺省 = i64 上限（无实际界限）。 */
  max?: number;
  /** enum 的可选值。 */
  options?: readonly string[];
}

const bool = (key: string, dflt: string): DaemonConfigField => ({ key, kind: 'bool', default: dflt });
const int = (key: string, dflt: string, min: number, max?: number): DaemonConfigField =>
  max === undefined
    ? { key, kind: 'integer', default: dflt, min }
    : { key, kind: 'integer', default: dflt, min, max };
const float = (key: string, dflt: string, min: number): DaemonConfigField => ({
  key,
  kind: 'float',
  default: dflt,
  min,
});
const oneOf = (key: string, dflt: string, options: readonly string[]): DaemonConfigField => ({
  key,
  kind: 'enum',
  default: dflt,
  options,
});
const text = (key: string, dflt = ''): DaemonConfigField => ({ key, kind: 'text', default: dflt });
/** 引擎自行维护：可读、不可经 patch 写入。 */
const readOnly = (key: string, dflt = ''): DaemonConfigField => ({
  key,
  kind: 'readOnly',
  default: dflt,
});

/** 全部 daemon 配置键（顺序无语义）。 */
export const DAEMON_CONFIG_FIELDS: readonly DaemonConfigField[] = [
  // 下载
  text('default_save_dir'),
  int('default_segments', '0', 0, 512),
  int('auto_max_connections', '16', 0, 128),
  bool('cdn_multi_enabled', 'false'),
  int('cdn_max_nodes', '0', 0, 8),
  bool('multi_nic_enabled', 'false'),
  int('max_concurrent_tasks', '5', 1, 1024),
  int('speed_limit_bytes', '0', 0),
  int('upload_limit_bytes', '0', 0),
  int('max_auto_retries', '3', -1, 20),
  int('auto_retry_delay_secs', '5', 0, 86_400),
  bool('auto_resume_on_start', 'false'),
  bool('use_server_time', 'false'),
  bool('dedup_same_url', 'false'),
  oneOf('file_exists_behavior', 'rename', FILE_EXISTS_BEHAVIORS),
  oneOf('file_missing_action', 'keep', FILE_MISSING_ACTIONS),
  int('completed_record_retention_days', '0', 0, 3650),
  bool('idle_file_scan', 'false'),
  text('global_user_agent'),
  text('default_queue_id'),
  readOnly('domain_conn_caps'),
  // BT
  bool('bt_enabled', 'true'),
  bool('bt_enable_dht', 'true'),
  bool('bt_enable_upnp', 'true'),
  bool('bt_enable_utp', 'true'),
  int('bt_port_start', '6881', 1, 65_535),
  int('bt_port_end', '6891', 1, 65_535),
  oneOf('bt_mse_mode', 'enabled', BT_MSE_MODES),
  text('bt_custom_trackers'),
  bool('bt_tracker_sub_enabled', 'true'),
  text('bt_tracker_sub_urls'),
  readOnly('bt_tracker_sub_cache'),
  readOnly('bt_tracker_sub_updated_at', '0'),
  bool('bt_seed_enabled', 'true'),
  bool('bt_auto_reseed', 'true'),
  int('bt_seed_max_active', '0', 0),
  float('bt_seed_ratio_limit', '0', 0),
  float('bt_seed_post_ratio_limit', '0', 0),
  int('bt_seed_time_limit_minutes', '0', 0),
  oneOf('bt_seed_time_limit_unit', 'minutes', BT_SEED_TIME_UNITS),
  int('bt_seed_inactive_time_limit_minutes', '0', 0),
  oneOf('bt_seed_inactive_time_limit_unit', 'minutes', BT_SEED_TIME_UNITS),
  oneOf('bt_seed_limit_operator', 'or', BT_SEED_LIMIT_OPERATORS),
  oneOf('bt_seed_then_action', 'stop', BT_SEED_THEN_ACTIONS),
  // BT 防吸血（热更新）
  bool('bt_guard_enabled', 'true'),
  oneOf('bt_guard_rule_excessive', 'observe', BT_GUARD_MODES),
  oneOf('bt_guard_rule_rerequest', 'observe', BT_GUARD_MODES),
  oneOf('bt_guard_rule_rewind', 'observe', BT_GUARD_MODES),
  oneOf('bt_guard_rule_lag', 'observe', BT_GUARD_MODES),
  oneOf('bt_guard_rule_fingerprint', 'observe', BT_GUARD_MODES),
  oneOf('bt_guard_cloud_mode', 'observe', BT_GUARD_MODES),
  int('bt_guard_excess_ratio_pct', '200', 110, 1000),
  int('bt_guard_min_torrent_mib', '64', 0, 1_048_576),
  int('bt_guard_rewind_tolerance_pct', '7', 0, 100),
  int('bt_guard_throttle_kib', '32', 1, 1_048_576),
  int('bt_guard_ban_base_hours', '24', 1, 720),
  int('bt_guard_ban_max_days', '30', 1, 365),
  oneOf('bt_guard_downloading_max_action', 'throttle', BT_GUARD_DOWNLOADING_ACTIONS),
  bool('bt_guard_skip_private', 'true'),
  bool('bt_guard_skip_lan', 'true'),
  int('bt_guard_ipv6_prefix', '64', 48, 128),
  text('bt_guard_filter_urls'),
  bool('bt_guard_cloud_enabled', 'true'),
  // 设备本地：不进云同步
  bool('bt_guard_report_enabled', 'false'),
  // ED2K
  bool('ed2k_enable_kad', 'true'),
  bool('ed2k_enable_upnp', 'true'),
  int('ed2k_listen_port', '0', 0, 65_535),
  text('ed2k_server_list'),
  bool('ed2k_server_sub_enabled', 'true'),
  text('ed2k_server_sub_urls'),
  readOnly('ed2k_server_sub_cache'),
  readOnly('ed2k_server_sub_updated_at', '0'),
  text('ed2k_nodes_dat_url'),
  // 代理
  oneOf('proxy_mode', 'none', PROXY_MODES),
  oneOf('proxy_type', 'http', PROXY_TYPES),
  text('proxy_host'),
  text('proxy_port'),
  text('proxy_username'),
  text('proxy_password'),
  text('proxy_no_list'),
  text('proxy_rules'),
  // Webhook：JSON 数组字符串（端点列表）
  text('webhook.endpoints'),
  // 受管组件手动路径（空 = 自动解析）
  text('component.ffmpeg.path'),
  text('component.ytdlp.path'),
  // 组件下载镜像基址（空 = 直连 GitHub）；只接受 https
  text('component_mirror_base'),
  // 日志目录总大小上限（MB）
  int('log_max_size_mb', '10', 1, 1024),
];

/** 查询配置键描述；未知键返回 undefined。 */
export function daemonConfigField(key: string): DaemonConfigField | undefined {
  return DAEMON_CONFIG_FIELDS.find((field) => field.key === key);
}
