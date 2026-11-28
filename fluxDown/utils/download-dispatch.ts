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
 * remoteUrl 未配置或当前地址/token 未验证时，远程通道视为不可用，路由退化为
 * "off"。需要远程且凭据齐全时先自动补验；失败后 30 秒内不重复等待网络超时，
 * 仅本次退回桌面通道，保留所选远程模式以便连接恢复后自动继续投递。
 *
 * 「本机」通道在桌面是 NMH；Android 版浏览器没有 Native Messaging，本机通道改为 Intent 唤起
 * FluxDown Android App（background/android-handoff.ts），可用性探测恒为可达。
 *
 * === 设置读取策略：缓存 + storage.onChanged 失效 ===
 *
 * 与 background.ts 自身的设置缓存策略一致：缓存永不主动过期，只在
 * storage.sync 的 settings 或 storage.local 的远程配置变化时失效。
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

import * as nmh from "./native-messaging";
import {
  getCachedSettings,
  peekCachedSettings,
} from "@/background/settings-cache";
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
import { ensureRemoteSettingsVerified } from "./settings";
import type { FluxDownSettings, RemoteMode } from "./settings";
import { isAndroidPlatform, peekAndroidPlatform } from "./platform";
import {
  ANDROID_UNSUPPORTED_MESSAGES,
  androidSendBatchDownloadRequest,
  androidSendDownloadRequest,
} from "@/background/android-handoff";

// ──────────────────────────────────────────────────────────────
// 本机通道：桌面 = NMH；Android（浏览器没有 Native Messaging）= Intent 唤起 FluxDown App。
// ──────────────────────────────────────────────────────────────

async function localSend(request: DownloadRequest): Promise<ApiResponse> {
  return (await isAndroidPlatform()) ? androidSendDownloadRequest(request) : nmh.nmhSendDownloadRequest(request);
}

async function localSendBatch(items: BatchDownloadItem[]): Promise<ApiResponse> {
  return (await isAndroidPlatform())
    ? androidSendBatchDownloadRequest(items)
    : nmh.nmhSendBatchDownloadRequest(items);
}

/** Android 的 Intent 通道无法探测 App 是否安装，视为始终可投递。 */
async function localAvailable(withRetry: boolean): Promise<boolean> {
  if (await isAndroidPlatform()) return true;
  return withRetry ? nmh.nmhCheckFluxDownAvailableWithRetry() : nmh.nmhCheckFluxDownAvailable();
}

/** fallback 模式下本机结果是否应改投远程：本机不可达，或 Android Intent 承载不了该请求。 */
function shouldTryRemote(response: ApiResponse): boolean {
  return (
    isNmhUnreachable(response) ||
    (!response.success && response.message !== undefined && ANDROID_UNSUPPORTED_MESSAGES.has(response.message))
  );
}

// NMH 侧代表"不可达"（连接层/瞬态失败）而非 App 业务拒绝的 message 集合，
// 与 nmh-shared.ts 的连接层失败（SharedFailure）及中继应答的 app_not_running 一一对应。
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

/**
 * 失败是否属于「不可达/超时」（请求可能已送达、也可能没送达），而非 App/远端的
 * 明确业务拒绝。上层据此决定回退前是否需要保守探活以防双下载。
 */
export function isUnreachableFailure(response: ApiResponse): boolean {
  return (
    isNmhUnreachable(response) ||
    (!response.success &&
      typeof response.message === "string" &&
      response.message.startsWith("remote_unreachable"))
  );
}

// ──────────────────────────────────────────────────────────────
// 设置读取：与 background 共用 settings-cache（单一缓存，见该模块说明）
// ──────────────────────────────────────────────────────────────

async function getRoutingSettings(): Promise<FluxDownSettings> {
  return ensureRemoteSettingsVerified(await getCachedSettings());
}

/** 当前设置下的实际路由模式（remoteUrl 未配置或未验证时为 "off"）。 */
export async function effectiveRemoteMode(): Promise<RemoteMode> {
  return effectiveMode(toRoutingConfig(await getRoutingSettings()));
}

interface RoutingConfig {
  mode: RemoteMode;
  remote: RemoteServerConfig;
}

/** 给路由结果盖上实际处理通道的戳（供上层按通道分流通知等行为） */
function stamp(response: ApiResponse, channel: "local" | "remote"): ApiResponse {
  return { ...response, channel };
}

function remoteConfigOf(settings: FluxDownSettings): RemoteServerConfig {
  return {
    remoteUrl: settings.remoteUrl?.trim() ?? "",
    remoteToken: settings.remoteToken ?? "",
  };
}

function toRoutingConfig(settings: FluxDownSettings): RoutingConfig {
  return {
    mode: settings.remoteVerified ? settings.remoteMode : "off",
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

/** 显式指定通道时（UI 的「发到本机 / 发到远程」）绕过 remoteMode 路由。 */
export type DispatchChannel = "local" | "remote";

export async function sendDownloadRequest(
  request: DownloadRequest,
  channel?: DispatchChannel,
): Promise<ApiResponse> {
  if (channel === "local") {
    return stamp(await localSend(request), "local");
  }
  if (channel === "remote") {
    const remote = remoteConfigOf(await getRoutingSettings());
    if (!remote.remoteUrl) return stamp({ success: false, message: "remote_not_configured" }, "remote");
    return stamp(await remoteSendDownloadRequest(request, remote), "remote");
  }
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return stamp(await localSend(request), "local");
  }
  if (mode === "always") {
    return stamp(await remoteSendDownloadRequest(request, cfg.remote), "remote");
  }

  // fallback：先本机，仅"不可达"/Android 承载不了或抛异常时改投远程。
  try {
    const result = await localSend(request);
    if (!shouldTryRemote(result)) {
      return stamp(result, "local");
    }
    return stamp(await remoteSendDownloadRequest(request, cfg.remote), "remote");
  } catch {
    return stamp(await remoteSendDownloadRequest(request, cfg.remote), "remote");
  }
}

/**
 * 固定并发度分批执行一组异步任务，返回结果顺序与输入顺序一致。用于给
 * `remoteSendBatchPreservingAudio` 的音轨条目逐条 POST 加并发闸门——批量
 * 入口对条数唯一的约束是 NMH 侧的 1000（`MAX_BATCH_ITEMS`），远程路径此前
 * 没有任何闸门，用户「全选」多清晰度面板会瞬间发起数十~上百条并发连接，
 * 容易撞连接数/限流并让 `DOWNLOAD_TIMEOUT_MS` 虚假超时。
 */
async function mapWithConcurrency<T, R>(
  items: T[],
  concurrency: number,
  fn: (item: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  async function worker(): Promise<void> {
    while (next < items.length) {
      const index = next++;
      results[index] = await fn(items[index]);
    }
  }
  await Promise.all(
    Array.from({ length: Math.min(concurrency, items.length) }, () => worker()),
  );
  return results;
}

const TRACK_PAIR_FANOUT_CONCURRENCY = 6;

/**
 * 服务端 /download/batch 把整批 URL 共用「首个非空」的 cookies/referrer/headers，
 * 且丢弃 filename/fileSize/mimeType/method/body/audioUrl/browserOriginalUrl。因此只有上下文完全相同、
 * 且不带这些逐条字段的条目才能安全合并为一次批量 POST；其余逐条 POST /download，
 * 避免 A 站的 Cookie/Authorization 被发往批内其他主机。
 */
function batchContextKey(item: BatchDownloadItem): string | null {
  if (
    item.audioUrl ||
    item.browserOriginalUrl ||
    item.filename ||
    item.mimeType ||
    item.method ||
    item.body ||
    (item.fileSize !== undefined && item.fileSize > 0)
  ) {
    return null;
  }
  return JSON.stringify([
    item.cookies ?? "",
    item.referrer ?? "",
    Object.entries(item.headers ?? {}).sort(([a], [b]) => (a < b ? -1 : 1)),
  ]);
}

async function remoteSendBatchPreservingAudio(
  items: BatchDownloadItem[],
  cfg: RemoteServerConfig,
): Promise<ApiResponse> {
  const groups = new Map<string, BatchDownloadItem[]>();
  const singles: BatchDownloadItem[] = [];
  for (const item of items) {
    const key = batchContextKey(item);
    if (key === null) {
      singles.push(item);
      continue;
    }
    const group = groups.get(key);
    if (group) group.push(item);
    else groups.set(key, [item]);
  }
  // 单条成组没有合并收益，走单条端点（保留 filename 之外的全部字段）。
  const units: Array<{ size: number; run: () => Promise<ApiResponse> }> = [];
  for (const group of groups.values()) {
    if (group.length === 1) singles.push(group[0]);
    else
      units.push({
        size: group.length,
        run: () => remoteSendBatchDownloadRequest(group, cfg),
      });
  }
  for (const item of singles) {
    units.push({ size: 1, run: () => remoteSendDownloadRequest(item, cfg) });
  }

  // 各单元互相独立，受统一并发闸门约束（见 mapWithConcurrency）。
  const results = await mapWithConcurrency(
    units,
    TRACK_PAIR_FANOUT_CONCURRENCY,
    (unit) => unit.run(),
  );
  // 整批只有一个请求时原样返回，保留 remote_auth_failed 等稳定 message 前缀。
  if (results.length === 1) return results[0];

  // 部分成功聚合语义对齐 NMH legacy 路径的 "x/y items sent (z failed)"：
  // 任一条目失败都不能让已经建好的任务被上层判定为「整批失败」进而重试
  // （background.ts 收到 success:false 后 incrementStat("failed") + 失败
  // 通知，用户手动重试会把已接受的条目重复创建）。批量 POST 是单个 HTTP
  // 调用，拿不到内部逐条结果，只能按该次调用的成败整体计数。
  let succeeded = 0;
  results.forEach((result, i) => {
    if (result.success) succeeded += units[i].size;
  });
  const total = items.length;
  const failed = total - succeeded;

  if (succeeded === 0) {
    const firstFailureMessage =
      results.find((result) => !result.success)?.message ?? "All items failed";
    return { success: false, message: `Batch failed: ${firstFailureMessage}` };
  }
  return {
    success: true,
    message: failed > 0
      ? `${succeeded}/${total} items sent (${failed} failed)`
      : `${succeeded} items sent`,
  };
}

export async function sendBatchDownloadRequest(
  items: BatchDownloadItem[],
  channel?: DispatchChannel,
): Promise<ApiResponse> {
  if (channel === "local") {
    return stamp(await localSendBatch(items), "local");
  }
  if (channel === "remote") {
    const remote = remoteConfigOf(await getRoutingSettings());
    if (!remote.remoteUrl) return stamp({ success: false, message: "remote_not_configured" }, "remote");
    return stamp(await remoteSendBatchPreservingAudio(items, remote), "remote");
  }
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return stamp(await localSendBatch(items), "local");
  }
  if (mode === "always") {
    return stamp(
      await remoteSendBatchPreservingAudio(items, cfg.remote),
      "remote",
    );
  }

  try {
    const result = await localSendBatch(items);
    if (!shouldTryRemote(result)) {
      return stamp(result, "local");
    }
    return stamp(
      await remoteSendBatchPreservingAudio(items, cfg.remote),
      "remote",
    );
  } catch {
    return stamp(
      await remoteSendBatchPreservingAudio(items, cfg.remote),
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
  const cached = peekCachedSettings();
  const cfg = cached ? toRoutingConfig(cached) : null;
  if (cfg && effectiveMode(cfg) === "always") return;
  // Android 没有 Native Messaging，本机通道是 Intent，无需预热。
  if (peekAndroidPlatform() === true) return;

  nmh.nmhWarmupNativeHost();

  // 缓存尚未填充（SW 冷启动后首次调用）时顺带异步预热一次，不阻塞本次调用，
  // 让后续调用（以及本函数下次调用）尽快用上真实设置值。
  if (!cached) void getCachedSettings().catch(() => {});
}

export async function checkFluxDownAvailable(): Promise<boolean> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return localAvailable(false);
  }
  if (mode === "always") {
    const result = await remotePing(cfg.remote);
    return result.success === true;
  }

  // fallback：本机或 remote 任一可达即视为可用。
  const [nmhUp, remoteUp] = await Promise.all([
    localAvailable(false).catch(() => false),
    remotePing(cfg.remote)
      .then((r) => r.success === true)
      .catch(() => false),
  ]);
  return nmhUp || remoteUp;
}

/** 远程通道（已配置且已验证、remoteMode 非 off）当前是否可达；不探测 NMH。 */
export async function checkRemoteAvailable(): Promise<boolean> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  if (effectiveMode(cfg) === "off") return false;
  const result = await remotePing(cfg.remote);
  return result.success === true;
}

/**
 * 带一次重连重试的可用性探测（NMH 侧沿用原重试语义）。
 * fallback 模式同样取"NMH 或 remote 任一可达"。
 */
export async function checkFluxDownAvailableWithRetry(): Promise<boolean> {
  const cfg = toRoutingConfig(await getRoutingSettings());
  const mode = effectiveMode(cfg);

  if (mode === "off") {
    return localAvailable(true);
  }
  if (mode === "always") {
    const result = await remotePing(cfg.remote);
    return result.success === true;
  }

  const [nmhUp, remoteUp] = await Promise.all([
    localAvailable(true).catch(() => false),
    remotePing(cfg.remote)
      .then((r) => r.success === true)
      .catch(() => false),
  ]);
  return nmhUp || remoteUp;
}
