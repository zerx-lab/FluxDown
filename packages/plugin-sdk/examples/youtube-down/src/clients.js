// InnerTube 客户端定义，逐字取自 yt-dlp `extractor/youtube/_base.py`（INNERTUBE_CLIENTS，
// 2026-11 版）。只保留拿 HTTPS 直链不需要 PO Token 的客户端：web / mweb / android / ios 的
// 直链没有 GVS PO Token 会 403，yt-dlp 自己也会把它们的格式丢掉。web_embedded 例外：
// yt-dlp 自 2026-11 起把它标为需要 GVS PO Token（登录态直链会 403），只排在 tv_samsung 之后兜底。

const SAFARI_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)";

/**
 * - `id`：X-YouTube-Client-Name；
 * - `js`：直链是否带签名 / n 挑战（需要播放器 JS 求解）；
 * - `cookies`：是否接受登录 Cookie（yt-dlp SUPPORTS_COOKIES）；
 * - `embedded`：嵌入式播放器（需 thirdParty.embedUrl 与嵌入页 ytcfg）。
 */
export const CLIENTS = {
  tv_samsung: {
    id: 7,
    js: true,
    cookies: true,
    context: {
      clientName: "TVHTML5",
      clientVersion: "5.20260707",
      deviceMake: "Samsung",
      deviceModel: "UKS9800",
      userAgent:
        "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1",
      osName: "Tizen",
      osVersion: "2.4.0",
    },
  },
  tv: {
    id: 7,
    js: true,
    cookies: true,
    context: {
      clientName: "TVHTML5",
      clientVersion: "7.20260707.07.00",
      userAgent:
        "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)",
    },
  },
  tv_downgraded: {
    id: 7,
    js: true,
    cookies: true,
    context: {
      clientName: "TVHTML5",
      clientVersion: "5.20260707",
      userAgent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version",
    },
  },
  // 「为儿童打造」的视频在该客户端不可用（回退 web_embedded）。
  visionos: {
    id: 101,
    js: false,
    cookies: false,
    context: {
      clientName: "VISIONOS",
      clientVersion: "1.02",
      deviceMake: "Apple",
      deviceModel: "RealityDevice17,1",
      userAgent:
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
      osName: "visionOS",
      osVersion: "26.5.23O471",
    },
  },
  web_embedded: {
    id: 56,
    js: true,
    cookies: true,
    embedded: true,
    context: {
      clientName: "WEB_EMBEDDED_PLAYER",
      clientVersion: "2.20260708.00.00",
      userAgent: SAFARI_UA,
    },
  },
};

/** 播放列表 browse 用的网页客户端（不取直链，无 PO Token 要求）。 */
export const WEB_CLIENT = {
  id: 1,
  context: { clientName: "WEB", clientVersion: "2.20260708.00.00" },
};

/** 匿名默认：yt-dlp `_DEFAULT_CLIENTS` 去掉强制 SABR 的 web。 */
export const DEFAULT_CLIENTS = ["tv_samsung", "visionos"];
/**
 * 登录默认：yt-dlp `_DEFAULT_AUTHED_CLIENTS` 去掉 web（visionos 不接受 Cookie）。tv_samsung 在前：
 * 同一格式先请求的客户端优先，web_embedded 的直链缺 PO Token 会 403，只补 tv_samsung 没有的格式。
 */
export const DEFAULT_AUTHED_CLIENTS = ["tv_samsung", "web_embedded"];

/** 嵌入式播放器的第三方来源（yt-dlp 同值：任意非 YouTube 地址）。 */
export const EMBED_URL = "https://www.reddit.com/";

/** 抓网页 / 播放器 JS 时用的桌面浏览器 UA。 */
export const BROWSER_UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/** 解析「播放器客户端（高级）」设置：逗号分隔、只认上表里的名字、去重；空 = null（用默认）。 */
export function parseClientSetting(value) {
  const names = String(value || "")
    .split(",")
    .map((s) => s.trim())
    .filter((s) => Object.hasOwn(CLIENTS, s));
  return names.length ? [...new Set(names)] : null;
}
