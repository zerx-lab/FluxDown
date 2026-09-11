/**
 * 将嗅探到的原始媒体请求投影为用户真正能理解的「视频候选」。
 *
 * resource-store 保留原始请求，便于认证信息恢复和高级诊断；本模块只负责
 * UI/下载层的聚合，不把 m4s/ts 分片伪装成独立视频。
 */

import type { DashManifest } from "./dash-manifest";
import type { DetectedResource } from "./resource-types";
import {
  extractExtension,
  isStreamingUrl,
  normalizeUrlForDedup,
} from "./resource-types";

export type MediaCandidateSource =
  | "direct"
  | "hls"
  | "dash"
  | "fragments"
  | "ignored";

export interface DashManifestEntry {
  url: string;
  manifest: DashManifest;
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
  fileSize?: number;
  resourceId?: string;
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
}

export interface MediaCandidateOptions {
  pageTitle?: string;
  pageUrl?: string;
  /** Localized fallback supplied by the caller. */
  fallbackTitle: string;
  /** Localized label used only when several candidates share one page title. */
  videoLabel: string;
  manifests?: DashManifestEntry[];
}

function urlKey(url: string): string {
  return normalizeUrlForDedup(url);
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
    return urlKey(url);
  }
}

/**
 * 返回不含 Origin、查询参数和 hash 的媒体路径。
 *
 * 同一媒体的备用 CDN 常常只替换域名，分片路径和文件名保持不变；这
 * 个键只用于把分片关联到已经解析出的清单，不用于生成下载 URL 或跨
 * 媒体去重。
 */
function mediaPathKey(url: string): string {
  try {
    const parsed = new URL(url);
    const pathname = parsed.pathname.replace(/\/+$/, "");
    return pathname || "/";
  } catch {
    return urlKey(url);
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

function isFragmentUrl(url: string): boolean {
  const ext = extractExtension(url);
  return ext === "m4s" || ext === "ts";
}

function isManifestUrl(url: string): boolean {
  return isStreamingUrl(url) && !isFragmentUrl(url);
}

function isCompleteVideoResource(resource: DetectedResource): boolean {
  if (isFragmentUrl(resource.url) || isManifestUrl(resource.url)) return false;
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
function relatedFragmentPath(trackUrl: string, resourceUrl: string): boolean {
  const track = urlPathInfo(trackUrl);
  const resource = urlPathInfo(resourceUrl);
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
  if (height && height >= 2160) return "4K";
  if (height && height > 0) return `${height}p`;
  if (bandwidth && bandwidth > 0) return `${Math.round(bandwidth / 1000)}kbps`;
  return "unknown";
}

function shortCodec(codecs?: string): string | undefined {
  if (!codecs) return undefined;
  return codecs.split(".")[0];
}

function bestAudioUrl(manifest: DashManifest): string | undefined {
  return manifest.audio.filter((track) => track.downloadable !== false).reduce<string | undefined>((best, track) => {
    if (!best) return track.url;
    const current = manifest.audio.find((item) => item.url === best);
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
    stableMediaPath(track.url),
    track.mimeType?.toLowerCase() || "",
    track.codecs?.toLowerCase() || "",
    track.width ?? 0,
    track.height ?? 0,
    track.bandwidth ?? 0,
  ].join("|");
}

function manifestSignature(manifest: DashManifest): string {
  return [
    ...manifest.video.map((track) => trackIdentity(track, "video")),
    ...manifest.audio.map((track) => trackIdentity(track, "audio")),
  ].sort().join("\n");
}

function relatedManifestResources(
  resources: DetectedResource[],
  manifestUrl: string,
  manifest: DashManifest,
  allowBroadFragmentMatch: boolean,
): DetectedResource[] {
  const trackUrlKeys = new Set<string>();
  const trackPathKeys = new Set<string>();
  const trackMediaPathKeys = new Set<string>();
  const fragmentFamilies = new Set<string>();
  for (const track of [...manifest.video, ...manifest.audio]) {
    trackUrlKeys.add(urlKey(track.url));
    trackPathKeys.add(stableMediaPath(track.url));
    trackMediaPathKeys.add(mediaPathKey(track.url));
    fragmentFamilies.add(fragmentFamilyKey(track.url));
  }
  if (manifestUrl) {
    trackUrlKeys.add(urlKey(manifestUrl));
    trackPathKeys.add(stableMediaPath(manifestUrl));
  }

  const trackUrls = [...manifest.video, ...manifest.audio].map((track) => track.url);
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
      if (
        trackUrlKeys.has(urlKey(resource.url)) ||
        trackPathKeys.has(stableMediaPath(resource.url))
      ) {
        return true;
      }
      if (!isFragmentUrl(resource.url)) return false;

      const resourceUrls = [resource.url, resource.finalUrl].filter(
        (url): url is string => !!url,
      );
      // CDN 备用域名可能不出现在当前清单的 BaseURL 中，但同一轨道的
      // pathname 和文件名仍然一致。Bilibili 等站点会同时请求这种镜像
      // 地址；它们应归入清单候选，而不是被误报为“无播放清单”。
      if (resourceUrls.some((resourceUrl) =>
        trackMediaPathKeys.has(mediaPathKey(resourceUrl)))) {
        return true;
      }
      if (resourceUrls.some((resourceUrl) =>
        fragmentFamilies.has(fragmentFamilyKey(resourceUrl)) ||
        trackUrls.some((trackUrl) => relatedFragmentPath(trackUrl, resourceUrl)))) {
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

function titleForIndex(
  base: string,
  index: number,
  count: number,
  videoLabel: string,
): string {
  return count > 1 ? `${base} · ${videoLabel} ${index + 1}` : base;
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

  // Map replacement deliberately keeps the newest manifest. CDN signatures and
  // track URLs are often short-lived; retaining the first copy would dedupe the
  // row but leave the user with an expired download URL.
  const latestByManifestUrl = new Map<string, {
    entry: DashManifestEntry;
    manifestIndex: number;
    signature: string;
  }>();
  for (const [manifestIndex, entry] of (options.manifests || []).entries()) {
    if (!entry?.manifest) continue;
    const manifestKey = entry.url ? urlKey(entry.url) : "__legacy__";
    latestByManifestUrl.set(manifestKey, {
      entry,
      manifestIndex,
      signature: manifestSignature(entry.manifest),
    });
  }

  const latestBySignature = new Map<string, {
    entry: DashManifestEntry;
    manifestIndex: number;
    manifestKey: string;
  }>();
  for (const [manifestKey, item] of latestByManifestUrl) {
    latestBySignature.set(item.signature, {
      entry: item.entry,
      manifestIndex: item.manifestIndex,
      manifestKey,
    });
  }

  const manifestItems = Array.from(latestBySignature.values());
  const currentPagePath = pagePathKey(options.pageUrl);
  const pageManifests = currentPagePath
    ? manifestItems.filter((item) => pagePathKey(item.entry.url) === currentPagePath)
    : [];
  // 页面内嵌清单的 URL 通常就是当前页面 URL。若存在这种强关联，只保留
  // 同页面的清单，避免播放器预加载的另一个视频因共用页面标题而出现在
  // 资源列表中；若没有强关联，则维持多播放器页面的兼容行为。
  const selectedManifestItems = pageManifests.length > 0
    ? pageManifests
    : manifestItems;
  const ignoredManifestItems = pageManifests.length > 0
    ? manifestItems.filter((item) => !pageManifests.includes(item))
    : [];

  for (const { entry, manifestIndex, manifestKey } of selectedManifestItems) {
    seenManifestUrls.add(manifestKey);

    const root = entry.url
      ? mediaResources.find((resource) => urlKey(resource.url) === urlKey(entry.url))
      : undefined;
    const related = entry.url
      ? relatedManifestResources(
        mediaResources,
        entry.url,
        entry.manifest,
        latestBySignature.size === 1,
      )
        .filter((resource) => !usedResourceIds.has(resource.id))
      : mediaResources.filter(
        (resource) =>
          !usedResourceIds.has(resource.id),
      );
    for (const resource of related) usedResourceIds.add(resource.id);
    if (root) usedResourceIds.add(root.id);

    const audioUrl = bestAudioUrl(entry.manifest);
    const seenVariantKeys = new Set<string>();
    const variants = entry.manifest.video.flatMap((track, trackIndex) => {
      if (track.downloadable === false) return [];
      // Signed URLs can change between two identical manifest responses. Use
      // the stable track identity for the row key so the same 1080p/360p
      // variant is not shown again just because its CDN signature rotated.
      const trackKey = trackIdentity(track, "video");
      if (seenVariantKeys.has(trackKey)) return [];
      seenVariantKeys.add(trackKey);
      return [{
        id: `dash:${manifestIndex}:${track.id ?? trackIndex}:${trackKey}`,
        label: qualityLabel(track.height, track.bandwidth),
        videoUrl: track.url,
        audioUrl,
        mimeType: track.mimeType,
        bandwidth: track.bandwidth,
        codec: shortCodec(track.codecs),
      }];
    });

    candidates.push({
      id: `dash:${manifestKey}`,
      title: baseTitle,
      type: "stream",
      source: "dash",
      pageUrl: options.pageUrl || root?.pageUrl || "",
      variants,
      rawResourceIds: Array.from(new Set(related.map((resource) => resource.id))),
      fragmentCount: related.filter((resource) => isFragmentUrl(resource.url)).length,
      downloadable: variants.length > 0,
    });
  }

  // 被当前页面强关联清单淘汰的预加载媒体仍然是已识别资源，不能掉进
  // fragments:unresolved，也不能作为音频/视频原始资源再次展示。保留一
  // 条不可下载的内部候选，既消费其原始资源，也让调试日志保留关联关系。
  for (const { entry, manifestKey } of ignoredManifestItems) {
    const related = relatedManifestResources(
      mediaResources,
      entry.url,
      entry.manifest,
      true,
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
      fragmentCount: related.filter((resource) => isFragmentUrl(resource.url)).length,
      downloadable: false,
    });
  }

  // Manifest URLs not parsed as DASH are still valid complete HLS/DASH sources.
  for (const resource of mediaResources) {
    if (
      usedResourceIds.has(resource.id) ||
      !isManifestUrl(resource.url) ||
      seenManifestUrls.has(urlKey(resource.url))
    ) {
      continue;
    }
    usedResourceIds.add(resource.id);
    const source: MediaCandidateSource = extractExtension(resource.url) === "m3u8"
      ? "hls"
      : "dash";
    candidates.push({
      id: `${source}:${urlKey(resource.url)}`,
      title: baseTitle,
      type: "stream",
      source,
      pageUrl: options.pageUrl || resource.pageUrl,
      variants: [{
        id: `${source}:${resource.id}`,
        label: "auto",
        videoUrl: resource.url,
        mimeType: resource.mimeType,
        fileSize: resource.size > 0 ? resource.size : undefined,
        resourceId: resource.id,
      }],
      rawResourceIds: [resource.id],
      fragmentCount: 0,
      downloadable: true,
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
    if (usedResourceIds.has(resource.id) || !isFragmentUrl(resource.url)) continue;
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

  // Make duplicate page titles distinguishable without exposing CDN names.
  const count = candidates.length;
  return candidates.map((candidate, index) => ({
    ...candidate,
    title: titleForIndex(candidate.title, index, count, options.videoLabel),
  }));
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
  if (ext === "m3u8" || ext === "mpd") return "ts";
  if (ext === "m4s" || ext === "ts" || !ext) return "mp4";
  return ext;
}

/** 生成可读且不会把 CDN 分片名暴露给用户的默认任务文件名。 */
export function candidateFilename(
  candidate: MediaCandidate,
  variant: MediaCandidateVariant,
): string {
  const variantDetails = [
    variant.label === "auto" || variant.label === "original" ? "" : variant.label,
    variant.codec,
    variant.bandwidth ? `${Math.round(variant.bandwidth / 1000)}kbps` : "",
  ].filter(Boolean).join(" ");
  const label = variantDetails ? ` - ${safeFilenamePart(variantDetails)}` : "";
  return `${safeFilenamePart(candidate.title)}${label}.${variantExtension(variant)}`;
}

export function defaultCandidateVariant(
  candidate: MediaCandidate,
): MediaCandidateVariant | undefined {
  return candidate.variants[0];
}

/**
 * 返回候选在资源面板中占用的行数。
 *
 * 可下载候选按清晰度/轨道各占一行；不可下载的孤立分片汇总不计入资源数，
 * 因为它们不会出现在用户可操作的资源列表中。
 */
export function countMediaCandidateRows(candidates: MediaCandidate[]): number {
  return candidates.reduce(
    (count, candidate) => count + (
      candidate.downloadable ? Math.max(candidate.variants.length, 1) : 0
    ),
    0,
  );
}
