// 引擎实时消息（`DaemonEvent.engine` 的 data；native/protocol/src/daemon.rs WsServerMsg）。
// JSON 形态：`{"type":"taskProgress","taskId":"…",…}`（`type` 判别 + 扁平 camelCase 字段）。

import type { BtFileDto, HlsQualityOptionDto, ResolveVariantOptionDto } from './selection';
import type { GroupDto, QueueDto, QueueOrderEntryDto, QueuePositionDto } from './queue';
import type { RssItemDto, RssSourceDto } from './rss';
import type { TaskDto } from './task';
import type { WebhookDeliveryDto } from './webhook';

/** 分段字节范围与进度。 */
export interface SegmentDetailDto {
  index: number;
  startByte: number;
  endByte: number;
  downloadedBytes: number;
}

/** 多 CDN 单节点描述。 */
export interface CdnNodeDto {
  /** 节点 IP；SYS 兜底节点为 `SYS`。 */
  ip: string;
  /** 候选来源：`sys` / `doh:<端点IP>` / `ecs:<端点IP>`；SYS 为空串。 */
  origin: string;
  /** 本任务经该节点下载的字节数（summary 有效，其余 0）。 */
  bytes: number;
  /** EWMA 吞吐（B/s）。 */
  ewmaBps: number;
  /** 当前未归还的段租约数（leases 快照有效）。 */
  active: number;
}

/** 已完成任务产物文件存在性变化。 */
export interface FileMissingUpdateDto {
  taskId: string;
  /** true = 目标文件已消失；false = 重新探测到存在。 */
  missing: boolean;
}

export type WsServerMsg =
  /** 任务进度（下载中周期推送；含实时 speed，TaskDto 无此字段）。 */
  | {
      type: 'taskProgress';
      taskId: string;
      /** 0 待处理 / 1 下载中 / 2 暂停 / 3 完成 / 4 出错 / 5 准备中。 */
      status: number;
      downloadedBytes: number;
      totalBytes: number;
      /** 字节/秒。 */
      speed: number;
      /** BT 上传速率（字节/秒）。 */
      uploadSpeed: number;
      fileName: string;
      saveDir: string;
      url: string;
      /** `status == 4 && errorMessage == "deleted"` 表示任务已被删除。 */
      errorMessage: string;
      uploadedBytes: number;
      /** 0 无 / 1 做种中 / 2-7 停止原因 / 8 排队做种。 */
      seedingStatus: number;
      seedingMessage: string;
      seedingTimeSecs: number;
    }
  /** 全部任务快照。 */
  | { type: 'tasksSnapshot'; tasks: TaskDto[] }
  | {
      type: 'segmentProgress';
      taskId: string;
      totalBytes: number;
      segmentCount: number;
      segments: SegmentDetailDto[];
    }
  /** 动态分段拆分事件。 */
  | {
      type: 'segmentSplit';
      taskId: string;
      parentIndex: number;
      parentNewEnd: number;
      childIndex: number;
      childStart: number;
      childEnd: number;
      isProactive: boolean;
      totalSegments: number;
    }
  /** 多 CDN 节点级活动事件。 */
  | {
      type: 'taskCdnEvent';
      taskId: string;
      /** `pool` / `kick` / `breaker` / `fallback` / `summary`。 */
      kind: string;
      host: string;
      nodes: CdnNodeDto[];
      ip: string;
      reason: string;
      candidates: number;
      alive: number;
      cap: number;
      autoCap: boolean;
    }
  | { type: 'taskMetaProbed'; taskId: string; fileName: string; totalBytes: number }
  | { type: 'queuesChanged'; queues: QueueDto[] }
  | { type: 'taskQueueChanged'; taskId: string; queueId: string }
  /** 任务出口链路标签落定 / 变更；`route` 同 `TaskDto.autoRoute`，恒非空。 */
  | { type: 'taskRouteChanged'; taskId: string; route: string }
  | { type: 'queuePositionsChanged'; positions: QueuePositionDto[] }
  /**
   * 队列顺序增量：先移除 `removed`，再按 `order` 插入 / 更新 `added`（同 id 多次以最后一项为准），
   * 结果按键升序，`position` = 名次（语义同 Rust `apply_queue_order`）。
   */
  | { type: 'queueOrderChanged'; removed: string[]; added: QueueOrderEntryDto[] }
  | { type: 'fileMissingChanged'; updates: FileMissingUpdateDto[] }
  /** Boost 优先任务变化。 */
  | { type: 'priorityTaskChanged'; priorityTaskId: string; autoPausedCount: number }
  // 以下三种选择请求属旧 WS 服务端协议，daemon 事件流不会发出；交互选择走 selection.* 与 selectionPending 事件。
  | { type: 'hlsSelectionRequest'; taskId: string; options: HlsQualityOptionDto[] }
  | { type: 'btSelectionRequest'; taskId: string; files: BtFileDto[] }
  | {
      type: 'resolveVariantRequest';
      taskId: string;
      defaultIndex: number;
      options: ResolveVariantOptionDto[];
    }
  | { type: 'pong' }
  /** 插件被自动禁用（`reason` 为 `CircuitBreaker` 熔断或 `Revoked` 被插件市场撤回）。 */
  | { type: 'pluginAutoDisabled'; identity: string; reason: string }
  /** BT 重复添加：占位任务 `taskId` 已被删除，已有任务为 `existingTaskId`。 */
  | {
      type: 'duplicateTorrent';
      taskId: string;
      existingTaskId: string;
      existingName: string;
    }
  /** 插件 onDone 钩子进行中；可能丢失 `running=false`，客户端需自带看门狗超时。 */
  | { type: 'pluginHookActivity'; taskId: string; pluginId: string; running: boolean }
  /** 插件表变化的空载荷 ping。 */
  | { type: 'pluginsChanged' }
  | { type: 'groupsChanged'; groups: GroupDto[] }
  | { type: 'rssSourcesChanged'; sources: RssSourceDto[] }
  /** 某订阅条目流快照（新→旧）；`notifyTitles` 是本轮自动建任务的条目标题。 */
  | {
      type: 'rssItemsChanged';
      sourceId: string;
      items: RssItemDto[];
      notifyTitles: string[];
    }
  | {
      type: 'rssFeedValidated';
      requestId: string;
      url: string;
      feedTitle: string;
      items: RssItemDto[];
      error: string;
    }
  /** 组件安装进度（`component` 为 `ffmpeg` / `ytdlp`；`totalBytes = 0` 未知）。 */
  | { type: 'componentProgress'; component: string; downloadedBytes: number; totalBytes: number }
  | { type: 'componentResult'; component: string; ok: boolean; message: string }
  /** 入站设备配对请求待核验（设备互联，Web 一般不用）。 */
  | {
      type: 'linkIncomingPairing';
      sessionId: string;
      sas: string;
      name: string;
      platform: string;
    }
  /** 已配对设备名册变化的空载荷 ping。 */
  | { type: 'linkDevicesChanged' }
  | { type: 'webhookDeliveriesChanged'; deliveries: WebhookDeliveryDto[] };
