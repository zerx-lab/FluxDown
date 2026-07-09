/**
 * 资源类型定义 & 分类工具函数
 *
 * 核心模块，被 resource-store / background / content script / UI 共同依赖。
 *
 * v2: 新增可信度分级、分类型大小阈值、URL 归一化去重、域名/路径黑名单。
 */

// ===== 数据模型 =====

/** 资源分类 */
export type ResourceType =
  | "video"
  | "audio"
  | "document"
  | "archive"
  | "image"
  | "executable"
  | "torrent"
  | "stream"
  | "subtitle"
  | "magnet"
  | "other";

/** 资源检测来源 */
export type DetectionMethod =
  | "webRequest"
  | "dom-scan"
  | "mutation-observer"
  | "fetch-intercept"
  | "xhr-intercept"
  | "blob-intercept"
  | "mse-intercept";

/**
 * 资源可信度等级
 *
 *  high   — 用户明确想要的（attachment、<a download>、大文件视频/音频）
 *  medium — 大概率有价值（HTTP 媒体 >阈值、文档、压缩包）
 *  low    — 可能有价值但噪音风险高（小 XHR、blob、未知类型）
 */
export type ConfidenceLevel = "high" | "medium" | "low";

/** HLS/DASH 多画质选项 */
export interface QualityOption {
  url: string;
  label: string; // "1080p", "720p"
  bandwidth: number; // bps
  estimatedSize: number; // bytes, -1 = 未知
}

/** 检测到的可下载资源 */
export interface DetectedResource {
  id: string;
  url: string;
  finalUrl?: string;
  filename: string;
  type: ResourceType;
  size: number; // bytes, -1 = 未知
  mimeType?: string;
  quality?: string;
  qualities?: QualityOption[];
  detectedBy: DetectionMethod;
  detectedAt: number;
  tabId: number;
  pageUrl: string;
  /** 可信度等级 */
  confidence: ConfidenceLevel;
  /** 是否有 Content-Disposition: attachment */
  isAttachment?: boolean;
  /** 资源请求的 Cookie（webRequest 嗅探时捕获，下载时传递给 FluxDown） */
  cookies?: string;
  /** 资源请求的自定义头（Authorization 等，webRequest 嗅探时捕获） */
  headers?: Record<string, string>;
}

/** Content Script / Main World → Background 的资源消息格式 */
export interface ResourceMessage {
  action: "resourceDetected";
  resources: ResourceMessagePayload[];
}

export interface ResourceMessagePayload {
  url: string;
  type: ResourceType;
  filename?: string;
  size?: number;
  mimeType?: string;
  quality?: string;
  detectedBy: DetectionMethod;
  pageUrl?: string;
  isAttachment?: boolean;
}

/** Main World → Content Script 的 CustomEvent detail 格式 */
export interface FetchInterceptDetail {
  type:
    | "fetch-detected"
    | "xhr-detected"
    | "blob-detected"
    | "hls-manifest"
    | "dash-manifest"
    | "mse-detected";
  url: string;
  contentType?: string;
  size?: number;
  responseUrl?: string;
}

// ===== 扩展名 → 类型映射 =====

const EXTENSION_CATEGORIES: Record<ResourceType, string[]> = {
  video: [
    "mp4",
    "mkv",
    "avi",
    "mov",
    "wmv",
    "flv",
    "webm",
    "ts",
    "m4v",
    "3gp",
    "mpg",
    "mpeg",
    "f4v",
    "vob",
    "ogv",
  ],
  audio: [
    "mp3",
    "flac",
    "wav",
    "aac",
    "ogg",
    "wma",
    "m4a",
    "opus",
    "ape",
    "alac",
    "aiff",
  ],
  document: [
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "txt",
    "rtf",
    "epub",
    "mobi",
    "csv",
    "odt",
    "ods",
    "odp",
  ],
  archive: [
    "zip",
    "rar",
    "7z",
    "tar",
    "gz",
    "bz2",
    "xz",
    "iso",
    "img",
    "zst",
    "lz",
    "cab",
    "z",
  ],
  image: [
    "jpg",
    "jpeg",
    "png",
    "gif",
    "webp",
    "svg",
    "bmp",
    "ico",
    "tiff",
    "psd",
    "raw",
    "heic",
    "avif",
  ],
  executable: [
    "exe",
    "msi",
    "dmg",
    "deb",
    "rpm",
    "appimage",
    "apk",
    "ipa",
    "snap",
    "flatpak",
  ],
  torrent: ["torrent"],
  stream: ["m3u8", "m3u", "mpd", "m4s"],
  subtitle: ["vtt", "srt", "ass", "ssa", "sub", "idx", "sup", "lrc"],
  magnet: [],
  other: [],
};

const EXT_TO_TYPE = new Map<string, ResourceType>();
for (const [type, exts] of Object.entries(EXTENSION_CATEGORIES)) {
  for (const ext of exts) {
    EXT_TO_TYPE.set(ext, type as ResourceType);
  }
}

// ===== MIME → 类型映射 =====

const MIME_CATEGORIES: Record<string, ResourceType> = {
  // 视频
  "video/mp4": "video",
  "video/webm": "video",
  "video/x-flv": "video",
  "video/x-matroska": "video",
  "video/quicktime": "video",
  "video/x-msvideo": "video",
  "video/x-ms-wmv": "video",
  "video/mp2t": "video",
  "video/3gpp": "video",
  // 音频
  "audio/mpeg": "audio",
  "audio/mp4": "audio",
  "audio/ogg": "audio",
  "audio/flac": "audio",
  "audio/wav": "audio",
  "audio/aac": "audio",
  "audio/x-ms-wma": "audio",
  "audio/opus": "audio",
  "audio/x-wav": "audio",
  // 流媒体清单
  "application/vnd.apple.mpegurl": "stream",
  "application/x-mpegurl": "stream",
  "application/mpegurl": "stream",
  "application/octet-stream-m3u8": "stream",
  "application/dash+xml": "stream",
  // 文档
  "application/pdf": "document",
  "application/msword": "document",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document":
    "document",
  "application/vnd.ms-excel": "document",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet":
    "document",
  "application/vnd.ms-powerpoint": "document",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation":
    "document",
  "application/epub+zip": "document",
  "text/csv": "document",
  // 压缩包
  "application/zip": "archive",
  "application/x-rar-compressed": "archive",
  "application/x-7z-compressed": "archive",
  "application/gzip": "archive",
  "application/x-tar": "archive",
  "application/x-bzip2": "archive",
  "application/x-xz": "archive",
  "application/x-iso9660-image": "archive",
  "application/zstd": "archive",
  // 可执行
  "application/x-msdownload": "executable",
  "application/x-msi": "executable",
  "application/vnd.android.package-archive": "executable",
  "application/x-apple-diskimage": "executable",
  "application/vnd.debian.binary-package": "executable",
  // 种子
  "application/x-bittorrent": "torrent",
  // 字幕
  "text/vtt": "subtitle",
  "application/x-subrip": "subtitle",
  "text/x-ssa": "subtitle",
  "text/x-ass": "subtitle",
  "application/x-subtitle": "subtitle",
};

// ===== 数据驱动嗅探过滤表 =====
//
// 不靠代码内固定后缀白名单 + 分片智能过滤，而是一张声明式规则表：
// 命中即展示，大小/黑名单由每条规则自身携带的条件决定。
// 内置默认规则，预留 storage.sync 覆盖（编辑 UI 后续接入 applySniffRuleOverrides）。

/** 单条嗅探规则。 */
export interface SniffRule {
  /** 匹配目标：后缀名（不含点，如 "m4s"）或 MIME（支持 "video/*" 前缀通配）。 */
  match: string;
  /** 匹配维度：按 URL 后缀 或 按 Content-Type。 */
  kind: "ext" | "mime";
  /** 命中后归入的资源类别。 */
  category: ResourceType;
  /** 最小字节数，0 = 不限（小于此值不展示，仅在大小已知时生效）。 */
  minSize: number;
  /** 规则是否启用。false = 显式屏蔽该类资源。 */
  enabled: boolean;
  /** true = 命中即丢弃整个请求。 */
  blacklist?: boolean;
}

/** 匹配结果。 */
export interface SniffRuleMatch {
  /** 是否命中一条启用的正向规则（应当展示）。 */
  hit: boolean;
  /** 命中规则归类（未命中时为 "other"）。 */
  category: ResourceType;
  /** 命中黑名单或被禁用规则显式拦截 → 整个请求应丢弃。 */
  blocked: boolean;
}

/**
 * 内置默认嗅探规则表。
 *
 * 与 EXTENSION_CATEGORIES / MIME_CATEGORIES 保持一致，额外覆盖分片/流媒体后缀
 * （m4s / ts / m3u8 / mpd）。分片当作普通可下载文件直接展示，不做 <10MB 丢弃
 * （minSize:0）。
 */
const DEFAULT_SNIFF_RULES: SniffRule[] = [
  // --- 后缀规则（由 EXTENSION_CATEGORIES 派生，全部 minSize:0 命中即收） ---
  ...Object.entries(EXTENSION_CATEGORIES).flatMap(([category, exts]) =>
    (category === "image" || category === "magnet" || category === "other"
      ? []
      : exts
    ).map(
      (ext): SniffRule => ({
        match: ext,
        kind: "ext",
        category: category as ResourceType,
        minSize: 0,
        enabled: true,
      }),
    ),
  ),
  // --- MIME 规则（通配优先） ---
  { match: "video/*", kind: "mime", category: "video", minSize: 0, enabled: true },
  { match: "audio/*", kind: "mime", category: "audio", minSize: 0, enabled: true },
  {
    match: "application/vnd.apple.mpegurl",
    kind: "mime",
    category: "stream",
    minSize: 0,
    enabled: true,
  },
  {
    match: "application/x-mpegurl",
    kind: "mime",
    category: "stream",
    minSize: 0,
    enabled: true,
  },
  {
    match: "application/mpegurl",
    kind: "mime",
    category: "stream",
    minSize: 0,
    enabled: true,
  },
  {
    match: "application/octet-stream-m3u8",
    kind: "mime",
    category: "stream",
    minSize: 0,
    enabled: true,
  },
  {
    match: "application/dash+xml",
    kind: "mime",
    category: "stream",
    minSize: 0,
    enabled: true,
  },
  {
    match: "application/m4s",
    kind: "mime",
    category: "stream",
    minSize: 0,
    enabled: true,
  },
];

/** 当前生效的规则表（默认 = 内置；storage 覆盖后经 applySniffRuleOverrides 替换）。 */
let activeSniffRules: SniffRule[] = DEFAULT_SNIFF_RULES;
// 派生索引：kind=ext → Map<ext, rule>；mime 通配单独存。
let extRuleIndex = new Map<string, SniffRule>();
let mimeRuleIndex = new Map<string, SniffRule>();

function rebuildSniffIndex(): void {
  extRuleIndex = new Map();
  mimeRuleIndex = new Map();
  for (const rule of activeSniffRules) {
    if (rule.kind === "ext") {
      extRuleIndex.set(rule.match.toLowerCase(), rule);
    } else {
      mimeRuleIndex.set(rule.match.toLowerCase(), rule);
    }
  }
}
rebuildSniffIndex();

/** 返回内置默认规则（供设置页初始化 / 重置用）。 */
export function getDefaultSniffRules(): SniffRule[] {
  return DEFAULT_SNIFF_RULES.map((r) => ({ ...r }));
}

/** 返回当前生效规则的副本。 */
export function getActiveSniffRules(): SniffRule[] {
  return activeSniffRules.map((r) => ({ ...r }));
}

/**
 * 用用户自定义规则覆盖内置默认表（编辑 UI 落地后调用）。
 * 传空数组或 undefined 恢复内置默认。
 */
export function applySniffRuleOverrides(rules?: SniffRule[] | null): void {
  activeSniffRules =
    rules && rules.length > 0 ? rules : DEFAULT_SNIFF_RULES;
  rebuildSniffIndex();
}

/**
 * 命中判定：先查后缀规则，再查 MIME 规则（通配优先）。
 *
 * @param url 资源 URL（取后缀）
 * @param contentType Content-Type（可空；取主类型）
 * @param size 字节数，<=0 表示未知（未知时不因 minSize 拦截）
 */
export function matchSniffRule(
  url: string,
  contentType: string | undefined,
  size: number,
): SniffRuleMatch {
  const miss: SniffRuleMatch = { hit: false, category: "other", blocked: false };

  // 1) 后缀维度
  const ext = extractExtension(url);
  if (ext) {
    const rule = extRuleIndex.get(ext);
    if (rule) {
      if (!rule.enabled || rule.blacklist)
        return { hit: false, category: rule.category, blocked: true };
      if (rule.minSize > 0 && size > 0 && size < rule.minSize)
        return { hit: false, category: rule.category, blocked: true };
      return { hit: true, category: rule.category, blocked: false };
    }
  }

  // 2) MIME 维度（"video/mp4" 先查 "video/*" 再查精确）
  if (contentType) {
    const ct = contentType.toLowerCase().split(";")[0].trim();
    const wildcard = ct.split("/")[0] + "/*";
    const rule = mimeRuleIndex.get(wildcard) || mimeRuleIndex.get(ct);
    if (rule) {
      if (!rule.enabled || rule.blacklist)
        return { hit: false, category: rule.category, blocked: true };
      if (rule.minSize > 0 && size > 0 && size < rule.minSize)
        return { hit: false, category: rule.category, blocked: true };
      return { hit: true, category: rule.category, blocked: false };
    }
  }

  return miss;
}

// ===== 分类型大小阈值 =====
// 低于阈值的资源被认为是噪音（预加载片段、缩略图等）

/** 分类型最低大小阈值（字节），-1 表示不限 */
const SIZE_THRESHOLDS: Record<ResourceType, number> = {
  video: 500 * 1024,
  audio: 100 * 1024,
  document: -1,
  archive: -1,
  image: 100 * 1024,
  executable: -1,
  torrent: -1,
  stream: -1,
  subtitle: -1,
  magnet: -1,
  other: 50 * 1024,
};

/**
 * 根据资源类型获取大小阈值
 */
export function getSizeThreshold(type: ResourceType): number {
  return SIZE_THRESHOLDS[type];
}

// ===== 域名黑名单（过滤 tracking / analytics / ads） =====

/** 匹配方式：域名包含这些字符串即命中 */
const NOISE_DOMAIN_PATTERNS: string[] = [
  // Analytics
  "google-analytics.com",
  "googletagmanager.com",
  "analytics.google.com",
  "doubleclick.net",
  "googlesyndication.com",
  "googleadservices.com",
  "google.com/pagead",
  // Facebook
  "facebook.com/tr",
  "connect.facebook.net",
  "fbcdn.net/signals",
  // 其他追踪
  "hotjar.com",
  "clarity.ms",
  "mixpanel.com",
  "segment.io",
  "segment.com",
  "amplitude.com",
  "sentry.io",
  "bugsnag.com",
  "newrelic.com",
  "nr-data.net",
  // Ads
  "adsense",
  "adservice",
  "ad.doubleclick",
  "moat.com",
  "adsrvr.org",
  "advertising.com",
  "criteo.com",
  "outbrain.com",
  "taboola.com",
  "adnxs.com",
  // CDN 追踪/监控
  "cdn.mxpnl.com",
  "stats.wp.com",
  "pixel.wp.com",
  "bat.bing.com",
  "sb.scorecardresearch.com",
  "b.scorecardresearch.com",
];

/** 域名精确黑名单 */
const NOISE_DOMAINS_EXACT = new Set<string>([
  "www.google-analytics.com",
  "ssl.google-analytics.com",
  "stats.g.doubleclick.net",
  "cm.g.doubleclick.net",
  "pixel.facebook.com",
  "www.facebook.com",
  "tr.snapchat.com",
  "analytics.tiktok.com",
  "analytics.twitter.com",
]);

/** URL 路径黑名单模式（部分匹配） */
const NOISE_PATH_PATTERNS: string[] = [
  "/api/v", // API 端点
  "/graphql",
  "/_next/static", // Next.js 静态资源
  "/static/js/",
  "/static/css/",
  "/static/media/", // 一般是小图标
  "/assets/fonts/",
  "/fonts/",
  "/favicon",
  "/sw.js", // Service Worker
  "/workbox-",
  "/manifest.json",
  "/robots.txt",
  "/sitemap",
  "/__/", // Firebase 等内部路径
  "/beacon",
  "/collect", // analytics collect 端点
  "/pixel",
  "/track",
  "/log",
  "/telemetry",
];

/**
 * 判断 URL 是否命中噪音黑名单
 */
export function isNoiseUrl(url: string): boolean {
  try {
    const u = new URL(url);
    const hostname = u.hostname.toLowerCase();
    const pathname = u.pathname.toLowerCase();

    // 精确域名黑名单
    if (NOISE_DOMAINS_EXACT.has(hostname)) return true;

    // 模糊域名匹配
    for (const pattern of NOISE_DOMAIN_PATTERNS) {
      if (hostname.includes(pattern) || url.includes(pattern)) return true;
    }

    // 路径黑名单
    for (const pattern of NOISE_PATH_PATTERNS) {
      if (pathname.includes(pattern)) return true;
    }

    // data URI / extension internal
    if (
      url.startsWith("data:") ||
      url.startsWith("chrome-extension://") ||
      url.startsWith("moz-extension://")
    ) {
      return true;
    }
  } catch {
    return false;
  }
  return false;
}

// ===== URL 归一化（用于去重） =====

/**
 * 已知的缓存破坏 / 时间戳 / 追踪 query 参数。
 * 去掉这些后，同一资源的不同请求会归一化到相同 key。
 */
const STRIP_PARAMS = new Set<string>([
  // 缓存破坏
  "_",
  "__",
  "t",
  "ts",
  "timestamp",
  "v",
  "ver",
  "version",
  "cache",
  "cb",
  "nocache",
  "rand",
  "random",
  "r",
  "bust",
  // CDN / 签名（保留签名会导致同一资源多次出现）
  "token",
  "auth",
  "signature",
  "sig",
  "sign",
  "expire",
  "expires",
  "e",
  "st",
  "nva",
  "nvb",
  // 追踪
  "utm_source",
  "utm_medium",
  "utm_campaign",
  "utm_term",
  "utm_content",
  "fbclid",
  "gclid",
  "msclkid",
  "twclid",
  "ref",
  "referer",
  "referrer",
  "source",
  // 流媒体动态参数
  "start",
  "end",
  "begin",
  "offset",
  "sq",
  "rn",
  "rbuf", // YouTube 片段参数
]);

/**
 * 归一化 URL：去掉缓存破坏/追踪参数 + 去掉 hash
 * 返回用于去重的 canonical URL
 */
export function normalizeUrlForDedup(url: string): string {
  try {
    const u = new URL(url);
    // 去掉 hash
    u.hash = "";

    // 去掉噪音参数
    const toDelete: string[] = [];
    u.searchParams.forEach((_val, key) => {
      if (STRIP_PARAMS.has(key.toLowerCase())) {
        toDelete.push(key);
      }
    });
    for (const key of toDelete) {
      u.searchParams.delete(key);
    }

    // 排序剩余参数（确保顺序不影响去重）
    u.searchParams.sort();

    return u.toString();
  } catch {
    return url;
  }
}

/**
 * 基于归一化 URL 生成资源唯一 ID
 *
 * 直接使用归一化后的 URL 作为 ID，避免 32 位 djb2 哈希的碰撞风险。
 * Map 查找 string key 的性能在现代引擎中足够好（V8 对短字符串做了内联哈希优化）。
 */
export function generateResourceId(url: string): string {
  return normalizeUrlForDedup(url);
}

// ===== 工具函数 =====

export function extractExtension(url: string): string {
  try {
    const pathname = new URL(url).pathname;
    const lastSegment = pathname.split("/").pop() || "";
    const dotIndex = lastSegment.lastIndexOf(".");
    if (dotIndex > 0 && dotIndex < lastSegment.length - 1) {
      return lastSegment.substring(dotIndex + 1).toLowerCase();
    }
  } catch {
    const match = url.match(/\.([a-zA-Z0-9]{1,10})(?:[?#]|$)/);
    if (match) return match[1].toLowerCase();
  }
  return "";
}

export function extractFilenameFromUrl(url: string): string {
  try {
    const pathname = new URL(url).pathname;
    const lastSegment = pathname.split("/").pop() || "";
    const decoded = decodeURIComponent(lastSegment);
    if (decoded && /\.[a-zA-Z0-9]{1,10}$/.test(decoded)) {
      return decoded;
    }
  } catch {
    /* */
  }
  return "";
}

export function classifyByExtension(url: string): ResourceType {
  const ext = extractExtension(url);
  return EXT_TO_TYPE.get(ext) || "other";
}

export function classifyByMime(mime: string): ResourceType {
  const lower = mime.toLowerCase().split(";")[0].trim();

  const exact = MIME_CATEGORIES[lower];
  if (exact) return exact;

  if (lower.startsWith("video/")) return "video";
  if (lower.startsWith("audio/")) return "audio";
  if (lower.startsWith("image/")) return "image";

  if (
    lower === "application/octet-stream" ||
    lower === "application/x-download" ||
    lower === "application/force-download"
  ) {
    return "other"; // 需配合扩展名
  }

  // Office 前缀匹配
  if (lower.startsWith("application/vnd.openxmlformats-officedocument"))
    return "document";
  if (lower.startsWith("application/vnd.ms-")) return "document";

  return "other";
}

export function classifyResource(url: string, mime?: string): ResourceType {
  if (mime) {
    const byMime = classifyByMime(mime);
    if (byMime !== "other") return byMime;
  }
  return classifyByExtension(url);
}

export function isStreamingUrl(url: string): boolean {
  const lower = url.toLowerCase();
  return (
    lower.includes(".m3u8") ||
    lower.includes(".mpd") ||
    lower.includes("/manifest") ||
    lower.includes("/playlist")
  );
}

/**
 * 判断 URL 是否指向可嗅探的媒体/可下载资源（页面内 DOM/fetch 上报的兜底判定）。
 *
 * 走规则表的后缀维度：命中一条启用的正向规则即可。当服务器对媒体文件返回不规范的
 * Content-Type（text/plain、binary/octet-stream、空值、错误的 text/html）时，靠 URL
 * 后缀兜底命中。规则表已排除 image/magnet/other，无需在此额外判断。
 */
export function isSniffableExtension(url: string): boolean {
  return matchSniffRule(url, undefined, -1).hit;
}

// ===== 可信度计算 =====

/**
 * 根据资源的各种属性计算可信度等级。
 *
 * high:
 *  - Content-Disposition: attachment
 *  - <a download> 链接
 *  - 视频/音频/文档/压缩包且 size > 阈值
 *  - stream manifest (m3u8 / mpd)
 *
 * medium:
 *  - 已知类型（非 other/image）但大小未知
 *  - 图片 > 阈值
 *
 * low:
 *  - 小文件
 *  - 未知类型
 *  - blob: URL
 */
export function computeConfidence(
  type: ResourceType,
  size: number,
  detectedBy: DetectionMethod,
  isAttachment?: boolean,
): ConfidenceLevel {
  // attachment 直接高可信度
  if (isAttachment) return "high";

  // stream manifest 高可信度
  if (type === "stream") return "high";

  // torrent / executable / subtitle / magnet 高可信度
  if (
    type === "torrent" ||
    type === "executable" ||
    type === "subtitle" ||
    type === "magnet"
  )
    return "high";

  // 已知类型 + 超过阈值 → high
  const threshold = SIZE_THRESHOLDS[type];
  if (type !== "other" && type !== "image") {
    if (size > 0 && threshold > 0 && size >= threshold) return "high";
    if (size <= 0) return "medium"; // 大小未知，但类型明确
    // 大小低于阈值
    return "low";
  }

  // 图片
  if (type === "image") {
    if (size > 0 && size >= (threshold > 0 ? threshold : 100 * 1024))
      return "medium";
    return "low";
  }

  // other 类型
  if (size > 0 && size >= 1024 * 1024) return "medium"; // > 1MB
  if (size > 0 && size >= (threshold > 0 ? threshold : 50 * 1024)) return "low";

  // blob 检测来源 → low
  if (detectedBy === "blob-intercept") return "low";

  return "low";
}

// ===== 过滤判断 =====

/**
 * 判断资源是否值得展示给用户（v2 — 多维过滤）
 */
export function isWorthShowing(resource: DetectedResource): boolean {
  // magnet: URI 始终展示
  if (resource.type === "magnet") return true;

  // 1. blob: / data: URL 不展示
  if (resource.url.startsWith("blob:") || resource.url.startsWith("data:")) {
    return false;
  }

  // 2. 噪音域名/路径
  if (isNoiseUrl(resource.url)) {
    return false;
  }

  // 4. 分类型大小阈值过滤（仅当大小已知时）
  if (resource.size > 0) {
    const threshold = SIZE_THRESHOLDS[resource.type];
    if (threshold > 0 && resource.size < threshold) {
      // attachment 豁免阈值过滤
      if (!resource.isAttachment) {
        return false;
      }
    }
  }

  // 5. 图片默认不展示（除非 isAttachment 或用户开启了图片嗅探 — 由调用方控制）
  //    这里先放行 image 类型，让 store 层根据设置决定
  //    但过滤掉明显是小图标的（< 10KB 的图片）
  if (
    resource.type === "image" &&
    resource.size > 0 &&
    resource.size < 10 * 1024
  ) {
    return false;
  }

  return true;
}

// ===== 展示辅助 =====

export function detectVideoQuality(width: number, height: number): string {
  if (height >= 2160) return "4K";
  if (height >= 1440) return "1440p";
  if (height >= 1080) return "1080p";
  if (height >= 720) return "720p";
  if (height >= 480) return "480p";
  if (height >= 360) return "360p";
  if (height > 0) return `${height}p`;
  return "";
}

export function formatFileSize(bytes: number): string {
  if (bytes < 0) return "";
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const k = 1024;
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  const value = bytes / Math.pow(k, i);
  return `${value.toFixed(i > 0 ? 1 : 0)} ${units[i]}`;
}

export function getResourceTypeIcon(type: ResourceType): string {
  const icons: Record<ResourceType, string> = {
    video: "\u{1F4F9}",
    audio: "\u{1F3B5}",
    document: "\u{1F4C4}",
    archive: "\u{1F4E6}",
    image: "\u{1F5BC}",
    executable: "\u{2699}",
    torrent: "\u{1F9F2}",
    stream: "\u{1F4FA}",
    subtitle: "\u{1F4DD}",
    magnet: "\u{1F9F2}",
    other: "\u{1F4CE}",
  };
  return icons[type];
}

// ===== 轨道分组（离散音视频轨对） =====
//
// DASH 分片场景下视频与音频常被拆成两个独立文件下发（同清晰度一对），
// 需要把「视频轨 + 音频轨」配对后一起交给下载引擎（引擎按 audio_url 是否
// 为空判定要不要 mux）。判轨规则通用、不解析任何站点私有 JSON schema：
//   - mimeType 以 `video/` 开头 → 视频轨；以 `audio/` 开头 → 音频轨。
//   - mimeType 缺失时按 size 回退：>=2 条缺失且大小均已知时，最小的一条
//     视为音频轨，其余视为视频轨；只有 1 条或大小不可比较时全部归为独立
//     视频轨，不猜测音频配对。
// 音频轨通常只有一条，所有视频清晰度共享同一条（体积/码率最大的）音频轨。

/** 一组「清晰度 + 视频轨 + 可选音频轨」的下载配对。 */
export interface TrackPairGroup {
  /** 清晰度标签：优先取 DetectedResource.quality，否则按 size 降序生成占位档位名 */
  quality: string;
  videoUrl: string;
  /** 缺省 = 该清晰度无独立音频轨，视频轨可单独直下 */
  audioUrl?: string;
  videoRes: DetectedResource;
  audioRes?: DetectedResource;
}

/**
 * 把一个 tab 内嗅探到的媒体资源（video/audio/stream）按轨道类型 + 清晰度
 * 分组为「视频轨 + 共享音频轨」列表。纯函数，不依赖 DOM / chrome API。
 */
export function groupTrackPairs(
  resources: DetectedResource[],
): TrackPairGroup[] {
  const candidates = resources.filter(
    (r) => r.type === "video" || r.type === "audio" || r.type === "stream",
  );
  if (candidates.length === 0) return [];

  const videoTracks: DetectedResource[] = [];
  const audioTracks: DetectedResource[] = [];
  const unknown: DetectedResource[] = [];

  for (const r of candidates) {
    const mime = r.mimeType?.toLowerCase();
    if (mime?.startsWith("audio/")) {
      audioTracks.push(r);
    } else if (mime?.startsWith("video/")) {
      videoTracks.push(r);
    } else {
      unknown.push(r);
    }
  }

  // mimeType 缺失回退：见函数头注释。
  if (unknown.length === 1) {
    videoTracks.push(unknown[0]);
  } else if (unknown.length >= 2) {
    const allSizeKnown = unknown.every((r) => r.size > 0);
    if (allSizeKnown) {
      const sorted = [...unknown].sort((a, b) => b.size - a.size);
      audioTracks.push(sorted[sorted.length - 1]);
      videoTracks.push(...sorted.slice(0, -1));
    } else {
      videoTracks.push(...unknown);
    }
  }

  if (videoTracks.length === 0) return [];

  // 共享音频轨：取体积最大的一条，供所有视频档共用。
  const sharedAudio =
    audioTracks.length > 0
      ? audioTracks.reduce((best, cur) => (cur.size > best.size ? cur : best))
      : undefined;

  // 清晰度顺序：按视频轨 size 降序排列；quality 字段缺失时用顺位占位命名。
  const sortedVideo = [...videoTracks].sort((a, b) => b.size - a.size);

  return sortedVideo.map((v, idx) => ({
    quality: v.quality || `画质${idx + 1}`,
    videoUrl: v.url,
    audioUrl: sharedAudio?.url,
    videoRes: v,
    audioRes: sharedAudio,
  }));
}
