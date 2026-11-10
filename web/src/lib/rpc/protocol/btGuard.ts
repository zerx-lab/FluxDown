// BT 防吸血（`daemon.bt.guard.*`，镜像 native/protocol/src/daemon.rs 的 BtGuard* DTO）。
// `cloud.apply` / `evidence.drain` 只供 agent 内部使用，Web 不封装也不调用。

import type { JsonValue } from './common';

/** 过滤条目所在层（优先级 allow > manual > auto / cloud / file）。 */
export const BT_GUARD_LAYERS = ['allow', 'manual', 'auto', 'cloud', 'file'] as const;
export type BtGuardLayer = (typeof BT_GUARD_LAYERS)[number];

/** 过滤条目动作。 */
export const BT_GUARD_ENTRY_ACTIONS = ['allow', 'ban', 'choke', 'throttle'] as const;
export type BtGuardEntryAction = (typeof BT_GUARD_ENTRY_ACTIONS)[number];

/**
 * 规则 id：检测规则 + `cloud`（事件 / 条目 / 证据共用），以及手动 / 订阅文件层条目的 `manual` / `file`
 * （引擎为这两层条目写入的 rule 值，只出现在条目与筛选里）。未知取值按原文显示。
 */
export const BT_GUARD_RULES = ['excessive', 'rerequest', 'rewind', 'lag', 'fingerprint', 'cloud', 'manual', 'file'] as const;
export type BtGuardRule = (typeof BT_GUARD_RULES)[number];

/** 列表单页上限 / 默认页大小 / 批量解除单次 id 上限。 */
export const BT_GUARD_LIST_MAX_LIMIT = 500;
export const BT_GUARD_LIST_DEFAULT_LIMIT = 100;
export const BT_GUARD_REMOVE_MAX_IDS = 1000;

export interface BtGuardStatsDto {
  todayObserved: number;
  todayEnforced: number;
  todayBlockedConnections: number;
  todayRefusedBytes: number;
}

export interface BtGuardCloudStatusDto {
  /** 当前已应用文档序号；0 = 无。 */
  sequence: number;
  /** 文档应用时间（unix 秒）；0 = 无。 */
  updatedAt: number;
  /** 文档过期时间（unix 秒）；0 = 无。 */
  expiresAt: number;
  entries: number;
  clientRules: number;
  /** 文档已过期，云层停用。 */
  stale: boolean;
  lastError?: string;
  /** 最近一份被拒绝文档的原因；成功应用后清空。 */
  rejectedReason?: string;
  attribution?: string[];
}

export interface BtGuardFilterSourceDto {
  url: string;
  ok: boolean;
  entries: number;
  /** 最近一次成功时间（unix 秒）；0 = 从未成功。 */
  updatedAt: number;
  error?: string;
}

export interface BtGuardCountsDto {
  allow: number;
  manual: number;
  auto: number;
  cloud: number;
  file: number;
}

/** `daemon.bt.guard.state` 结果。 */
export interface BtGuardStateDto {
  enabled: boolean;
  /** BT 会话存活（防吸血仅在会话内生效）。 */
  sessionActive: boolean;
  stats: BtGuardStatsDto;
  cloud: BtGuardCloudStatusDto;
  filterSources: BtGuardFilterSourceDto[];
  counts: BtGuardCountsDto;
}

export interface BtGuardEntryDto {
  id: number;
  layer: BtGuardLayer;
  /** `1.2.3.4` / `1.2.3.0/24` / `2001:db8::/64`。 */
  cidr: string;
  action: BtGuardEntryAction;
  /** 触发规则 id；手动 / 订阅条目缺省。 */
  rule?: string;
  reason?: string;
  /** 证据摘要（JSON）。 */
  evidence?: JsonValue;
  /** `auto` / `user` / 订阅 URL / `cloud:community` 等。 */
  source?: string;
  createdAt: number;
  /** 到期 unix 秒；缺省 = 永久。 */
  expiresAt?: number;
  /** 距到期剩余秒数（≥ 0）；永久缺省。 */
  remainingSecs?: number;
}

export interface BtGuardEntriesListParams {
  layer?: BtGuardLayer;
  rule?: BtGuardRule;
  /** 对 cidr / reason 的子串搜索。 */
  query?: string;
  /** 只返回地址范围包含该 IP 的条目（服务端过滤）。 */
  containsIp?: string;
  offset: number;
  limit: number;
}

export interface BtGuardEntriesListDto {
  /** 满足过滤条件的总数（不受分页影响）。 */
  total: number;
  items: BtGuardEntryDto[];
}

export interface BtGuardEntriesAddParams {
  cidr: string;
  action: BtGuardEntryAction;
  /** 有效期秒数；缺省 = 永久。 */
  durationSecs?: number;
  reason: string;
}

export interface BtGuardEntriesRemoveParams {
  ids: number[];
}

export interface BtGuardEntriesRemoveDto {
  removed: number;
}

export interface BtGuardEventDto {
  id: number;
  at: number;
  /** 事件类型：`observed` / `enforced` / `probe` / `probe_cleared` / `entry_added` / `entry_removed` / `cloud_applied` / `cloud_rejected` / `filter_applied` 等（未知取值原样显示）。 */
  kind: string;
  ipGroup: string;
  /** 种子 infohash（40 hex）；无关种子的事件缺省。 */
  infoHash?: string;
  rule?: string;
  /** 规则当时的模式（`observe` / `enforce`）。 */
  mode?: string;
  action?: string;
  detail?: string;
}

export interface BtGuardEventsListParams {
  offset: number;
  limit: number;
  rule?: BtGuardRule;
  kind?: string;
  /** 只返回该 IP 组的事件（服务端过滤，与事件的 `ipGroup` 文本一致）。 */
  ipGroup?: string;
}

export interface BtGuardEventsListDto {
  total: number;
  items: BtGuardEventDto[];
}

export interface BtGuardExportParams {
  layer?: BtGuardLayer;
}

/** 每行 `cidr # layer rule reason expires`。 */
export interface BtGuardExportDto {
  text: string;
  lines: number;
}

export interface BtGuardFilterSourcesRefreshDto {
  sources: BtGuardFilterSourceDto[];
}
