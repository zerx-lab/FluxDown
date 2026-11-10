// JSON-RPC 2.0 信封与握手（native/protocol/src/rpc.rs）。

import type { JsonValue, ServiceRole } from './common';
import type { RpcErrorObject } from './error';

export const JSONRPC_VERSION = '2.0';
/** 当前协议版本；v4 增加 ShellChanged / PowerChanged，v5 增加 CaptureTasksStarted，v6 增加错误 reason 扩展、远程任务/局域网互联新参数与事件，v7 增加 CloudConnectionChanged，v8 增加应用内更新（UpdateStatusDto / UpdateChanged / agent.update.*），v9 增加「文件已存在」交互选择（SelectionKind::FileExists / SelectionOutcome::FileExists）与 `file_exists_behavior = ask`，v10 增加云端推送（TaskNotice / CloudNotifyChanged / AgentSnapshot.cloudNotify / agent.cloudNotify.*），v11 增加批量建任务（`daemon.task.createMany`）与批量任务事实事件（`tasksChanged`），v12 增加批量删除事实事件（`tasksDeleted`），v13 增加插件可移植视图事件（`pluginViewFrame` / `pluginViewClosed` / `pluginToast`），v14 增加「多插件择一」交互选择（SelectionKind::Plugin / SelectionOutcome::Plugin）与站点默认插件 RPC（`daemon.plugin.siteDefaults` / `daemon.plugin.setSiteDefault`），v15 增加队列顺序增量（`queueOrderChanged`、`QueuePositionDto.order`）与落后订阅者的帧合并（`EventFrame.previous`，按 cursor.ts 的 judgeFrame 判定），v16 增加外部恢复任务通知（`captureTasksResumed`），v17 取消全量任务单帧（`DaemonSnapshot.taskTotal` / `queueTotal`、`tablesReloading`、`system.taskPage` 分页载入），v18–v24 曾为已移除的功能扩展（版本号保留，不再使用），v25 增加周期会员卡：`CloudPlan.billingPeriodDays`、`CloudProfile` / `AgentSessionDto.activeSubscription`、`ErrorReason::SubscriptionConflict`，v26 增加 BT 防吸血（`daemon.bt.guard.*`），v27 增加定时睡眠与无任务定时（`PowerArmParams.action` / `PowerStatusDto.action`）（均为纯增量）。 */
export const PROTOCOL_VERSION = 27;
/** 服务端拒绝低于此版本的客户端，握手时不兼容即断开。 */
export const MIN_PROTOCOL_VERSION = 19;
/** 服务因 `system.shutdown` 退出时的 WebSocket 关闭原因：客户端据此停止重连与重拉。 */
export const CLOSE_REASON_SERVICE_QUIT = 'service-quit';
/** server 模式更新重启时的关闭原因：客户端保持重连，serviceVersion 变化后刷新页面。 */
export const CLOSE_REASON_SERVICE_RESTART = 'service-restart';
/** 订阅者落后于事件广播（lagged）时服务端发送的关闭码：客户端必须重新 snapshot。 */
export const EVENT_GAP_CLOSE_CODE = 4009;
/** 与 {@link EVENT_GAP_CLOSE_CODE} 一同发送的关闭原因。 */
export const EVENT_GAP_CLOSE_REASON = 'event-gap';

/** 请求 ID：字符串或有符号 64 位整数。 */
export type RequestId = string | number;

/** 首帧 `system.hello` 的参数。 */
export interface ClientHello {
  clientName: string;
  clientVersion: string;
  minProtocolVersion: number;
  maxProtocolVersion: number;
  requestedRole: ServiceRole;
  /** 客户端能力（如 `client.selections`）；缺省视为空。 */
  capabilities?: string[];
}

/** `system.hello` 的结果。 */
export interface ServiceHello {
  role: ServiceRole;
  serviceName: string;
  serviceVersion: string;
  protocolVersion: number;
  instanceId: string;
  capabilities: string[];
}

/** 带 ID 的调用请求（服务端 deny_unknown_fields）。 */
export interface RpcRequest {
  jsonrpc: typeof JSONRPC_VERSION;
  id: RequestId;
  method: string;
  params?: JsonValue | object;
}

/** 不带 ID 的通知（如 `service.event`）。 */
export interface RpcNotification {
  jsonrpc: typeof JSONRPC_VERSION;
  method: string;
  params?: JsonValue | object;
}

/** 成功响应：永远没有 `error` 字段。 */
export interface RpcSuccessResponse {
  jsonrpc: typeof JSONRPC_VERSION;
  id: RequestId;
  result: unknown;
}

/** 失败响应：永远没有 `result` 字段；无法恢复请求 ID 的解析/无效请求错误时 `id` 为 null。 */
export interface RpcFailureResponse {
  jsonrpc: typeof JSONRPC_VERSION;
  id: RequestId | null;
  error: RpcErrorObject;
}

export type RpcResponse = RpcSuccessResponse | RpcFailureResponse;
export type RpcIncoming = RpcRequest | RpcNotification;
