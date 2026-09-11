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

export type MediaCandidateSource = "direct" | "hls" | "dash" | "fragments";

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

function isFragmentUrl(url: string): boolean {
  const ext = extractExtension(url);
  return ext === "m4s" || ext === "ts";
}

function isManifestUrl(url: string): boolean {
  return isStreamingUrl(url) && !isFragmentUrl(url);
}

function isCompleteVideoResource(resource: DetectedResource): boolean {
  if (isFragmentUrl(resource.url) || isManifestUrl(resource.url)) return false;
  if (resource.type === "video") return true;
  const mime = resource.mimeType?.toLowerCase() || "";
  const ext = extractExtension(resource.url);
  return mime.startsWith("video/") || ["mp4", "webm", "mov", "mkv", "avi"].includes(ext);
}

/** 在已有权威 manifest 时，把本 tab 的分片作为其证据，不再暴露为独立下载项。 */
function fragmentFamilyKey(url: string): string {
  try {
    const parsed = new URL(url);
    const segments = parsed.pathname.split("/");
    segments.pop();
    return `${parsed.origin}${segments.join("/")}/<fragments>`;
  } catch {
    return url;
  }
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
  return manifest.audio.reduce<string | undefined>((best, track) => {
    if (!best) return track.url;
    const current = manifest.audio.find((item) => item.url === best);
    return (track.bandwidth ?? 0) > (current?.bandwidth ?? 0)
      ? track.url
      : best;
  }, undefined);
}

function manifestSignature(manifest: DashManifest): string {
  return [
    ...manifest.video.map((track) => `v:${urlKey(track.url)}:${track.bandwidth ?? 0}:${track.codecs ?? ""}`),
    ...manifest.audio.map((track) => `a:${urlKey(track.url)}:${track.bandwidth ?? 0}:${track.codecs ?? ""}`),
  ].sort().join("\n");
}

function relatedFragmentResources(
  resources: DetectedResource[],
  manifestUrl: string,
): DetectedResource[] {
  return resources.filter(
    (resource) =>
      resource.type === "stream" &&
      (urlKey(resource.url) === urlKey(manifestUrl) ||
        isFragmentUrl(resource.url)),
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
 * 重要约束：只有完整直链或 manifest 才生成可下载 variant；孤立分片只生成
 * 不可下载的诊断卡片，避免用户把单个 m4s/ts 当成完整视频。
 */
export function buildMediaCandidates(
  resources: DetectedResource[],
  options: MediaCandidateOptions,
): MediaCandidate[] {
  const mediaResources = resources.filter(
    (resource) => resource.type === "video" || resource.type === "stream",
  );
  if (mediaResources.length === 0 && !(options.manifests?.length ?? 0)) return [];

  const baseTitle = cleanTitle(options.pageTitle, options.fallbackTitle);
  const usedResourceIds = new Set<string>();
  const candidates: MediaCandidate[] = [];
  const seenManifestUrls = new Set<string>();
  const seenManifestSignatures = new Set<string>();

  for (const [manifestIndex, entry] of (options.manifests || []).entries()) {
    if (!entry?.manifest || (entry.url && seenManifestUrls.has(urlKey(entry.url)))) continue;
    const signature = manifestSignature(entry.manifest);
    if (seenManifestSignatures.has(signature)) continue;
    const manifestKey = entry.url ? urlKey(entry.url) : `__manifest_${manifestIndex}`;
    seenManifestUrls.add(manifestKey);
    seenManifestSignatures.add(signature);

    const root = entry.url
      ? mediaResources.find((resource) => urlKey(resource.url) === urlKey(entry.url))
      : undefined;
    const related = entry.url
      ? relatedFragmentResources(mediaResources, entry.url)
        .filter((resource) => !usedResourceIds.has(resource.id))
      : mediaResources.filter(
        (resource) =>
          !usedResourceIds.has(resource.id) &&
          (isFragmentUrl(resource.url) || isManifestUrl(resource.url)),
      );
    for (const resource of related) usedResourceIds.add(resource.id);
    if (root) usedResourceIds.add(root.id);

    const audioUrl = bestAudioUrl(entry.manifest);
    const seenVariantUrls = new Set<string>();
    const variants = entry.manifest.video.flatMap((track, trackIndex) => {
      // Some manifests reuse the same Representation id for different codec
      // tracks. The normalized URL is the actual identity of a downloadable
      // variant; use it both to keep row selection independent and to avoid
      // showing the exact same source twice.
      const trackUrlKey = urlKey(track.url);
      if (seenVariantUrls.has(trackUrlKey)) return [];
      seenVariantUrls.add(trackUrlKey);
      return [{
        id: `dash:${manifestIndex}:${track.id ?? trackIndex}:${trackUrlKey}`,
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

  // Keep orphan fragments visible as one warning card per URL family, never as
  // individually selectable/downloadable rows.
  const orphanGroups = new Map<string, DetectedResource[]>();
  for (const resource of mediaResources) {
    if (usedResourceIds.has(resource.id) || !isFragmentUrl(resource.url)) continue;
    const key = fragmentFamilyKey(resource.url);
    const group = orphanGroups.get(key) || [];
    group.push(resource);
    orphanGroups.set(key, group);
  }
  for (const [family, group] of orphanGroups) {
    candidates.push({
      id: `fragments:${family}`,
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
