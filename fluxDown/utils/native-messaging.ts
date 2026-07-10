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
 *   - 使用 connectNative() 持久连接，复用同一 NMH 进程
 *   - 请求-响应通过 msg_id 匹配（递增 ID + pending Map）
 *   - App 未运行时由 NMH 自动启动，扩展端无需关心唤起逻辑
 *   - "warmup" 消息由 NMH 本地应答（确保 App 已拉起 + 管道已连接，不转发给 App），
 *     下载流程入口 fire-and-forget 发送，让 App 冷启动与 cookie 收集并行
 *   - 超时 12s（预留 NMH 启动 App 的等待时间）
 */

import { browser } from "wxt/browser";

const NMH_NAME = "com.fluxdown.nmh";

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
}

export interface BatchDownloadItem {
  url: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string>;
  fileSize?: number;
  mimeType?: string;
  method?: string;
  body?: RequestBody;
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
}

// ──────────────────────────────────────────────────────────────
// 内部状态
// ──────────────────────────────────────────────────────────────

let _port: chrome.runtime.Port | null = null;
let _nextMsgId = 1;

interface PendingRequest {
  resolve: (value: ApiResponse) => void;
  timer: ReturnType<typeof setTimeout>;
}

const _pendingRequests = new Map<number, PendingRequest>();

// ──────────────────────────────────────────────────────────────
// 端口管理
// ──────────────────────────────────────────────────────────────

function getPort(): chrome.runtime.Port | null {
  if (_port) return _port;

  try {
    _port = browser.runtime.connectNative(NMH_NAME);
  } catch (e) {
    // connectNative() throws synchronously if the API is unavailable (e.g. permission denied).
    console.error("[FluxDown NMH] connectNative() threw:", e);
    return null;
  }

  _port.onMessage.addListener((msg: any) => {
    const msgId = msg?.msg_id;
    if (msgId == null) return;

    const pending = _pendingRequests.get(msgId);
    if (!pending) return;

    _pendingRequests.delete(msgId);
    clearTimeout(pending.timer);

    pending.resolve({
      success: msg.success ?? false,
      message: msg.message,
      taskId: msg.taskId,
      tasks: Array.isArray(msg.tasks) ? (msg.tasks as TaskBrief[]) : undefined,
    });
  });

  _port.onDisconnect.addListener((p) => {
    // Log disconnect reason to help diagnose NMH failures.
    // IMPORTANT: Firefox exposes the error on the port parameter p.error,
    // NOT on browser.runtime.lastError (which is always null in Firefox).
    // Chrome uses browser.runtime.lastError instead.
    // Common errors: "No such native application", "Access to the specified
    // native messaging host is forbidden" (extension ID mismatch).
    const err = (p as any).error ?? browser.runtime.lastError;
    if (err?.message) {
      console.error("[FluxDown NMH] port disconnected, reason:", err.message);
    } else {
      console.warn("[FluxDown NMH] port disconnected (no error reason)");
    }
    _port = null;
    // Reject all pending requests
    for (const [id, pending] of _pendingRequests) {
      clearTimeout(pending.timer);
      pending.resolve({ success: false, message: "port disconnected" });
      _pendingRequests.delete(id);
    }
  });

  return _port;
}

function disconnectPort() {
  if (_port) {
    try {
      _port.disconnect();
    } catch {
      /* ignore */
    }
    _port = null;
  }
}

// ──────────────────────────────────────────────────────────────
// 消息发送
// ──────────────────────────────────────────────────────────────

function sendMessage(
  action: string,
  payload: Record<string, any> = {},
  timeoutMs: number = REQUEST_TIMEOUT_MS,
): Promise<ApiResponse> {
  return new Promise<ApiResponse>((resolve) => {
    const port = getPort();
    if (!port) {
      resolve({ success: false, message: "native_messaging_unavailable" });
      return;
    }

    const msgId = _nextMsgId++;
    const timer = setTimeout(() => {
      _pendingRequests.delete(msgId);
      resolve({ success: false, message: "timeout" });
    }, timeoutMs);

    _pendingRequests.set(msgId, { resolve, timer });

    try {
      port.postMessage({ action, msg_id: msgId, ...payload });
    } catch {
      _pendingRequests.delete(msgId);
      clearTimeout(timer);
      disconnectPort();
      resolve({ success: false, message: "postMessage failed" });
    }
  });
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

  disconnectPort();

  // "port disconnected" 特殊处理：NMH 进程可能在将消息转发给 App 后才断开，
  // 此时消息已送达但响应丢失。重连后先 ping：如果 App 在线，说明消息
  // 大概率已送达，直接返回成功，避免重复发送导致 App 创建重复任务。
  if (result.message === "port disconnected" && action !== "ping") {
    const pingResult = await sendMessage("ping", {}, timeoutMs);
    if (pingResult.success) {
      console.log(
        "[FluxDown NMH] App alive after port disconnect — message likely delivered, skipping retry",
      );
      return {
        success: true,
        message: "delivered (reconnected after disconnect)",
      };
    }
    // ping 也失败，断开后重试发送原消息
    disconnectPort();
  }

  return sendMessage(action, payload, timeoutMs);
}

// ──────────────────────────────────────────────────────────────
// 导出接口（与原 HTTP 版本完全兼容）
// ──────────────────────────────────────────────────────────────

export async function nmhSendDownloadRequest(
  request: DownloadRequest,
): Promise<ApiResponse> {
  return sendWithRetry("download", request as Record<string, any>);
}

export async function nmhSendBatchDownloadRequest(
  items: BatchDownloadItem[],
): Promise<ApiResponse> {
  if (items.length === 0) {
    return { success: false, message: "No items" };
  }

  // Send each item as an individual download request to preserve per-item
  // cookies, headers, referrer, fileSize, and mimeType.  The Rust NMH
  // protocol only supports single-item DownloadRequest — batching by
  // newline-joining URLs discards all per-item auth metadata.
  const results = await Promise.allSettled(
    items.map((item) =>
      nmhSendDownloadRequest({
        url: item.url,
        filename: item.filename || "",
        referrer: item.referrer || "",
        cookies: item.cookies,
        headers: item.headers,
        fileSize: item.fileSize,
        mimeType: item.mimeType,
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
