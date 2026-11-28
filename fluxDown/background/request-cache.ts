/**
 * 第一层：webRequest 缓存。
 * - 请求头（Cookie / Authorization 等）按 URL 缓存 60 s，供发送时携带认证信息；
 * - 导航请求的 method + body（form-POST 触发的下载）按 URL 缓存，供一比一回放。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { logWarn } from "@/utils/error-log";
import { carryAcrossRedirect, recordCapturedRequest } from "./request-record";
import type { CapturedBody, RequestRecordCache } from "./request-record";

// 请求头缓存（Cookie / Authorization）；兼作「该 URL 确有网络请求」的证据（见 sawNetworkRequest）。
export const requestHeaderCache = new Map<
  string,
  { cookies: string; headers: Record<string, string>; ts: number; navigationPreload: boolean }
>();

export const requestRecordCache: RequestRecordCache = new Map();
// ArrayBuffer → base64 字符串（chrome.webRequest 抓到的 raw body 是 ArrayBuffer）。
function arrayBufferToBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  // 分块处理，避免对超大 body 触发 String.fromCharCode 栈溢出
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    const slice = bytes.subarray(i, Math.min(i + chunk, bytes.length));
    binary += String.fromCharCode(...slice);
  }
  return btoa(binary);
}

// 仅当 method 非 GET 或带有 body 时才有捕获价值——单纯的 GET 请求与现有
// URL 重发逻辑等价，缓存它会无谓占用内存。
//
// OPTIONS 必须排除：跨域下载 API（飞书云盘等）的浏览器 CORS 预检会先对
// 同一 URL 发一个 OPTIONS，紧随其后的真实 GET 因"无捕获价值"不会写缓存，
// 于是 OPTIONS 记录留存下来被 lookupRequestRecord 命中，App 端一比一回放
// OPTIONS → 服务器返回 404/HTML，且非 GET 请求被引擎强制单线程下载。
// 预检请求本身永远不可能触发下载，捕获它只有害处。
function shouldCaptureRequest(
  details: Browser.webRequest.OnBeforeRequestDetails,
): boolean {
  if (details.method === "OPTIONS") return false;
  if (details.method !== "GET" && details.method !== "HEAD") return true;
  if (details.requestBody && (details.requestBody.formData || details.requestBody.raw)) {
    return true;
  }
  return false;
}

// 表单 POST 体通常只有几 KB；超过此上限的 raw 体不缓存。
const MAX_CAPTURED_RAW_BODY_BYTES = 1024 * 1024;

function captureBody(
  details: Browser.webRequest.OnBeforeRequestDetails,
): CapturedBody | undefined {
  const body = details.requestBody;
  if (!body) return undefined;
  if (body.formData && Object.keys(body.formData).length > 0) {
    const fields: Record<string, string[]> = {};
    for (const [name, values] of Object.entries(body.formData)) {
      fields[name] = values.filter((v): v is string => typeof v === "string");
    }
    return { kind: "formData", fields };
  }
  if (body.raw && body.raw.length > 0) {
    // 只取首块 raw bytes——多块拼接对常见 form-urlencoded / JSON POST 不必要
    const part = body.raw[0];
    if (part?.bytes && part.bytes.byteLength <= MAX_CAPTURED_RAW_BODY_BYTES) {
      return {
        kind: "raw",
        bytesB64: arrayBufferToBase64(part.bytes),
        // chrome.webRequest 不直接暴露 Content-Type，留给后续 header 配对
      };
    }
  }
  return undefined;
}

function onBeforeRequestHandler(
  details: Browser.webRequest.OnBeforeRequestDetails,
): Browser.webRequest.BlockingResponse | undefined {
  if (!shouldCaptureRequest(details)) {
    // 无 body 的 GET 是该 URL 的最新真实事务——清除同 URL 的陈旧非 GET
    // 记录（如先前的 CORS 预检 OPTIONS 或已过期的 form POST），确保
    // lookupRequestRecord 不会把旧事务误配给这次 GET 触发的下载。
    if (details.method === "GET" && requestRecordCache.has(details.url)) {
      requestRecordCache.delete(details.url);
    }
    return undefined;
  }
  recordCapturedRequest(requestRecordCache, details.url, details.method, captureBody(details), Date.now());
  return undefined;
}

function onBeforeRedirectHandler(details: Browser.webRequest.OnBeforeRedirectDetails): void {
  carryAcrossRedirect(
    requestRecordCache,
    details.url,
    details.redirectUrl,
    details.statusCode,
    Date.now(),
  );
}

function onSendHeadersHandler(
  details: Browser.webRequest.OnSendHeadersDetails,
) {
  if (!details.requestHeaders) return;
  const headers: Record<string, string> = {};
  let cookies = "";
  let navigationPreload = false;
  for (const h of details.requestHeaders) {
    if (h.name && h.value) {
      headers[h.name] = h.value;
      const name = h.name.toLowerCase();
      if (name === "cookie") {
        cookies = h.value;
      } else if (name === "service-worker-navigation-preload") {
        navigationPreload = true;
      }
    }
  }
  requestHeaderCache.set(details.url, { cookies, headers, ts: Date.now(), navigationPreload });
}

/**
 * 该 URL 近 60 s 内是否真的发往过网络（含页面 SW 直通 `fetch(event.request)`）。
 * SW 导航预载不算：SW 可以弃用预载结果、在浏览器内自行合成响应。
 */
export function sawNetworkRequest(url: string): boolean {
  const entry = requestHeaderCache.get(url);
  return entry !== undefined && !entry.navigationPreload;
}

// === 认证信息提取辅助 ===

/**
 * 浏览器内部头黑名单 — IDM/NDM 策略：只跳过连接管理和已单独处理的头，
 * 其余（Accept、Sec-Fetch-*、Accept-Language 等）全部原样传递给 FluxDown。
 *
 * 许多站点（如 ctbpsp.com）的服务器会检查 Sec-Fetch-Dest / Sec-Fetch-Mode /
 * Accept / Accept-Language 等头来判断请求是否来自真实的浏览器导航。
 * 缺少这些头会导致服务器返回 HTML 错误页面而非实际文件。
 */
export const SKIP_HEADERS = new Set([
  "cookie", // 已单独提取为 cookieString 字段
  "host", // 由 URL 决定，reqwest 自动设置
  "connection", // HTTP 连接管理，reqwest 自动处理
  "content-length", // 请求体相关，GET 请求无需
  "accept-encoding", // reqwest 自动处理压缩（gzip/deflate/brotli），传了会重复解压
  "range", // 播放器 seek 产生的分段 Range，透传会与下载引擎自管的分段/续传冲突（B站 .m4s 音频轨 416）
  "if-range", // 与 Range 配套的条件头，同样不应透传
]);

/** 缺少 User-Agent 时补上浏览器真实 UA；所有下载路径共用。 */
export function withBrowserUserAgent(
  headers: Record<string, string>,
): Record<string, string> | undefined {
  const merged = Object.keys(headers).some(
    (k) => k.toLowerCase() === "user-agent",
  )
    ? headers
    : { ...headers, "User-Agent": navigator.userAgent };
  return Object.keys(merged).length > 0 ? merged : undefined;
}

/**
 * 从 requestHeaderCache 提取认证信息（Cookie + 自定义头）。
 * 不会删除缓存条目——调用方按需自行清理。
 */
export function extractAuthFromCache(
  url: string,
  fallbackUrl?: string,
): {
  cookies: string | undefined;
  headers: Record<string, string> | undefined;
} {
  let cached = requestHeaderCache.get(url);
  if (!cached && fallbackUrl && fallbackUrl !== url) {
    try {
      const target = new URL(url);
      const source = new URL(fallbackUrl);
      if (
        (target.protocol === "http:" || target.protocol === "https:") &&
        target.origin === source.origin
      ) {
        cached = requestHeaderCache.get(fallbackUrl);
      }
    } catch {
      // Invalid URLs cannot authorize a credential cache fallback.
    }
  }
  if (!cached) return { cookies: undefined, headers: undefined };

  const cookies = cached.cookies || undefined;
  const filtered: Record<string, string> = {};
  for (const [name, value] of Object.entries(cached.headers)) {
    if (!SKIP_HEADERS.has(name.toLowerCase())) {
      filtered[name] = value;
    }
  }
  const headers = Object.keys(filtered).length > 0 ? filtered : undefined;
  return { cookies, headers };
}

/** 同步注册 webRequest 监听与周期清理；由 startBackground 调用一次。 */
export function installRequestCache(): void {
  try {
    // 只有导航（表单 POST 触发下载）才可能产生下载项；XHR/fetch 的请求体
    // （上传分块、埋点）捕获后永远用不上，却要做 O(body) 的复制与 base64。
    browser.webRequest.onBeforeRequest.addListener(
      onBeforeRequestHandler as any,
      { urls: ["<all_urls>"], types: ["main_frame", "sub_frame"] },
      ["requestBody"] as any,
    );
    console.log(
      "[FluxDown] webRequest.onBeforeRequest listener registered (with requestBody)",
    );
  } catch (e) {
    logWarn(
      "[FluxDown] Failed to register onBeforeRequest listener:",
      e,
    );
  }

  try {
    browser.webRequest.onBeforeRedirect.addListener(onBeforeRedirectHandler, {
      urls: ["<all_urls>"],
      types: ["main_frame", "sub_frame"],
    });
  } catch (e) {
    logWarn("[FluxDown] Failed to register onBeforeRedirect listener:", e);
  }

  // requestRecordCache 周期清理（与 requestHeaderCache 同步策略）
  setInterval(() => {
    const now = Date.now();
    for (const [k, rec] of requestRecordCache) {
      if (now - rec.ts > 60_000) {
        requestRecordCache.delete(k);
      }
    }
    if (requestRecordCache.size > 500) {
      const excess = requestRecordCache.size - 400;
      let deleted = 0;
      for (const k of requestRecordCache.keys()) {
        if (deleted >= excess) break;
        requestRecordCache.delete(k);
        deleted++;
      }
    }
  }, 30_000);

  // 只缓存可能产生下载 / 嗅探资源的请求类型：图片、脚本、样式、字体等占页面请求的绝大多数，
  // 带 extraHeaders 的监听会让浏览器为每个命中的请求多走一次请求头回调，且下载从不经由这些类型。
  const sendHeadersFilter: Browser.webRequest.RequestFilter = {
    urls: ["<all_urls>"],
    types: ["main_frame", "sub_frame", "xmlhttprequest", "media", "object", "other"],
  };
  const sendHeadersOpts: Browser.webRequest.OnSendHeadersOptions[] | ("requestHeaders" | "extraHeaders")[] = ["requestHeaders"];
  try {
    // 先尝试带 extraHeaders（Chrome MV3），失败则降级（Firefox）
    browser.webRequest.onSendHeaders.addListener(
      onSendHeadersHandler,
      sendHeadersFilter,
      [...sendHeadersOpts, "extraHeaders"] as ("requestHeaders" | "extraHeaders")[],
    );
    console.log(
      "[FluxDown] webRequest.onSendHeaders listener registered (with extraHeaders)",
    );
  } catch {
    try {
      browser.webRequest.onSendHeaders.addListener(
        onSendHeadersHandler,
        sendHeadersFilter,
        sendHeadersOpts,
      );
      console.log(
        "[FluxDown] webRequest.onSendHeaders listener registered (without extraHeaders)",
      );
    } catch (e) {
      logWarn(
        "[FluxDown] Failed to register webRequest.onSendHeaders listener:",
        e,
      );
    }
  }

  // Bug R2-8 修复：将 requestHeaderCache 清理从"每次请求时全量遍历"改为周期性清理。
  // 高流量页面（如视频网站）每秒可能触发数百次 onSendHeaders，之前每次都 O(n) 遍历会造成性能问题。
  setInterval(() => {
    const now = Date.now();
    for (const [cachedUrl, entry] of requestHeaderCache) {
      if (now - entry.ts > 60_000) {
        requestHeaderCache.delete(cachedUrl);
      }
    }
    // 强制大小上限（防止突发积累过多条目）
    if (requestHeaderCache.size > 1000) {
      const excess = requestHeaderCache.size - 800;
      let deleted = 0;
      for (const key of requestHeaderCache.keys()) {
        if (deleted >= excess) break;
        requestHeaderCache.delete(key);
        deleted++;
      }
    }
  }, 30_000); // 每 30 秒清理一次，远低于 60 秒有效期
}
