/**
 * 下载拦截（第二层 + 第三层，Firefox 另有 blocking 第一层）。
 *
 * === 下载拦截三层防线 ===
 * 第一层（HTTP 响应感知）: webRequest.onHeadersReceived 把下载类响应的元数据
 *   缓存到 responseDownloadCache（由 sniff.ts 注册监听、写入本模块的缓存）。
 * 第二层（主拦截）: downloads.onDeterminingFilename —— 浏览器弹出「另存为」之前
 *   先请求取消，再 suggest() 释放文件名管线；最优先、最干净。
 * 第三层（兜底拦截）: downloads.onCreated + 元数据轮询 —— onDeterminingFilename
 *   在限定时间内未处理时接管；Firefox 无第二层，这里是唯一路径（另有 blocking
 *   webRequest 从源头取消导航下载，见 issue #21）。
 *
 * 只能由浏览器完成的下载（本地 scheme、页面 Service Worker 合成的 http(s) 响应）三层都
 * 放行，判定见 utils/download-source.ts。
 *
 * #527：拦截关闭后必须真正 removeListener 把这些事件让给其他扩展
 * （脚本猫 / PDM / IDM 等），由 setInterceptListenersActive 统一增删。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { cancelBeforeFilenameResolution } from "@/utils/download-cancellation";
import { browserOnlyReason, isReplayableUrl } from "@/utils/download-source";
import type { NetworkEvidence, ServiceWorkerScopes } from "@/utils/download-source";
import {
  dispositionNeedsPageCharset,
  extractCleanFilename,
  parseContentDispositionFilename,
  readDispositionHeader,
  resolveDownloadFilename,
} from "@/utils/filename";
import { shouldIntercept } from "@/utils/settings";
import type { DownloadItemInfo } from "@/utils/settings";
import { hasActiveBypass, isAppKnownDown } from "./breaker";
import { linkCapture } from "./link-capture";
import { sleep } from "./notify";
import type { SendChannel } from "./protocol";
import { sawNetworkRequest } from "./request-cache";
import { fallbackAfterSendFailure, sendToFluxDown } from "./sender";
import { getCachedProfiles, getCachedSettings, peekCachedProfiles, peekCachedSettings } from "./settings-cache";
import { loadServiceWorkerScopes, peekServiceWorkerScopes } from "./sw-scopes";
import { getTabPageUrl, whenRestored } from "./tab-state";
import { logError, logWarn } from "@/utils/error-log";
import { hasDownloadsApi } from "@/utils/platform";

// 响应头缓存 —— 当 HTTP 响应指示"这是一个下载"时，缓存其元数据
// 这是第三层兜底拦截的关键数据来源
export interface ResponseDownloadInfo {
  url: string;
  contentType: string; // Content-Type
  contentLength: number; // Content-Length（-1 = 未知）
  dispositionFilename: string; // 从 Content-Disposition 解析出的文件名（含 host 先验）
  /** 原始 Content-Disposition（供页面字符集先验到位后重解码）。 */
  disposition?: { text: string; byteCarrier: boolean };
  tabId: number; // 发起导航的 tab（-1 = 未知）
  ts: number;
}
export const responseDownloadCache = new Map<string, ResponseDownloadInfo>();

/**
 * 发起下载导航的标签页当前页面 URL，只供站点配置（never / always）判定。
 * 跨域 CDN 下载在来源站 `Referrer-Policy: same-origin`（Nexus Mods 等）时没有 referrer，
 * downloadItem.referrer 为空；tab-state 记录的页面 URL 不受该策略影响。
 * 不得当 Referer 发给引擎。
 */
function sourcePageUrlOf(urls: readonly (string | undefined)[]): string | undefined {
  for (const u of urls) {
    const tabId = u ? responseDownloadCache.get(u)?.tabId : undefined;
    if (tabId === undefined || tabId < 0) continue;
    const pageUrl = getTabPageUrl(tabId);
    if (pageUrl) return pageUrl;
  }
  return undefined;
}

/** 下载 URL 的主机名（TLD 先验）；非法 URL 返回 undefined。 */
export function urlHost(url: string): string | undefined {
  try {
    return new URL(url).hostname || undefined;
  } catch {
    return undefined;
  }
}

/** 询问发起页字符集的超时：失败/超时视为无先验。 */
const PAGE_CHARSET_TIMEOUT_MS = 300;

/**
 * 向 tab 顶层框架的内容脚本询问 `document.characterSet`
 * （消息 `{ action: "getPageCharset" }` → `{ charset: string }`）。
 * 无内容脚本、tab 已关闭、超时均返回 undefined。
 */
async function getPageCharset(tabId: number): Promise<string | undefined> {
  if (tabId < 0) return undefined;
  try {
    const reply: unknown = await Promise.race([
      browser.tabs.sendMessage(tabId, { action: "getPageCharset" }, {
        frameId: 0,
      }),
      new Promise<undefined>((resolve) =>
        setTimeout(resolve, PAGE_CHARSET_TIMEOUT_MS),
      ),
    ]);
    if (reply && typeof reply === "object" && "charset" in reply) {
      return typeof reply.charset === "string" && reply.charset
        ? reply.charset
        : undefined;
    }
  } catch {
    // 内容脚本不存在（受限页面/尚未注入）：无先验
  }
  return undefined;
}

/**
 * 重新解码 Content-Disposition：仅当原始字节需要页面字符集先验
 * （字节载体、非 ASCII、非合法 UTF-8）时才询问发起页；否则或无先验时
 * 返回 `fallback`（已带 host 先验的解码结果）。
 */
async function decodeDispositionWithPageCharset(
  text: string,
  byteCarrier: boolean,
  tabId: number,
  url: string,
  fallback: string,
): Promise<string> {
  if (!dispositionNeedsPageCharset(text, byteCarrier)) return fallback;
  const pageCharset = await getPageCharset(tabId);
  if (!pageCharset) return fallback;
  return (
    parseContentDispositionFilename(text, byteCarrier, {
      host: urlHost(url),
      pageCharset,
    }) || fallback
  );
}

export function resolveResponseDispositionFilename(
  info: ResponseDownloadInfo,
): Promise<string> {
  if (!info.disposition) return Promise.resolve(info.dispositionFilename);
  return decodeDispositionWithPageCharset(
    info.disposition.text,
    info.disposition.byteCarrier,
    info.tabId,
    info.url,
    info.dispositionFilename,
  );
}

const downloadItemCache = new Map<number, Browser.downloads.DownloadItem>();
// primary / fallback：该层已接管；released：主拦截已按完整信息放行，兜底层不再重判。
const handledDownloads = new Map<number, "primary" | "fallback" | "released">();
// 预抢占 URL 表：URL → {expiry: 过期时间戳, ruleId: DNR 规则 ID}
// 当 AJAX 拦截器在浏览器发起 CDN GET 之前检测到一次性下载 URL 时填入。
// onDeterminingFilename 检查此表，避免重复发送给 FluxDown。
const preemptedUrls = new Map<string, { expiry: number }>();

/** 记录「预抢占」URL（一次性 CDN 中转页）：30 s 内 onDeterminingFilename 不重复发送。 */
export function markPreempted(url: string): void {
  preemptedUrls.set(url, { expiry: Date.now() + 30_000 });
  setTimeout(() => {
    preemptedUrls.delete(url);
  }, 30_000);
}

// Firefox 判为浏览器内合成的导航响应（SW 作用域内、无服务器 IP、非缓存）：URL → 过期时间。
const syntheticResponses = new Map<string, number>();
const SYNTHETIC_TTL_MS = 60_000;

function markSynthetic(url: string): void {
  syntheticResponses.set(url, Date.now() + SYNTHETIC_TTL_MS);
  setTimeout(() => syntheticResponses.delete(url), SYNTHETIC_TTL_MS);
}

function evidenceOf(url: string): NetworkEvidence {
  if ((syntheticResponses.get(url) ?? 0) > Date.now()) return "synthetic";
  return sawNetworkRequest(url) ? "network" : "none";
}

/** 下载只能由浏览器完成（本地 scheme / SW 合成）时返回 true 并记录原因。 */
function isBrowserOnly(urls: readonly (string | undefined)[], scopes: ServiceWorkerScopes): boolean {
  const reason = browserOnlyReason(urls, scopes, evidenceOf);
  if (reason) console.log("[FluxDown] leaving download to the browser:", reason, urls);
  return reason !== null;
}

// Firefox 不支持 onDeterminingFilename，兜底层是唯一拦截路径，
// 需要更长等待让浏览器填充 downloadItem 元数据。
// Firefox for Android 连 downloads 命名空间都没有：只剩 webRequest blocking 一条拦截路径。
const downloadsApi = hasDownloadsApi() ? browser.downloads : undefined;
const hasDeterminingFilename = !!downloadsApi?.onDeterminingFilename;

// ──────────────────────────────────────────────────────────────
// 弱网可靠性：统一等待元数据就绪
// ──────────────────────────────────────────────────────────────
// 设计原则：
//   - 路径A（main_frame 导航下载）：等待 responseDownloadCache 填充
//   - 路径B（XHR / JS 触发下载）：等待 downloadItem.mime/filename 填充
//   - 两条路径共享同一个 deadline，总等待上限为 META_MAX_WAIT（5s）
//   - 两路并发轮询：responseCache 先到就走路径A，否则走路径B
//
// Bug 1 修复：之前路径A的 waitForResponseCache 耗尽 5s 后，路径B才开始
//             等待，导致总等待 10s，且路径B内不再检查缓存（死路）。
// Bug 6 修复：现在两路并发，5s 是总上限，不是每路各 5s。
const POLL_INTERVAL = 60; // 统一轮询间隔 ms
const META_MAX_WAIT = 5000; // 总等待上限（弱网场景覆盖范围）

/**
 * 并发等待两种元数据来源，哪个先到用哪个，共享 deadline。
 * 返回：
 *   { source: 'responseCache', data: ResponseDownloadInfo } — main_frame 导航下载
 *   { source: 'downloadMeta', data: DownloadItem }          — XHR/JS 触发下载
 *   null — deadline 到达或下载已被其他层处理
 */
type MetaResult =
  | { source: "responseCache"; data: ResponseDownloadInfo }
  | { source: "downloadMeta"; data: Browser.downloads.DownloadItem };

/** 响应头缓存按响应 URL 记录：重定向下载要用 finalUrl 才能命中（原始 URL 只有 3xx）。 */
function cachedResponseFor(item: Browser.downloads.DownloadItem): ResponseDownloadInfo | undefined {
  const direct = responseDownloadCache.get(item.url);
  if (direct) return direct;
  // Chrome 54+ 才有 finalUrl，类型声明里没有：按运行时字段收窄。
  const finalUrl = "finalUrl" in item ? item.finalUrl : undefined;
  return typeof finalUrl === "string" ? responseDownloadCache.get(finalUrl) : undefined;
}

async function waitForMeta(
  downloadId: number,
  originalItem: Browser.downloads.DownloadItem,
  deadlineMs: number,
): Promise<MetaResult | null> {
  let bestItem = originalItem;
  while (Date.now() < deadlineMs) {
    if (handledDownloads.has(downloadId)) return null;

    // 路径A：responseDownloadCache 到达
    const rc = cachedResponseFor(originalItem);
    if (rc) return { source: "responseCache", data: rc };

    // 路径B：downloadItem 元数据就绪
    try {
      const results = await browser.downloads.search({ id: downloadId });
      if (results && results.length > 0) {
        const item = results[0];
        if (
          item.state === "complete" ||
          (item as any).state === "interrupted"
        ) {
          // 下载已结束（可能极快完成），用最新状态返回
          return { source: "downloadMeta", data: item };
        }
        if (item.mime || item.filename) {
          return { source: "downloadMeta", data: item };
        }
        bestItem = item;
      } else if (results && results.length === 0) {
        // Bug R4-4 修复：search 返回空数组说明该下载项已不存在（被第二层 erase，或已被删除）。
        // 视为已被其他路径处理，退出，避免误用过期的 originalItem 重复发送。
        return null;
      }
    } catch {
      // search 失败（Firefox 偶发），继续等待
    }

    await sleep(POLL_INTERVAL);
  }
  // deadline 到达，用 bestItem 兜底（宁可用不完整数据判断也不放弃拦截）
  // 再最后检查一次缓存
  const rc = cachedResponseFor(originalItem);
  if (rc) return { source: "responseCache", data: rc };
  return { source: "downloadMeta", data: bestItem };
}

/**
 * 兜底拦截入口。
 *
 * 策略：
 * 1. 给第二层（onDeterminingFilename）一个短窗口优先处理（Chrome）。
 * 2. 并发等待两种元数据来源（responseDownloadCache / downloadItem），
 *    共享 META_MAX_WAIT(5s) 总上限，哪个先到走哪条路径。
 * 3. 拿到元数据后做 shouldIntercept 判断，命中则 cancel+erase+发送。
 */
async function startFallbackInterception(
  downloadId: number,
  originalItem: Browser.downloads.DownloadItem,
) {
  const url = originalItem.url;

  // 总开关 / 绕过令牌 / 熔断在等待前同步判定：回退出来的浏览器下载、熔断期内的下载
  // 不必再轮询元数据。设置缓存未就绪时留给后续路径判定。
  const cachedSettings = peekCachedSettings();
  if ((cachedSettings && !cachedSettings.enabled) || hasActiveBypass(url) || isAppKnownDown()) return;

  console.log("[FluxDown] startFallbackInterception:", {
    id: downloadId,
    url,
    cacheHit: cachedResponseFor(originalItem) !== undefined,
  });

  // 给第二层一个处理窗口（Chrome 有 onDeterminingFilename，suggest cancel 更干净）
  // Firefox 无此 API，10ms 仅用于让事件循环稳定
  await sleep(hasDeterminingFilename ? 150 : 10);
  if (handledDownloads.has(downloadId)) return;

  // 快速路径：缓存已就绪，直接处理，不进入轮询
  const immediateCached = cachedResponseFor(originalItem);
  if (immediateCached) {
    await handleResponseCacheHit(
      downloadId,
      url,
      originalItem,
      immediateCached,
    );
    return;
  }

  // 慢速路径：两路并发轮询，共享 deadline（Bug 1+6 修复）
  const deadline = Date.now() + META_MAX_WAIT;
  const metaResult = await waitForMeta(downloadId, originalItem, deadline);
  if (!metaResult) return; // 被其他层处理或 deadline 到达且结果为 null

  if (handledDownloads.has(downloadId)) return;

  if (metaResult.source === "responseCache") {
    await handleResponseCacheHit(
      downloadId,
      url,
      originalItem,
      metaResult.data,
    );
  } else {
    await handleDownloadMetaHit(
      downloadId,
      url,
      originalItem,
      metaResult.data,
    );
  }
}

/** 路径A：基于 responseDownloadCache 的响应头数据做拦截判断 */
async function handleResponseCacheHit(
  downloadId: number,
  url: string,
  originalItem: Browser.downloads.DownloadItem,
  rc: ResponseDownloadInfo,
) {
  if (handledDownloads.has(downloadId)) return;
  console.log("[FluxDown] Fallback path A (response cache):", {
    id: downloadId,
    url,
    contentType: rc.contentType,
    contentLength: rc.contentLength,
    dispositionFilename: rc.dispositionFilename,
  });

  const settings = await getCachedSettings();
  if (!settings.enabled) return;
  if (handledDownloads.has(downloadId)) return;

  if (hasActiveBypass(url)) return;

  // App 熔断期内跳过拦截，让浏览器原生下载，避免反复"发送失败→回退"弹窗。
  if (isAppKnownDown()) {
    responseDownloadCache.delete(url);
    return;
  }

  // 优先使用 finalUrl（重定向后的真实 URL）
  const fallbackAUrl = (originalItem as any).finalUrl || url;
  if (isBrowserOnly([url, fallbackAUrl], await loadServiceWorkerScopes())) return;

  const dispositionFilename = await resolveResponseDispositionFilename(rc);
  const itemInfo: DownloadItemInfo = {
    url,
    fileSize: rc.contentLength > 0 ? rc.contentLength : -1,
    mime: rc.contentType || undefined,
    filename:
      resolveDownloadFilename(
        dispositionFilename,
        originalItem.filename,
        fallbackAUrl,
        rc.contentType,
      ) || undefined,
    referrerUrl: originalItem.referrer || undefined,
    pageUrl: getTabPageUrl(rc.tabId) || undefined,
  };

  const intercept = shouldIntercept(itemInfo, settings, await getCachedProfiles());
  console.log("[FluxDown] Path A shouldIntercept:", intercept, itemInfo);
  if (!intercept) return;
  if (handledDownloads.has(downloadId)) return;

  // Bug R2-5 修复：用 try/finally 确保无论 executeFallbackIntercept 是否抛出，
  // responseDownloadCache 中对应 URL 的条目都会被清理，防止再次下载同 URL 命中旧缓存
  try {
    await executeFallbackIntercept(
      downloadId,
      fallbackAUrl,
      originalItem.referrer,
      itemInfo,
      // 重定向场景：传入原始 URL，让 sendToFluxDown 可回退查找 headers 缓存
      fallbackAUrl !== url ? url : undefined,
    );
  } finally {
    responseDownloadCache.delete(url);
    responseDownloadCache.delete(rc.url);
  }
}

/** 路径B：基于 downloadItem 元数据做拦截判断 */
async function handleDownloadMetaHit(
  downloadId: number,
  url: string,
  originalItem: Browser.downloads.DownloadItem,
  freshItem: Browser.downloads.DownloadItem,
) {
  if (handledDownloads.has(downloadId)) return;
  console.log("[FluxDown] Fallback path B (download meta):", {
    id: downloadId,
    state: freshItem.state,
    mime: freshItem.mime,
    filename: freshItem.filename,
    fileSize: freshItem.fileSize,
  });

  // 下载已完成或被中断，无需处理
  if (
    freshItem.state === "complete" ||
    (freshItem as any).state === "interrupted"
  ) {
    console.log(
      "[FluxDown] Path B: download already complete/interrupted, skip",
    );
    return;
  }

  const settings = await getCachedSettings();
  if (!settings.enabled) return;
  if (handledDownloads.has(downloadId)) return;

  const mime = freshItem.mime || originalItem.mime || undefined;
  const fileSize =
    (freshItem.fileSize > 0 ? freshItem.fileSize : undefined) ??
    (originalItem.fileSize > 0 ? originalItem.fileSize : undefined) ??
    -1;
  const filename = freshItem.filename || originalItem.filename || undefined;

  const itemInfo: DownloadItemInfo = {
    url: freshItem.url || url,
    fileSize,
    mime,
    filename,
    referrerUrl: freshItem.referrer || originalItem.referrer || undefined,
  };

  if (hasActiveBypass(url)) return;

  // App 熔断期内跳过拦截，让浏览器原生下载，避免反复"发送失败→回退"弹窗。
  if (isAppKnownDown()) return;

  // 优先使用 finalUrl（重定向后的真实 URL）
  const fallbackDownloadUrl =
    (freshItem as any).finalUrl ||
    (originalItem as any).finalUrl ||
    itemInfo.url;
  if (isBrowserOnly([url, fallbackDownloadUrl], await loadServiceWorkerScopes())) return;

  const intercept = shouldIntercept(itemInfo, settings, await getCachedProfiles());
  console.log("[FluxDown] Path B shouldIntercept:", intercept, itemInfo);
  if (!intercept) return;
  if (handledDownloads.has(downloadId)) return;

  await executeFallbackIntercept(
    downloadId,
    fallbackDownloadUrl,
    freshItem.referrer || originalItem.referrer,
    itemInfo,
    // 重定向场景：传入原始 URL，让 sendToFluxDown 可回退查找 headers 缓存
    fallbackDownloadUrl !== url ? url : undefined,
  );
}

/**
 * 执行兜底拦截：cancel → 发送到 FluxDown，抹除记录在后台进行。
 *
 * 先取消再发送：先发送会让浏览器下载在异步等待期间继续，小文件可能在 cancel 前完成，
 * 导致「双下载」；Firefox 没有 onDeterminingFilename，兜底层是唯一拦截路径，尤为明显。
 * 发送只需取消已生效，不必等抹除完成（抹除只清下载列表残留，Firefox 上需轮询等终态再重试确认）。
 * 若发送失败，通过 fallbackToBrowserDownload 重新发起浏览器下载。
 */
async function executeFallbackIntercept(
  downloadId: number,
  url: string,
  referrer: string | undefined,
  itemInfo: DownloadItemInfo,
  originalUrl?: string,
) {
  // 标记为 fallback 已处理，阻止其他层重复拦截
  handledDownloads.set(downloadId, "fallback");

  const cleanFilename = extractCleanFilename(
    itemInfo.filename,
    url,
    itemInfo.mime,
  );

  try {
    await browser.downloads.cancel(downloadId);
  } catch (e) {
    logWarn("[FluxDown] executeFallbackIntercept: cancel failed:", e);
  }
  void eraseWhenSettled(downloadId);

  // 再发送到 FluxDown
  let sendOk = false;
  try {
    sendOk = (
      await sendToFluxDown({
        url,
        referrer,
        filename: cleanFilename,
        fileSize: itemInfo.fileSize,
        mimeType: itemInfo.mime,
        originalUrl,
        origin: "intercept",
        downloadId,
      })
    ).success;
  } catch (e) {
    logError(
      "[FluxDown] executeFallbackIntercept: sendToFluxDown threw:",
      e,
    );
  }

  if (!sendOk) {
    // 发送失败，先 ping 确认 App 是否在线再决定是否回退
    logWarn(
      "[FluxDown] executeFallbackIntercept: send failed, checking app status before fallback:",
      url,
    );
    handledDownloads.delete(downloadId);
    await fallbackAfterSendFailure(url, cleanFilename);
  }
}

// === 第二层：onDeterminingFilename（主拦截） ===
// 在浏览器弹出「另存为」对话框之前触发，
// suggest() 释放文件名管线 + downloads.cancel() 取消下载，不弹出任何浏览器下载 UI。
// Firefox 不支持此 API，完全依赖第三层兜底拦截
// #527/#609/#199/#293 修复：
// 1) 拦截关闭（或未启用）时必须真正 removeListener 把该事件让给其他扩展
//    （脚本猫/PDM/IDM 等）——仅在回调里 suggest() 放行不够，Chrome 只要
//    本扩展仍注册着 listener 就视为参与"确定文件名"的竞争，见上方
//    setInterceptListenersActive()（由 settings-cache 变化驱动）。
// 2) 所有"放行"分支一律改为不带参数的 suggest()，不再重复回填
//    { filename: downloadItem.filename }——哪怕值与浏览器当前默认值一致，
//    显式传参也会被 Chrome 视为本扩展"确定了文件名"，与其他扩展的建议冲突
//    （典型现象：脚本猫报"另一扩展已确定其他文件名"，PDM/IDM 自定义文件名丢失）。
type DeterminingFilenameListener = Parameters<
  NonNullable<typeof browser.downloads.onDeterminingFilename>["addListener"]
>[0];
const handleDeterminingFilename: DeterminingFilenameListener = (
  downloadItem,
  suggest,
) => {
  const url = downloadItem.url;
  // finalUrl covers every ordinary href redirected through an API/CDN, not just
  // a site-specific preempted URL.
  const downloadUrl = (downloadItem as any).finalUrl || url;
  let filenameResolved = false;

  const resolveFilename = () => {
    if (filenameResolved) return;
    filenameResolved = true;
    suggest();
  };
  const releaseToBrowser = () => {
    handledDownloads.set(downloadItem.id, "released");
    resolveFilename();
  };
  // 放行但不下最终结论（元数据不全 / 出错）：撤掉占位，兜底层仍可接手
  const handBackToFallback = () => {
    handledDownloads.delete(downloadItem.id);
    resolveFilename();
  };
  const cancelBeforeResolvingFilename = () =>
    cancelBeforeFilenameResolution(
      downloadItem.id,
      (id) => browser.downloads.cancel(id),
      (query) => browser.downloads.erase(query),
      resolveFilename,
      async (id) => (await browser.downloads.search({ id }))[0]?.state,
    );
  const collectItemInfo = (): {
    itemInfo: DownloadItemInfo;
    referrer: string | undefined;
  } => {
    const cached = downloadItemCache.get(downloadItem.id);
    const mime = downloadItem.mime || cached?.mime || undefined;
    const fileSize =
      (downloadItem.fileSize > 0 ? downloadItem.fileSize : undefined) ??
      (cached && cached.fileSize > 0 ? cached.fileSize : undefined) ??
      -1;
    const filename = downloadItem.filename || cached?.filename || undefined;
    const referrer = downloadItem.referrer || cached?.referrer || undefined;
    return {
      itemInfo: {
        url,
        fileSize,
        mime,
        filename,
        referrerUrl: referrer,
        pageUrl: sourcePageUrlOf([url, downloadUrl]),
      },
      referrer,
    };
  };

  /**
   * `manual` = 右键链接经浏览器捕获（用户主动发送）：不按拦截来源跟踪桌面端取消，
   * 沿用菜单选定的通道与失败回退语义（与菜单直接发送一致）。
   */
  const interceptAfterCancellation = async (
    itemInfo: DownloadItemInfo,
    referrer: string | undefined,
    manual?: { channel?: SendChannel },
  ) => {
    let cancellationSucceeded = false;
    let cleanFilename: string | undefined;
    try {
      // Chrome only settles downloads.cancel after this listener advances. The
      // helper requests cancel first, then releases suggest immediately.
      cancellationSucceeded = await cancelBeforeResolvingFilename();
      if (!cancellationSucceeded) {
        // Browser cancellation did not happen: retain its native download and
        // do not also create a FluxDown capture window.
        console.debug(
          "[FluxDown] onDeterminingFilename: cancellation did not complete; leaving browser download intact:",
          downloadItem.id,
        );
        return;
      }

      const cachedResponse =
        [downloadUrl, url]
          .map((u) => responseDownloadCache.get(u))
          .find((r) => r?.dispositionFilename) ?? undefined;
      const dispositionFilename = cachedResponse
        ? await resolveResponseDispositionFilename(cachedResponse)
        : "";
      cleanFilename = resolveDownloadFilename(
        dispositionFilename,
        itemInfo.filename,
        downloadUrl,
        itemInfo.mime,
      );
      const sendOk = (
        await sendToFluxDown({
          url: downloadUrl,
          referrer,
          filename: cleanFilename,
          fileSize: itemInfo.fileSize,
          mimeType: itemInfo.mime,
          // finalUrl is the engine target; original url remains a cache fallback
          // for request headers and captured form transactions.
          originalUrl: downloadUrl !== url ? url : undefined,
          origin: manual ? undefined : "intercept",
          downloadId: downloadItem.id,
          channel: manual?.channel,
        })
      ).success;
      if (!sendOk) {
        handledDownloads.delete(downloadItem.id);
        await fallbackAfterSendFailure(downloadUrl, cleanFilename, false, manual?.channel).catch(
          (error) =>
            logError(
              "[FluxDown] failed to restore browser download after send failure:",
              error,
            ),
        );
      }
    } catch (error) {
      logError(
        "[FluxDown] onDeterminingFilename interception failed:",
        error,
      );
      if (cancellationSucceeded) {
        handledDownloads.delete(downloadItem.id);
        await fallbackAfterSendFailure(downloadUrl, cleanFilename, false, manual?.channel).catch(
          (fallbackError) =>
            logError(
              "[FluxDown] failed to restore browser download after interception error:",
              fallbackError,
            ),
        );
      } else if (!filenameResolved) {
        handBackToFallback();
      }
    }
  };

  const resolveWithLoadedSettings = async () => {
    try {
      // 冷启动：tab-state 快照恢复前页面 URL 为空
      await whenRestored();
      const settings = await getCachedSettings();
      if (
        !settings.enabled ||
        hasActiveBypass(url) ||
        isAppKnownDown() ||
        isBrowserOnly([url, downloadUrl], await loadServiceWorkerScopes())
      ) {
        releaseToBrowser();
        return;
      }

      const { itemInfo, referrer } = collectItemInfo();
      if (!shouldIntercept(itemInfo, settings, await getCachedProfiles())) {
        // 元数据不全时的「不拦截」不是最终结论：留给兜底层拿到完整元数据后重判
        if (itemInfo.mime || itemInfo.filename) releaseToBrowser();
        else handBackToFallback();
        return;
      }

      console.log("[FluxDown] Intercepting download (onDeterminingFilename):", {
        url,
        downloadUrl,
        mime: itemInfo.mime,
        filename: itemInfo.filename,
        fileSize: itemInfo.fileSize,
        mode: settings.interceptMode,
      });
      await interceptAfterCancellation(itemInfo, referrer);
    } catch (error) {
      logError(
        "[FluxDown] failed while resolving interception settings:",
        error,
      );
      if (!filenameResolved) handBackToFallback();
    } finally {
      downloadItemCache.delete(downloadItem.id);
    }
  };

  // 本地 scheme（blob:/data:/filesystem: 等）引擎无法重放：直接放行，且不占用
  // 文件名决定权，免得与其它下载扩展竞争。
  if (!isReplayableUrl(url) || !isReplayableUrl(downloadUrl)) {
    resolveFilename();
    return;
  }

  // 右键「下载链接」发起的浏览器下载：用户已明确要交给 FluxDown，跳过拦截开关 / 文件类型
  // 过滤 / 绕过令牌，用浏览器解析出的元数据发送。浏览器拿到的是网页（多为防盗链 / 登录页：
  // 浏览器发起的下载带不了 Referer）时交还菜单直接发送原链接，与改动前一致；事务已超时
  // 交还直接发送的只取消。
  if (linkCapture.owns(downloadItem)) {
    handledDownloads.set(downloadItem.id, "primary");
    const { itemInfo } = collectItemInfo();
    const mime = itemInfo.mime?.toLowerCase() ?? "";
    const isPage = mime.startsWith("text/html") || mime.startsWith("application/xhtml+xml");
    const linkClaim = linkCapture.claim(downloadItem, !isPage);
    if (linkClaim?.kind !== "claimed") {
      void cancelBeforeResolvingFilename().finally(() => downloadItemCache.delete(downloadItem.id));
      return true;
    }
    console.log("[FluxDown] Intercepting context-menu link capture:", {
      url,
      downloadUrl,
      mime: itemInfo.mime,
      filename: itemInfo.filename,
      fileSize: itemInfo.fileSize,
    });
    void interceptAfterCancellation(itemInfo, linkClaim.request.referrer || undefined, {
      channel: linkClaim.request.channel,
    }).finally(() => downloadItemCache.delete(downloadItem.id));
    return true;
  }

  // The onCreated fallback already owns this item. Cancel first here as well,
  // otherwise suggest could launch Save As while that capture is in flight.
  if (handledDownloads.get(downloadItem.id) === "fallback") {
    console.log(
      "[FluxDown] onDeterminingFilename: already handled by fallback, cancelling:",
      downloadItem.id,
    );
    void cancelBeforeResolvingFilename().then((cancelled) => {
      if (!cancelled) {
        console.debug(
          "[FluxDown] fallback download had already ended before filename cancellation:",
          downloadItem.id,
        );
      }
    });
    return true;
  }

  // One-time transit URLs must first execute their page script. Direct URLs and
  // all ordinary redirects take the generic cancellation path below.
  const preemptEntry = preemptedUrls.get(url);
  if (preemptEntry && preemptEntry.expiry > Date.now()) {
    if (downloadUrl === url) {
      console.log(
        "[FluxDown] onDeterminingFilename: preempted URL has not redirected; letting browser handle:",
        url,
      );
      resolveFilename();
      return;
    }
    console.log(
      "[FluxDown] onDeterminingFilename: preempted URL redirected, intercepting finalUrl:",
      downloadUrl,
    );
  }

  // Claim before any await so onCreated cannot send a second capture while
  // Chrome waits for the filename decision.
  handledDownloads.set(downloadItem.id, "primary");

  const cachedSettings = peekCachedSettings();
  const cachedProfiles = peekCachedProfiles();
  const cachedScopes = peekServiceWorkerScopes();
  if (cachedSettings !== null && cachedProfiles !== null && cachedScopes !== null) {
    if (
      !cachedSettings.enabled ||
      hasActiveBypass(url) ||
      isAppKnownDown() ||
      isBrowserOnly([url, downloadUrl], cachedScopes)
    ) {
      releaseToBrowser();
      downloadItemCache.delete(downloadItem.id);
      return;
    }

    const { itemInfo, referrer } = collectItemInfo();
    if (shouldIntercept(itemInfo, cachedSettings, cachedProfiles)) {
      console.log("[FluxDown] Intercepting download (sync settings):", {
        url,
        downloadUrl,
        mime: itemInfo.mime,
        filename: itemInfo.filename,
        fileSize: itemInfo.fileSize,
        mode: cachedSettings.interceptMode,
      });
      void interceptAfterCancellation(itemInfo, referrer).finally(() => {
        downloadItemCache.delete(downloadItem.id);
      });
      return true;
    }

    // Complete metadata lets us immediately hand non-matching downloads back.
    // Otherwise wait for the same metadata/settings path used during cold start.
    if (itemInfo.mime || itemInfo.filename) {
      releaseToBrowser();
      downloadItemCache.delete(downloadItem.id);
      return;
    }
  }

  // Chrome may surface Save As before onCreated. Keep the filename pipeline
  // pending for cold-start/metadata-empty decisions; it is the only path that
  // can cancel before the chooser is scheduled.
  void resolveWithLoadedSettings();
  return true;
};

/**
 * 抹除一条已取消的浏览器下载记录（发送不依赖它，由调用方放到后台）。
 *
 * Firefox 的 downloads.cancel() 返回的 Promise 会在状态真正落到 interrupted
 * 之前 resolve，此时立刻 erase 常常删不掉记录，浏览器下载器残留一条"已取消"
 * 记录。正确做法：轮询 search 等记录进入终态（interrupted/complete）再 erase，
 * 并在 erase 后确认记录确实消失，未消失则重试。
 */
async function eraseWhenSettled(downloadId: number): Promise<void> {
  // 等记录落到终态（interrupted/complete），最多等 ~1s；
  // 记录已消失（length===0）说明已被抹除，直接返回。
  const settleDeadline = Date.now() + 1000;
  while (Date.now() < settleDeadline) {
    try {
      const results = await browser.downloads.search({ id: downloadId });
      if (!results || results.length === 0) return;
      const state = (results[0] as any).state;
      if (state === "interrupted" || state === "complete") break;
    } catch {
      break;
    }
    await sleep(50);
  }

  // erase 并确认删除，未删掉则重试（Firefox 偶发时序松弛）
  for (let attempt = 0; attempt < 4; attempt++) {
    try {
      await browser.downloads.erase({ id: downloadId });
    } catch (e) {
      logWarn("[FluxDown] eraseWhenSettled: erase failed:", e);
    }
    try {
      const remain = await browser.downloads.search({ id: downloadId });
      if (!remain || remain.length === 0) return;
    } catch {
      return;
    }
    await sleep(100);
  }
  logWarn(
    "[FluxDown] eraseWhenSettled: record still present after retries:",
    downloadId,
  );
}

/**
 * Firefox 会在标签页/iframe 内联展示（pdf.js、原生播放器）的类型。
 * 这类响应只有带 Content-Disposition: attachment 才是真正的下载，
 * 否则取消导航会把「查看」误当成下载。
 */
function isFirefoxInlineViewableType(contentType: string): boolean {
  const ct = contentType.toLowerCase();
  return (
    ct === "application/pdf" ||
    ["video/mp4", "video/webm", "video/ogg"].includes(ct) ||
    [
      "audio/mpeg",
      "audio/mp3",
      "audio/ogg",
      "audio/wav",
      "audio/x-wav",
      "audio/flac",
      "audio/mp4",
      "audio/webm",
      "audio/aac",
      "audio/opus",
    ].includes(ct)
  );
}

/**
 * 判断 Content-Type 是否为"下载类型"（即浏览器会将导航转为下载的类型）
 */
export function isDownloadContentType(contentType: string): boolean {
  const ct = contentType.toLowerCase();
  const downloadTypes = [
    "application/octet-stream",
    "application/x-download",
    "application/force-download",
    "application/zip",
    "application/x-rar-compressed",
    "application/x-7z-compressed",
    "application/gzip",
    "application/x-tar",
    "application/x-bzip2",
    "application/x-xz",
    "application/x-msdownload",
    "application/x-msi",
    "application/x-apple-diskimage",
    "application/vnd.debian.binary-package",
    "application/x-iso9660-image",
    "application/x-raw-disk-image",
    "application/pdf",
    "application/vnd.android.package-archive",
    "application/x-bittorrent",
  ];
  // 精确匹配 + 前缀匹配
  if (downloadTypes.includes(ct)) return true;
  if (ct.startsWith("video/") || ct.startsWith("audio/")) return true;
  if (ct.startsWith("application/vnd.openxmlformats-officedocument"))
    return true;
  if (ct.startsWith("application/vnd.ms-")) return true;
  return false;
}

// Firefox 专用：blocking webRequest 从源头取消导航下载（issue #21）。
// 同步做拦截决策，异步 fire-and-forget 发送，不阻塞响应管线。
const onDownloadHeadersBlocking = (
  details: any,
): { cancel: boolean } | undefined => {
  // 只拦截导航类请求（点击下载链接 / 导航转下载）；xhr/fetch 取 blob 不碰
  if (details.type !== "main_frame" && details.type !== "sub_frame") {
    return undefined;
  }
  if (!details.responseHeaders) return undefined;
  // 仅成功响应（重定向/错误交给浏览器）
  if (details.statusCode < 200 || details.statusCode >= 300) {
    return undefined;
  }

  let contentType = "";
  let contentLength = -1;
  let contentDisposition = "";
  let dispositionByteCarrier = false;
  for (const h of details.responseHeaders) {
    const name = h.name.toLowerCase();
    if (name === "content-type" && h.value) {
      contentType = h.value.split(";")[0].trim().toLowerCase();
    } else if (name === "content-length" && h.value) {
      const parsed = parseInt(h.value, 10);
      if (!isNaN(parsed)) contentLength = parsed;
    } else if (name === "content-disposition") {
      const d = readDispositionHeader(h, import.meta.env.FIREFOX);
      if (d) {
        contentDisposition = d.text;
        dispositionByteCarrier = d.byteCarrier;
      }
    }
  }

  const isAttachment = contentDisposition
    .toLowerCase()
    .startsWith("attachment");
  const isDownloadMime = isDownloadContentType(contentType);
  // Firefox 可内联展示的类型（PDF/常见音视频）仅在 attachment 时才是下载，
  // main_frame 与 sub_frame 同规则；其余下载类 MIME 照旧。
  if (!isAttachment && (!isDownloadMime || isFirefoxInlineViewableType(contentType))) {
    return undefined;
  }

  // 冷启动首个下载时 settings / 站点配置 / SW 作用域可能尚未就绪：保守放行（交给兜底层），
  // 异步预热
  const settings = peekCachedSettings();
  const profiles = peekCachedProfiles();
  const scopes = peekServiceWorkerScopes();
  if (!settings || !profiles || !scopes) {
    Promise.all([getCachedSettings(), getCachedProfiles(), loadServiceWorkerScopes()]).catch(() => {});
    return undefined;
  }
  if (!settings.enabled) return undefined;

  if (hasActiveBypass(details.url)) return undefined;
  // App 熔断期内放行浏览器原生下载，避免用户完全无法下载
  if (isAppKnownDown()) return undefined;

  const dispositionFilename = parseContentDispositionFilename(
    contentDisposition,
    dispositionByteCarrier,
    { host: urlHost(details.url) },
  );
  const referrer: string | undefined =
    details.originUrl || details.documentUrl || undefined;
  const itemInfo: DownloadItemInfo = {
    url: details.url,
    fileSize: contentLength > 0 ? contentLength : -1,
    mime: contentType || undefined,
    filename: dispositionFilename || undefined,
    referrerUrl: referrer,
    pageUrl: getTabPageUrl(details.tabId) || undefined,
  };
  if (!shouldIntercept(itemInfo, settings, profiles)) return undefined;

  // SW 作用域内的响应既无服务器 IP 又非缓存：由页面 SW 在浏览器内合成，引擎无法重放。
  // 记下判定，兜底层看到同一 URL 的下载项时同样放行。
  if (scopes.covers(details.url) && !details.ip && !details.fromCache) {
    markSynthetic(details.url);
    console.log("[FluxDown] Firefox blocking: service-worker response, leaving to browser:", details.url);
    return undefined;
  }

  // 命中：同步取消该请求（浏览器不创建下载项），异步发送到 FluxDown
  console.log("[FluxDown] Firefox blocking intercept (cancel at source):", {
    url: details.url,
    contentType,
    contentLength,
    dispositionFilename,
  });

  // fire-and-forget：blocking 回调必须尽快返回（cancel 同步返回）；原始字节
  // 文件名需要发起页字符集先验时，异步取回后再重解码并发送。
  // 发送失败时回退浏览器下载。
  void (async () => {
    const decodedFilename = await decodeDispositionWithPageCharset(
      contentDisposition,
      dispositionByteCarrier,
      details.tabId,
      details.url,
      dispositionFilename,
    );
    const cleanFilename = resolveDownloadFilename(
      decodedFilename,
      undefined,
      details.url,
      contentType,
    );
    try {
      const ok = (
        await sendToFluxDown({
          url: details.url,
          referrer,
          filename: cleanFilename,
          fileSize: itemInfo.fileSize,
          mimeType: itemInfo.mime,
          origin: "intercept",
        })
      ).success;
      if (!ok) {
        logWarn(
          "[FluxDown] Firefox blocking: send failed, falling back to browser download:",
          details.url,
        );
        await fallbackAfterSendFailure(details.url, cleanFilename);
      }
    } catch (e) {
      logError(
        "[FluxDown] Firefox blocking: sendToFluxDown threw:",
        e,
      );
      await fallbackAfterSendFailure(details.url, cleanFilename).catch(
        () => {},
      );
    }
  })();

  return { cancel: true };
};

const handleDownloadCreated = (downloadItem: Browser.downloads.DownloadItem): void => {
  const downloadId = downloadItem.id;
  const url = downloadItem.url;

  console.log("[FluxDown] onCreated:", {
    id: downloadId,
    url,
    mime: downloadItem.mime,
    filename: downloadItem.filename,
  });

  // 本地 scheme（blob:/data:/filesystem: 等）引擎无法重放，不缓存也不启动兜底
  if (!isReplayableUrl(url)) return;

  // 缓存 downloadItem 信息，onDeterminingFilename 会用到
  downloadItemCache.set(downloadId, downloadItem);

  // 启动兜底计时器
  // 给 onDeterminingFilename 一个处理窗口，超时后由 onCreated 兜底
  //
  // 关键点：不使用固定的"猜测"超时，而是利用 onChanged 获取完整元数据后再判断。
  // 这里只是一个启动延迟，等 onDeterminingFilename 有机会先处理。
  // 如果 onDeterminingFilename 已处理，兜底逻辑会跳过。
  //
  // 注意：我们注册一个 onChanged 监听器，一旦 downloadItem 的 filename 或 mime
  // 字段被填充（说明浏览器已解析完响应头），就可以做出更准确的判断。
  //
  // R6-8 修复：加 .catch() 防止内部未捕获异常成为 unhandled rejection 噪音
  if (linkCapture.owns(downloadItem)) {
    // 右键链接事务只由 onDeterminingFilename 认领；响应前已中断的交还直接发送。
    if (downloadItem.state === "interrupted") linkCapture.interrupted(downloadItem);
  } else {
    startFallbackInterception(downloadId, downloadItem).catch((e) => {
      logError(
        "[FluxDown] Unexpected error in startFallbackInterception:",
        e,
      );
    });
  }

  // 30 秒后全面清理
  setTimeout(() => {
    downloadItemCache.delete(downloadId);
    handledDownloads.delete(downloadId);
  }, 30_000);
};

// #527/#609/#199/#293：拦截关闭后必须真正 removeListener，把事件让给其他扩展
// （脚本猫/PDM/IDM 等）——仅在回调里 suggest() 放行不够，Chrome 只要本扩展仍
// 注册着 listener 就视为参与"确定文件名"的竞争。onDeterminingFilename、
// downloads.onCreated 与 Firefox blocking 的 onHeadersReceived 统一在此增删。
// 启动时先按「开启」注册（保留 MV3 冷启动唤醒可靠性），设置加载后再校正。
let interceptListenersActive = false;
const FIREFOX_BLOCKING_FILTER: Browser.webRequest.RequestFilter = { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] };

/** Firefox 的 blocking 形态：类型声明只有非 blocking 重载，这里按运行时实际形状声明（原因同上）。 */
interface BlockingHeadersEvent {
  addListener(cb: typeof onDownloadHeadersBlocking, filter: Browser.webRequest.RequestFilter, extra: string[]): void;
  removeListener(cb: typeof onDownloadHeadersBlocking): void;
}
const blockingHeadersEvent = browser.webRequest.onHeadersReceived as unknown as BlockingHeadersEvent;

export function setInterceptListenersActive(active: boolean): void {
  if (active === interceptListenersActive) return;
  interceptListenersActive = active;
  if (downloadsApi?.onDeterminingFilename) {
    if (active) downloadsApi.onDeterminingFilename.addListener(handleDeterminingFilename);
    else downloadsApi.onDeterminingFilename.removeListener(handleDeterminingFilename);
  }
  if (active) downloadsApi?.onCreated.addListener(handleDownloadCreated);
  else downloadsApi?.onCreated.removeListener(handleDownloadCreated);
  if (!hasDeterminingFilename) {
    try {
      if (active) {
        blockingHeadersEvent.addListener(onDownloadHeadersBlocking, FIREFOX_BLOCKING_FILTER, [
          "blocking",
          "responseHeaders",
        ]);
      } else {
        blockingHeadersEvent.removeListener(onDownloadHeadersBlocking);
      }
    } catch (e) {
      logWarn("[FluxDown] Firefox blocking interceptor toggle failed:", e);
    }
  }
}

/** 同步注册拦截监听；由 startBackground 调用一次。 */
export function installIntercept(): void {
  setInterceptListenersActive(true);
}

/** 拦截监听当前是否挂着（总开关关闭时已整体摘除）。 */
export function isInterceptActive(): boolean {
  return interceptListenersActive;
}

/**
 * 右键链接能否走浏览器捕获（与点击下载同一路径）：需要 onDeterminingFilename（Firefox 没有，
 * 其 blocking 拦截只覆盖页面导航）、拦截已开启（关闭时监听已移除且浏览器下载栏可见），且 App
 * 未处于熔断期。不满足时调用方照旧直接发送链接。
 */
export async function canCaptureLinkViaBrowser(): Promise<boolean> {
  if (!hasDeterminingFilename || isAppKnownDown()) return false;
  const settings = await getCachedSettings();
  return settings.enabled && interceptListenersActive;
}

// ===== 同类产品核心策略：隐藏浏览器下载 UI =====
// IDM / Motrix / FDM 等下载管理器均通过此 API 全局禁用浏览器下载栏，
// 防止 cancel/erase 下载时下载条/下载栏短暂闪现。
// Chrome 116+ 使用 setUiOptions，旧版使用 setShelfEnabled（Chrome 117+ 已弃用）。
// Firefox 不支持这些 API，但 Firefox 下使用 webRequest blocking 方案无此问题。
export function syncDownloadShelfState(enabled: boolean) {
  const noop = () => {};
  try {
    const downloads = browser.downloads as any;
    const uiEnabled = !enabled; // 拦截启用时隐藏 UI，拦截禁用时恢复 UI
    if (downloads.setShelfEnabled) {
      // 旧版 Chrome（< 117），同步 API，不返回 Promise
      downloads.setShelfEnabled(uiEnabled);
    }
    if (downloads.setUiOptions) {
      // Chrome 116+，返回 Promise，需要 .catch() 捕获异步权限错误
      Promise.resolve(downloads.setUiOptions({ enabled: uiEnabled })).catch(
        noop,
      );
    }
  } catch {
    // Firefox 等不支持这些 API 的浏览器，静默忽略
  }
}
