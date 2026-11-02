// 全量快照与可重放事件帧（native/protocol/src/event.rs）。

import type {
  AgentPreferencesDto,
  AgentSessionDto,
  CloudConnectionDto,
  CloudDevice,
  GatewayStatusDto,
  LinkDeviceInfo,
  LinkDiscoveredPeer,
  LinkPairingRequestDto,
  PendingCaptureDto,
  PowerStatusDto,
  RemoteTaskDto,
  ShellStatusDto,
  SyncStatusDto,
  UpdateStatusDto,
} from './agent';
import type { ErrorReason } from './error';
import type { DaemonConfigSnapshot, DaemonRuntimeStatsDto } from './config';
import type { ComponentStatusDto, PluginDto } from './plugin';
import type { GroupDto, QueueDto, QueuePositionDto } from './queue';
import type { RssSourceDto } from './rss';
import type { SelectionRequestDto } from './selection';
import type { TaskActivityDto, TaskDto, TaskRuntimeDto } from './task';
import type { CloudNotifyStateDto, TaskNoticeDto } from './cloudNotify';
import type { PluginViewFrameDto } from './pluginView';
import type { WebhookDeliveryDto } from './webhook';
import type { WsServerMsg } from './ws';

/** daemon 的完整物化投影。 */
export interface DaemonSnapshot {
  /** 任务最新传输采样，按 taskId 索引。 */
  taskRuntime: Record<string, TaskRuntimeDto>;
  /**
   * 任务全表。线上快照 / 替换事件不内嵌：`taskTotal` 存在时本字段为空，须经 `system.taskPage`
   * 分页载入（见 `../tableSync.ts`）。
   */
  tasks: TaskDto[];
  /** 存在 = 任务表尚未载入完整（服务端共这么多行）：UI 只能显示加载态，不得把 `tasks` 当完整列表。 */
  taskTotal?: number;
  queues: QueueDto[];
  queuePositions: QueuePositionDto[];
  /** 存在 = 队列位置表尚未载入完整，语义同 `taskTotal`，对应 `queuePositions`。 */
  queueTotal?: number;
  groups: GroupDto[];
  config: DaemonConfigSnapshot;
  rssSources: RssSourceDto[];
  /** 每个订阅的条目流版本；变化时重拉 `daemon.rss.getItems`。 */
  rssItemRevisions: Record<string, number>;
  plugins: PluginDto[];
  components: ComponentStatusDto[];
  webhookDeliveries: WebhookDeliveryDto[];
  /** Boost 优先任务 ID 列表。 */
  priority: string[];
  runtimeStats: DaemonRuntimeStatsDto;
  pendingSelections: SelectionRequestDto[];
}

/** agent 的完整物化投影；下载事实只来自嵌套的 `daemon`。 */
export interface AgentSnapshot {
  daemon: DaemonSnapshot;
  daemonConnected: boolean;
  session: AgentSessionDto | null;
  sync: SyncStatusDto;
  /** 旧快照缺失时视为 disconnected。 */
  cloudConnection?: CloudConnectionDto;
  update: UpdateStatusDto;
  preferences: AgentPreferencesDto;
  gateway: GatewayStatusDto;
  cloudDevices: CloudDevice[];
  linkedDevices: LinkDeviceInfo[];
  remoteTasks: RemoteTaskDto[];
  pendingCaptures: PendingCaptureDto[];
  linkPairingRequests?: LinkPairingRequestDto[];
  linkDiscovered?: LinkDiscoveredPeer[];
  shell: ShellStatusDto;
  power: PowerStatusDto;
  /** 旧快照缺失时视为默认（上报关、无概览）。 */
  cloudNotify?: CloudNotifyStateDto;
}

/** `system.snapshot` 主体（`role` 相邻标记，`snapshot` 为内容）。 */
export type SnapshotBody =
  | { role: 'daemon'; snapshot: DaemonSnapshot }
  | { role: 'agent'; snapshot: AgentSnapshot };

/** 带原子事件位置的全量快照：之后应从 `sequence + 1` 起消费同 `epoch` 的事件帧。 */
export interface Snapshot {
  epoch: string;
  sequence: number;
  body: SnapshotBody;
}

/** daemon 状态变化事件（`type` + `data` 相邻标记）。 */
export type DaemonEvent =
  | { type: 'taskRuntimeChanged'; data: TaskRuntimeDto }
  | { type: 'taskActivityAdded'; data: TaskActivityDto }
  /** 载荷永远是头（无任务表、带 `taskTotal` / `queueTotal`）：整体替换投影，随后分页载入。 */
  | { type: 'snapshotReplaced'; data: DaemonSnapshot }
  /** 整表替换行数过多：清空对应表并置待载入标记，客户端分页重载。 */
  | { type: 'tablesReloading'; data: { tasks?: number; queue?: number } }
  | { type: 'engine'; data: WsServerMsg }
  | { type: 'taskChanged'; data: TaskDto }
  /** 一批任务事实：等价于按序逐条 `taskChanged`（`daemon.task.createMany` 一块一条）。 */
  | { type: 'tasksChanged'; data: TaskDto[] }
  | { type: 'taskDeleted'; data: { taskId: string } }
  /** 一批任务已删除：等价于按序逐条 `taskDeleted`（`daemon.task.deleteMany` 一批一条）。 */
  | { type: 'tasksDeleted'; data: { taskIds: string[] } }
  | { type: 'queuesChanged'; data: QueueDto[] }
  | { type: 'groupsChanged'; data: GroupDto[] }
  | { type: 'configChanged'; data: DaemonConfigSnapshot }
  | { type: 'rssChanged'; data: { sourceId: string; itemRevision: number } }
  | { type: 'pluginsChanged'; data: PluginDto[] }
  | { type: 'componentsChanged'; data: ComponentStatusDto[] }
  /** 投递日志增量：按 `deliveryId` 合并（上限 1000，`timestampMs` 降序）；空增量不改变列表。 */
  | { type: 'webhooksChanged'; data: WebhookDeliveryDto[] }
  /** 投递日志被显式清空。 */
  | { type: 'webhooksCleared' }
  | { type: 'runtimeStatsChanged'; data: DaemonRuntimeStatsDto }
  | { type: 'selectionPending'; data: SelectionRequestDto }
  | { type: 'selectionResolved'; data: { requestId: string } }
  /** 任务生命周期语义事件：只供 agent 消费，不转发给 UI，不改变快照。 */
  | { type: 'taskNotice'; data: TaskNoticeDto }
  /** 可移植插件视图的增量帧：不改变快照；客户端按 `session` 过滤，忽略未知会话。 */
  | { type: 'pluginViewFrame'; data: PluginViewFrameDto }
  /** 可移植视图会话已结束（`reason`：`closed` / `expired` / `pluginUnloaded` / `requested`）；不改变快照。 */
  | { type: 'pluginViewClosed'; data: { session: string; reason: string } }
  /** 插件经 `flux.ui.toast` 请求的提示；不改变快照。 */
  | { type: 'pluginToast'; data: { identity: string; level: string; text: string } }
  /** BT 防吸血条目 / 状态变化：UI 据此重新拉取 `daemon.bt.guard.*`；不改变快照。 */
  | { type: 'btGuardChanged' }
  /** daemon 请求 agent 刷新云名单：只供 agent 消费，不转发给 UI，不改变快照。 */
  | { type: 'btGuardCloudRefreshWanted'; data: { haveSequence: number } }
  /** 待上报证据已累积：只供 agent 消费，不转发给 UI，不改变快照。 */
  | { type: 'btGuardEvidenceReady'; data: { count: number } };

/** agent 自有或转发的状态变化事件（`type` + `data` 相邻标记）。 */
export type AgentEvent =
  /** 转发的 daemon 事件：注意嵌套两层 `{type:'daemon', data:{type, data}}`。 */
  | { type: 'daemon'; data: DaemonEvent }
  /** 载荷永远是头（语义同 `snapshotReplaced`）。 */
  | { type: 'daemonSnapshotReplaced'; data: DaemonSnapshot }
  | { type: 'daemonConnectionChanged'; data: boolean }
  /** 会话被撤销（一次性通知，不进快照；随后是 `sessionChanged(null)`；主动登出 / 删本设备不发）。 */
  | { type: 'sessionRevoked'; data: ErrorReason }
  | { type: 'sessionChanged'; data: AgentSessionDto | null }
  | { type: 'syncChanged'; data: SyncStatusDto }
  | { type: 'cloudConnectionChanged'; data: CloudConnectionDto }
  | { type: 'updateChanged'; data: UpdateStatusDto }
  | { type: 'preferencesChanged'; data: AgentPreferencesDto }
  | { type: 'gatewayChanged'; data: GatewayStatusDto }
  | { type: 'cloudDevicesChanged'; data: CloudDevice[] }
  | { type: 'linkedDevicesChanged'; data: LinkDeviceInfo[] }
  | { type: 'linkPairingRequestsChanged'; data: LinkPairingRequestDto[] }
  | { type: 'linkDiscoveredChanged'; data: LinkDiscoveredPeer[] }
  | { type: 'remoteTasksChanged'; data: RemoteTaskDto[] }
  | { type: 'pendingCapturesChanged'; data: PendingCaptureDto[] }
  | { type: 'shellChanged'; data: ShellStatusDto }
  | { type: 'powerChanged'; data: PowerStatusDto }
  /** 外部来源未经确认直接建成的任务 ID（含插件页面脚本新建；一次性通知，不进快照）。 */
  | { type: 'captureTasksStarted'; data: string[] }
  /** 外部来源恢复的已有任务 ID（插件页面脚本恢复；一次性通知，不进快照）。 */
  | { type: 'captureTasksResumed'; data: string[] }
  /** 云端推送状态整体替换。 */
  | { type: 'cloudNotifyChanged'; data: CloudNotifyStateDto };

/** `service.event` 事件主体（`service` + `event` 相邻标记）。 */
export type ServiceEvent =
  | { service: 'daemon'; event: DaemonEvent }
  | { service: 'agent'; event: AgentEvent };

/**
 * 单个严格递增的事件帧（`service.event` 通知的 params）。
 *
 * 帧覆盖序号区间 `(previous, sequence]`：转发者为落后的订阅者合并过可取代的帧时，用 `previous`
 * 声明被合并掉的序号；缺省即 `sequence - 1`（逐帧连续）。消费方统一用 `judgeFrame` 判定。
 */
export interface EventFrame {
  epoch: string;
  sequence: number;
  previous?: number;
  event: ServiceEvent;
}
