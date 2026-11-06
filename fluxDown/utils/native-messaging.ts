/**
 * Native Messaging 通信模块
 * 通过 Native Messaging 协议与 FluxDown 桌面应用通信
 *
 * 通信链路：
 *   Browser Extension
 *     <-> browser.runtime.connectNative() (stdin/stdout, 4字节LE长度前缀+JSON)
 *   fluxdown_nmh.exe (中继进程)
 *     <-> Named Pipe \\.\pipe\fluxdown (4字节LE长度前缀+JSON)
 *   FluxDown App
 *
 * 设计决策：
 *   - 使用 connectNative() 持久连接，复用同一 NMH 进程；后台休眠（Service Worker / Firefox event page）
 *     会丢弃端口及内存状态，下一次请求按需重建，不依赖端口保活
 *   - 端口上的请求串行处理；队首卡死即回收端口（见 nmh-shared.ts）
 *   - 请求-响应通过 msg_id 匹配（递增 ID + pending Map）
 *   - App 未运行时由 NMH 自动启动，扩展端无需关心唤起逻辑
 *   - "warmup" 消息由 NMH 本地应答（确保 App 已拉起 + 管道已连接，不转发给 App），
 *     下载流程入口 fire-and-forget 发送，让 App 冷启动与 cookie 收集并行
 *   - 超时 12s（预留 NMH 启动 App 的等待时间）
 */

import { browser } from "wxt/browser";
import { recordConnectionDiagnostic } from "./connection-diagnostics";
import type { NmhPortLike } from "./nmh-dedicated";
import { createSharedChannel } from "./nmh-shared";

export const NMH_NAME = "com.fluxdown.nmh";

// 每请求超时时间（NMH 启动 App 最多需要 ~7.5s，预留充足余量）
const REQUEST_TIMEOUT_MS = 12000;

// 熔断确认 ping 的短超时：仅用于探测 App 是否"当下可达"（liveness），
// 无需等待 App 冷启动——下载发送阶段已用 12s×2 给过 App 充足的拉起窗口。
// 用短超时避免回退前再额外阻塞 ~24s（review round2 发现 #3/#5 的 ~48s 卡顿）。
const PING_TIMEOUT_MS = 4000;

// 任务面板轮询超时：不走 sendWithRetry（失败即视为"未连接"，由 popup/alarm
// 轮询下一轮自然重试），短超时避免 App 无响应时长时间阻塞 UI 刷新。
const TASKS_POLL_TIMEOUT_MS = 3000;

// ──────────────────────────────────────────────────────────────
// 类型定义
// ──────────────────────────────────────────────────────────────

/**
 * 浏览器原始请求体——通过 webRequest.onBeforeRequest 抓取，
 * 用于让 Rust 下载器一比一重建浏览器看到的请求事务。
 *
 * - `formData`：来自 chrome.webRequest 的 `requestBody.formData`。
 *   Rust 端用 reqwest::form() 编码为 application/x-www-form-urlencoded。
 * - `raw`：原始字节（base64 编码），覆盖 fetch/XHR 直接传 ArrayBuffer 的场景。
 */
export type RequestBody =
  | { kind: "formData"; fields: Record<string, string[]> }
  | { kind: "raw"; bytesB64: string; contentType?: string };

export interface DownloadRequest {
  url: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string>;
  fileSize?: number;
  mimeType?: string;
  /**
   * 浏览器原始 HTTP method。省略 = "GET"。
   * 在 form-POST 触发的下载场景下必传，否则 FluxDown 会用 GET 重发拿到错误内容。
   */
  method?: string;
  /** 浏览器原始请求体（仅非 GET 时有意义）。 */
  body?: RequestBody;
  /**
   * 离散音视频轨对的音频轨 URL（通用语义，非站点特判）。
   * 非空 = 这是一对需要分别下载后 mux 合并的视频轨 + 音频轨；
   * 省略/空 = 普通单 URL 下载。与 NMH 契约 audio_url 字段对应。
   */
  audioUrl?: string;
  /** 目标设备上的保存目录（站点配置 saveDir）；省略 = 桌面端默认目录。 */
  saveDir?: string;
}

export interface ApiResponse {
  success: boolean;
  message?: string;
  taskId?: string;
  /**
   * 实际处理本次请求的通道，由 download-dispatch 路由层回填：
   * "local"=桌面 App（NMH），"remote"=远程 fluxdown_server。
   * 直接调用 native-messaging/remote-server 时不设置。
   */
  channel?: "local" | "remote";
  /**
   * action:"tasks" 响应携带的任务列表（仅 "tasks" action 使用，其余 action 不设置）。
   */
  tasks?: TaskBrief[];
  /** 协议 v2 应答附带的结构化数据（download / capabilities / capture_status 等）。 */
  data?: unknown;
}

/**
 * 批量下载条目——nmhSendBatchDownloadRequest 的入参形状，字段名与单条
 * DownloadRequest 完全一致，经 toBatchWireItem 精简后随 batch_download
 * action 一次性发出（含 method/body：与单条协议同构，App 侧按 URL 缓存恢复）。
 */
export interface BatchDownloadItem {
  url: string;
  audioUrl?: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string>;
  fileSize?: number;
  mimeType?: string;
  method?: string;
  body?: RequestBody;
  saveDir?: string;
}

/**
 * 任务面板简要信息（action:"tasks" 响应条目，字段与 Rust 端 camelCase 契约一一对应）。
 * status: 0=pending,1=downloading,2=paused,3=completed,4=error,5=preparing
 */
export interface TaskBrief {
  taskId: string;
  fileName: string;
  status: number;
  downloadedBytes: number;
  totalBytes: number;
  /** 实时下载速率，单位 B/s；无记录（未在下载中）为 0 */
  speed: number;
  errorMessage?: string;
  /** Unix 秒级时间戳（字符串），与 Rust 端 TaskDto.createdAt 格式一致 */
  createdAt: string;
  /** 已完成任务的落盘文件已被删除 / 移走（主程序维护）；缺省 = 未缺失。仅 true 时由 App 发出。 */
  fileMissing?: boolean;
}

// ──────────────────────────────────────────────────────────────
// 常驻共享端口（连接管理、串行队列与卡死回收见 nmh-shared.ts）
// ──────────────────────────────────────────────────────────────

/** Firefox 把断开原因放在 port.error（runtime.lastError 恒为 null），Chrome 用 runtime.lastError。 */
function disconnectReason(port: NmhPortLike): string | undefined {
  const portError = "error" in port ? port.error : undefined;
  const error = portError ?? browser.runtime.lastError;
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
    return error.message;
  }
  return undefined;
}

const shared = createSharedChannel({
  connect: () => browser.runtime.connectNative(NMH_NAME) as NmhPortLike,
  disconnectReason,
  record: recordConnectionDiagnostic,
});

function disconnectPort() {
  shared.recycle();
}

async function sendMessage(
  action: string,
  payload: Record<string, unknown> = {},
  timeoutMs: number = REQUEST_TIMEOUT_MS,
): Promise<ApiResponse> {
  const reply = await shared.send(action, payload, timeoutMs);
  return {
    success: reply.success === true,
    message: typeof reply.message === "string" ? reply.message : undefined,
    taskId: typeof reply.taskId === "string" ? reply.taskId : undefined,
    tasks: Array.isArray(reply.tasks) ? (reply.tasks as TaskBrief[]) : undefined,
    data: reply.data,
  };
}

/**
 * Send a message with one retry on transient failures.
 * If the first attempt fails due to a stale port or the App not running,
 * disconnects the old port and retries once — Chrome will spawn a fresh
 * NMH process which auto-launches the App.
 */
async function sendWithRetry(
  action: string,
  payload: Record<string, any>,
  timeoutMs: number = REQUEST_TIMEOUT_MS,
): Promise<ApiResponse> {
  const result = await sendMessage(action, payload, timeoutMs);
  if (result.success) return result;

  // Retry once on transient failures (gets a fresh NMH process that auto-launches App)
  const retryable =
    result.message === "port disconnected" ||
    result.message === "postMessage failed" ||
    result.message === "app_not_running" ||
    result.message === "timeout";

  if (!retryable) return result;
  recordConnectionDiagnostic({ event: "native.retry", action, detail: result.message });

  disconnectPort();

  // 「消息可能已送达但响应丢失」的失败类先 ping 探活，App 在线即判定已送达、
  // 不重发——盲重发会让 App 重复建任务（批量场景下放大为整块重复）：
  //   • "port disconnected"：NMH 可能在转发给 App 后才断开；
  //   • "timeout"：postMessage 已成功（消息已进内核缓冲），NMH 大概率已转发，
  //     只是 App 处理慢（批量建任务/冷启动）或响应丢失；
  //   • "app_not_running"：NMH 对「管道读失败」（写已成功 = 消息已送达 App）
  //     与「压根没连上」回的是同一文案，无法区分——按已送达保守处理，与
  //     NMH 层「读失败不重发,防重复任务」的架构不变式一致。
  // "postMessage failed" 不在此列：消息未进内核，重发安全。
  const maybeDelivered =
    result.message === "port disconnected" ||
    result.message === "timeout" ||
    result.message === "app_not_running";
  if (maybeDelivered && action !== "ping") {
    const pingResult = await sendMessage("ping", {}, timeoutMs);
    if (pingResult.success) {
      console.log(
        `[FluxDown NMH] App alive after "${result.message}" — message likely delivered, skipping retry`,
      );
      return {
        success: true,
        message: "delivered (reconnected after transient failure)",
      };
    }
    // ping 也失败（App 确实不在）→ 消息未被处理，重发安全
    disconnectPort();
  }

  return sendMessage(action, payload, timeoutMs);
}

// ──────────────────────────────────────────────────────────────
// 导出接口（与原 HTTP 版本完全兼容）
// ──────────────────────────────────────────────────────────────

/**
 * 过滤伪 referrer：浏览器 downloads API / fetch 规范在 JS 触发下载等场景
 * 会给出 "about:client" 等占位符，并非真实来源页 URL。原样透传会被部分
 * CDN 防盗链判为非法 Referer（HTTP 403），视同缺失。
 */
function sanitizeReferrer(referrer: string | undefined): string {
  const r = (referrer || "").trim();
  return /^https?:\/\//i.test(r) ? r : "";
}

export async function nmhSendDownloadRequest(
  request: DownloadRequest,
): Promise<ApiResponse> {
  return sendWithRetry("download", {
    ...request,
    referrer: sanitizeReferrer(request.referrer),
  } as Record<string, any>);
}

// NMH/hub 两端对单帧强制 1MB 上限；留给 action/msg_id 等帧头开销及安全冗余，
// 单块 items 序列化后不超过该字节数（典型批量数十条会落在一块内）。
const BATCH_CHUNK_BYTES_LIMIT = 700 * 1024;

// 与 hub 端 parse_batch_download 的 MAX_BATCH_ITEMS=1000 硬上限对齐：700KB
// 理论上能塞下数千条短 URL 条目，若单块条目数越过 App 端上限，会收到
// "too many items" 错误——它既不含 "unknown action"（不触发 legacy 回退）
// 也不在 NMH 不可达文案集合里（不触发远程 fallback），整批直接失败。
// 故分块必须同时满足字节与条目数两个上限。
const BATCH_CHUNK_ITEMS_LIMIT = 1000;

/**
 * 把 BatchDownloadItem 映射为 batch_download wire 条目：字段名与单条 download
 * action 完全一致，undefined/空值字段省略以压缩帧体积。
 */
function toBatchWireItem(item: BatchDownloadItem): Record<string, any> {
  const wire: Record<string, any> = { url: item.url };
  if (item.audioUrl) wire.audioUrl = item.audioUrl;
  if (item.filename) wire.filename = item.filename;
  const referrer = sanitizeReferrer(item.referrer);
  if (referrer) wire.referrer = referrer;
  if (item.cookies) wire.cookies = item.cookies;
  if (item.headers && Object.keys(item.headers).length > 0) {
    wire.headers = item.headers;
  }
  if (item.fileSize != null) wire.fileSize = item.fileSize;
  if (item.mimeType) wire.mimeType = item.mimeType;
  if (item.method) wire.method = item.method;
  if (item.body) wire.body = item.body;
  if (item.saveDir) wire.saveDir = item.saveDir;
  return wire;
}

/** 按 UTF-8 字节数（而非字符数）估算 JSON 序列化体积——文件名等字段常含中文。 */
function jsonByteLength(value: unknown): number {
  return new TextEncoder().encode(JSON.stringify(value)).length;
}

/**
 * 贪心切块：单块同时满足 ≤ BATCH_CHUNK_BYTES_LIMIT 字节与
 * ≤ BATCH_CHUNK_ITEMS_LIMIT 条（与 App 端 MAX_BATCH_ITEMS 对齐）。单个条目
 * 本身超过字节阈值时（理论上极罕见）仍独占一块发送，交由 App/hub 端把关。
 */
function chunkBatchWireItems(
  wireItems: Record<string, any>[],
): Record<string, any>[][] {
  const chunks: Record<string, any>[][] = [];
  let current: Record<string, any>[] = [];
  let currentBytes = 0;

  for (const wireItem of wireItems) {
    const itemBytes = jsonByteLength(wireItem);
    if (
      current.length > 0 &&
      (currentBytes + itemBytes > BATCH_CHUNK_BYTES_LIMIT ||
        current.length >= BATCH_CHUNK_ITEMS_LIMIT)
    ) {
      chunks.push(current);
      current = [];
      currentBytes = 0;
    }
    current.push(wireItem);
    currentBytes += itemBytes;
  }
  if (current.length > 0) chunks.push(current);
  return chunks;
}

/**
 * Legacy 回退路径：旧版桌面 App 不识别 batch_download action 时，退化为逐条
 * 发送单个 download 请求（新协议引入前的原始实现，聚合语义原样保留）。
 * 仅由 nmhSendBatchDownloadRequest 在探测到 "unknown action" 响应时整批调用。
 */
async function nmhSendBatchDownloadLegacy(
  items: BatchDownloadItem[],
): Promise<ApiResponse> {
  const results = await Promise.allSettled(
    items.map((item) =>
      nmhSendDownloadRequest({
        url: item.url,
        audioUrl: item.audioUrl,
        filename: item.filename || "",
        referrer: item.referrer || "",
        cookies: item.cookies,
        headers: item.headers,
        fileSize: item.fileSize,
        mimeType: item.mimeType,
        method: item.method,
        body: item.body,
        saveDir: item.saveDir,
      }),
    ),
  );

  const succeeded = results.filter(
    (r) => r.status === "fulfilled" && r.value.success,
  ).length;
  const failed = results.length - succeeded;

  if (succeeded === 0) {
    const firstError = results.find(
      (r) =>
        (r.status === "fulfilled" && !r.value.success) ||
        r.status === "rejected",
    );
    const message =
      firstError?.status === "fulfilled"
        ? firstError.value.message
        : firstError?.status === "rejected"
          ? String(firstError.reason)
          : "All items failed";
    return { success: false, message: `Batch failed: ${message}` };
  }

  return {
    success: true,
    message:
      failed > 0
        ? `${succeeded}/${results.length} items sent (${failed} failed)`
        : `${succeeded} items sent`,
  };
}

/**
 * 批量下载：单条 batch_download NMH 消息携带全部条目，取代逐条循环发送。
 * per-item 的 cookies/headers/referrer/fileSize/mimeType/audioUrl 随消息一并
 * 送达，由 Rust 侧按 URL 缓存、在用户于快速下载对话框确认后逐条恢复
 * （单条确认 ConfirmExternalDownload 与多条确认 BatchCreateTask 两条路径都
 * 会回填该缓存）——不再需要为每个 URL 单独打一次 NMH 往返（旧实现的
 * 性能/时序问题的根因），也不会像早期实现那样静默丢弃 audioUrl。
 *
 * 分块：NMH 与 hub 两端都对单帧强制 1MB 上限，因此按条目 JSON 字节数贪心
 * 切块（见 chunkBatchWireItems），单块 ≤ 700KB；典型批量（数十条）落在
 * 一块内。多块时按顺序 sendWithRetry：首块唤起 App 小窗，后续块携带同一
 * 批次的剩余条目，由 App 侧 append 到已打开的窗口，不重复弹窗。
 *
 * Legacy 回退：若某块的失败响应 message 含 "unknown action"，说明连接的是
 * 不认识 batch_download action 的旧版 App——整批切换到
 * nmhSendBatchDownloadLegacy 的逐条 download 循环（保留旧实现原样，包括
 * 其 "x/y items sent (z failed)" 部分成功聚合语义）。其余失败（端口不可达、
 * 超时等）按现有语义直接返回该块的失败 ApiResponse，不做逐条回退——是否
 * 改投远程通道由上层 download-dispatch 决定。
 */
export async function nmhSendBatchDownloadRequest(
  items: BatchDownloadItem[],
): Promise<ApiResponse> {
  if (items.length === 0) {
    return { success: false, message: "No items" };
  }

  const chunks = chunkBatchWireItems(items.map(toBatchWireItem));

  let sentChunks = 0;
  const transactionIds: string[] = [];
  for (const chunk of chunks) {
    const result = await sendWithRetry("batch_download", { items: chunk });
    if (!result.success) {
      // 部分成功守卫优先于一切回退：已有分块送达后,无论何种失败都不能
      // 触发「全量重发」类兜底(legacy 逐条 / 远程改投都会重复已建任务)。
      if (sentChunks > 0) {
        return {
          success: false,
          message: `partial batch: ${sentChunks}/${chunks.length} chunks sent, then: ${result.message}`,
        };
      }
      if (result.message?.includes("unknown action")) {
        return nmhSendBatchDownloadLegacy(items);
      }
      return result;
    }
    sentChunks += 1;
    const data = result.data;
    if (data && typeof data === "object" && "transactionIds" in data && Array.isArray(data.transactionIds)) {
      for (const id of data.transactionIds) if (typeof id === "string") transactionIds.push(id);
    }
  }

  return {
    success: true,
    message: `${items.length} items sent`,
    data: transactionIds.length > 0 ? { transactionIds } : undefined,
  };
}

/**
 * 协议 v2 的通用请求：不重试，调用方自行处理 `unknown action`
 * （旧桌面端）等失败。`data` 为应答的结构化载荷。
 */
export async function nmhCall<T>(
  action: string,
  payload: object,
  timeoutMs: number = REQUEST_TIMEOUT_MS,
): Promise<{ success: boolean; message?: string; data?: T }> {
  const result = await sendMessage(action, payload as Record<string, unknown>, timeoutMs);
  return { success: result.success, message: result.message, data: result.data as T | undefined };
}

// ──────────────────────────────────────────────────────────────
// 任务面板（action:"tasks" / "task_op" / "open_file" / "reveal_file"）
// ──────────────────────────────────────────────────────────────

/**
 * 拉取任务面板列表：全部非 completed 任务 + 最近完成 10 条。
 *
 * 有意不走 sendWithRetry：这是低频轮询（popup 打开时 + alarm 周期性刷新），
 * 单次失败无副作用（不像 download 那样"消息可能已送达但响应丢失"需要谨慎
 * 处理），失败直接视为"未连接"，等下一轮轮询自然恢复即可，避免重试拖长
 * 单次调用耗时。用短超时（3s）保证 UI/alarm 不被无响应的 App 卡住。
 */
export async function nmhListTasks(): Promise<{
  success: boolean;
  tasks: TaskBrief[];
  message?: string;
}> {
  const result = await sendMessage("tasks", {}, TASKS_POLL_TIMEOUT_MS);
  if (!result.success) {
    return { success: false, tasks: [], message: result.message };
  }
  return { success: true, tasks: result.tasks ?? [] };
}

/**
 * 任务操作：暂停 / 继续 / 删除。与 download 共用 sendWithRetry 语义——
 * "port disconnected" 时先 ping 确认 App 是否已收到消息，避免盲目重发；
 * 其余瞬态失败（超时/端口断开前的写失败/App 未运行）可安全重发：remove
 * 对已删除任务重发是幂等的，pause/resume 重发到达同一目标状态同样无害。
 */
export async function nmhTaskOp(
  op: "pause" | "resume" | "remove",
  taskId: string,
): Promise<ApiResponse> {
  return sendWithRetry("task_op", { op, taskId });
}

/** 用系统默认程序打开已完成任务的文件。语义同 nmhTaskOp——重发安全。 */
export async function nmhOpenFile(taskId: string): Promise<ApiResponse> {
  return sendWithRetry("open_file", { taskId });
}

/** 在文件管理器中定位已完成任务的文件。语义同 nmhTaskOp——重发安全。 */
export async function nmhRevealFile(taskId: string): Promise<ApiResponse> {
  return sendWithRetry("reveal_file", { taskId });
}

// 进行中的 warmup 请求（去重：并发下载入口只发一个 warmup）
let _warmupInFlight: Promise<ApiResponse> | null = null;

/**
 * 预热 NMH 链路：让 NMH 提前拉起 App 并建立管道连接。
 *
 * Fire-and-forget 语义——调用方不等待、不处理结果。价值在冷启动路径：
 * 下载入口先发 warmup，App 启动（~0.7-1s）与 cookie/认证收集（最多 500ms）
 * 并行进行，而不是串行叠加。App 已运行时 warmup 是 ~1ms 的本地快速应答。
 *
 * 有意用 sendMessage 而非 sendWithRetry：warmup 是纯优化，失败（旧版 NMH
 * 不识别、App 未安装）静默忽略，真正的下载发送自带完整重试链。
 * 旧版 NMH 会把 warmup 当普通消息转发给 App（App 回 unknown action），
 * 但转发前同样会 auto-launch——预热效果不变。
 */
export function nmhWarmupNativeHost(): void {
  if (_warmupInFlight) return;
  _warmupInFlight = sendMessage("warmup").finally(() => {
    _warmupInFlight = null;
  });
  // 吞掉结果与异常：预热失败不影响任何现有流程
  _warmupInFlight.catch(() => {});
}

export async function nmhCheckFluxDownAvailable(): Promise<boolean> {
  const result = await sendMessage("ping");
  return result.success === true;
}

/**
 * 带一次重连重试的可用性探测。
 *
 * 与 nmhCheckFluxDownAvailable 的区别：首次 ping 失败（超时/端口断开/app_not_running）时，
 * 断开旧端口并以全新 NMH 进程再 ping 一次。用于"是否要熔断"这类需排除瞬态失败的判定：
 * App 冷启动（NMH 拉起 App 最多 ~7.5s）或瞬时繁忙时，单次 ping 可能误报不可达，
 * 重连重试给 App 第二次机会，避免把已安装但临时不可达的 App 误判为不可用（review 发现 #3）。
 */
export async function nmhCheckFluxDownAvailableWithRetry(): Promise<boolean> {
  // 用短超时（PING_TIMEOUT_MS）做重连重试探测：最坏 ~2×4s，而非 2×12s，
  // 既保留对瞬时断连/陈旧端口的重连第二次机会，又不显著拖慢回退（review #3/#5）。
  const result = await sendWithRetry("ping", {}, PING_TIMEOUT_MS);
  return result.success === true;
}
