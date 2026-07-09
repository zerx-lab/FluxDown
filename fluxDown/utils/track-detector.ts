/**
 * 分片轨道识别（视频轨 / 音频轨）
 *
 * 判定优先级（通用优先，站点规则兜底）：
 *   1. 真实 MIME —— `audio/*` / `video/*`（manifest 已解析出的权威轨道走这条）
 *   2. URL/文件名里的 codecs / mime 线索 —— 如 YouTube `mime=audio%2Fmp4`、`&itag=140`
 *   3. 站点识别器 —— 按 hostname 匹配的私有规则，处理服务器统一返回 `video/mp4`
 *      导致 MIME 不可区分的站点（B 站等）
 *
 * 单个已下载的 DASH 分片（.m4s 等）常被服务器统一标 `video/mp4`，无法靠 MIME
 * 区分音视频；此时唯一可靠依据是站点私有的流命名约定，故需 site rule 兜底。
 * 新增站点：在 `SITE_RULES` 追加一条 `SiteTrackRule` 即可，无需改判定主流程。
 */

/** 轨道类型：视频轨 / 音频轨 / 无法判定（不标注）。 */
export type TrackKind = "video" | "audio" | null;

/** 轨道识别输入（DetectedResource 的最小子集，便于复用/测试）。 */
export interface TrackDetectInput {
  url: string;
  filename?: string;
  mimeType?: string;
  /** 资源所在页面 URL，用于 hostname 匹配站点规则。缺失则跳过站点规则。 */
  pageUrl?: string;
}

/**
 * 站点识别器：当通用 MIME/codecs 判定失败时，按站点私有约定判定轨道。
 *
 * `match` 命中（hostname / url / filename 任一维度）后调用 `detect`，
 * 返回非 null 即采纳；返回 null 继续回退（下一个匹配的规则 → 最终 MIME）。
 */
export interface SiteTrackRule {
  /** 规则名（日志/调试用）。 */
  readonly name: string;
  /** 是否适用于该资源（通常按 pageUrl/url 的 hostname 判断）。 */
  match(input: TrackDetectInput): boolean;
  /** 判定轨道；无法判定返回 null。 */
  detect(input: TrackDetectInput): TrackKind;
}

/** 从任意 URL 安全提取 hostname，失败返回空串。 */
function hostnameOf(rawUrl?: string): string {
  if (!rawUrl) return "";
  try {
    return new URL(rawUrl).hostname.toLowerCase();
  } catch {
    return "";
  }
}

/**
 * B 站（bilibili）DASH 规则。
 *
 * B 站视频/音频分片 Content-Type 均为 `video/mp4`，无法靠 MIME 区分。
 * 文件名/URL 尾部的流 ID（`-<id>.m4s`）是 B 站 API `dash.audio[].id` /
 * `dash.video[].id` 的约定：音频流 ID 固定为下列集合，其余（视频编码档）判为视频轨。
 */
const BILI_AUDIO_STREAM_IDS = new Set([
  30216, // 64K
  30232, // 132K
  30280, // 192K
  30250, // 杜比全景声
  30251, // Hi-Res / FLAC
]);
const BILI_STREAM_ID_RE = /[-_](\d{4,6})\.m4s(?:[?#]|$)/i;

const biliRule: SiteTrackRule = {
  name: "bilibili",
  match(input) {
    const host = hostnameOf(input.pageUrl) || hostnameOf(input.url);
    return (
      host.endsWith("bilibili.com") ||
      host.endsWith("bilivideo.com") ||
      host.endsWith("bilivideo.cn") ||
      host.endsWith("acgvideo.com")
    );
  },
  detect(input) {
    const id = (input.filename || input.url).match(BILI_STREAM_ID_RE);
    if (!id) return null;
    return BILI_AUDIO_STREAM_IDS.has(Number(id[1])) ? "audio" : "video";
  },
};

/** 站点规则表；追加新站点规则即可扩展，判定主流程无需改动。 */
const SITE_RULES: readonly SiteTrackRule[] = [biliRule];

/**
 * URL/文件名里的 codecs / mime 线索。
 *
 * 通用于把 mime 编进 URL 的站点（YouTube `mime=audio%2Fmp4` 等）。
 * 对 query 做解码后按子串匹配，避免各家参数名差异。
 */
function trackFromUrlHint(input: TrackDetectInput): TrackKind {
  let hint: string;
  try {
    hint = decodeURIComponent(input.url).toLowerCase();
  } catch {
    hint = input.url.toLowerCase();
  }
  if (hint.includes("audio/") || hint.includes("mime=audio")) return "audio";
  if (hint.includes("video/") || hint.includes("mime=video")) return "video";
  return null;
}

/**
 * 判定分片轨道类型。通用判定优先，站点规则兜底。
 *
 * @example
 * detectTrackKind({ url: "https://x.bilivideo.com/a-1-30280.m4s", pageUrl: "https://www.bilibili.com/video/BV1" });
 * // => "audio"
 */
export function detectTrackKind(input: TrackDetectInput): TrackKind {
  // 1. 真实 MIME —— manifest 已解析出的权威轨道最可信。
  const mime = input.mimeType?.toLowerCase();
  if (mime?.startsWith("audio/")) return "audio";
  if (mime?.startsWith("video/") && !isAmbiguousVideoMime(input)) return "video";

  // 2. URL/文件名里的 codecs / mime 线索。
  const hint = trackFromUrlHint(input);
  if (hint) return hint;

  // 3. 站点识别器兜底。
  for (const rule of SITE_RULES) {
    if (rule.match(input)) {
      const kind = rule.detect(input);
      if (kind) return kind;
    }
  }

  // 4. 仍无法判定：video/* 归视频轨，其余不标注。
  if (mime?.startsWith("video/")) return "video";
  return null;
}

/**
 * `video/mp4` 是否"可疑"（可能是被服务器统一标错 MIME 的音频分片）。
 *
 * 命中任一站点规则时，`video/*` 不再直接采信，交由站点规则重新判定，
 * 避免 B 站音频分片被 MIME 误判为视频轨。非目标站点的 `video/*` 照常采信。
 */
function isAmbiguousVideoMime(input: TrackDetectInput): boolean {
  return SITE_RULES.some((rule) => rule.match(input));
}
