/**
 * 发送层：把下载交给 FluxDown（NMH / 远程 / Android Intent，经 download-dispatch 路由；或 fluxdown:// 协议），以及失败回退。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { profileFor } from "@/utils/site-profiles";
import { sendBatchDownloadRequest, sendDownloadRequest, warmupNativeHost, isUnreachableFailure } from "@/utils/download-dispatch";
import type { DownloadRequest } from "@/utils/native-messaging";
import { extractCleanFilename } from "@/utils/filename";
import { t } from "@/utils/i18n";
import { allowBypass, claimAppDownNotice, markAppDown, markAppUp, probeAppReachable } from "./breaker";
import { notify } from "./notify";
import type { SendChannel, SendResult } from "./protocol";
import { extractAuthFromCache, requestHeaderCache, requestRecordCache, withBrowserUserAgent } from "./request-cache";
import { lookupRequestRecord } from "./request-record";
import type { CapturedBody } from "./request-record";
import { getCachedProfiles, getCachedSettings } from "./settings-cache";
import { incrementStat } from "./stats";
import { maybeArmTaskPollForChannel } from "./tasks";
import { getEngine } from "./rules-store";
import { logError, logWarn } from "@/utils/error-log";
import { hasDownloadsApi, isAndroidPlatform } from "@/utils/platform";
import { ANDROID_UNSUPPORTED_MESSAGES } from "./android-handoff";
import { openInHandoffTab } from "./handoff-tab";
import { createSendDeduper, interceptDedupeKey } from "./send-dedupe";

/**
 * 补上 request 阶段 headers 规则为该 URL 声明的请求头，只补缺失的名字（浏览器真实请求头优先）。
 * 单条与批量下载共用。
 */
export function withRuleHeaders(url: string, pageUrl: string, headers: Record<string, string>): Record<string, string> {
  const present = new Set(Object.keys(headers).map((name) => name.toLowerCase()));
  const missing = Object.entries(getEngine().requestHeaders(url, pageUrl)).filter(([name]) => !present.has(name));
  return missing.length > 0 ? { ...headers, ...Object.fromEntries(missing) } : headers;
}

/** 一次发送请求（单条下载）。 */
export interface SendInput {
  url: string;
  referrer?: string;
  filename?: string;
  fileSize?: number;
  mimeType?: string;
  /** 重定向前的 URL：用于回退查找 headers / 请求事务缓存。 */
  originalUrl?: string;
  storedCookies?: string;
  storedHeaders?: Record<string, string>;
  /** 离散音视频轨对的音频轨 URL；非空 = 视频轨 + 音频轨配对，引擎分别下载后 mux。 */
  audioUrl?: string;
  /** 显式通道（绕过 remoteMode 路由）。 */
  channel?: SendChannel;
  /** 发送来源：`intercept` = 浏览器下载拦截（仅此来源才跟踪桌面端取消并可能回退浏览器下载）；缺省为用户手动发送。 */
  origin?: "intercept";
  /** 页面 host：用于查站点配置（保存目录）；缺省取 referrer / URL 的 host。 */
  pageHost?: string;
  /** 触发本次发送的浏览器 download id（仅用于去重命中时的诊断日志）。 */
  downloadId?: number;
}

export type SendOutcome = SendResult & { data?: unknown };

type SendObserver = (input: SendInput, outcome: SendOutcome) => void;
const sendObservers: SendObserver[] = [];

/** 订阅「发送成功」事件（capture 跟踪等）。避免 sender 反向依赖上层模块。 */
export function addSendObserver(observer: SendObserver): void {
  sendObservers.push(observer);
}

function hostOfUrl(url: string | undefined): string | undefined {
  if (!url) return undefined;
  try {
    return new URL(url).hostname || undefined;
  } catch {
    return undefined;
  }
}

/** 站点配置的保存目录：页面 host 优先，其次 referrer、下载 URL 的 host；全部为空则不下发。 */
export async function resolveSaveDir(input: SendInput): Promise<string | undefined> {
  const profiles = await getCachedProfiles();
  for (const host of [input.pageHost, hostOfUrl(input.referrer), hostOfUrl(input.url)]) {
    if (!host) continue;
    const { saveDir } = profileFor(profiles, host).profile;
    if (saveDir) return saveDir;
  }
  return undefined;
}

/**
 * 打开 fluxdown://download?url=...&filename=... 协议 URL，触发系统唤起
 * 已注册该协议的应用（交接标签页复用与回收见 handoff-tab.ts）。
 */
export async function openProtocolUrl(
  url: string,
  filename?: string,
): Promise<boolean> {
  const params = new URLSearchParams({ url });
  if (filename) params.set("filename", filename);
  const protocolUrl = `fluxdown://download?${params.toString()}`;
  console.log("[FluxDown] protocol mode — opening:", protocolUrl);
  return openInHandoffTab(protocolUrl);
}

// 投递失败的表单 POST 下载：原下载已被取消，回退时需要原 method/body 重放。
const failedPostRecords = new Map<string, { body?: CapturedBody }>();

// 被 App/远端明确拒绝（而非不可达）的发送，按 URL 记录供 fallbackAfterSendFailure
// 区分处理；条目在下次发送同一 URL 或回退消费时清除，数量封顶防泄漏。
const rejectedSends = new Map<
  string,
  { message?: string; notified: boolean }
>();
const REJECT_NOTIFY_INTERVAL_MS = 30_000;
let _lastRejectNotifyAt = 0;

// ===== 自动拦截的短窗去重（#1048） =====
// 一次点击在部分 Chromium 分支会产生两个 download item，按 id 互斥拦不住；
// 同 method + URL + 请求体在发送中 / 成功后 3s 内再次自动发送，复用第一次的结果（调用方照常取消多余的下载项）。
const INTERCEPT_DEDUPE_WINDOW_MS = 3000;
const interceptDeduper = createSendDeduper<SendOutcome>(INTERCEPT_DEDUPE_WINDOW_MS);

// ===== 核心：发送下载请求到 FluxDown App =====
export function sendToFluxDown(input: SendInput): Promise<SendOutcome> {
  // 只对自动拦截去重：用户从菜单 / 弹窗 / 面板显式发起的发送，重复点击就是想要两份。
  if (input.origin !== "intercept" || input.channel) return deliverToFluxDown(input);
  // 键必须在同步段（任何 await 之前）算出：deliverToFluxDown 之后会消费并清掉该 URL 的请求记录。
  // 与 deliverToFluxDown 一致只认下载 URL 自身的记录，键才对应真正送出的 method / body。
  const key = interceptDedupeKey(input.url, lookupRequestRecord(requestRecordCache, input.url));
  const { promise, reused } = interceptDeduper.run(key, () => deliverToFluxDown(input));
  if (reused) {
    console.info(
      "[FluxDown] duplicate intercept send suppressed:",
      { url: input.url, downloadId: input.downloadId },
      new Error("intercept dedupe").stack,
    );
  }
  return promise;
}

async function deliverToFluxDown(input: SendInput): Promise<SendOutcome> {
  const { url, referrer, filename, fileSize, mimeType, originalUrl, storedCookies, storedHeaders, audioUrl, channel } = input;
  // === fluxdown:// 自定义协议模式 ===
  // 开启后跳过 NMH/远端投递，经协议 URL 唤起 FluxDown 客户端。
  // 限制：不经过 NMH，Cookie/Headers/method/body 无法携带，适用于公开文件。仅桌面：Android 由 Intent 交接（可携带 Cookie / 请求头）取代。
  {
    const protocolSettings = await getCachedSettings();
    // fluxdown:// 只承载单 URL；音视频分轨对必须继续走 NMH/远程，
    // 否则音频轨会被静默丢弃，最终得到无声视频。
    if (protocolSettings.enableFluxdownProtocol && !audioUrl && !channel && !(await isAndroidPlatform())) {
      const ok = await openProtocolUrl(url, filename);
      await incrementStat(ok ? "sent" : "failed");
      if (ok && (await shouldNotifyChannel("local"))) {
        const shownName = filename || extractCleanFilename(undefined, url) || url;
        notify(t("notify.downloadSent"), t("notify.sentToFluxDown", { name: shownName }));
      }
      return { success: ok, channel: "local" };
    }
  }

  // === 预热 NMH 链路（fire-and-forget） ===
  // App 冷启动 ~0.7-1s，下方 cookie/认证收集最多 ~500ms。先发 warmup 让两者并行。
  warmupNativeHost();

  // === 提取认证信息（Cookie / Authorization 等） ===
  // 策略 1：从 webRequest 缓存获取（最可靠 — 浏览器真正发出的请求头）。
  // 重定向修复：传入的 url 可能是 302 之后的 finalUrl，cache miss 时用 originalUrl 再查。
  const authFromCache = extractAuthFromCache(url, originalUrl);
  let cookieString = authFromCache.cookies || "";
  let extraHeaders: Record<string, string> = authFromCache.headers || {};
  if (authFromCache.cookies || authFromCache.headers) {
    requestHeaderCache.delete(url);
    if (originalUrl && originalUrl !== url) requestHeaderCache.delete(originalUrl);
  }

  // 策略 2：chrome.cookies API（兜底），500ms 超时保护。
  if (!cookieString) {
    try {
      const cookiesResult = await Promise.race([
        browser.cookies.getAll({ url }),
        new Promise<Browser.cookies.Cookie[]>((_, reject) =>
          setTimeout(() => reject(new Error("cookies timeout")), 500),
        ),
      ]);
      cookieString = cookiesResult.map((c) => `${c.name}=${c.value}`).join("; ");
    } catch (e) {
      logWarn("[FluxDown] Failed/timeout to extract cookies via API:", e);
    }
  }

  // 策略 3：嗅探时保存在 tab 资源里的认证信息（最终兜底）。
  if (!cookieString && storedCookies) cookieString = storedCookies;
  if (Object.keys(extraHeaders).length === 0 && storedHeaders && Object.keys(storedHeaders).length > 0) {
    extraHeaders = storedHeaders;
  }

  // 策略 4：request 阶段 headers 规则为该 CDN 声明的请求头（Referer / Origin 等）。
  extraHeaders = withRuleHeaders(url, referrer || "", extraHeaders);

  // #610：所有下载路径统一兜底补上浏览器真实 UA。
  extraHeaders = withBrowserUserAgent(extraHeaders) ?? extraHeaders;

  // 反查浏览器原始 method 与 body —— 修复 form-POST 触发的下载（uupdump 等）。
  // 只认下载 URL 自身的记录：307/308 跳转已由 onBeforeRedirect 把事务带到目标
  // URL；301/302/303 后浏览器改发 GET，跳转前的 POST 不属于这次下载。
  const reqRecord = lookupRequestRecord(requestRecordCache, url);
  if (reqRecord.method || reqRecord.body) {
    // 用过即清——避免下次同 URL 复用旧 record
    requestRecordCache.delete(url);
  }

  const request: DownloadRequest = {
    url,
    filename: filename || "",
    referrer: referrer || "",
    cookies: cookieString,
    headers: Object.keys(extraHeaders).length > 0 ? extraHeaders : undefined,
    fileSize,
    mimeType,
    method: reqRecord.method,
    body: reqRecord.body,
    audioUrl: audioUrl || undefined,
    saveDir: await resolveSaveDir(input),
  };

  rejectedSends.delete(url);
  failedPostRecords.delete(url);
  const response = await sendDownloadRequest(request, channel);
  const notifyOk = await shouldNotifyChannel(response.channel);

  if (response.success) {
    // 发送成功 → 本机 App 在线，立即解除可用性熔断；远程成功不代表本机在线。
    if (response.channel !== "remote") markAppUp();
    await incrementStat("sent");
    maybeArmTaskPollForChannel(response.channel);
    // notify 内置 5s 同文去重，密集拦截同一文件不会弹窗风暴。
    if (notifyOk) {
      const shownName = filename || extractCleanFilename(undefined, url) || url;
      notify(t("notify.downloadSent"), t("notify.sentToFluxDown", { name: shownName }));
    }
    const outcome: SendOutcome = { success: true, channel: response.channel, data: response.data };
    for (const observer of sendObservers) observer(input, outcome);
    return outcome;
  }

  await incrementStat("failed");
  logWarn("[FluxDown] sendToFluxDown failed:", response.message, "url:", url);
  if (notifyOk) {
    notify(t("notify.sendFailed"), describeSendError(response.message));
  }
  if (!isUnreachableFailure(response)) {
    if (rejectedSends.size >= 64) rejectedSends.clear();
    rejectedSends.set(url, { message: response.message, notified: notifyOk });
  }
  if (reqRecord.method) {
    if (failedPostRecords.size >= 32) failedPostRecords.clear();
    failedPostRecords.set(url, { body: reqRecord.body });
  }
  return { success: false, message: response.message, channel: response.channel };
}

/**
 * 把发送失败的原始 message 转成用户可读文案：
 * 远程通道的稳定前缀（remote_*）映射到与 popup「测试连接」一致的 i18n 文案，
 * 其余（NMH 超时/端口断开/服务端业务错误）原样带入通用连接失败模板。
 */
export function describeSendError(message?: string): string {
  if (message === "remote_auth_failed") return t("remote.testAuthFailed");
  if (message === "remote_not_configured")
    return t("remote.testNotConfigured");
  if (message?.startsWith("remote_unreachable"))
    return t("remote.testUnreachable");
  if (message && ANDROID_UNSUPPORTED_MESSAGES.has(message)) return t("notify.androidUnsupported");
  if (message === "android_handoff_failed") return t("notify.androidHandoffFailed");
  return t("notify.connectionFailed", { message: message || "unknown" });
}

/**
 * 任务发送通知的按通道开关（配置页「通用」分类）：
 * "remote" → notifyRemoteTask，"local"/未标记（直连 NMH 旧路径）→ notifyLocalTask。
 */
export async function shouldNotifyChannel(
  channel?: "local" | "remote",
): Promise<boolean> {
  const settings = await getCachedSettings();
  return channel === "remote"
    ? settings.notifyRemoteTask !== false
    : settings.notifyLocalTask === true;
}

/** 把捕获的 POST 请求体还原为 downloads.download 可用的字符串 body；无法还原返回 null。 */
function buildPostReplay(
  url: string,
  body?: CapturedBody,
): { body: string; headers: { name: string; value: string }[] } | null {
  if (!body) return { body: "", headers: [] };
  if (body.kind === "formData") {
    const params = new URLSearchParams();
    for (const [field, values] of Object.entries(body.fields)) {
      for (const value of values) params.append(field, value);
    }
    return {
      body: params.toString(),
      headers: [
        {
          name: "Content-Type",
          value: "application/x-www-form-urlencoded",
        },
      ],
    };
  }
  try {
    const binary = atob(body.bytesB64);
    const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const cached = extractAuthFromCache(url).headers ?? {};
    const contentType = Object.entries(cached).find(
      ([name]) => name.toLowerCase() === "content-type",
    )?.[1];
    return {
      body: text,
      headers: contentType ? [{ name: "Content-Type", value: contentType }] : [],
    };
  } catch {
    return null;
  }
}

/**
 * 回退到浏览器下载：当发送到 FluxDown 失败时，重新发起浏览器下载。
 * 用于 onDeterminingFilename 同步路径中，下载已被 cancel+erase 后需要恢复的场景。
 * 设置 bypassToken 防止新下载被再次拦截。
 *
 * @param silent - 静默模式，不弹出通知。默认 true：所有现有调用方都显式静默，
 *                 失败通知统一由 fallbackAfterSendFailure 的一次性提示接管。
 *                 默认静默可避免未来新增调用方误触发逐次回退弹窗（#308 #346）。
 */
export async function fallbackToBrowserDownload(
  url: string,
  filename?: string,
  silent = true,
) {
  // 设置 bypass token，15 秒内对该 URL 的下载不拦截
  allowBypass(url);
  // 表单 POST 触发的下载必须以原 method/body 重放，GET 会拿到错误页。
  const post = failedPostRecords.get(url);
  failedPostRecords.delete(url);
  try {
    if (!hasDownloadsApi()) {
      // Firefox for Android 没有 downloads API：在新标签页导航到该链接，交给浏览器自己下载
      // （上面的 bypass 令牌让 blocking 拦截放行）。POST 事务无法以导航重放。
      if (post) {
        notify(t("notify.sendFailed"), url);
        return;
      }
      await browser.tabs.create({ url, active: true });
      console.log("[FluxDown] Fallback: opened link for browser download:", url);
    } else {
      const opts: Browser.downloads.DownloadOptions = { url };
      if (filename) opts.filename = filename;
      if (post) {
        const replay = buildPostReplay(url, post.body);
        if (!replay) {
          // 多部分/二进制请求体无法无损重建，GET 重发只会落盘一个损坏文件。
          notify(t("notify.sendFailed"), url);
          return;
        }
        opts.method = "POST";
        opts.body = replay.body;
        opts.headers = replay.headers;
      }
      await browser.downloads.download(opts);
      console.log("[FluxDown] Fallback: re-initiated browser download:", url);
    }
  } catch (e) {
    logError(
      "[FluxDown] Fallback: failed to re-initiate browser download:",
      e,
    );
  }
  if (!silent) {
    notify(
      t("notify.fallbackBrowser"),
      t("notify.fallbackBrowserDetail", { url }),
    );
  }
}

/**
 * sendToFluxDown 失败后的智能回退。
 *
 * 失败分两类：
 * - 业务拒绝（App/远端明确回了失败：队列满、鉴权失败、建任务失败…）：请求没有
 *   被处理，直接回退浏览器下载，不探活、不动熔断器（App 明明在线）。
 * - 不可达/超时/未知：NMH 通信存在瞬态失败，消息可能已送达 App 但响应丢失，
 *   直接回退会导致"双下载"。此时先 ping：在线 → 视为已送达，跳过回退；
 *   不可达 → 回退并启动熔断。
 */
export async function fallbackAfterSendFailure(
  url: string,
  filename?: string,
  silent = false,
  channel?: SendChannel,
): Promise<void> {
  const rejection = rejectedSends.get(url);
  rejectedSends.delete(url);
  // 显式通道（用户手动选「本机 / 远程」）：失败只通知，不探活、不熔断、不回退浏览器下载。
  if (channel) {
    if (rejection && !rejection.notified) {
      notify(t("notify.sendFailed"), describeSendError(rejection.message));
    }
    return;
  }
  if (rejection) {
    const { fallbackOnReject } = await getCachedSettings();
    const now = Date.now();
    // 回退关闭时只通知：用户明确不希望被拒绝的下载落回浏览器。
    const mustNotify = !fallbackOnReject && !rejection.notified;
    if (
      mustNotify ||
      (!silent && !rejection.notified && now - _lastRejectNotifyAt > REJECT_NOTIFY_INTERVAL_MS)
    ) {
      _lastRejectNotifyAt = now;
      notify(t("notify.sendFailed"), describeSendError(rejection.message));
    }
    if (!fallbackOnReject) return;
    logWarn(
      "[FluxDown] Send rejected by FluxDown — falling back to browser download:",
      rejection.message,
      url,
    );
    await fallbackToBrowserDownload(url, filename, true);
    return;
  }

  try {
    // 用带重连重试的探测确认 App 状态：给 App 冷启动/瞬时繁忙第二次机会，
    // 避免单次瞬态 ping 失败把已安装的 App 误判为不可用而误熔断（review 发现 #3）。
    // 经单航班守卫复用并发探测（review round2 发现 #4）。
    const appAlive = await probeAppReachable();
    if (appAlive) {
      // App 在线（消息大概率已送达）→ 解除熔断，跳过回退避免双下载。
      markAppUp();
      console.log(
        "[FluxDown] App is alive after send failure — skipping browser fallback (message likely delivered):",
        url,
      );
      return;
    }
  } catch {
    // ping 本身异常，视为 App 不可达，继续回退
  }

  // App 二次确认仍不可达 → 启动熔断：冷却期内后续下载直接放行给浏览器，
  // 不再反复"拦截→失败→回退"。同时"未检测到 App"提示每轮熔断只弹一次，
  // 替代过去逐次回退都弹"已回退到浏览器下载"造成的弹窗风暴（#308 #346）。
  markAppDown();
  if (!silent && claimAppDownNotice()) {
    notify(t("notify.appUnavailable"), t("notify.appUnavailableDetail"));
  }
  // 内层回退强制 silent：通知已由上面的一次性提示统一接管。
  await fallbackToBrowserDownload(url, filename, true);
}
