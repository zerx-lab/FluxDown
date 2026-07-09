/**
 * DASH manifest（JSON 形态）结构化解析 —— 纯函数，零 DOM / chrome 依赖，可单测。
 *
 * 背景：webRequest/fetch 嗅探到的是 MSE 播放器按需请求的碎片 URL（.m4s 等），
 * 无法可靠反推「这条属于哪个清晰度 / 是视频轨还是音频轨」。真正权威的清晰度 +
 * 轨道列表来自页面本身请求的 DASH manifest（本模块只处理 JSON 形态；标准
 * `<MPD>` XML 见 TODO，暂回退到调用方的碎片分组兜底）。
 *
 * 行业通用铁律：只识别「结构特征」，不解析任何站点私有字段名 / 不做
 * `if (url.includes("xxx"))` 式站点特判。结构特征 = 「JSON 中存在 video[]
 * 和/或 audio[] 数组，且数组元素带 DASH 标准字段（baseUrl + bandwidth/
 * codecs/id/width/height 之一）」，这是多家 DASH JSON API 的通用共享约定，
 * 不专属任何单一站点。
 */

/** 一条 DASH 轨道（视频或音频档位）。 */
export interface DashTrack {
  /** baseUrl 绝对化后的完整 URL */
  url: string;
  /** e.g. "video/mp4" | "audio/mp4" */
  mimeType?: string;
  /** e.g. "avc1.640032" | "mp4a.40.2" */
  codecs?: string;
  /** bps */
  bandwidth?: number;
  width?: number;
  height?: number;
  id?: string | number;
}

/** 一份 manifest 里的全部轨道，按清晰度/码率降序排列。 */
export interface DashManifest {
  /** 各清晰度视频轨（按 height 降序，height 缺失时按 bandwidth 降序） */
  video: DashTrack[];
  /** 各档音频轨（按 bandwidth 降序） */
  audio: DashTrack[];
}

// ===== 递归扫描预算（仿 media-sniff.ts scanForMediaUrls，防御超大 JSON） =====
const MAX_SCAN_DEPTH = 12;
const MAX_SCAN_NODES = 3000;

function isVideoCodec(codecs: string): boolean {
  const lower = codecs.toLowerCase();
  return (
    lower.includes("avc") ||
    lower.includes("hev") ||
    lower.includes("hvc") ||
    lower.includes("av01") ||
    lower.includes("vp9") ||
    lower.includes("vp09")
  );
}

function isAudioCodec(codecs: string): boolean {
  const lower = codecs.toLowerCase();
  return (
    lower.includes("mp4a") ||
    lower.includes("opus") ||
    lower.includes("ac-3") ||
    lower.includes("ec-3") ||
    lower.includes("flac")
  );
}

/** 数组元素是否具备「DASH 轨道」的最小结构特征：有 URL + 至少一个 DASH 特征字段。 */
function isTrackLike(item: unknown): item is Record<string, unknown> {
  if (!item || typeof item !== "object") return false;
  const o = item as Record<string, unknown>;
  const hasUrl =
    typeof o.baseUrl === "string" ||
    typeof o.base_url === "string" ||
    typeof o.url === "string";
  if (!hasUrl) return false;
  return (
    typeof o.bandwidth === "number" ||
    typeof o.codecs === "string" ||
    o.id !== undefined ||
    typeof o.width === "number" ||
    typeof o.height === "number"
  );
}

/** 把一个候选元素转成 DashTrack；不满足最小结构特征或 URL 无法绝对化则返回 null。 */
function toTrack(item: unknown, baseUrl: string): DashTrack | null {
  if (!isTrackLike(item)) return null;
  const o = item;
  const rawUrl = (o.baseUrl ?? o.base_url ?? o.url) as string;

  let abs: string;
  try {
    abs = new URL(rawUrl, baseUrl).href;
  } catch {
    return null;
  }

  const track: DashTrack = { url: abs };
  if (typeof o.mimeType === "string") track.mimeType = o.mimeType;
  if (typeof o.codecs === "string") track.codecs = o.codecs;
  if (typeof o.bandwidth === "number") track.bandwidth = o.bandwidth;
  if (typeof o.width === "number") track.width = o.width;
  if (typeof o.height === "number") track.height = o.height;
  if (typeof o.id === "string" || typeof o.id === "number") track.id = o.id;
  return track;
}

/** 轨道的 mimeType/codecs 是否明确与"视频"矛盾（用于过滤 video[] 数组里的误入项）。 */
function contradictsVideo(t: DashTrack): boolean {
  const mime = t.mimeType?.toLowerCase();
  if (mime?.startsWith("audio/")) return true;
  if (t.codecs && isAudioCodec(t.codecs) && !isVideoCodec(t.codecs)) return true;
  return false;
}

/** 轨道的 mimeType/codecs 是否明确与"音频"矛盾（用于过滤 audio[] 数组里的误入项）。 */
function contradictsAudio(t: DashTrack): boolean {
  const mime = t.mimeType?.toLowerCase();
  if (mime?.startsWith("video/")) return true;
  if (t.codecs && isVideoCodec(t.codecs) && !isAudioCodec(t.codecs)) return true;
  return false;
}

interface DashArrays {
  video?: unknown[];
  audio?: unknown[];
}

/** 深度优先搜索：找到第一个同时/单独含有效 video[]/audio[] 轨道数组的节点。 */
function findDashNode(
  value: unknown,
  depth: number,
  budget: { count: number },
): DashArrays | null {
  if (depth > MAX_SCAN_DEPTH) return null;
  if (++budget.count > MAX_SCAN_NODES) return null;
  if (!value || typeof value !== "object") return null;

  if (!Array.isArray(value)) {
    const obj = value as Record<string, unknown>;
    const video = Array.isArray(obj.video) ? obj.video : undefined;
    const audio = Array.isArray(obj.audio) ? obj.audio : undefined;
    if ((video && video.some(isTrackLike)) || (audio && audio.some(isTrackLike))) {
      return { video, audio };
    }
  }

  const children: unknown[] = Array.isArray(value)
    ? value
    : Object.values(value as Record<string, unknown>);
  for (const child of children) {
    if (budget.count > MAX_SCAN_NODES) return null;
    const found = findDashNode(child, depth + 1, budget);
    if (found) return found;
  }
  return null;
}

function rankVideo(t: DashTrack): number {
  return (t.height ?? 0) * 1_000_000 + (t.bandwidth ?? 0);
}

/**
 * 从已解析的 JSON 对象中识别标准 DASH 结构（video[]/audio[] 轨道数组）。
 * 非 DASH 结构、解析异常、或识别出的轨道全部为空 → 返回 null（调用方回退碎片分组）。
 */
export function parseDashJson(root: unknown, baseUrl: string): DashManifest | null {
  try {
    const node = findDashNode(root, 0, { count: 0 });
    if (!node) return null;

    const video = (node.video ?? [])
      .map((item) => toTrack(item, baseUrl))
      .filter((t): t is DashTrack => t !== null && !contradictsVideo(t))
      .sort((a, b) => rankVideo(b) - rankVideo(a));

    const audio = (node.audio ?? [])
      .map((item) => toTrack(item, baseUrl))
      .filter((t): t is DashTrack => t !== null && !contradictsAudio(t))
      .sort((a, b) => (b.bandwidth ?? 0) - (a.bandwidth ?? 0));

    if (video.length === 0 && audio.length === 0) return null;
    return { video, audio };
  } catch {
    // 解析异常绝不冒泡（可能是页面响应体畸形 JSON 结构）
    return null;
  }
}
