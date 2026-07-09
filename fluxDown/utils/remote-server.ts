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
 * 统一 POST JSON 请求，把 fetch 异常/HTTP 状态码整形为 ApiResponse。
 *
 * message 前缀约定（供上层字符串匹配，不做本地化——本地化由 popup/dispatch 按
 * 前缀映射到 i18n key）：
 *   - "remote_not_configured"：remoteUrl 为空
 *   - "remote_auth_failed"：HTTP 401/403（token 错误）
 *   - "remote_unreachable"：fetch 抛异常（网络错误/超时/DNS 失败等）
 *   - 其余：服务端业务返回的失败信息（HTTP 状态非 2xx 或 body.success=false）
 */
async function postJson(
  url: string,
  cfg: RemoteServerConfig,
  body: unknown,
  timeoutMs: number,
): Promise<ApiResponse> {
  if (!cfg.remoteUrl) {
    return { success: false, message: "remote_not_configured" };
  }

  let resp: Response;
  try {
    resp = await fetch(url, {
      method: "POST",
      headers: buildHeaders(cfg),
      body: JSON.stringify(body),
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
  return postJson(`${cfg.remoteUrl}/download`, cfg, req, DOWNLOAD_TIMEOUT_MS);
}

/** 批量投递下载请求：POST {remoteUrl}/download/batch，body 为 {items:[...]} */
export async function remoteSendBatchDownloadRequest(
  items: BatchDownloadItem[],
  cfg: RemoteServerConfig,
): Promise<ApiResponse> {
  return postJson(
    `${cfg.remoteUrl}/download/batch`,
    cfg,
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
