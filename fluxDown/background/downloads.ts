/**
 * UI / 右键菜单触发的下载：候选（含清晰度）、资源行、任意 URL 列表、整页转交桌面端解析。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { sendBatchDownloadRequest, warmupNativeHost } from "@/utils/download-dispatch";
import type { DispatchTarget } from "@/utils/download-dispatch";
import { extractCleanFilename } from "@/utils/filename";
import { t } from "@/utils/i18n";
import { candidateFilename, defaultCandidateVariant } from "@/utils/media-candidates";
import type { BatchDownloadItem } from "@/utils/native-messaging";
import { isAndroidPlatform } from "@/utils/platform";
import { getCachedSettings } from "./settings-cache";
import { notify, sleep } from "./notify";
import type { SendResult } from "./protocol";
import { cancelledSelection, chooseManualTarget } from "./manual-target";
import { extractAuthFromCache, requestHeaderCache, withBrowserUserAgent } from "./request-cache";
import {
  describeSendError,
  openProtocolUrl,
  resolveSaveDir,
  sendToFluxDown,
  shouldNotifyChannel,
  withRuleHeaders,
} from "./sender";
import { incrementStat } from "./stats";
import { maybeArmTaskPollForChannel } from "./tasks";
import {
  findStoredResource,
  getResources,
  getTabPageUrl,
  mergeStoredHeaders,
  sameOrigin,
} from "./tab-state";
import { buildTabView } from "./tab-view";
import { allRows } from "./view-model";

/** 单次 batch_download 条数上限（与桌面端 MAX_BATCH_ITEMS 对齐）。 */
const BATCH_LIMIT = 1000;

/** 候选下载（可指定清晰度）。 */
export async function downloadCandidate(
  tabId: number,
  candidateId: string,
  variantId: string | undefined,
  channel: DispatchTarget | undefined,
): Promise<SendResult> {
  const view = await buildTabView(tabId, { withHandoff: false });
  const row = allRows(view).find((r) => r.candidate.id === candidateId);
  if (!row) return { success: false, message: "candidate_not_found" };
  const candidate = row.candidate;
  // DRM 内容下载得到的是无法播放的密文，不发给下载器。
  if (candidate.protected) return { success: false, message: t("float.protectedHint") };
  // SW 合成的地址引擎重放只会拿到网页。
  if (candidate.browserOnly) return { success: false, message: t("float.browserOnlyHint") };
  const variant = variantId
    ? candidate.variants.find((v) => v.id === variantId)
    : defaultCandidateVariant(candidate);
  if (!variant) return { success: false, message: "variant_not_found" };

  // 从嗅探到的资源取回当时的 cookies / headers（requestHeaderCache 可能已过期）。
  const stored = getResources(tabId);
  // 播放器没请求过的档（其它清晰度 / 主清单）没有自己的记录：退回同一候选里同域名的资源，
  // 同一 CDN 上的鉴权头（Referer / Origin / 自定义签名头）通常一致。Cookie 不跨域名借用。
  const sibling = (url: string) => {
    const host = hostOf(url);
    const ids = new Set(candidate.rawResourceIds);
    return stored.find((r) => ids.has(r.id) && hostOf(r.url) === host && (r.headers || r.cookies));
  };
  const video = findStoredResource(stored, variant.videoUrl) ?? sibling(variant.videoUrl);
  const audio =
    variant.audioUrl && sameOrigin(variant.videoUrl, variant.audioUrl)
      ? findStoredResource(stored, variant.audioUrl)
      : undefined;
  const result = await sendToFluxDown({
    url: variant.videoUrl,
    audioUrl: variant.audioUrl,
    filename: candidateFilename(candidate, variant),
    fileSize: variant.fileSize && variant.fileSize > 0 ? variant.fileSize : -1,
    mimeType: variant.mimeType,
    referrer: view.pageUrl,
    storedCookies: video?.cookies || audio?.cookies,
    storedHeaders: mergeStoredHeaders(video?.headers, audio?.headers),
    channel,
    pageHost: view.site.host,
  });
  return {
    success: result.success,
    message: result.success ? undefined : result.message ?? t("notify.sendFailed"),
    channel: result.channel,
  };
}

/**
 * 快捷键「下载正在播放的媒体」：取活动标签页当前播放元素关联的候选（正在播放的档优先），
 * 结果用系统通知反馈（快捷键没有页面内 UI）。
 */
export async function downloadPlayingMedia(): Promise<void> {
  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  if (tab?.id === undefined) return;
  const view = await buildTabView(tab.id, { withHandoff: false });
  const row = view.playing.find((r) => r.candidate.downloadable && !r.candidate.protected && !r.candidate.browserOnly);
  if (!row) {
    const blocked = view.playing.find((r) => r.candidate.protected || r.candidate.browserOnly)?.candidate;
    const hint = blocked?.protected ? t("float.protectedHint") : blocked ? t("float.browserOnlyHint") : t("shortcut.noPlayingMedia");
    notify(t("shortcut.downloadTitle"), hint);
    return;
  }
  const channel = await chooseManualTarget();
  if (channel === null) return;
  const result = await downloadCandidate(tab.id, row.candidate.id, row.playingVariantId ?? undefined, channel);
  if (!result.success) notify(t("shortcut.downloadTitle"), result.message ?? t("notify.sendFailed"));
}

interface BatchInput {
  url: string;
  audioUrl?: string;
  referrer?: string;
  filename?: string;
  fileSize?: number;
  mimeType?: string;
}

/**
 * 批量发送。本地通道单条 batch_download 携带全部条目（超大批量由 native-messaging 分块），
 * 旧桌面端自动回退逐条；远程通道单次 POST /download/batch。
 */
async function sendBatch(
  rawItems: BatchInput[],
  tabId: number | undefined,
  channel: DispatchTarget | undefined,
  pageHost: string | undefined,
): Promise<SendResult> {
  // 相同视频轨 + 音频轨只允许创建一个任务；不同清晰度/编码的 URL 保留。
  const seen = new Set<string>();
  const items = rawItems
    .filter((item) => {
      if (!item?.url) return false;
      const key = `${item.url}\u0000${item.audioUrl || ""}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    })
    .map((item) => ({
      ...item,
      filename: extractCleanFilename(item.filename, item.url, item.mimeType) || undefined,
    }));
  if (items.length === 0) return { success: false, message: "No valid items" };

  // === fluxdown:// 协议模式（仅桌面；Android 由 Intent 交接取代）：逐条唤起（认证信息无法携带；带音频轨的必须走 NMH/远程） ===
  const settings = await getCachedSettings();
  if (settings.enableFluxdownProtocol && !channel && !items.some((item) => item.audioUrl) && !(await isAndroidPlatform())) {
    let sent = 0;
    for (const item of items) {
      if (await openProtocolUrl(item.url, item.filename)) sent++;
      await sleep(800);
    }
    await incrementStat(sent > 0 ? "sent" : "failed");
    if (sent > 0 && (await shouldNotifyChannel("local"))) {
      notify(t("notify.downloadSent"), t("notify.batchSentDetail", { count: String(sent) }));
    }
    return { success: sent > 0, channel: "local" };
  }

  // 预热 NMH：App 冷启动与下方逐项 cookie 收集（各 500ms 上限）并行
  if (!channel || channel === "local") warmupNativeHost();
  const tabResources = tabId !== undefined && tabId >= 0 ? getResources(tabId) : [];

  const batchItems: BatchDownloadItem[] = await Promise.all(
    items.map(async (item) => {
      const itemAuth = extractAuthFromCache(item.url);
      let cookieString = itemAuth.cookies || "";
      let extraHeaders: Record<string, string> = itemAuth.headers || {};
      if (itemAuth.cookies || itemAuth.headers) requestHeaderCache.delete(item.url);
      if (!cookieString) {
        try {
          const cookies = await Promise.race([
            browser.cookies.getAll({ url: item.url }),
            new Promise<Browser.cookies.Cookie[]>((_, reject) =>
              setTimeout(() => reject(new Error("cookies timeout")), 500),
            ),
          ]);
          cookieString = cookies.map((c) => `${c.name}=${c.value}`).join("; ");
        } catch {
          /* timeout 或权限不足，跳过 */
        }
      }
      // 兜底：从 tab 资源恢复认证信息
      if (!cookieString || Object.keys(extraHeaders).length === 0) {
        const matched = findStoredResource(tabResources, item.url);
        const audioMatched =
          item.audioUrl && sameOrigin(item.url, item.audioUrl)
            ? findStoredResource(tabResources, item.audioUrl)
            : undefined;
        if (!cookieString) cookieString = matched?.cookies || audioMatched?.cookies || "";
        if (Object.keys(extraHeaders).length === 0) {
          extraHeaders = mergeStoredHeaders(matched?.headers, audioMatched?.headers) || {};
        }
      }
      return {
        url: item.url,
        audioUrl: item.audioUrl,
        referrer: item.referrer || "",
        filename: item.filename,
        cookies: cookieString,
        headers: withBrowserUserAgent(withRuleHeaders(item.url, item.referrer || "", extraHeaders)),
        fileSize: item.fileSize,
        mimeType: item.mimeType,
        saveDir: await resolveSaveDir({ url: item.url, referrer: item.referrer, pageHost }),
      };
    }),
  );

  const response = await sendBatchDownloadRequest(batchItems, channel);
  const notifyOk = await shouldNotifyChannel(response.channel);
  if (response.success) {
    await incrementStat("sent");
    maybeArmTaskPollForChannel(response.channel);
    if (notifyOk) {
      notify(t("notify.downloadSent"), t("notify.batchSentDetail", { count: String(items.length) }));
    }
    return { success: true, channel: response.channel };
  }
  await incrementStat("failed");
  if (notifyOk) notify(t("notify.sendFailed"), describeSendError(response.message));
  return { success: false, message: response.message, channel: response.channel };
}

/** 资源行下载：只有一条走 sendToFluxDown（可携带完整认证与请求事务），多条走批量。 */
export async function downloadResources(
  tabId: number,
  resourceIds: string[],
  channel: DispatchTarget | undefined,
): Promise<SendResult> {
  const wanted = new Set(resourceIds);
  const resources = getResources(tabId).filter((r) => wanted.has(r.id));
  if (resources.length === 0) return { success: false, message: "No items" };
  const pageUrl = getTabPageUrl(tabId);
  const pageHost = hostOf(pageUrl);

  if (resources.length === 1) {
    const r = resources[0];
    const result = await sendToFluxDown({
      url: r.url,
      referrer: r.pageUrl || pageUrl,
      filename: extractCleanFilename(r.filename, r.url, r.mimeType),
      // IDM/NDM 策略：嗅探来源跳过 probe（一次性 token URL 会被 probe 消耗）。
      fileSize: r.size > 0 ? r.size : -1,
      mimeType: r.mimeType,
      storedCookies: r.cookies,
      storedHeaders: r.headers,
      channel,
      pageHost,
    });
    return { success: result.success, message: result.message, channel: result.channel };
  }
  return sendBatch(
    resources.map((r) => ({
      url: r.url,
      referrer: r.pageUrl || pageUrl,
      filename: r.filename,
      fileSize: r.size > 0 ? r.size : -1,
      mimeType: r.mimeType,
    })),
    tabId,
    channel,
    pageHost,
  );
}

function hostOf(url: string): string | undefined {
  try {
    return new URL(url).hostname || undefined;
  } catch {
    return undefined;
  }
}

/** magnet 链接的 dn 参数作文件名。 */
function magnetName(url: string): string | undefined {
  if (!url.toLowerCase().startsWith("magnet:")) return undefined;
  try {
    return new URL(url).searchParams.get("dn") || undefined;
  } catch {
    return undefined;
  }
}

/** 任意 URL 列表下载（快速下载输入框、链接页、磁力点击接管）。 */
export async function downloadUrls(
  urls: string[],
  referrer: string | undefined,
  tabId: number | undefined,
  channel: DispatchTarget | undefined,
): Promise<SendResult> {
  const unique = [...new Set(urls.map((u) => u.trim()).filter(Boolean))];
  if (unique.length === 0) return { success: false, message: "No items" };
  const pageUrl = tabId !== undefined && tabId >= 0 ? getTabPageUrl(tabId) : "";
  const effectiveReferrer = referrer || pageUrl || undefined;
  const pageHost = hostOf(effectiveReferrer ?? "");

  if (unique.length === 1) {
    const url = unique[0];
    const result = await sendToFluxDown({
      url,
      referrer: effectiveReferrer,
      filename: magnetName(url),
      fileSize: 0,
      channel,
      pageHost,
    });
    return { success: result.success, message: result.message, channel: result.channel };
  }

  let last: SendResult = { success: true };
  for (let i = 0; i < unique.length; i += BATCH_LIMIT) {
    const chunk = unique.slice(i, i + BATCH_LIMIT);
    last = await sendBatch(
      chunk.map((url) => ({ url, referrer: effectiveReferrer, filename: magnetName(url), fileSize: 0 })),
      tabId,
      channel,
      pageHost,
    );
    if (!last.success) return last;
  }
  return last;
}

/** 把页面 URL 交给桌面端；命中解析插件时由桌面端选择清晰度。 */
export async function handoffPage(tabId: number): Promise<SendResult> {
  let pageUrl = getTabPageUrl(tabId);
  if (!pageUrl) {
    try {
      pageUrl = (await browser.tabs.get(tabId)).url ?? "";
    } catch {
      /* tab 已关闭 */
    }
  }
  if (!pageUrl) return { success: false, message: "No page url" };
  const channel = await chooseManualTarget();
  if (channel === null) return cancelledSelection();
  const result = await sendToFluxDown({ url: pageUrl, referrer: pageUrl, fileSize: 0, channel });
  return {
    success: result.success,
    message: result.message,
    channel: result.channel,
    taskId: result.channel === "local" ? createdTaskId(result.data) : undefined,
  };
}

/** 本机 `download` 应答的 `data`：直接建任务时为 `{ taskId }`，进确认框时为 `{ transactionIds }`。 */
function createdTaskId(data: unknown): string | undefined {
  if (typeof data !== "object" || data === null || !("taskId" in data)) return undefined;
  const taskId = data.taskId;
  return typeof taskId === "string" && taskId ? taskId : undefined;
}
