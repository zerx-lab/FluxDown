/**
 * 远程下载源 HTTP 客户端
 *
 * 通信链路：
 *   Browser Extension (background / popup)
 *     <-> fetch() HTTP/JSON（X-FluxDown-Token 鉴权）
 *   fluxdown_server（headless 下载服务，与桌面 App 并列的第二条投递通道）
 *
 * 设计决策：
 *   - payload 直接复用 native-messaging.ts 的 DownloadRequest / BatchDownloadItem
 *     类型（import type）——与 NMH wire 契约完全一致，服务端按同一套字段解析，
 *     本文件不重新定义、不做字段转换。
 *   - 返回值统一整形为与 NMH 同形的 ApiResponse，失败时 message 用稳定前缀
 *     区分"鉴权失败"（remote_auth_failed）与"网络不可达"（remote_unreachable /
 *     remote_not_configured），供 dispatch 层的路由判定与 popup 的错误文案复用。
 *   - 用 AbortSignal.timeout 做请求级超时，不依赖调用方自行 race。
 */

import type {
  ApiResponse,
  DownloadRequest,
  BatchDownloadItem,
  TaskBrief,
} from "./native-messaging";

/** remote-server 所需的最小配置（对应 FluxDownSettings 的 remoteUrl/remoteToken 子集） */
export interface RemoteServerConfig {
  /** fluxdown_server 地址，如 http://192.168.1.10:17800（不带尾部斜杠） */
  remoteUrl: string;
  /** 鉴权 token（server 端强制校验，恒非空） */
  remoteToken: string;
}

// 下载投递超时：服务端只需入队建任务、无需等待下载完成，但预留网络抖动余量。
const DOWNLOAD_TIMEOUT_MS = 15000;
// ping 探活超时：短超时，用于快速判定远程是否在线（fallback 路由决策 / popup 测试连接）。
const PING_TIMEOUT_MS = 4000;
// 任务面板轮询超时：与 native-messaging.ts 的 TASKS_POLL_TIMEOUT_MS 同一语义——
// 低频轮询，失败直接视为"未连接"，不重试，等下一轮自然恢复。
const TASKS_POLL_TIMEOUT_MS = 3000;
// 任务操作（暂停/继续/删除）超时：与 DOWNLOAD_TIMEOUT_MS 同量级，预留网络抖动余量。
const TASK_OP_TIMEOUT_MS = 10000;

/** ping 成功时的附加信息（服务端 /ping 返回 {app, version, message: "pong"}） */
export interface RemotePingResult extends ApiResponse {
  app?: string;
  version?: string;
}

function buildHeaders(cfg: RemoteServerConfig): HeadersInit {
  return {
    "Content-Type": "application/json",
    "X-FluxDown-Client": "extension",
    "X-FluxDown-Token": cfg.remoteToken,
  };
}

/**
 * 通用 JSON 请求：把 fetch 异常/HTTP 状态码整形为 ApiResponse。
 * remoteSendDownloadRequest/remoteSendBatchDownloadRequest（POST）与
 * remoteTaskOp（PUT/DELETE）共用这条路径；remoteListTasks 的响应体是裸
 * 数组、不套 ApiResponse 契约，未走这里，单独实现解析。
 *
 * message 前缀约定（供上层字符串匹配，不做本地化——本地化由 popup/dispatch 按
 * 前缀映射到 i18n key）：
 *   - "remote_not_configured"：remoteUrl 为空
 *   - "remote_auth_failed"：HTTP 401/403（token 错误）
 *   - "remote_unreachable"：fetch 抛异常（网络错误/超时/DNS 失败等）
 *   - 其余：服务端业务返回的失败信息（HTTP 状态非 2xx 或 body.success=false）
 */
async function requestJson(
  url: string,
  cfg: RemoteServerConfig,
  method: string,
  body: unknown,
  timeoutMs: number,
): Promise<ApiResponse> {
  if (!cfg.remoteUrl) {
    return { success: false, message: "remote_not_configured" };
  }

  let resp: Response;
  try {
    resp = await fetch(url, {
      method,
      headers: buildHeaders(cfg),
      body: body !== undefined ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (err) {
    return { success: false, message: `remote_unreachable: ${String(err)}` };
  }

  if (resp.status === 401 || resp.status === 403) {
    return { success: false, message: "remote_auth_failed" };
  }

  const data = await resp.json().catch(() => ({}) as Record<string, unknown>);

  if (!resp.ok) {
    return {
      success: false,
      message:
        typeof data?.message === "string"
          ? data.message
          : `HTTP ${resp.status}`,
    };
  }

  return {
    success: data?.success !== false,
    message: typeof data?.message === "string" ? data.message : undefined,
  };
}

/** 投递单个下载请求到远程服务器：POST {remoteUrl}/download */
export async function remoteSendDownloadRequest(
  req: DownloadRequest,
  cfg: RemoteServerConfig,
): Promise<ApiResponse> {
  return requestJson(
    `${cfg.remoteUrl}/download`,
    cfg,
    "POST",
    req,
    DOWNLOAD_TIMEOUT_MS,
  );
}

/** 批量投递下载请求：POST {remoteUrl}/download/batch，body 为 {items:[...]} */
export async function remoteSendBatchDownloadRequest(
  items: BatchDownloadItem[],
  cfg: RemoteServerConfig,
): Promise<ApiResponse> {
  return requestJson(
    `${cfg.remoteUrl}/download/batch`,
    cfg,
    "POST",
    { items },
    DOWNLOAD_TIMEOUT_MS,
  );
}

/**
 * 探活：GET {remoteUrl}/ping（无鉴权，200 即在线）。
 * 用于 fallback 模式的可用性判定与 popup「测试连接」按钮。
 */
export async function remotePing(
  cfg: RemoteServerConfig,
): Promise<RemotePingResult> {
  if (!cfg.remoteUrl) {
    return { success: false, message: "remote_not_configured" };
  }

  let resp: Response;
  try {
    resp = await fetch(`${cfg.remoteUrl}/ping`, {
      method: "GET",
      signal: AbortSignal.timeout(PING_TIMEOUT_MS),
    });
  } catch (err) {
    return { success: false, message: `remote_unreachable: ${String(err)}` };
  }

  if (resp.status === 401 || resp.status === 403) {
    return { success: false, message: "remote_auth_failed" };
  }

  if (!resp.ok) {
    return { success: false, message: `HTTP ${resp.status}` };
  }

  const data = await resp.json().catch(() => ({}) as Record<string, unknown>);
  return {
    success: true,
    message: typeof data?.message === "string" ? data.message : undefined,
    app: typeof data?.app === "string" ? data.app : undefined,
    version: typeof data?.version === "string" ? data.version : undefined,
  };
}

/**
 * 连接验证：/ping 探活 + 带 token 请求 `GET {remoteUrl}/api/v1/info` 校验鉴权。
 *
 * /ping 无鉴权，token 填错也会 200——只用它做「测试连接」会误报成功。
 * fluxdown_server 的管理 API 恒开且强制 token，/api/v1/info 是最轻量的
 * 鉴权校验端点：401/403 → token 错误。404（指向管理 API 关闭的桌面端等
 * 无法校验 token 的宿主）不视为失败，退化为 ping 结果。
 * 用于 popup/options 的「测试连接」与远程模式解锁（settings.remoteVerified）。
 */
export async function remoteVerify(
  cfg: RemoteServerConfig,
): Promise<RemotePingResult> {
  const ping = await remotePing(cfg);
  if (!ping.success) return ping;

  let resp: Response;
  try {
    resp = await fetch(`${cfg.remoteUrl}/api/v1/info`, {
      method: "GET",
      headers: buildHeaders(cfg),
      signal: AbortSignal.timeout(PING_TIMEOUT_MS),
    });
  } catch (err) {
    return { success: false, message: `remote_unreachable: ${String(err)}` };
  }

  if (resp.status === 401 || resp.status === 403) {
    return { success: false, message: "remote_auth_failed" };
  }
  return ping;
}

/**
 * 拉取远程任务列表：GET {remoteUrl}/api/v1/tasks（管理 API，裸数组响应，
 * 与 postJson/requestJson 的 ApiResponse 包裹契约不同，单独实现解析）。
 *
 * TaskDto 与 TaskBrief 字段一一对应（camelCase 契约一致），唯一缺口是
 * speed——管理 API 是纯轮询接口，不含引擎的实时限速数字（只经 WebSocket
 * 推送），远程任务列表的 speed 恒为 0。
 */
export async function remoteListTasks(
  cfg: RemoteServerConfig,
): Promise<{ success: boolean; tasks: TaskBrief[]; message?: string }> {
  if (!cfg.remoteUrl) {
    return { success: false, tasks: [], message: "remote_not_configured" };
  }

  let resp: Response;
  try {
    resp = await fetch(`${cfg.remoteUrl}/api/v1/tasks`, {
      method: "GET",
      headers: buildHeaders(cfg),
      signal: AbortSignal.timeout(TASKS_POLL_TIMEOUT_MS),
    });
  } catch (err) {
    return {
      success: false,
      tasks: [],
      message: `remote_unreachable: ${String(err)}`,
    };
  }

  if (resp.status === 401 || resp.status === 403) {
    return { success: false, tasks: [], message: "remote_auth_failed" };
  }
  if (!resp.ok) {
    return { success: false, tasks: [], message: `HTTP ${resp.status}` };
  }

  const data = await resp.json().catch(() => null);
  if (!Array.isArray(data)) {
    return { success: false, tasks: [], message: "remote_bad_response" };
  }

  return {
    success: true,
    tasks: data.map((task: Record<string, unknown>) => ({
      taskId: String(task.taskId ?? ""),
      fileName: String(task.fileName ?? ""),
      status: Number(task.status ?? 0),
      downloadedBytes: Number(task.downloadedBytes ?? 0),
      totalBytes: Number(task.totalBytes ?? 0),
      speed: 0,
      errorMessage:
        typeof task.errorMessage === "string" && task.errorMessage
          ? task.errorMessage
          : undefined,
      createdAt: String(task.createdAt ?? ""),
    })),
  };
}

/**
 * 远程任务操作：暂停 / 继续 / 删除。语义同 nmhTaskOp——remove 对已删除任务
 * 重发幂等，pause/resume 重发到达同一目标状态同样无害。
 */
export async function remoteTaskOp(
  op: "pause" | "resume" | "remove",
  taskId: string,
  cfg: RemoteServerConfig,
): Promise<ApiResponse> {
  const path =
    op === "pause"
      ? `/api/v1/tasks/${taskId}/pause`
      : op === "resume"
        ? `/api/v1/tasks/${taskId}/continue`
        : `/api/v1/tasks/${taskId}`;
  const method = op === "remove" ? "DELETE" : "PUT";
  return requestJson(
    `${cfg.remoteUrl}${path}`,
    cfg,
    method,
    undefined,
    TASK_OP_TIMEOUT_MS,
  );
}
