/**
 * Resource sniffer diagnostics export.
 *
 * The export deliberately keeps raw URLs and parsed media metadata, because
 * those are the facts needed to diagnose aggregation. Cookies and header
 * values are never serialized; only their presence and header names are kept.
 */

import type { DashManifest } from "./dash-manifest";
import type {
  DashManifestEntry,
  MediaCandidate,
  MediaCandidateVariant,
} from "./media-candidates";
import { countMediaCandidateRows } from "./media-candidates";
import type { DetectedResource } from "./resource-types";
import { normalizeUrlForDedup } from "./resource-types";

export interface ResourceDebugLogOptions {
  resources: DetectedResource[];
  manifests?: DashManifestEntry[];
  candidates: MediaCandidate[];
  tabId?: number;
  pageUrl?: string;
  pageTitle?: string;
  source: "popup" | "content";
}

function canonicalUrl(url: string | undefined): string {
  return url ? normalizeUrlForDedup(url) : "";
}

function timestampIso(value: number): string | undefined {
  if (!Number.isFinite(value) || value <= 0) return undefined;
  try {
    return new Date(value).toISOString();
  } catch {
    return undefined;
  }
}

function debugTrack(track: DashManifest["video"][number]) {
  return {
    id: track.id,
    url: track.url,
    canonicalUrl: canonicalUrl(track.url),
    mimeType: track.mimeType,
    codecs: track.codecs,
    bandwidth: track.bandwidth,
    width: track.width,
    height: track.height,
    downloadable: track.downloadable !== false,
  };
}

function debugVariant(variant: MediaCandidateVariant) {
  return {
    id: variant.id,
    label: variant.label,
    videoUrl: variant.videoUrl,
    canonicalVideoUrl: canonicalUrl(variant.videoUrl),
    audioUrl: variant.audioUrl,
    canonicalAudioUrl: canonicalUrl(variant.audioUrl),
    mimeType: variant.mimeType,
    bandwidth: variant.bandwidth,
    codec: variant.codec,
    fileSize: variant.fileSize,
    resourceId: variant.resourceId,
  };
}

/** Build a versioned, JSON-serializable snapshot of the current sniffer state. */
export function buildResourceDebugLog(options: ResourceDebugLogOptions) {
  const manifests = options.manifests || [];
  const representedIds = new Set(
    options.candidates.flatMap((candidate) => candidate.rawResourceIds),
  );
  const rawNonMediaCount = options.resources.filter(
    (resource) =>
      resource.type !== "video" &&
      resource.type !== "stream" &&
      !representedIds.has(resource.id),
  ).length;
  const mediaResourceCount = options.resources.filter(
    (resource) =>
      resource.type === "video" ||
      resource.type === "audio" ||
      resource.type === "stream",
  ).length;

  return {
    format: "fluxdown-resource-debug",
    schemaVersion: 1,
    exportedAt: new Date().toISOString(),
    source: options.source,
    context: {
      tabId: options.tabId,
      pageUrl: options.pageUrl || "",
      pageTitle: options.pageTitle || "",
    },
    summary: {
      rawResourceCount: options.resources.length,
      rawMediaResourceCount: mediaResourceCount,
      manifestCount: manifests.length,
      candidateCount: options.candidates.length,
      downloadableCandidateCount: options.candidates.filter(
        (candidate) => candidate.downloadable,
      ).length,
      representedRawResourceCount: representedIds.size,
      displayedRowCount: countMediaCandidateRows(options.candidates) + rawNonMediaCount,
    },
    resources: options.resources.map((resource) => ({
      id: resource.id,
      url: resource.url,
      canonicalUrl: canonicalUrl(resource.url),
      finalUrl: resource.finalUrl,
      canonicalFinalUrl: canonicalUrl(resource.finalUrl),
      filename: resource.filename,
      type: resource.type,
      size: resource.size,
      mimeType: resource.mimeType,
      quality: resource.quality,
      qualities: resource.qualities?.map((quality) => ({
        ...quality,
        canonicalUrl: canonicalUrl(quality.url),
      })),
      detectedBy: resource.detectedBy,
      detectedAt: resource.detectedAt,
      detectedAtIso: timestampIso(resource.detectedAt),
      tabId: resource.tabId,
      pageUrl: resource.pageUrl,
      confidence: resource.confidence,
      isAttachment: resource.isAttachment,
      auth: {
        hasCookies: Boolean(resource.cookies),
        headerNames: Object.keys(resource.headers || {}).sort(),
      },
    })),
    manifests: manifests.map((entry) => ({
      url: entry.url,
      canonicalUrl: canonicalUrl(entry.url),
      video: entry.manifest.video.map(debugTrack),
      audio: entry.manifest.audio.map(debugTrack),
    })),
    candidates: options.candidates.map((candidate) => ({
      id: candidate.id,
      title: candidate.title,
      type: candidate.type,
      source: candidate.source,
      pageUrl: candidate.pageUrl,
      downloadable: candidate.downloadable,
      fragmentCount: candidate.fragmentCount,
      rawResourceIds: candidate.rawResourceIds,
      variants: candidate.variants.map(debugVariant),
    })),
  };
}

export function stringifyResourceDebugLog(log: ReturnType<typeof buildResourceDebugLog>): string {
  return `${JSON.stringify(log, null, 2)}\n`;
}
