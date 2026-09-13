/**
 * DASH manifest（JSON 形态）结构化解析 —— 纯函数，零 DOM / chrome 依赖，可单测。
 *
 * 背景：webRequest/fetch 嗅探到的是 MSE 播放器按需请求的碎片 URL（.m4s 等），
 * 无法可靠反推「这条属于哪个清晰度 / 是视频轨还是音频轨」。真正权威的清晰度 +
 * 轨道列表来自页面本身请求的 DASH manifest。JSON 形态与标准 `<MPD>` XML
 * 都支持；无法还原成单个可下载 URL 的 SegmentTemplate 轨道仍会被保留为
 * 关联线索，但不会被误当成可下载文件。
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
  /** 视频帧率；常见 JSON 值为数字，XML 值也可能是 `60000/1001`。 */
  frameRate?: number;
  /** SegmentTemplate/SegmentList 只有轨道线索，没有单个完整文件 URL。 */
  downloadable?: boolean;
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
const MAX_TEXT_SCAN_LENGTH = 2 * 1024 * 1024;
const MAX_JSON_ROOTS = 64;

/**
 * 从页面内嵌脚本中提取平衡的 JSON 根对象/数组。
 *
 * 这里故意不执行页面脚本，也不依赖站点私有变量名；只在脚本文本中做
 * 一个有界的 JS 字符串/注释感知扫描，再把完整 JSON 交给 JSON.parse。
 */
function extractJsonRoots(text: string): string[] {
  const roots: string[] = [];
  const stack: string[] = [];
  let start = -1;
  let quote: '"' | "'" | "`" | null = null;
  let escaped = false;
  let lineComment = false;
  let blockComment = false;

  for (let index = 0; index < text.length; index += 1) {
    const current = text[index];
    const next = text[index + 1];

    if (lineComment) {
      if (current === '\n' || current === '\r') lineComment = false;
      continue;
    }
    if (blockComment) {
      if (current === '*' && next === '/') {
        blockComment = false;
        index += 1;
      }
      continue;
    }
    if (quote) {
      if (escaped) {
        escaped = false;
      } else if (current === '\\') {
        escaped = true;
      } else if (current === quote) {
        quote = null;
      }
      continue;
    }

    if (current === '/' && next === '/') {
      lineComment = true;
      index += 1;
      continue;
    }
    if (current === '/' && next === '*') {
      blockComment = true;
      index += 1;
      continue;
    }
    if (current === '"' || current === "'" || current === '`') {
      quote = current;
      continue;
    }
    if (current === '{' || current === '[') {
      if (stack.length === 0) start = index;
      stack.push(current);
      continue;
    }
    if (current !== '}' && current !== ']') continue;
    if (stack.length === 0) continue;

    const opening = stack[stack.length - 1];
    const matches = (opening === '{' && current === '}')
      || (opening === '[' && current === ']');
    if (!matches) {
      stack.length = 0;
      start = -1;
      continue;
    }

    stack.pop();
    if (stack.length === 0 && start >= 0) {
      roots.push(text.slice(start, index + 1));
      start = -1;
      if (roots.length >= MAX_JSON_ROOTS) break;
    }
  }

  return roots;
}

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
  const frameRate = parseFrameRate(o.frameRate ?? o.frame_rate ?? o.framerate ?? o.fps);
  if (frameRate !== undefined) track.frameRate = frameRate;
  return track;
}

function parseFrameRate(value: unknown): number | undefined {
  if (typeof value === "number") return Number.isFinite(value) && value > 0 ? value : undefined;
  if (typeof value !== "string") return undefined;
  const raw = value.trim().replace(/fps$/i, "");
  if (!raw) return undefined;
  const parts = raw.split("/");
  const numerator = Number(parts[0]);
  const denominator = parts.length > 1 ? Number(parts[1]) : 1;
  if (!Number.isFinite(numerator) || !Number.isFinite(denominator) || denominator <= 0) {
    return undefined;
  }
  const frameRate = numerator / denominator;
  return frameRate > 0 ? frameRate : undefined;
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

/**
 * 从响应/内嵌脚本文本中寻找 JSON 形态 DASH 清单。
 *
 * 响应拦截可能在页面播放器初始化之后才注入；扫描内嵌状态可以补上这类
 * 已经存在于 DOM 的清单，同时仍然只接受结构化 video[]/audio[] 轨道。
 */
export function parseDashJsonText(text: string, baseUrl: string): DashManifest | null {
  if (!text || text.length > MAX_TEXT_SCAN_LENGTH) return null;

  let audioOnly: DashManifest | null = null;
  for (const rootText of extractJsonRoots(text)) {
    let root: unknown;
    try {
      root = JSON.parse(rootText);
    } catch {
      continue;
    }
    const manifest = parseDashJson(root, baseUrl);
    if (!manifest) continue;
    if (manifest.video.length > 0) return manifest;
    audioOnly ||= manifest;
  }
  return audioOnly;
}

// ===== 标准 MPD XML 解析 =====

/** XML 解析只保留 DASH 所需的轻量节点，避免给扩展增加 DOM/XML 依赖。 */
interface XmlNode {
  name: string;
  attributes: Record<string, string>;
  children: XmlNode[];
  text: string;
}

function xmlLocalName(name: string): string {
  const colon = name.indexOf(":");
  return (colon >= 0 ? name.slice(colon + 1) : name).toLowerCase();
}

function decodeXmlEntities(value: string): string {
  return value.replace(
    /&(#x[0-9a-f]+|#\d+|amp|lt|gt|quot|apos);/gi,
    (whole, entity: string) => {
      const lower = entity.toLowerCase();
      if (lower === "amp") return "&";
      if (lower === "lt") return "<";
      if (lower === "gt") return ">";
      if (lower === "quot") return '"';
      if (lower === "apos") return "'";
      const code = lower.startsWith("#x")
        ? Number.parseInt(lower.slice(2), 16)
        : Number.parseInt(lower.slice(1), 10);
      return Number.isFinite(code) && code >= 0 && code <= 0x10ffff
        ? String.fromCodePoint(code)
        : whole;
    },
  );
}

function parseXmlAttributes(source: string): Record<string, string> {
  const attributes: Record<string, string> = {};
  const pattern = /([A-Za-z_:][\w:.-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;
  for (const match of source.matchAll(pattern)) {
    const value = match[2] ?? match[3];
    if (value !== undefined) {
      attributes[xmlLocalName(match[1])] = decodeXmlEntities(value);
    }
  }
  return attributes;
}

/** 有界、容错的 XML token 扫描器；解析失败返回 null，不影响页面请求。 */
function parseXmlDocument(text: string): XmlNode | null {
  const documentNode: XmlNode = {
    name: "#document",
    attributes: {},
    children: [],
    text: "",
  };
  const stack: XmlNode[] = [documentNode];
  const tokenPattern = /<!--[\s\S]*?-->|<!\[CDATA\[[\s\S]*?\]\]>|<\?[\s\S]*?\?>|<![^>]*>|<[^>]*>|[^<]+/g;

  for (const match of text.matchAll(tokenPattern)) {
    const token = match[0];
    if (token.startsWith("<!--") || token.startsWith("<?") || token.startsWith("<!")) {
      if (token.startsWith("<![CDATA[")) {
        stack[stack.length - 1].text += token.slice(9, -3);
      }
      continue;
    }
    if (!token.startsWith("<")) {
      stack[stack.length - 1].text += token;
      continue;
    }
    if (token.startsWith("</")) {
      if (stack.length > 1) stack.pop();
      continue;
    }

    const selfClosing = /\/\s*>$/.test(token);
    const inner = token.slice(1, selfClosing ? -2 : -1).trim();
    const nameMatch = /^([A-Za-z_:][\w:.-]*)/.exec(inner);
    if (!nameMatch) continue;
    const node: XmlNode = {
      name: xmlLocalName(nameMatch[1]),
      attributes: parseXmlAttributes(inner.slice(nameMatch[0].length)),
      children: [],
      text: "",
    };
    stack[stack.length - 1].children.push(node);
    if (!selfClosing) stack.push(node);
  }

  return documentNode.children.find((node) => node.name === "mpd") || null;
}

function directChild(node: XmlNode, name: string): XmlNode | undefined {
  return node.children.find((child) => child.name === name);
}

function directText(node: XmlNode, name: string): string | undefined {
  const child = directChild(node, name);
  const value = child?.text.trim();
  return value ? decodeXmlEntities(value) : undefined;
}

function resolveXmlBase(node: XmlNode, inherited: string): string {
  const raw = directText(node, "baseurl");
  if (!raw) return inherited;
  try {
    return new URL(raw, inherited).href;
  } catch {
    return inherited;
  }
}

function childSegmentTemplate(node: XmlNode): XmlNode | undefined {
  return directChild(node, "segmenttemplate") || directChild(node, "segmentlist");
}

function templateTrackUrl(template: XmlNode, baseUrl: string): string | null {
  const segmentUrl = directChild(template, "segmenturl");
  const raw = template.attributes.media || segmentUrl?.attributes.media;
  if (!raw) return null;
  // 仅用于轨道/分片关联的稳定线索，绝不把未展开的 $Number$ 当真实 URL 下载。
  const placeholder = raw.replace(/\$[^$]*\$/g, "__fluxdown_segment__");
  try {
    return new URL(placeholder, baseUrl).href;
  } catch {
    return null;
  }
}

function finiteNumber(value: string | undefined): number | undefined {
  if (!value) return undefined;
  const number = Number(value);
  return Number.isFinite(number) ? number : undefined;
}

function isXmlVideo(
  mimeType: string | undefined,
  codecs: string | undefined,
  node: XmlNode,
): boolean {
  if (mimeType?.toLowerCase().startsWith("video/")) return true;
  if (codecs && isVideoCodec(codecs)) return true;
  return node.attributes.height !== undefined || node.attributes.width !== undefined;
}

function isXmlAudio(
  mimeType: string | undefined,
  codecs: string | undefined,
): boolean {
  if (mimeType?.toLowerCase().startsWith("audio/")) return true;
  return !!codecs && isAudioCodec(codecs) && !isVideoCodec(codecs);
}

function sortXmlTracks(video: DashTrack[], audio: DashTrack[]): DashManifest | null {
  if (video.length === 0 && audio.length === 0) return null;
  return {
    video: video.sort((a, b) => rankVideo(b) - rankVideo(a)),
    audio: audio.sort((a, b) => (b.bandwidth ?? 0) - (a.bandwidth ?? 0)),
  };
}

/**
 * 解析标准 MPEG-DASH MPD XML。
 *
 * 直接 Representation/BaseURL 可作为下载轨道；使用 SegmentTemplate 或
 * SegmentList 的轨道只标记 `downloadable:false`，用于把实际 m4s 分片归入
 * 正确清单，避免生成大量“未找到播放清单”假阳性卡片。
 */
export function parseDashXml(text: string, baseUrl: string): DashManifest | null {
  if (!text || text.length > MAX_TEXT_SCAN_LENGTH) return null;
  try {
    const root = parseXmlDocument(text);
    if (!root) return null;

    const video: DashTrack[] = [];
    const audio: DashTrack[] = [];

    const visit = (
      node: XmlNode,
      inheritedBase: string,
      inheritedTemplate?: XmlNode,
    ): void => {
      const nodeBase = resolveXmlBase(node, inheritedBase);
      const ownTemplate = childSegmentTemplate(node) || inheritedTemplate;

      if (node.name === "adaptationset") {
        const adaptationMime = node.attributes.mimetype ||
          (node.attributes.contenttype === "video" ? "video/mp4" :
            node.attributes.contenttype === "audio" ? "audio/mp4" : undefined);
        const adaptationCodecs = node.attributes.codecs;
        for (const representation of node.children.filter(
          (child) => child.name === "representation",
        )) {
          const representationBase = resolveXmlBase(representation, nodeBase);
          const mimeType = representation.attributes.mimetype || adaptationMime;
          const codecs = representation.attributes.codecs || adaptationCodecs;
          const template = childSegmentTemplate(representation) || ownTemplate;
          const templateUrl = template ? templateTrackUrl(template, representationBase) : null;
          const hasTemplate = !!template;
          const rawUrl = hasTemplate ? templateUrl : representationBase;
          if (!rawUrl || rawUrl.endsWith("/")) continue;

          const track: DashTrack = {
            url: rawUrl,
            mimeType,
            codecs,
            bandwidth: finiteNumber(representation.attributes.bandwidth),
            width: finiteNumber(representation.attributes.width),
            height: finiteNumber(representation.attributes.height),
            id: representation.attributes.id,
            frameRate: parseFrameRate(representation.attributes.framerate),
            downloadable: !hasTemplate,
          };
          if (isXmlVideo(mimeType, codecs, representation)) video.push(track);
          else if (isXmlAudio(mimeType, codecs)) audio.push(track);
        }
      }

      for (const child of node.children) {
        if (node.name === "adaptationset" && child.name === "representation") continue;
        visit(child, nodeBase, ownTemplate);
      }
    };

    visit(root, baseUrl);
    return sortXmlTracks(video, audio);
  } catch {
    return null;
  }
}

/** 统一入口：按响应前缀选择 XML MPD 或 JSON DASH。 */
export function parseDashManifestText(text: string, baseUrl: string): DashManifest | null {
  const head = text.trimStart().slice(0, 300).toUpperCase();
  if (head.includes("<MPD")) return parseDashXml(text, baseUrl);
  return parseDashJsonText(text, baseUrl);
}
