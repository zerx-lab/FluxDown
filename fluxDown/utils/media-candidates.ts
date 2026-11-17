/**
 * 将嗅探到的原始媒体请求投影为用户真正能理解的「视频候选」。
 *
 * background/tab-state 保留原始请求，便于认证信息恢复和高级诊断；本模块只负责
 * UI/下载层的聚合，不把 m4s/ts 分片伪装成独立视频。
 */

import type { DashManifest } from "./dash-manifest";
import type { DetectedResource } from "./resource-types";
import { extractFilenameFromUrl, isStreamingUrl } from "./resource-types";
import { hostMatches } from "@/rules/glob";
import { extractExtension } from "@/rules/url";
import { normalizeDashManifest, SEGMENT_PLACEHOLDER } from "./dash-manifest";
import { hlsSegmentDir, type HlsPlaylistEntry, type HlsPlaylistInfo } from "./hls-playlist";

export type MediaCandidateSource =
  | "direct"
  | "hls"
  | "dash"
  | "fragments"
  | "ignored";

export interface DashManifestEntry {
  url: string;
  manifest: DashManifest;
  /** 换页前观察到的清单（预取的下一个视频等）：只在关联到播放元素后展示。 */
  carried?: boolean;
}

export interface MediaCandidateVariant {
  id: string;
  /** UI-facing stable label: "1080p", "720p", or "auto". */
  label: string;
  videoUrl: string;
  audioUrl?: string;
  mimeType?: string;
  bandwidth?: number;
  codec?: string;
  frameRate?: number;
  fileSize?: number;
  resourceId?: string;
  /**
   * 同一画质档被合并掉的其它编码轨 URL（只用于把播放元素关联到本档，不用于下载）：
   * 播放器可能正在放 av01，而本档下载选的是码率最高的 avc1。
   */
  alternateUrls?: string[];
}

export interface MediaCandidate {
  id: string;
  title: string;
  type: "video" | "stream";
  source: MediaCandidateSource;
  pageUrl: string;
  variants: MediaCandidateVariant[];
  /** Raw resource IDs retained for diagnostics and fragment count display. */
  rawResourceIds: string[];
  fragmentCount: number;
  /** False when only orphan fragments were seen and no complete source is known. */
  downloadable: boolean;
  /** 全部来源清单都是换页前的（见 `DashManifestEntry.carried`）：未关联到播放元素时不展示。 */
  provisional?: boolean;
  /** 内容受 DRM 保护（清单声明或播放元素触发了 EME 加密）：下载得到的是密文，不提供下载。 */
  protected?: boolean;
  /**
   * 字节只存在于浏览器内（页面 Service Worker 合成的地址，如 Telegram Web 的 `/k/stream/…`）：
   * 引擎按 URL 重放只会拿到网页，不提供下载。
   */
  browserOnly?: boolean;
}

export interface MediaCandidateOptions {
  pageTitle?: string;
  pageUrl?: string;
  /** Localized fallback supplied by the caller. */
  fallbackTitle: string;
  /** Localized label used when multiple candidates share one page title. */
  videoLabel?: string;
  manifests?: DashManifestEntry[];
  /** 已观察到的 HLS 播放列表关联（子列表 / 分片目录）。 */
  hlsPlaylists?: HlsPlaylistEntry[];
  /** mirror 规则声明的镜像域名组（host 模式）。 */
  mirrorGroups?: string[][];
  /** URL 去重键（调用方传入 `engine.dedupeKey` 的闭包）。 */
  key: (url: string) => string;
}

/**
 * 仅用于判断同一媒体轨道是否因签名/鉴权参数变化而重复出现。
 *
 * 不能直接把所有查询参数都从下载 URL 中删除：某些站点会用 itag、quality
 * 等参数区分不同清晰度。这里保留原始 URL 用于下载，只在候选身份判断时使用
 * origin + pathname；清晰度、码率、编码和 representation id 会在 trackIdentity
 * 中继续区分真正不同的轨道。
 */
function stableMediaPath(url: string): string {
  try {
    const parsed = new URL(url);
    parsed.search = "";
    parsed.hash = "";
    return parsed.toString();
  } catch {
    return url;
  }
}

/** 用于识别“清单 URL 就是当前页面”的强关联。 */
function pagePathKey(url: string | undefined): string {
  if (!url) return "";
  try {
    const parsed = new URL(url);
    const pathname = parsed.pathname.replace(/\/+$/, "");
    return `${parsed.origin}${pathname || "/"}`;
  } catch {
    return "";
  }
}

/**
 * 伪装分片的 URL 扩展名：图片 / 前端静态资源。防盗链站点常把 HLS 分片命名为 `video0.jpeg`、
 * `seg-1.png`、`1.js` 等，响应却是音视频（`video/mp2t`）。动态脚本扩展名（php / asp…）不在内：
 * 它们常是整段视频的下载接口。
 */
const DISGUISED_FRAGMENT_EXTS: ReadonlySet<string> = new Set([
  "jpg", "jpeg", "png", "gif", "webp", "bmp", "ico", "avif", "svg",
  "js", "css", "html", "htm", "txt", "woff", "woff2", "ttf", "otf",
]);

/** 只用于媒体分片的 MIME（整段 .ts 文件另由 `isCompleteFragmentResource` 按大小 / 文件名放行）。 */
const FRAGMENT_MIMES: ReadonlySet<string> = new Set(["video/mp2t", "video/iso.segment", "audio/iso.segment"]);

/**
 * 是否为流媒体分片：`.m4s` / `.ts`、分片专用 MIME，或扩展名伪装成图片 / 静态资源而响应是
 * 音视频（清单 MIME 除外）。分片不是独立视频，归属清单或汇总为不可下载项。
 */
export function isMediaFragment(url: string, mimeType?: string): boolean {
  const ext = extractExtension(url);
  if (ext === "m4s" || ext === "ts") return true;
  const mime = mimeType?.toLowerCase().split(";", 1)[0].trim() ?? "";
  if (FRAGMENT_MIMES.has(mime)) return true;
  return (
    (mime.startsWith("video/") || mime.startsWith("audio/")) &&
    !mime.endsWith("mpegurl") &&
    DISGUISED_FRAGMENT_EXTS.has(ext)
  );
}

function isFragmentResource(resource: DetectedResource): boolean {
  return isMediaFragment(resource.url, resource.mimeType);
}

/** URL / MIME 是否为播放清单（HLS / DASH）；分片不算。 */
export function isManifestUrl(url: string, mimeType?: string): boolean {
  if (isStreamingUrl(url) && !isMediaFragment(url, mimeType)) return true;
  const mime = mimeType?.toLowerCase().split(";", 1)[0].trim() ?? "";
  // HLS 的 MIME 写法很多（application/vnd.apple.mpegurl、audio/mpegurl、audio/x-mpegurl…）
  return mime.endsWith("mpegurl") ||
    mime === "application/octet-stream-m3u8" ||
    mime === "application/dash+xml";
}

function isCompleteFragmentResource(resource: DetectedResource): boolean {
  if (!isFragmentResource(resource)) return false;
  // tab-state 入库时会用 URL 基名补全 filename，所以 filename 非空并不代表服务器
  // 给了 Content-Disposition 文件名；只有与 URL 基名不同的名字才算显式信号。
  // 无显式名字的媒体，超大响应也不是普通 MSE 分片。
  const filename = resource.filename?.trim();
  const explicitName =
    Boolean(filename) &&
    filename !== extractFilenameFromUrl(resource.url) &&
    filename !== extractFilenameFromUrl(resource.finalUrl ?? "");
  return Boolean(resource.isAttachment || explicitName) || resource.size >= 16 * 1024 * 1024;
}

function isCompleteVideoResource(resource: DetectedResource): boolean {
  if (isFragmentResource(resource) && !isCompleteFragmentResource(resource)) return false;
  if (isManifestUrl(resource.url, resource.mimeType)) return false;
  if (resource.type === "audio") return false;
  if (resource.type === "video") return true;
  const mime = resource.mimeType?.toLowerCase() || "";
  const ext = extractExtension(resource.url);
  return mime.startsWith("video/") || ["mp4", "webm", "mov", "mkv", "avi"].includes(ext);
}

/** 在已有权威 manifest 时，把本 tab 的分片作为其证据，不再暴露为独立下载项。 */
function fragmentFamilyKey(url: string): string {
  try {
    const parsed = new URL(url);
    const pathname = parsed.pathname.replace(/\/+$/, "");
    const slash = pathname.lastIndexOf("/");
    const directory = slash > 0 ? pathname.slice(0, slash) : "/";
    return `${parsed.origin}${directory}/<fragments>`;
  } catch {
    return url;
  }
}

interface UrlPathInfo {
  origin: string;
  directory: string;
}

function urlPathInfo(url: string): UrlPathInfo | null {
  try {
    const parsed = new URL(url);
    const pathname = parsed.pathname.replace(/\/+$/, "");
    const slash = pathname.lastIndexOf("/");
    return {
      origin: parsed.origin,
      directory: slash > 0 ? pathname.slice(0, slash) : "/",
    };
  } catch {
    return null;
  }
}

/**
 * 播放器常把 Representation 的 BaseURL 放在目录上层，再把实际分片放到
 * 子目录；也有 CDN 会在重定向后改变签名参数。目录前缀匹配比“父目录必须
 * 完全相同”更适合这种通用 DASH 结构，但只在同一 origin 内启用。
 */
function relatedFragmentPath(
  track: UrlPathInfo | null,
  resource: UrlPathInfo | null,
): boolean {
  if (!track || !resource || track.origin !== resource.origin) return false;
  if (track.directory === "/" || resource.directory === "/") return false;
  return track.directory === resource.directory
    || track.directory.startsWith(`${resource.directory}/`)
    || resource.directory.startsWith(`${track.directory}/`);
}

function cleanTitle(raw: string | undefined, fallback: string): string {
  const title = (raw || "").replace(/\s+/g, " ").trim();
  if (title) return title.slice(0, 160);
  return fallback.trim() || "Video";
}

function qualityLabel(height?: number, bandwidth?: number): string {
  if (height && height > 0) return `${height}p`;
  if (bandwidth && bandwidth > 0) return `${Math.round(bandwidth / 1000)}kbps`;
  return "unknown";
}

function shortCodec(codecs?: string): string | undefined {
  if (!codecs) return undefined;
  return codecs.split(".")[0];
}

function periodKey(track: DashManifest["video"][number]): string {
  return track.periodId || "__default__";
}

/**
 * 主播放列表的各档：每档的媒体播放列表可单独交给 HLS 下载器。
 * 引用了独立音频组（EXT-X-MEDIA AUDIO）的档单独下载会没有声音，这种主列表只保留「自动」
 * （由下载器读主列表并合并音轨）。同分辨率同帧率档只留码率最高的一档。
 */
function hlsQualityVariants(playlist: HlsPlaylistInfo | undefined, resourceId: string): MediaCandidateVariant[] {
  const variants = playlist?.variants ?? [];
  if (variants.length < 2 || variants.some((v) => v.audioGroup)) return [];
  const tracks: DashManifest["video"] = variants.map((v) => ({
    url: v.url,
    height: v.height,
    width: v.width,
    bandwidth: v.bandwidth,
    frameRate: v.frameRate,
    codecs: v.codecs,
  }));
  return selectQualityVideoTracks(tracks)
    .sort((a, b) => (b.height ?? 0) - (a.height ?? 0) || (b.bandwidth ?? 0) - (a.bandwidth ?? 0))
    .map((track, index) => ({
      id: `hls:${resourceId}:${index}:${trackFileKey(track.url)}`,
      label: qualityLabel(track.height, track.bandwidth),
      videoUrl: track.url,
      bandwidth: track.bandwidth,
      codec: shortCodec(track.codecs),
      frameRate: track.frameRate,
    }));
}

function bestAudioUrl(audio: DashManifest["audio"]): string | undefined {
  return audio.filter((track) => track.downloadable !== false).reduce<string | undefined>((best, track) => {
    if (!best) return track.url;
    const current = audio.find((item) => item.url === best);
    return (track.bandwidth ?? 0) > (current?.bandwidth ?? 0)
      ? track.url
      : best;
  }, undefined);
}

function trackIdentity(
  track: DashManifest["video"][number],
  kind: "video" | "audio",
): string {
  return [
    kind,
    track.periodId || "",
    stableMediaPath(track.url),
    track.mimeType?.toLowerCase() || "",
    track.codecs?.toLowerCase() || "",
    track.width ?? 0,
    track.height ?? 0,
    track.bandwidth ?? 0,
    track.frameRate ?? 0,
  ].join("|");
}

/**
 * 画质档键：同一 Period、同一分辨率（无高度时按码率）、同一帧率档。
 *
 * 帧率按档（≈30 / ≈60 / ≈120）比较：不同编码对同一档常报出略有差异的值
 * （B 站同一 720P 的 avc1 报 29.412、av01 报 30.019），按整数比较会拆成两档。
 */
function qualityTierKey(track: DashManifest["video"][number]): string {
  const height = track.height ?? 0;
  const frameRateKey = track.frameRate && track.frameRate > 0
    ? String(Math.max(1, Math.round(track.frameRate / 30)))
    : "unknown";
  const qualityKey = height > 0
    ? `height:${height}`
    : `bandwidth:${track.bandwidth ?? 0}`;
  return `${periodKey(track)}|${qualityKey}|${frameRateKey}`;
}

/** 将同一画质档（见 `qualityTierKey`）的不同编码合并为一个用户可选择的画质档。 */
export function selectQualityVideoTracks(
  tracks: DashManifest["video"],
): DashManifest["video"] {
  const selected = new Map<string, DashManifest["video"][number]>();
  for (const track of tracks) {
    if (track.downloadable === false) continue;
    const key = qualityTierKey(track);
    const current = selected.get(key);
    if (!current || (track.bandwidth ?? 0) > (current.bandwidth ?? 0)) {
      selected.set(key, track);
    }
  }
  return Array.from(selected.values());
}

export function qualityResolutionLabel(label: string): string | undefined {
  const normalized = label.trim().toLowerCase();
  const match = /^(\d+)p$/.exec(normalized);
  return match ? `${Number(match[1])}P` : undefined;
}

/** 常见标称帧率：编码器报出的实测值（29.412、59.94 等）就近归到这些档位显示。 */
const NOMINAL_FRAME_RATES = [24, 25, 30, 48, 50, 60, 90, 120, 144, 240];

export function qualityFrameRateLabel(frameRate?: number): string | undefined {
  if (!frameRate || !Number.isFinite(frameRate) || frameRate <= 0) return undefined;
  const nominal = NOMINAL_FRAME_RATES.find((rate) => Math.abs(rate - frameRate) <= rate * 0.03);
  return `${nominal ?? Math.round(frameRate)}FPS`;
}

/**
 * 轨道文件身份：路径末两段（`<目录>/<文件名>`），不含域名、query。
 *
 * 同一媒体常由多台 CDN 镜像提供，镜像只换域名或在路径前加前缀（B 站
 * `upos-*.bilivideo.com/upgcxcode/…` 与 `*.mcdn.bilivideo.cn:8082/v1/resource/upgcxcode/…`），
 * 签名 query 每次请求清单都会轮换。只有一段路径时文件名太泛，保留域名。
 */
export function trackFileKey(url: string): string {
  try {
    const parsed = new URL(url);
    const segments = parsed.pathname.split("/").filter(Boolean);
    if (segments.length < 2) return `${parsed.host}/${segments.join("/")}`;
    return segments.slice(-2).join("/");
  } catch {
    return url;
  }
}

/** 泛用文件名（index / video / playlist / segment… 或过短）：不同媒体也常同名，不能单凭它认定同一文件。 */
function isGenericFileKey(fileKey: string): boolean {
  const base = (fileKey.split("/").pop() ?? "").replace(/\.[a-z0-9]{1,5}$/i, "");
  return base.length <= 3 ||
    /^(?:index|video|audio|playlist|master|manifest|segment|init|stream|media|chunk|main|output|file|default|track)[-_]?\d{0,4}$/i
      .test(base);
}

/** 域名归类：mirror 规则声明的镜像组内的域名视为同一来源，其余按 host 本身。 */
export type HostClassifier = (host: string) => string;

export function hostClassifier(mirrorGroups: readonly string[][] = []): HostClassifier {
  return (host) => {
    const index = mirrorGroups.findIndex((group) => group.some((pattern) => hostMatches(host, pattern)));
    return index >= 0 ? `mirror:${index}` : host.toLowerCase();
  };
}

function urlParts(url: string): { host: string; path: string } {
  try {
    const parsed = new URL(url);
    return { host: parsed.host, path: parsed.pathname };
  } catch {
    return { host: "", path: url };
  }
}

/**
 * 两个 URL 是否同一轨道文件：文件身份（`trackFileKey`）相同，且满足其一——文件名足够特定；
 * 完整路径相同（镜像只换域名）；来自同一来源（同 host / 同一 mirror 规则镜像组）。
 * 泛用文件名在不同域名、不同路径前缀下不认定为同一文件。
 */
function sameTrackFile(a: string, b: string, classify: HostClassifier): boolean {
  const key = trackFileKey(a);
  if (key !== trackFileKey(b)) return false;
  if (!isGenericFileKey(key)) return true;
  const pa = urlParts(a);
  const pb = urlParts(b);
  return pa.path === pb.path || classify(pa.host) === classify(pb.host);
}

interface ManifestItem {
  entry: DashManifestEntry;
  manifestIndex: number;
  manifestKey: string;
}

interface ManifestGroup {
  /** 合并后的清单；url 取最新一份，便于用最新签名做关联。 */
  entry: DashManifestEntry;
  /** 最新一份的下标（variant id 用）。 */
  manifestIndex: number;
  /** 最早一份的键：候选 id 在清单刷新（换签名 / 换清晰度集合）后保持稳定。 */
  manifestKey: string;
  memberKeys: string[];
  members: DashManifestEntry[];
}

/** 合并同一份媒体的轨道：新清单的同一文件覆盖旧清单（签名更新），旧清单独有的档位保留。 */
function mergeTracks(
  lists: DashManifest["video"][],
  compare: (a: DashManifest["video"][number], b: DashManifest["video"][number]) => number,
): DashManifest["video"] {
  const byFile = new Map<string, DashManifest["video"][number]>();
  for (const list of lists) {
    for (const track of list) byFile.set(`${periodKey(track)}|${trackFileKey(track.url)}`, track);
  }
  return [...byFile.values()].sort(compare);
}

/**
 * 把描述同一媒体的多份清单合并为一组（按出现顺序，旧 → 新）。
 *
 * 播放器会为同一视频多次请求清单：预加载低清晰度、登录后补全高清晰度、签名过期后刷新，
 * 页面内嵌状态与接口响应也会各给一份。它们共享轨道文件（见 `trackFileKey`）；不合并时
 * 同一个视频会以「Video 1 / Video 2」重复出现，且只有其中一份能关联到播放元素。
 */
function groupManifests(items: ManifestItem[], classify: HostClassifier): ManifestGroup[] {
  const groups: { items: ManifestItem[]; urls: string[] }[] = [];
  for (const item of items) {
    const urls = [...item.entry.manifest.video, ...item.entry.manifest.audio].map((track) => track.url);
    const matching = groups.filter((group) => urls.some((url) => group.urls.some((other) => sameTrackFile(url, other, classify))));
    const target = matching[0] ?? { items: [], urls: [] };
    if (matching.length === 0) groups.push(target);
    for (const other of matching.slice(1)) {
      target.items.push(...other.items);
      target.urls.push(...other.urls);
      groups.splice(groups.indexOf(other), 1);
    }
    target.items.push(item);
    target.urls.push(...urls);
  }
  return groups.map(({ items: members }) => {
    members.sort((a, b) => a.manifestIndex - b.manifestIndex);
    const newest = members[members.length - 1];
    const manifests = members.map((member) => member.entry.manifest);
    return {
      entry: {
        url: newest.entry.url,
        manifest: {
          video: mergeTracks(
            manifests.map((manifest) => manifest.video),
            (a, b) => (b.height ?? 0) - (a.height ?? 0) || (b.bandwidth ?? 0) - (a.bandwidth ?? 0),
          ),
          audio: mergeTracks(
            manifests.map((manifest) => manifest.audio),
            (a, b) => (b.bandwidth ?? 0) - (a.bandwidth ?? 0),
          ),
        },
      },
      manifestIndex: newest.manifestIndex,
      manifestKey: members[0].manifestKey,
      memberKeys: members.map((member) => member.manifestKey),
      members: members.map((member) => member.entry),
    };
  });
}

function manifestPeriods(manifest: DashManifest): Array<{
  key: string;
  video: DashManifest["video"];
  audio: DashManifest["audio"];
}> {
  const audioByPeriod = new Map<string, DashManifest["audio"]>();
  for (const track of manifest.audio) {
    const group = audioByPeriod.get(periodKey(track)) || [];
    group.push(track);
    audioByPeriod.set(periodKey(track), group);
  }
  const videoByPeriod = new Map<string, DashManifest["video"]>();
  for (const track of manifest.video) {
    const group = videoByPeriod.get(periodKey(track)) || [];
    group.push(track);
    videoByPeriod.set(periodKey(track), group);
  }
  return Array.from(videoByPeriod, ([key, video]) => ({
    key,
    video,
    audio: audioByPeriod.get(key) || [],
  }));
}

function originAndPath(url: string): string | null {
  try {
    const parsed = new URL(url);
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    return null;
  }
}

/** SegmentTemplate 轨 URL → 匹配其实际分片 `origin+pathname` 的正则（占位符匹配单个路径段内任意字符）。 */
function templatePattern(templateUrl: string): RegExp | null {
  const target = originAndPath(templateUrl);
  if (!target) return null;
  const escaped = target
    .split(SEGMENT_PLACEHOLDER)
    .map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
    .join("[^/]*");
  return new RegExp(`^${escaped}$`);
}

function relatedManifestResources(
  urlKey: (url: string) => string,
  resources: DetectedResource[],
  manifestUrl: string,
  manifest: DashManifest,
  allowBroadFragmentMatch: boolean,
  classify: HostClassifier,
): DetectedResource[] {
  const trackUrlKeys = new Set<string>();
  const trackPathKeys = new Set<string>();
  const fragmentFamilies = new Set<string>();
  for (const track of [...manifest.video, ...manifest.audio]) {
    trackUrlKeys.add(urlKey(track.url));
    trackPathKeys.add(stableMediaPath(track.url));
    fragmentFamilies.add(fragmentFamilyKey(track.url));
  }
  if (manifestUrl) {
    trackUrlKeys.add(urlKey(manifestUrl));
    trackPathKeys.add(stableMediaPath(manifestUrl));
  }

  const trackUrls = [...manifest.video, ...manifest.audio].map((track) => track.url);
  const trackPathInfos = trackUrls.map(urlPathInfo);
  // SegmentTemplate 轨：分片序号 / 时间以占位符表示，其余部分（目录、表示 id、扩展名）确定。
  // 分片扩展名不限于 m4s / ts（常见 .m4v / .m4a / .mp4），按模板匹配而不是按扩展名猜。
  const segmentPatterns = trackUrls
    .filter((url) => url.includes(SEGMENT_PLACEHOLDER))
    .map((url) => templatePattern(url))
    .filter((pattern): pattern is RegExp => pattern !== null);
  const manifestOrigins = new Set(
    [manifestUrl, ...trackUrls]
      .map((url) => urlPathInfo(url)?.origin)
      .filter((origin): origin is string => !!origin),
  );

  return resources.filter(
    (resource) => {
      if (
        resource.type !== "video" &&
        resource.type !== "audio" &&
        resource.type !== "stream"
      ) {
        return false;
      }
      if (isCompleteFragmentResource(resource)) return false;
      if (
        trackUrlKeys.has(urlKey(resource.url)) ||
        trackPathKeys.has(stableMediaPath(resource.url))
      ) {
        return true;
      }
      if (segmentPatterns.length > 0) {
        const target = originAndPath(resource.url);
        if (target && segmentPatterns.some((pattern) => pattern.test(target))) return true;
      }
      if (!isFragmentResource(resource) || isCompleteFragmentResource(resource)) return false;

      const resourceUrls = [resource.url, resource.finalUrl].filter(
        (url): url is string => !!url,
      );
      // CDN 备用域名可能不出现在当前清单的 BaseURL 中，但同一轨道的文件
      // 身份一致（镜像只换域名或加路径前缀，如 B 站 mcdn 的 /v1/resource/）。
      // 它们应归入清单候选，而不是被误报为“无播放清单”。
      const trackList = [...manifest.video, ...manifest.audio];
      if (resourceUrls.some((resourceUrl) =>
        trackList.some((track) => sameTrackFile(resourceUrl, track.url, classify)))) {
        return true;
      }
      if (resourceUrls.some((resourceUrl) => {
        const resourcePathInfo = urlPathInfo(resourceUrl);
        return fragmentFamilies.has(fragmentFamilyKey(resourceUrl)) ||
          trackPathInfos.some((trackPathInfo) => relatedFragmentPath(trackPathInfo, resourcePathInfo));
      })) {
        return true;
      }

      // 单清单页面通常只有一个播放器。此时即使 CDN 将不同 Representation
      // 放到不同目录，也应把同 origin 的分片归入这个清单；多个清单时保持
      // 保守匹配，避免把广告/第二个播放器的分片错误合并。
      return allowBroadFragmentMatch && resourceUrls.some((resourceUrl) =>
        manifestOrigins.has(urlPathInfo(resourceUrl)?.origin || ""),
      );
    },
  );
}

function directVariant(resource: DetectedResource, index: number): MediaCandidateVariant {
  return {
    id: `direct:${resource.id}:${index}`,
    label: resource.quality || "original",
    videoUrl: resource.url,
    mimeType: resource.mimeType,
    fileSize: resource.size > 0 ? resource.size : undefined,
    resourceId: resource.id,
  };
}

/**
 * 从一 tab 的原始资源和可选 DASH manifest 构建候选列表。
 *
 * 重要约束：只有完整直链或 manifest 才生成可下载 variant；孤立分片只保留
 * 为内部诊断/去重用的一条汇总记录，不在资源列表中刷出大量不可下载卡片。
 */
export function buildMediaCandidates(
  resources: DetectedResource[],
  options: MediaCandidateOptions,
): MediaCandidate[] {
  const urlKey = options.key;
  const mediaResources = resources.filter(
    (resource) =>
      resource.type === "video" ||
      resource.type === "audio" ||
      resource.type === "stream",
  );
  if (mediaResources.length === 0 && !(options.manifests?.length ?? 0)) return [];

  const baseTitle = cleanTitle(options.pageTitle, options.fallbackTitle);
  const usedResourceIds = new Set<string>();
  const candidates: MediaCandidate[] = [];
  const seenManifestUrls = new Set<string>();
  /** 声明了 DRM（ContentProtection）的模板清单键。 */
  const manifestProtected = new Set<string>();
  /** 无完整轨道 URL 的清单（SegmentTemplate / HLS 播放列表）键 → 归属它的分片资源。 */
  const manifestSegments = new Map<string, DetectedResource[]>();

  // 同一 URL 只保留最新一份（签名有时效，旧 URL 会过期），再把描述同一媒体的清单合并。
  const latestByManifestUrl = new Map<string, ManifestItem>();
  for (const [manifestIndex, entry] of (options.manifests || []).entries()) {
    const normalizedManifest = normalizeDashManifest(entry?.manifest);
    if (!entry || !normalizedManifest) continue;
    const manifestKey = entry.url ? urlKey(entry.url) : "__legacy__";
    latestByManifestUrl.delete(manifestKey);
    latestByManifestUrl.set(manifestKey, {
      entry: { ...entry, manifest: normalizedManifest },
      manifestIndex,
      manifestKey,
    });
  }
  const classify = hostClassifier(options.mirrorGroups);
  const manifestGroups = groupManifests([...latestByManifestUrl.values()], classify);

  const currentPagePath = pagePathKey(options.pageUrl);
  const pageManifests = currentPagePath
    ? manifestGroups.filter((group) => group.members.some((member) => pagePathKey(member.url) === currentPagePath))
    : [];
  // 页面内嵌清单的 URL 通常就是当前页面 URL。若存在这种强关联，只保留
  // 同页面的清单，避免播放器预加载的另一个视频因共用页面标题而出现在
  // 资源列表中；若没有强关联，则维持多播放器页面的兼容行为。
  const selectedManifestItems = pageManifests.length > 0
    ? pageManifests
    : manifestGroups;
  const ignoredManifestItems = pageManifests.length > 0
    ? manifestGroups.filter((group) => !pageManifests.includes(group))
    : [];

  for (const { entry, manifestIndex, manifestKey, memberKeys, members } of selectedManifestItems) {
    const root = mediaResources.find((resource) =>
      members.some((member) => member.url && urlKey(resource.url) === urlKey(member.url)),
    );
    const related = entry.url
      ? relatedManifestResources(
        urlKey,
        mediaResources,
        entry.url,
        entry.manifest,
        manifestGroups.length === 1,
        classify,
      )
        .filter((resource) => !usedResourceIds.has(resource.id))
      : mediaResources.filter(
        (resource) =>
          !usedResourceIds.has(resource.id),
      );
    const periodCandidates = manifestPeriods(entry.manifest);
    const periodRows: MediaCandidate[] = [];
    for (const period of periodCandidates) {
      const audioUrl = bestAudioUrl(period.audio);
      const seenVariantKeys = new Set<string>();
      const variants = selectQualityVideoTracks(period.video).flatMap((track, trackIndex) => {
        // Signed URLs can change between two identical manifest responses. Use
        // the stable track identity for the row key so the same quality is not
        // shown again just because its CDN signature rotated.
        const trackKey = trackIdentity(track, "video");
        if (seenVariantKeys.has(trackKey)) return [];
        seenVariantKeys.add(trackKey);
        const tier = qualityTierKey(track);
        const alternateUrls = period.video
          .filter((other) => other !== track && other.downloadable !== false && qualityTierKey(other) === tier)
          .map((other) => other.url);
        return [{
          id: `dash:${manifestIndex}:${period.key}:${track.id ?? trackIndex}:${trackKey}`,
          label: qualityLabel(track.height, track.bandwidth),
          videoUrl: track.url,
          audioUrl,
          mimeType: track.mimeType,
          bandwidth: track.bandwidth,
          codec: shortCodec(track.codecs),
          frameRate: track.frameRate,
          ...(alternateUrls.length > 0 ? { alternateUrls } : {}),
        }];
      });
      if (variants.length === 0) continue;
      // rawResourceIds/fragmentCount 归第一条【实际 push 成功】的 periodRow，
      // 而不是按 manifest 里的下标（periodIndex===0）。排在前面的 Period 可能
      // 因 `variants.length===0` 被跳过（如 SSAI 广告 Period 只有 SegmentTemplate
      // 占位、无完整轨道 URL），此时用下标判断会让存活候选拿到空
      // rawResourceIds，而相关资源仍被下方 usedResourceIds 消费——结果是它们
      // 既不在候选里也不在原始资源列表里，凭空消失一次；等下一轮渲染又会
      // 因为 usedResourceIds 未命中而作为独立音频/分片行重新冒出，角标数跳变。
      const isFirstSurvivingPeriod = periodRows.length === 0;
      periodRows.push({
        id: `dash:${manifestKey}:${period.key}`,
        title: baseTitle,
        type: "stream",
        source: "dash",
        pageUrl: options.pageUrl || root?.pageUrl || "",
        variants,
        rawResourceIds: isFirstSurvivingPeriod
          ? Array.from(new Set(related.map((resource) => resource.id)))
          : [],
        fragmentCount: isFirstSurvivingPeriod
          ? related.filter((resource) => isFragmentResource(resource) && !isCompleteFragmentResource(resource)).length
          : 0,
        downloadable: true,
        ...(members.every((member) => member.carried) ? { provisional: true } : {}),
        ...([...period.video, ...period.audio].some((track) => track.protected) ? { protected: true } : {}),
      });
    }

    // A parsed SegmentTemplate/SegmentList has no complete track URL. Leave the
    // original manifest resource available so the engine can handle it itself;
    // its segments still belong to it (they are not standalone videos).
    if (periodRows.length === 0) {
      const segments = related.filter((resource) => resource.id !== root?.id);
      for (const resource of segments) usedResourceIds.add(resource.id);
      for (const key of memberKeys) manifestSegments.set(key, segments);
      if ([...entry.manifest.video, ...entry.manifest.audio].some((track) => track.protected)) {
        for (const key of memberKeys) manifestProtected.add(key);
      }
      continue;
    }
    for (const key of memberKeys) seenManifestUrls.add(key);
    for (const resource of related) usedResourceIds.add(resource.id);
    if (root) usedResourceIds.add(root.id);
    candidates.push(...periodRows);
  }

  // 被当前页面强关联清单淘汰的预加载媒体仍然是已识别资源，不能掉进
  // fragments:unresolved，也不能作为音频/视频原始资源再次展示。保留一
  // 条不可下载的内部候选，既消费其原始资源，也让调试日志保留关联关系。
  for (const { entry, manifestKey } of ignoredManifestItems) {
    const related = relatedManifestResources(
      urlKey,
      mediaResources,
      entry.url,
      entry.manifest,
      true,
      classify,
    );
    for (const resource of related) usedResourceIds.add(resource.id);
    if (related.length === 0) continue;
    candidates.push({
      id: `ignored:${manifestKey}`,
      title: baseTitle,
      type: "stream",
      source: "ignored",
      pageUrl: options.pageUrl || related[0]?.pageUrl || "",
      variants: [],
      rawResourceIds: Array.from(new Set(related.map((resource) => resource.id))),
      fragmentCount: related.filter((resource) =>
        isFragmentResource(resource) && !isCompleteFragmentResource(resource),
      ).length,
      downloadable: false,
    });
  }

  // HLS：主播放列表吸收它引用的子列表（各档视频 / 音轨），播放列表吸收自己的分片。
  // 关联来自播放列表正文（见 `parseHlsPlaylist`），不按目录或文件名猜测。
  const presentManifestKeys = new Set(
    mediaResources
      .filter((resource) => !usedResourceIds.has(resource.id) && isManifestUrl(resource.url, resource.mimeType))
      .map((resource) => urlKey(resource.url)),
  );
  const hlsPlaylists = new Map<string, HlsPlaylistInfo>();
  for (const entry of options.hlsPlaylists ?? []) hlsPlaylists.set(urlKey(entry.url), entry.playlist);
  const hlsParent = new Map<string, string>();
  for (const [key, playlist] of hlsPlaylists) {
    if (!presentManifestKeys.has(key)) continue;
    for (const child of playlist.children) {
      const childKey = urlKey(child);
      if (childKey !== key && !hlsParent.has(childKey)) hlsParent.set(childKey, key);
    }
  }
  // 分片目录（或收敛后的公共目录前缀）→ 所属播放列表；按最长前缀匹配。
  const segmentOwners: Array<{ dir: string; owner: string }> = [];
  for (const [key, playlist] of hlsPlaylists) {
    const owner = hlsParent.get(key) ?? key;
    if (!presentManifestKeys.has(owner)) continue;
    for (const dir of playlist.segmentDirs) segmentOwners.push({ dir, owner });
  }
  segmentOwners.sort((a, b) => b.dir.length - a.dir.length);
  const attach = (owner: string, resource: DetectedResource): void => {
    usedResourceIds.add(resource.id);
    manifestSegments.set(owner, [...(manifestSegments.get(owner) ?? []), resource]);
  };
  for (const resource of mediaResources) {
    if (usedResourceIds.has(resource.id)) continue;
    if (isManifestUrl(resource.url, resource.mimeType)) {
      const parent = hlsParent.get(urlKey(resource.url));
      if (parent) attach(parent, resource);
      continue;
    }
    const dir = hlsSegmentDir(resource.url);
    const owner = dir ? segmentOwners.find((entry) => dir.startsWith(entry.dir))?.owner : undefined;
    if (owner) attach(owner, resource);
  }

  // Manifest URLs not parsed as DASH are still valid complete HLS/DASH sources.
  for (const resource of mediaResources) {
    if (
      usedResourceIds.has(resource.id) ||
      !isManifestUrl(resource.url, resource.mimeType) ||
      seenManifestUrls.has(urlKey(resource.url))
    ) {
      continue;
    }
    usedResourceIds.add(resource.id);
    const mime = resource.mimeType?.toLowerCase() || "";
    const source: MediaCandidateSource = extractExtension(resource.url) === "m3u8" ||
      mime.includes("mpegurl")
      ? "hls"
      : "dash";
    const key = urlKey(resource.url);
    const related = manifestSegments.get(key) ?? [];
    const playlist = hlsPlaylists.get(key);
    const childProtected = (playlist?.children ?? []).some((child) => hlsPlaylists.get(urlKey(child))?.protected);
    const isProtected = manifestProtected.has(key) || playlist?.protected === true || childProtected;
    const auto: MediaCandidateVariant = {
      id: `${source}:${resource.id}`,
      label: "auto",
      videoUrl: resource.url,
      mimeType: resource.mimeType,
      fileSize: resource.size > 0 ? resource.size : undefined,
      resourceId: resource.id,
    };
    candidates.push({
      id: `${source}:${key}`,
      title: baseTitle,
      type: "stream",
      source,
      pageUrl: options.pageUrl || resource.pageUrl,
      variants: [auto, ...hlsQualityVariants(playlist, resource.id)],
      rawResourceIds: [resource.id, ...related.map((item) => item.id)],
      fragmentCount: related.filter((item) => !isManifestUrl(item.url, item.mimeType)).length,
      downloadable: true,
      ...(isProtected ? { protected: true } : {}),
      ...(resource.carried ? { provisional: true } : {}),
    });
  }

  // A normal direct video URL is already a complete candidate.
  const directResources = mediaResources.filter(
    (resource) =>
      !usedResourceIds.has(resource.id) &&
      isCompleteVideoResource(resource),
  );
  for (const [index, resource] of directResources.entries()) {
    usedResourceIds.add(resource.id);
    candidates.push({
      id: `direct:${resource.id}`,
      title: baseTitle,
      type: "video",
      source: "direct",
      pageUrl: options.pageUrl || resource.pageUrl,
      variants: [directVariant(resource, index)],
      rawResourceIds: [resource.id],
      fragmentCount: 0,
      downloadable: true,
    });
  }

  // Keep orphan fragments as one internal summary. The popup and page panel only
  // render downloadable candidates, while raw media IDs are still consumed so
  // audio/m4s/ts requests do not reappear as ordinary resource rows.
  const orphanGroups = new Map<string, DetectedResource[]>();
  for (const resource of mediaResources) {
    if (usedResourceIds.has(resource.id) || !isFragmentResource(resource) || isCompleteFragmentResource(resource)) continue;
    const key = fragmentFamilyKey(resource.url);
    const group = orphanGroups.get(key) || [];
    group.push(resource);
    orphanGroups.set(key, group);
  }
  if (orphanGroups.size > 0) {
    const group = Array.from(orphanGroups.values()).flat();
    candidates.push({
      id: "fragments:unresolved",
      title: baseTitle,
      type: "stream",
      source: "fragments",
      pageUrl: options.pageUrl || group[0]?.pageUrl || "",
      variants: [],
      rawResourceIds: group.map((resource) => resource.id),
      fragmentCount: group.length,
      downloadable: false,
    });
  }

  const downloadableTitles = new Map<string, number>();
  for (const candidate of candidates) {
    if (candidate.downloadable) {
      downloadableTitles.set(candidate.title, (downloadableTitles.get(candidate.title) || 0) + 1);
    }
  }
  const seenTitles = new Map<string, number>();
  return candidates.map((candidate) => {
    if (!candidate.downloadable || (downloadableTitles.get(candidate.title) || 0) < 2) {
      return candidate;
    }
    const occurrence = (seenTitles.get(candidate.title) || 0) + 1;
    seenTitles.set(candidate.title, occurrence);
    const label = options.videoLabel || "Video";
    return { ...candidate, title: `${candidate.title} · ${label} ${occurrence}` };
  });
}

function safeFilenamePart(value: string): string {
  return value
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 120) || "video";
}

function variantExtension(variant: MediaCandidateVariant): string {
  if (variant.audioUrl) return "mp4";
  const ext = extractExtension(variant.videoUrl);
  // 清单类候选的产物都是 mp4：
  // - .m3u8 走引擎 HLS 下载器，分片拼接后转封装为 mp4（fMP4 直接改名；转封装失败才保留 .ts）；
  // - .mpd 走引擎 DASH 下载器，音视频轨下载后 mux 成 mp4。
  if (ext === "m3u8" || ext === "m3u" || ext === "mpd") return "mp4";
  if (ext === "m4s" || ext === "ts" || !ext) return "mp4";
  return ext;
}

/**
 * 生成可读且不会把 CDN 分片名暴露给用户的默认任务文件名。
 *
 * 同一候选有多档清晰度可选时（本 PR 的核心交付：每档清晰度独立一行 +
 * 可多选批量下载），把 `variant.label`（`1080p`/`5000kbps` 等稳定标识）
 * 拼进文件名——否则两档默认文件名完全相同，用户下载后无从分辨哪个是
 * 哪个（引擎侧 dedup 只会追加 `(1)`，不解决可读性问题）。单档候选
 * （直链/HLS/DASH auto 等）行为不变。
 */
export function candidateFilename(
  candidate: MediaCandidate,
  variant: MediaCandidateVariant,
): string {
  const base = safeFilenamePart(candidate.title);
  const name = candidate.variants.length > 1
    ? `${base} ${safeFilenamePart(variant.label)}`
    : base;
  return `${name}.${variantExtension(variant)}`;
}

export function defaultCandidateVariant(
  candidate: MediaCandidate,
): MediaCandidateVariant | undefined {
  return candidate.variants[0];
}

/**
 * 候选是否应在面板/弹窗中占一行。
 *
 * - 可下载候选总是可见。
 * - 被当前页面强关联清单淘汰的预加载媒体（`source: "ignored"`）永不可见。
 * - 无法解析出清单的孤立分片汇总（`source: "fragments"`，如 MSE 站点未捕获
 *   到 manifest）只在整页**没有任何可下载候选**时以一条禁用行展示（checkbox
 *   禁用 + `panel.videoNeedsManifest` 提示），否则用户会看到面板/角标显示零
 *   资源，即便页面明明嗅探到了媒体请求；页面已有可下载候选时它只是噪声
 *   （广告/预加载的零散分片），保持隐藏。
 */
export function isMediaCandidateVisible(
  candidate: MediaCandidate,
  candidates: readonly MediaCandidate[],
): boolean {
  if (candidate.downloadable) return true;
  if (candidate.source !== "fragments") return false;
  return !candidates.some((other) => other.downloadable);
}

/** 返回候选在资源面板中占用的行数：可下载候选按清晰度/轨道各占一行，
 *  可见的不可下载汇总占 1 行（可见性规则见 [`isMediaCandidateVisible`]）。 */
export function countMediaCandidateRows(candidates: MediaCandidate[]): number {
  return candidates.reduce((count, candidate) => {
    if (!isMediaCandidateVisible(candidate, candidates)) return count;
    return count + (candidate.downloadable ? Math.max(candidate.variants.length, 1) : 1);
  }, 0);
}
