/**
 * 下载投递路由层
 *
 * 职责：根据用户设置的 remoteMode，在 NMH（桌面 App，Native Messaging）与
 * 远程 HTTP 下载源（fluxdown_server）之间路由下载请求 / 可用性探测，对上层
 * （background.ts、popup）暴露与 native-messaging.ts 完全同名同签名的 5 个
 * 导出函数——调用方只需换 import 源，函数名、参数、返回值形状不变。
 *
 * === 路由策略（FluxDownSettings.remoteMode） ===
 *
 *   - "off"（默认）：直透 NMH，行为与远程功能上线前完全一致。
 *   - "always"：直透远程 HTTP；warmupNativeHost 变为 no-op（远程通道无冷启动
 *     问题，无需预热），checkFluxDownAvailable* 改为 remotePing。
 *   - "fallback"：优先 NMH；仅当 NMH 失败原因是"不可达"（连接层/超时，而非
 *     App 主动业务拒绝）或调用抛异常时，才改投远程——避免把一次已被 App
 *     明确拒绝的请求（如参数非法）无意义地在远程重试一遍。可用性探测取
 *     "NMH ping 或 remote ping 任一成功"。
 *
 * remoteUrl 未配置（空字符串）时，无论 remoteMode 是什么，远程通道一律视为
 * 不可用、路由退化为 "off"——避免用户切到 fallback/always 但忘填地址时，
 * 下载请求静默发往一个不存在的地址而失败。
 *
 * === 设置读取策略：缓存 + storage.onChanged 失效 ===
 *
 * 与 background.ts 自身的设置缓存策略一致：缓存永不主动过期，只在
 * chrome.storage.sync 的 settings 变化时失效，重新按需读取。
 *
 * 选择缓存而非"每次读取"的关键原因是 warmupNativeHost：它是同步触发的
 * fire-and-forget 优化（见 native-messaging.ts 对应函数文档），调用后必须
 * 立即发出 NMH warmup ping、不等待任何异步 I/O，让 App 冷启动与后续 cookie
 * 收集并行——插入一次 loadSettings() 的 await 会拖慢这一时序，在默认的
 * "off" 模式下也会引入原版没有的延迟，不满足"remoteMode=off 时行为与改动前
 * 完全一致"的验收要求。缓存未命中（Service Worker 刚冷启动，尚未发生过
 * 任何设置读取）时按 "off" 保守处理——无条件同步调用 nmh.nmhWarmupNativeHost()，
 * 与原版行为完全一致；随后异步补一次缓存填充，供后续调用使用真实值。
 */

import { browser } from "wxt/browser";
import * as nmh from "./native-messaging";
import type {
  ApiResponse,
  DownloadRequest,
  BatchDownloadItem,
} from "./native-messaging";
import {
  remoteSendDownloadRequest,
  remoteSendBatchDownloadRequest,
  remotePing,
} from "./remote-server";
import type { RemoteServerConfig } from "./remote-server";
import { loadSettings } from "./settings";
import type { FluxDownSettings, RemoteMode } from "./settings";

// NMH 侧代表"不可达"（连接层/瞬态失败）而非 App 业务拒绝的 message 集合，
// 与 native-messaging.ts 内部 sendMessage/sendWithRetry 的失败分支一一对应
// （见该文件 getPort/sendMessage/sendWithRetry 的 resolve({ success:false, message:... })）。
// 只有命中此集合，fallback 模式才允许改投远程；其余失败视为 App 已收到请求
// 并主动拒绝，原样返回，不重复投递。
const NMH_UNREACHABLE_MESSAGES = new Set<string>([
  "native_messaging_unavailable", // connectNative() 不可用/抛异常
  "timeout", // 请求超时无响应
  "port disconnected", // NMH 进程连接中断
  "postMessage failed", // 端口写入失败
  "app_not_running", // NMH 明确回报 App 未运行
]);

function isNmhUnreachable(response: ApiResponse): boolean {
  return (
    !response.success &&
    typeof response.message === "string" &&
    NMH_UNREACHABLE_MESSAGES.has(response.message)
  );
}

// ──────────────────────────────────────────────────────────────
// 设置缓存（见文件头说明）
// ──────────────────────────────────────────────────────────────

let _settingsCache: FluxDownSettings | null = null;

async function getRoutingSettings(): Promise<FluxDownSettings> {
  if (_settingsCache) return _settingsCache;
  _settingsCache = await loadSettings();
  return _settingsCache;
}

try {
  browser.storage.onChanged.addListener((changes, area) => {
    if (area === "sync" && changes.settings) _settingsCache = null;
  });
} catch {
  // storage.onChanged 在个别环境不可用；缓存仍会在下次 SW 生命周期内
  // 按需重新读取，只是不能感知运行期设置变化，属可接受的降级。
}

interface RoutingConfig {
  mode: RemoteMode;
  remote: RemoteServerConfig;
}

/** 给路由结果盖上实际处理通道的戳（供上层按通道分流通知等行为） */
function stamp(response: ApiResponse, channel: "local" | "remote"): ApiResponse {
  return { ...response, channel };
}

function toRoutingConfig(settings: FluxDownSettings): RoutingConfig {
  return {
    mode: settings.remoteMode,
    remote: {
      remoteUrl: settings.remoteUrl?.trim() ?? "",
      remoteToken: settings.remoteToken ?? "",
    },
  };
}

/** remoteUrl 为空则远程通道不可用，等价于 "off"（见文件头说明）。 */
function effectiveMode(cfg: RoutingConfig): RemoteMode {
  return cfg.remote.remoteUrl ? cfg.mode : "off";
}

export async function sendDownloadRequest(
  request: DownloadRequest,
): Promise<ApiResponse> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return stamp(await nmh.nmhSendDownloadRequest(request), "local");
  }
  if (mode === "always") {
    return stamp(await remoteSendDownloadRequest(request, cfg.remote), "remote");
  }

  // fallback：先 NMH，仅"不可达"或抛异常时改投远程。
  try {
    const result = await nmh.nmhSendDownloadRequest(request);
    if (result.success || !isNmhUnreachable(result)) {
      return stamp(result, "local");
    }
    return stamp(await remoteSendDownloadRequest(request, cfg.remote), "remote");
  } catch {
    return stamp(await remoteSendDownloadRequest(request, cfg.remote), "remote");
  }
}

export async function sendBatchDownloadRequest(
  items: BatchDownloadItem[],
): Promise<ApiResponse> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return stamp(await nmh.nmhSendBatchDownloadRequest(items), "local");
  }
  if (mode === "always") {
    return stamp(
      await remoteSendBatchDownloadRequest(items, cfg.remote),
      "remote",
    );
  }

  try {
    const result = await nmh.nmhSendBatchDownloadRequest(items);
    if (result.success || !isNmhUnreachable(result)) {
      return stamp(result, "local");
    }
    return stamp(
      await remoteSendBatchDownloadRequest(items, cfg.remote),
      "remote",
    );
  } catch {
    return stamp(
      await remoteSendBatchDownloadRequest(items, cfg.remote),
      "remote",
    );
  }
}

/**
 * 预热链路（同步 fire-and-forget，见文件头缓存策略说明）。
 * 远程 HTTP 通道无冷启动问题，仅当已确认 "always" 模式时才 no-op；
 * 缓存未命中或 "off"/"fallback" 一律照常预热 NMH。
 */
export function warmupNativeHost(): void {
  const cfg = _settingsCache ? toRoutingConfig(_settingsCache) : null;
  if (cfg && effectiveMode(cfg) === "always") return;

  nmh.nmhWarmupNativeHost();

  // 缓存尚未填充（SW 冷启动后首次调用）时顺带异步预热一次，不阻塞本次调用，
  // 让后续调用（以及本函数下次调用）尽快用上真实设置值。
  if (!_settingsCache) void getRoutingSettings();
}

export async function checkFluxDownAvailable(): Promise<boolean> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return nmh.nmhCheckFluxDownAvailable();
  }
  if (mode === "always") {
    const result = await remotePing(cfg.remote);
    return result.success === true;
  }

  // fallback：NMH 或 remote 任一可达即视为可用。
  const [nmhUp, remoteUp] = await Promise.all([
    nmh.nmhCheckFluxDownAvailable().catch(() => false),
    remotePing(cfg.remote)
      .then((r) => r.success === true)
      .catch(() => false),
  ]);
  return nmhUp || remoteUp;
}

/**
 * 带一次重连重试的可用性探测（NMH 侧沿用原重试语义）。
 * fallback 模式同样取"NMH 或 remote 任一可达"。
 */
export async function checkFluxDownAvailableWithRetry(): Promise<boolean> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return nmh.nmhCheckFluxDownAvailableWithRetry();
  }
  if (mode === "always") {
    const result = await remotePing(cfg.remote);
    return result.success === true;
  }

  const [nmhUp, remoteUp] = await Promise.all([
    nmh.nmhCheckFluxDownAvailableWithRetry().catch(() => false),
    remotePing(cfg.remote)
      .then((r) => r.success === true)
      .catch(() => false),
  ]);
  return nmhUp || remoteUp;
}
