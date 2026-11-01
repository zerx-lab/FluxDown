// 队列与任务组 DTO。

import type { CategoryRouteDto } from "./agent";

export interface ArchiveMetadataDto {
  platform: string;
  mediaKind: string;
  creatorId: string;
  creatorName: string;
  seriesId: string;
  seriesTitle: string;
  collectionId: string;
  collectionTitle: string;
}

/** 命名队列。 */
export interface QueueDto {
  queueId: string;
  name: string;
  /** 限速 KB/s，0 = 不限速。 */
  speedLimitKbps: number;
  /** 上传限速 KB/s，0 = 不限速。 */
  uploadLimitKbps: number;
  /** 并发上限，0 = 跟随全局。 */
  maxConcurrent: number;
  defaultSaveDir: string;
  position: number;
  defaultSegments: number;
  defaultUserAgent: string;
  /** 停止的队列不自动启动其中任务。 */
  isRunning: boolean;
  scheduleEnabled: boolean;
  /** 每日定时启动 `HH:MM`（空 = 不定时启动）。 */
  scheduleStart: string;
  /** 每日定时停止 `HH:MM`（空 = 不定时停止）。 */
  scheduleStop: string;
  /** 星期位掩码：bit0 = 周一 … bit6 = 周日；127 = 每天。 */
  scheduleDays: number;
}

/**
 * 任务在队列中的位置。全量列表按 `order` 升序排列，`position` = 下标 + 1。
 * 增量见 `WsServerMsg` 的 `queueOrderChanged`。
 */
export interface QueuePositionDto {
  taskId: string;
  /** 1-based，0 = 不在队列中。 */
  position: number;
  /**
   * 引擎分配的排序键：升序即启动顺序；任务留在队列期间不变（队首出队不改其余任务的键），
   * 只有用户调整顺序时整表重排。客户端排序用它，展示序号按键的名次计算。缺省按 0。
   */
  order: number;
}

/** 队列顺序增量的单项（`queueOrderChanged` 的 `added`）。 */
export interface QueueOrderEntryDto {
  taskId: string;
  order: number;
}

/** 创建/更新队列的字段（更新时同时需要 `queueId`）。除 `name` 外可省略。 */
export interface QueueInput {
  name: string;
  speedLimitKbps?: number;
  uploadLimitKbps?: number;
  maxConcurrent?: number;
  defaultSaveDir?: string;
  defaultSegments?: number;
  defaultUserAgent?: string;
}

/** 任务组。 */
export interface GroupDto {
  groupId: string;
  name: string;
  /** 原始分享/清单链接。 */
  sourceUrl: string;
  /** 组根目录。 */
  saveDir: string;
  /** Unix 秒级时间戳（字符串）。 */
  createdAt: string;
}

/** 创建任务组的单个成员条目。 */
export interface GroupItemRequest {
  /** `<itemId>` 或 `<itemId>@<variantId>`。 */
  resolverItem: string;
  fileName: string;
  /** 相对组根目录的子路径（空 = 组根）。 */
  relPath?: string;
  /** 已知大小（字节，0 = 未知）。 */
  size?: number;
}

/** 创建任务组请求；`items` 不可为空。 */
export interface CreateGroupRequest {
  sourceUrl?: string;
  /** 空 = 组根目录直接用 `saveDir`。 */
  groupName?: string;
  saveDir?: string;
  queueId?: string;
  segments?: number;
  cookies?: string;
  referrer?: string;
  userAgent?: string;
  proxyUrl?: string;
  extraHeaders?: Record<string, string>;
  ignoreTlsErrors?: boolean;
  startPaused?: boolean;
  /**
   * 预解析清单的来源插件（`ResolvePreviewResponse.pluginId` 原样回填）。非空 = 引擎把全部成员绑定到
   * 该插件，插件已不可用则成员 fail-closed；空 = 引擎按预解析同一规则选插件。
   */
  resolverPluginId?: string;
  archiveMetadata?: ArchiveMetadataDto;
  /** Only present when following classification; omit for explicit save directories. */
  categoryRoute?: CategoryRouteDto;
  items: GroupItemRequest[];
}

export interface CreateGroupResponse {
  groupId: string;
}

/** 前置预解析请求（只读，不建任务）。 */
export interface ResolvePreviewRequest {
  url: string;
  cookies?: string;
  referrer?: string;
  userAgent?: string;
  extraHeaders?: Record<string, string>;
}

/** 预解析清单的单个规格（画质/格式）。 */
export interface PreviewVariantDto {
  id: string;
  label: string;
  /** 字节，0 = 未知。 */
  size: number;
}

export interface PreviewItemDto {
  /** 建组时拼进 `resolverItem`。 */
  id: string;
  name: string;
  /** 相对组根目录的子路径（空 = 根）。 */
  path: string;
  /** 字节，0 = 未知。 */
  size: number;
  variants: PreviewVariantDto[];
}

/** `items` 与 `error` 均为空 = 插件未返回清单（回退普通建任务）；`error` 非空 = 预解析失败。 */
export interface ResolvePreviewResponse {
  name: string;
  sourceUrl: string;
  error: string;
  /** 产出清单的插件 identity；无清单为空串（旧 daemon 缺省）。建组时回填 `CreateGroupRequest.resolverPluginId`。 */
  pluginId?: string;
  items: PreviewItemDto[];
  /** Root metadata shared by all preview items. */
  archiveMetadata?: ArchiveMetadataDto;
}
