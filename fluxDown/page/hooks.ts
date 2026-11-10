/**
 * MAIN world 钩子：fetch / XHR 嗅探、MSE 与响应来源归因、history 事件。
 *
 * 运行在页面自己的 JS 环境里（所有浏览器均由 page-hooks.content.ts 以 world: MAIN 声明式注入），
 * 通过 {@link ./bridge} 的 CustomEvent 与 ISOLATED world 的 sensor 通信，detail 一律是 JSON 字符串。
 *
 * 铁律：所有钩子异常都必须走原函数，页面可见行为不变（#329 #337）。
 *
 * cat-catch(GPL-3.0) 嗅探思路的复用：JSON 内嵌媒体 URL 深扫见 utils/media-sniff.ts 归属说明。
 * 响应体流式部分读取与 MSE 归因为 FluxDown 自研。
 */
import { compileBundled, createClassifier } from "@/rules/engine";
import {
  asciiLower,
  compileGlob,
  hostMatchesLower,
  parseHostPattern,
  type GlobMatcher,
  type HostPattern,
} from "@/rules/glob";
import { REGEX_INPUT_CAP } from "@/rules/regex-safety";
import { parseDashJson, parseDashXml } from "@/utils/dash-manifest";
import type { DashManifest } from "@/utils/dash-manifest";
import {
  isHttpUrl,
  isSkippableCt,
  looksLikeJson,
  scanForMediaUrls,
  sniffManifestMagic,
} from "@/utils/media-sniff";
import { parseHlsPlaylist } from "@/utils/hls-playlist";
import { isStreamingUrl } from "@/utils/resource-types";
import {
  EV_CONFIG,
  EV_MEDIA,
  EV_SHADOW,
  EV_REPORT,
  HOOKS_PROTOCOL,
  decodeDetail,
  encodeDetail,
  type HookConfig,
  type MediaDetail,
  type PageConfig,
  type ReportDetail,
  type SourceBufferBinding,
} from "./bridge";
import { wrapFetch } from "./fetch-hook";
import { RecentResponses, hasAnyTag, propagate, tag, tagsOf } from "./provenance";

/** 被包装的原函数（this 由调用点决定）。 */
type Orig = (this: unknown, ...args: unknown[]) => unknown;
/** 包装后的实现：参数类型由各钩子自行声明。 */
type Impl = (...args: never[]) => unknown;

/**
 * 页面 window 上的安装标记。协议 ≥ 1 为 `{ protocol, hid }`；旧版本（协议 0）是布尔 `true`。
 * 已有任何标记都不再安装：原型方法不能安全地二次包装（会重复计数 / 重复打标）。
 */
const HOOK_FLAG = "__fluxdown_hooks__";

/** 每个 MediaSource 会话可重放的上限（sensor 重注入时只需要当前仍挂在元素上的那些）。 */
const MAX_REPLAY_MS = 64;

/**
 * 不装钩子的帧：Cloudflare 质询 / Turnstile 帧（challenges.cloudflare.com）没有可下载资源；其脚本会
 * fetch 被本帧 CSP 拦下的地址（如 `data:,`），经包装后的 fetch 发出时控制台报错的来源脚本是本扩展，
 * 会被记进扩展的错误列表。改写原生 fetch / XHR 也可能干扰质询的环境检测。
 * 声明式注入与补注入（background/probe.ts，allFrames）都经过这里，所以在运行期判断。
 */
const CHALLENGE_FRAME_HOST = "challenges.cloudflare.com";

function makeHookId(): string {
  try {
    return crypto.randomUUID().slice(0, 8);
  } catch {
    return Math.random().toString(36).slice(2, 10).padEnd(8, "0");
  }
}

// 读体预算：仅对「非媒体 CT + 非流 URL + http + 非可跳过 CT + 体积可控」的响应读体，
// 避免大文件 / 流式响应被 clone-read 强制整体缓冲拖爆内存。
const MAX_SNIFF_CL = 2 * 1024 * 1024; // Content-Length 门槛：>2MB 不读
const MAX_SNIFF_READ = 512 * 1024; // 累积读上限：清单 / JSON API 均远小于此
const MAX_WATCH_BODY = 1024 * 1024; // body watcher 读体上限（超出则放弃，不上报半截 JSON）

const MEDIA_EXT_RE = /\.(?:m4s|ts|mp4|m4a|webm|aac)(?:[?#]|$)/i;
const NON_MEDIA_CT_RE = /^(?:text\/|application\/(?:json|javascript|x-javascript|xml)|image\/|font\/)|json/i;
/** 页面自己用 octet-stream 一类通用二进制 CT 传输的下载物（与 media 无关的兜底）。 */
const GENERIC_BINARY_CT = new Set([
  "application/octet-stream",
  "application/x-download",
  "application/force-download",
]);

export function start(): void {
  const w = window as unknown as Record<string, unknown>;
  if (w[HOOK_FLAG] || location.hostname === CHALLENGE_FRAME_HOST) return;
  const hid = makeHookId();
  // document_start 注入时文档仍在 loading；之后才装上的钩子（扩展安装 / 更新后补注入）
  // 漏掉了已建立的 MediaSource ↔ 元素关联，sensor 据此把无证据的播放元素标为「需刷新」。
  const late = document.readyState !== "loading";
  w[HOOK_FLAG] = Object.freeze({ protocol: HOOKS_PROTOCOL, hid });

  // 收到 PageConfig 前用随包规则；之后换成用户实际生效（含覆盖 / 吊销 / 本地与插件规则）的 classifier。
  let classify = compileBundled().classify;
  const recent = new RecentResponses();

  // ===== 配置（EV_CONFIG） =====

  interface Watcher {
    ruleId: string;
    urls?: GlobMatcher[];
    hosts?: HostPattern[];
    path?: RegExp;
  }

  /** 收到配置前按 sniff = true 处理，但不读取任何 body watcher 的正文。 */
  let config: PageConfig | null = null;
  let watchers: Watcher[] = [];

  function applyConfig(next: PageConfig): void {
    config = next;
    if (Array.isArray(next.classify)) {
      try {
        classify = createClassifier(next.classify);
      } catch {
        /* 形状不对：沿用当前 classifier */
      }
    }
    const compiled: Watcher[] = [];
    for (const b of next.bodyWatch) {
      const watcher: Watcher = { ruleId: b.ruleId };
      if (b.url?.length) watcher.urls = b.url.map(compileGlob);
      if (b.host?.length) {
        watcher.hosts = b.host
          .map((h) => parseHostPattern(h))
          .filter((p): p is HostPattern => p !== null);
      }
      if (b.pathRegex) {
        try {
          watcher.path = new RegExp(b.pathRegex, "i");
        } catch {
          continue;
        }
      }
      if (!watcher.urls && !watcher.hosts && !watcher.path) continue;
      compiled.push(watcher);
    }
    watchers = compiled;
  }

  /** 当前 sensor 会话与其换页计数；导航代数 configEpoch 由本侧持有。 */
  let configSid = "";
  let configNav = 0;

  document.addEventListener(EV_CONFIG, (event) => {
    try {
      const cfg = decodeDetail((event as CustomEvent).detail) as Partial<HookConfig> | null;
      if (!cfg || typeof cfg !== "object" || typeof cfg.sniff !== "boolean") return;
      const sid = typeof cfg.sid === "string" ? cfg.sid : "";
      const nav = typeof cfg.nav === "number" ? cfg.nav : 0;
      let hello = false;
      if (sid !== configSid) {
        // 新的 sensor（首次加载或重注入）：只握手，不换代——SourceBuffer 证据仍属于当前页面。
        // 已通知集合清空，让新 sensor 能收到之后再次出现的地址（下游按 dedupeKey 去重）。
        configSid = sid;
        configNav = nav;
        hello = sid !== "";
        notified.clear();
        lastHlsReport.clear();
      } else if (nav > configNav) {
        // 同一 sensor 判定 SPA 换页：background 已清空该页状态，证据与去重集合换代
        configNav = nav;
        configEpoch++;
        notified.clear();
        lastHlsReport.clear();
      }
      applyConfig(cfg as PageConfig);
      if (hello) {
        dispatchReport({ t: "hooks", protocol: HOOKS_PROTOCOL, hid, late });
        replayMedia();
      }
    } catch {
      /* 配置异常保持原状 */
    }
  });

  const sniffOn = (): boolean => config === null || config.sniff;
  const mediaEventsOn = (): boolean => config === null || config.sniff || config.floatButton;

  // ===== 上报 =====

  function dispatchReport(detail: ReportDetail): void {
    try {
      document.dispatchEvent(new CustomEvent(EV_REPORT, { detail: encodeDetail(detail) }));
    } catch {
      /* 上报失败无妨 */
    }
  }

  /** 已通知过的 URL（防重复）；整体清空而非删前半，下游仍有去重兜底。 */
  const notified = new Set<string>();
  let configEpoch = 0;

  function once(key: string): boolean {
    if (notified.has(key)) return false;
    notified.add(key);
    if (notified.size > 500) notified.clear();
    return true;
  }

  /** via `json`：URL 出自响应 JSON 深扫（页面不一定请求），其余为页面实际发出的请求。 */
  function notify(via: "fetch" | "xhr" | "json", url: string, mime?: string, size?: number): void {
    if (!sniffOn() || !url) return;
    if (!once(`${via}:${url}`)) return;
    dispatchReport({ t: "resource", url, mime, size, via });
  }

  function streamMime(url: string): string | undefined {
    const lower = url.toLowerCase();
    if (lower.includes(".m3u8")) return "application/vnd.apple.mpegurl";
    if (lower.includes(".mpd")) return "application/dash+xml";
    return undefined;
  }

  /** 响应 CT 是否值得上报为可下载资源（图片不报：由 webRequest 路径按 sniffImages 处理）。 */
  function isReportableCt(ct: string, url: string): boolean {
    const mime = ct.split(";")[0].trim().toLowerCase();
    if (!mime) return false;
    if (GENERIC_BINARY_CT.has(mime)) return true;
    const type = classify(url, mime);
    return type !== "other" && type !== "image";
  }

  /** 响应 CT 是否是媒体 / 流清单：交给规则判定（不在此处内置 MIME 列表）。 */
  function isMediaCt(ct: string): boolean {
    const type = classify("", ct);
    return type === "video" || type === "audio" || type === "stream";
  }

  /** DASH 清单通常是 application/dash+xml，也可能是流 URL + text/xml/JSON。 */
  function isDashManifestResponse(ct: string, url: string): boolean {
    if (!isHttpUrl(url) || isSkippableCt(ct)) return false;
    const lower = ct.toLowerCase();
    if (lower.includes("dash+xml") || lower.startsWith("text/xml") || lower.startsWith("application/xml")) {
      return true;
    }
    if (lower.startsWith("text/html")) return false;
    // 通用 /playlist API 不当作 DASH；JSON DASH 由下方通用 JSON 分支兜底。
    return (
      /(?:\.mpd|\/manifest(?:\/|$))/i.test(url) &&
      !lower.startsWith("video/") &&
      !lower.startsWith("audio/") &&
      !lower.includes("mpegurl")
    );
  }

  /**
   * HLS 播放列表：mpegurl 类 CT（含常见的 `audio/mpegurl`、`audio/x-mpegurl`）或 .m3u8 / .m3u。
   * 正文很小，读出来建立主列表 / 子列表 / 分片的关联。
   */
  function isHlsPlaylistResponse(ct: string, url: string): boolean {
    if (!isHttpUrl(url) || isSkippableCt(ct)) return false;
    const lower = ct.toLowerCase();
    if (lower.includes("mpegurl")) return true;
    if (lower.startsWith("video/") || lower.startsWith("audio/")) return false;
    return /\.m3u8?(?:[?#]|$)/i.test(url);
  }

  function shouldReadBodyForSniff(ct: string, url: string): boolean {
    // 清单（.mpd / .m3u8 / 清单 MIME）始终读取，供魔数检测与关联解析
    if (isDashManifestResponse(ct, url) || isHlsPlaylistResponse(ct, url)) return true;
    // 媒体分片不读正文：每个 .m4s/.ts 都 clone 并解码代价过高。无媒体扩展名的
    // octet-stream 仍读（保持旧行为：部分站点以 octet-stream 下发无扩展名的 m3u8）。
    const lower = ct.split(";")[0].trim().toLowerCase();
    if (MEDIA_EXT_RE.test(url) || lower.startsWith("video/") || lower.startsWith("audio/")) {
      return false;
    }
    return !isMediaCt(ct) && !isStreamingUrl(url) && isHttpUrl(url) && !isSkippableCt(ct);
  }

  // ===== 响应体清单 / JSON 深扫 =====

  function emitManifest(manifest: DashManifest | null, manifestUrl: string): void {
    if (!manifest) return;
    dispatchReport({ t: "manifest", manifestUrl, manifest });
  }

  /** 直播播放列表会被反复轮询：关联信息没变就不重复上报。 */
  const lastHlsReport = new Map<string, string>();
  function emitHlsPlaylist(text: string, url: string): void {
    const playlist = parseHlsPlaylist(text, url);
    if (!playlist) return;
    const signature = JSON.stringify(playlist);
    if (lastHlsReport.get(url) === signature) return;
    if (lastHlsReport.size >= 256) lastHlsReport.clear();
    lastHlsReport.set(url, signature);
    dispatchReport({ t: "hls", url, playlist });
  }

  function sniffTextForMedia(text: string, url: string, ct: string, via: "fetch" | "xhr"): void {
    try {
      if (!text) return;
      const magic = sniffManifestMagic(text.slice(0, 512));
      if (magic) {
        if (magic === "dash-manifest") emitManifest(parseDashXml(text, url), url);
        notify(via, url, magic === "hls-manifest" ? "application/vnd.apple.mpegurl" : "application/dash+xml");
        if (magic === "hls-manifest") emitHlsPlaylist(text, url);
        return;
      }
      if (text.length <= MAX_SNIFF_CL && looksLikeJson(ct, text)) {
        const obj: unknown = JSON.parse(text);
        emitManifest(parseDashJson(obj, url), url);
        for (const mediaUrl of scanForMediaUrls(obj, url, classify)) notify("json", mediaUrl);
      }
    } catch {
      /* 嗅探异常绝不冒泡 */
    }
  }

  /** 有界累积读取（调用方传入 clone），随后解码；complete = 是否读到流尾。 */
  async function readBounded(
    response: Response,
    maxBytes: number,
    maxReads: number,
  ): Promise<{ text: string; complete: boolean } | null> {
    const body = response.body;
    if (!body) return null;
    const reader = body.getReader();
    const chunks: Uint8Array[] = [];
    let total = 0;
    let complete = false;
    try {
      for (let i = 0; i < maxReads && total < maxBytes; i++) {
        const { done, value } = await reader.read();
        if (done) {
          complete = true;
          break;
        }
        if (value) {
          chunks.push(value);
          total += value.length;
        }
      }
    } finally {
      try {
        await reader.cancel();
      } catch {
        /* 取消失败无妨 */
      }
    }
    const buf = new Uint8Array(total);
    let off = 0;
    for (const c of chunks) {
      buf.set(c, off);
      off += c.length;
    }
    return { text: new TextDecoder().decode(buf), complete };
  }

  async function readBodyAndSniff(response: Response, url: string, ct: string): Promise<void> {
    const r = await readBounded(response, MAX_SNIFF_READ, 8);
    if (r) sniffTextForMedia(r.text, url, ct, "fetch");
  }

  // ===== body watcher（声明式规则驱动，只在收到配置后生效） =====

  function watchedRuleIds(url: string): string[] {
    if (watchers.length === 0) return [];
    let parsed: URL;
    try {
      parsed = new URL(url, location.href);
    } catch {
      return [];
    }
    const full = asciiLower(parsed.href);
    const host = parsed.hostname.toLowerCase().replace(/\.$/, "");
    const ids: string[] = [];
    for (const wt of watchers) {
      if (wt.urls && !wt.urls.some((g) => g(full))) continue;
      if (wt.hosts && !wt.hosts.some((p) => hostMatchesLower(host, p))) continue;
      // 与引擎一致：超长路径视为不匹配，避免恶意正则在页面线程上回溯。
      if (wt.path && !(parsed.pathname.length <= REGEX_INPUT_CAP && wt.path.test(parsed.pathname))) continue;
      ids.push(wt.ruleId);
    }
    return ids;
  }

  function reportBody(url: string, ruleIds: string[], text: string): void {
    if (!text || text.length > MAX_WATCH_BODY) return;
    dispatchReport({ t: "body", url, ruleIds, text });
  }

  // ===== 响应来源打标 / MSE 归因 =====

  interface SbInfo {
    ms: MsInfo;
    sbId: number;
    mime: string;
    kind: SourceBufferBinding["kind"];
    exact: string[];
    temporal: Map<string, number>;
    /** 证据所属的导航代数：SPA 换页后复用的 SourceBuffer 不能再把上一页的 URL 当作当前证据。 */
    epoch: number;
  }
  interface MsInfo {
    id: number;
    sbs: SbInfo[];
    el: WeakRef<HTMLMediaElement> | null;
    lastSent: number;
    timer: number | undefined;
  }

  let nextMsId = 1;
  let nextSbId = 1;
  const msOf = new WeakMap<object, MsInfo>();
  const msByObjectUrl = new Map<string, MsInfo>();
  const msOfElement = new WeakMap<object, MsInfo>();
  /** 最近创建的 MediaSource 会话（插入序，有界）：sensor 重注入后据此重放仍在播放的会话证据。 */
  const msRegistry = new Set<MsInfo>();
  const hookedElements = new WeakSet<object>();
  const sbInfo = new WeakMap<object, SbInfo>();
  /** fetch 流 → 来源；reader → 来源（含是否已记入 recent）。 */
  const streamSource = new WeakMap<object, { url: string; media: boolean }>();
  const readerSource = new WeakMap<object, { url: string; media: boolean; pushed: boolean }>();

  function isMediaResponse(url: string, ct: string): boolean {
    const lower = ct.toLowerCase();
    return lower.startsWith("video/") || lower.startsWith("audio/") || MEDIA_EXT_RE.test(url);
  }

  /** 是否值得打标：明确的文本 / JSON / 图片 / 字体不打。 */
  function isTagWorthy(ct: string): boolean {
    return !ct || !NON_MEDIA_CT_RE.test(ct);
  }

  function tagResult(result: unknown, url: string, ct: string, byteLength: number): void {
    if (!url || !isTagWorthy(ct)) return;
    tag(result, url);
    if (isMediaResponse(url, ct)) recent.push(url, byteLength, Date.now());
  }

  function getMsInfo(ms: object): MsInfo {
    let info = msOf.get(ms);
    if (!info) {
      info = { id: nextMsId++, sbs: [], el: null, lastSent: 0, timer: undefined };
      msOf.set(ms, info);
      msRegistry.add(info);
      if (msRegistry.size > MAX_REPLAY_MS) {
        const oldest = msRegistry.values().next().value;
        if (oldest !== undefined) msRegistry.delete(oldest);
      }
    }
    return info;
  }

  function buildDetail(ms: MsInfo, elementless: boolean): MediaDetail {
    const detail: MediaDetail = {
      msId: ms.id,
      hid,
      sourceBuffers: ms.sbs.map((sb) => {
        const current = sb.epoch === configEpoch;
        return {
          sbId: sb.sbId,
          mime: sb.mime,
          kind: sb.kind,
          exact: current ? sb.exact.slice() : [],
          temporal: current ? [...sb.temporal.entries()].map(([url, count]) => ({ url, count })) : [],
        };
      }),
    };
    if (elementless) detail.elementless = true;
    return detail;
  }

  /**
   * 新 sensor 握手后重放：只发仍是元素当前来源、且元素仍在文档里的会话。
   * 已被替换的旧会话与无元素会话不重放——前者 sensor 不需要，后者会被误归到唯一的播放元素上。
   */
  function replayMedia(): void {
    for (const ms of msRegistry) {
      const el = ms.el?.deref();
      if (el && el.isConnected && msOfElement.get(el) === ms) flushMedia(ms);
    }
  }

  function flushMedia(ms: MsInfo): void {
    ms.timer = undefined;
    ms.lastSent = Date.now();
    if (!mediaEventsOn() || ms.sbs.length === 0) return;
    try {
      const el = ms.el?.deref();
      const init = { detail: "", composed: true, bubbles: false };
      if (el) {
        init.detail = encodeDetail(buildDetail(ms, false));
        el.dispatchEvent(new CustomEvent(EV_MEDIA, init));
      } else {
        init.detail = encodeDetail(buildDetail(ms, true));
        document.dispatchEvent(new CustomEvent(EV_MEDIA, init));
      }
    } catch {
      /* 派发失败无妨 */
    }
  }

  /** 1 s 节流：首次立即发，之后合并为 trailing。 */
  function scheduleMedia(ms: MsInfo): void {
    if (ms.timer !== undefined) return;
    const wait = 1000 - (Date.now() - ms.lastSent);
    if (wait <= 0) flushMedia(ms);
    else ms.timer = window.setTimeout(() => flushMedia(ms), wait);
  }

  const MEDIA_ELEMENT_EVENTS = ["play", "playing", "pause", "emptied", "loadedmetadata"];

  function linkElement(el: HTMLMediaElement, ms: MsInfo): void {
    ms.el = new WeakRef(el);
    msOfElement.set(el, ms);
    if (!hookedElements.has(el)) {
      hookedElements.add(el);
      const refresh = (): void => {
        const cur = msOfElement.get(el);
        if (cur) flushMedia(cur);
      };
      for (const name of MEDIA_ELEMENT_EVENTS) el.addEventListener(name, refresh, { passive: true });
    }
    scheduleMedia(ms);
  }

  function isMediaElement(value: unknown): value is HTMLMediaElement {
    return typeof HTMLMediaElement !== "undefined" && value instanceof HTMLMediaElement;
  }

  function linkBySrc(el: unknown, value: unknown): void {
    if (!isMediaElement(el)) return;
    if (typeof value === "string") {
      const ms = msByObjectUrl.get(value);
      if (ms) linkElement(el, ms);
    } else if (typeof MediaSource !== "undefined" && value instanceof MediaSource) {
      linkElement(el, getMsInfo(value));
    }
  }

  function onAppend(sb: object, data: unknown): void {
    const info = sbInfo.get(sb);
    if (!info) return;
    if (info.epoch !== configEpoch) {
      info.epoch = configEpoch;
      info.exact = [];
      info.temporal = new Map();
    }
    const tags = tagsOf(data);
    if (tags && tags.size > 0) {
      for (const url of tags) {
        const idx = info.exact.indexOf(url);
        if (idx >= 0) info.exact.splice(idx, 1);
        info.exact.push(url);
      }
      if (info.exact.length > 64) info.exact.splice(0, info.exact.length - 64);
    } else {
      for (const r of recent.withinWindow(Date.now())) {
        info.temporal.set(r.url, (info.temporal.get(r.url) ?? 0) + 1);
      }
      if (info.temporal.size > 16) {
        const top = [...info.temporal.entries()].sort((a, b) => b[1] - a[1]).slice(0, 16);
        info.temporal = new Map(top);
      }
    }
    scheduleMedia(info.ms);
  }

  // ===== 安装钩子的小工具 =====

  /** 替换原型方法；make 拿到原函数返回新函数。安装失败静默跳过。 */
  function wrapMethod(proto: object | undefined, key: string, make: (orig: Orig) => Impl): void {
    try {
      if (!proto) return;
      const orig = (proto as Record<string, unknown>)[key];
      if (typeof orig !== "function") return;
      (proto as Record<string, unknown>)[key] = make(orig as Orig);
    } catch {
      /* 页面冻结了原型等情况：放弃该钩子 */
    }
  }

  function wrapSetter(proto: object | undefined, key: string, after: (self: unknown, value: unknown) => void): void {
    try {
      if (!proto) return;
      const d = Object.getOwnPropertyDescriptor(proto, key);
      if (!d?.set || !d.configurable) return;
      const origSet = d.set;
      Object.defineProperty(proto, key, {
        ...d,
        set(value: unknown) {
          origSet.call(this, value);
          try {
            after(this, value);
          } catch {
            /* 归因失败不影响页面 */
          }
        },
      });
    } catch {
      /* ignore */
    }
  }

  // ===== history：SPA 路由变化 =====

  let lastHref = location.href;
  const notifyPageUrl = (kind: "push" | "replace" | "pop"): void => {
    if (location.href === lastHref) return;
    const from = lastHref;
    lastHref = location.href;
    dispatchReport({ t: "pageUrl", kind, from });
  };
  wrapMethod(history, "pushState", (orig) =>
    function (this: unknown, ...args: unknown[]) {
      const r = orig.apply(this, args);
      notifyPageUrl("push");
      return r;
    });
  wrapMethod(history, "replaceState", (orig) =>
    function (this: unknown, ...args: unknown[]) {
      const r = orig.apply(this, args);
      notifyPageUrl("replace");
      return r;
    });
  window.addEventListener("popstate", () => notifyPageUrl("pop"));
  // hash 路由（`#/video/1` → `#/video/2`）不经过 history API；是否算换页由 page-nav 按 hash 形态判定
  window.addEventListener("hashchange", () => notifyPageUrl("push"));
  // Navigation API 拦截的同文档导航（YouTube 站内换片：navigate 事件 intercept）不经过 history API。
  // history API 调用也会触发此事件；同一次 URL 变化由 notifyPageUrl 按 lastHref 去重。
  if ("navigation" in window && window.navigation instanceof EventTarget) {
    window.navigation.addEventListener("currententrychange", (event) => {
      const type = "navigationType" in event ? event.navigationType : null;
      if (type === "push" || type === "replace") notifyPageUrl(type);
      else if (type === "traverse") notifyPageUrl("pop");
    });
  }

  // ===== shadow DOM：通知 ISOLATED 观察新挂的 shadow root 里的媒体元素 =====
  wrapMethod(Element.prototype, "attachShadow", (orig) =>
    function (this: Element, ...args: unknown[]) {
      const root = orig.apply(this, args);
      try {
        // 未连接的宿主（构造函数里 attach）由 ISOLATED 的 MutationObserver 在插入时发现
        if (this.isConnected) this.dispatchEvent(new CustomEvent(EV_SHADOW, { bubbles: true, composed: true, detail: "" }));
      } catch {
        /* 通知失败无妨 */
      }
      return root;
    });

  // ===== fetch =====

  const originalFetch = window.fetch;
  if (typeof originalFetch === "function") {
    window.fetch = wrapFetch(originalFetch, {
      baseUrl: () => document.baseURI,
      sniffOn,
      mediaEventsOn,
      onRequest: (url) => {
        if (isStreamingUrl(url)) notify("fetch", url, streamMime(url));
      },
      onResponse: onFetchResponse,
    });
  }

  /**
   * 资源总大小：206 分段响应的 Content-Length 只是本段长度，取 Content-Range 的总长；
   * 跨域响应读不到 Content-Range 时视为未知，避免把几 KB 的分段长度当成文件大小。
   */
  function fileSizeOf(status: number, contentLength: number | undefined, contentRange: string | null): number | undefined {
    if (status !== 206) return contentLength;
    const total = contentRange ? /\/\s*(\d+)\s*$/.exec(contentRange) : null;
    return total ? parseInt(total[1], 10) : undefined;
  }

  function onFetchResponse(response: Response, requestUrl: string): void {
    const ct = response.headers.get("content-type") || "";
    const clRaw = response.headers.get("content-length");
    const cl = clRaw ? parseInt(clRaw, 10) : undefined;
    const finalUrl = response.url || requestUrl;

    // 先 clone 再登记：clone 会把 response.body 换成 tee 出的新流，页面实际读取的是替换后的 body
    let sniffClone: Response | null = null;
    let watchClone: Response | null = null;
    let ids: string[] = [];
    if (sniffOn()) {
      if (ct && isReportableCt(ct, finalUrl)) {
        notify("fetch", finalUrl, ct, fileSizeOf(response.status, cl, response.headers.get("content-range")));
      }
      if (finalUrl && finalUrl !== requestUrl && isStreamingUrl(finalUrl)) {
        notify("fetch", finalUrl, ct || streamMime(finalUrl));
      }
      // 清单 / JSON 内嵌媒体 URL 深扫
      if (shouldReadBodyForSniff(ct, finalUrl) && (cl === undefined || cl <= MAX_SNIFF_CL)) {
        sniffClone = response.clone();
      }
      // 声明式 body watcher（如蓝奏云 /ajaxm.php）
      ids = watchedRuleIds(requestUrl || finalUrl);
      if (ids.length > 0 && (cl === undefined || cl <= MAX_WATCH_BODY)) watchClone = response.clone();
    }

    // 来源打标：流与后续 arrayBuffer/blob 的结果用响应 URL 归因
    if (response.body && finalUrl && isTagWorthy(ct)) {
      streamSource.set(response.body, { url: finalUrl, media: isMediaResponse(finalUrl, ct) });
    }

    if (sniffClone) readBodyAndSniff(sniffClone, finalUrl, ct).catch(() => {});
    if (watchClone) {
      const watchedUrl = requestUrl || finalUrl;
      readBounded(watchClone, MAX_WATCH_BODY, 64)
        .then((r) => {
          if (r?.complete) reportBody(watchedUrl, ids, r.text);
        })
        .catch(() => {});
    }
  }

  // ===== XMLHttpRequest =====

  const xhrUrls = new WeakMap<object, string>();

  wrapMethod(XMLHttpRequest.prototype, "open", (orig) =>
    function (this: XMLHttpRequest, ...args: unknown[]) {
      try {
        const url = args[1];
        let abs = "";
        if (typeof url === "string") abs = new URL(url, location.href).href;
        else if (url && typeof (url as URL).href === "string") abs = (url as URL).href;
        xhrUrls.set(this, abs);
        if (abs && isStreamingUrl(abs)) notify("xhr", abs, streamMime(abs));
      } catch {
        /* 嗅探异常绝不打断页面请求 */
      }
      return orig.apply(this, args);
    });

  wrapMethod(XMLHttpRequest.prototype, "send", (orig) =>
    function (this: XMLHttpRequest, ...args: unknown[]) {
      try {
        this.addEventListener("load", onXhrLoad);
      } catch {
        /* ignore */
      }
      return orig.apply(this, args);
    });

  function onXhrLoad(this: XMLHttpRequest): void {
    try {
      const url = xhrUrls.get(this);
      if (!url) return;
      const ct = this.getResponseHeader("content-type") || "";
      const cl = this.getResponseHeader("content-length");
      const responseUrl = this.responseURL || url;
      const rt = this.responseType;

      // 来源打标
      if (rt === "arraybuffer" && this.response instanceof ArrayBuffer) {
        tagResult(this.response, responseUrl, ct, this.response.byteLength);
      } else if (rt === "blob" && this.response instanceof Blob) {
        tagResult(this.response, responseUrl, ct, this.response.size);
      }

      if (!sniffOn()) return;
      if (ct && isReportableCt(ct, responseUrl)) {
        // getAllResponseHeaders 只返回可读的头，不会像 getResponseHeader 那样在控制台报 "unsafe header"
        const range = this.status === 206
          ? /^content-range:\s*(.+)$/im.exec(this.getAllResponseHeaders())?.[1] ?? null
          : null;
        notify("xhr", responseUrl, ct, fileSizeOf(this.status, cl ? parseInt(cl, 10) : undefined, range));
      }
      if (responseUrl !== url && isStreamingUrl(responseUrl)) {
        notify("xhr", responseUrl, ct || streamMime(responseUrl));
      }

      if (shouldReadBodyForSniff(ct, responseUrl)) {
        if (rt === "" || rt === "text") {
          sniffTextForMedia(this.responseText, responseUrl, ct, "xhr");
        } else if (rt === "json" && this.response && typeof this.response === "object") {
          emitManifest(parseDashJson(this.response, responseUrl), responseUrl);
          for (const mediaUrl of scanForMediaUrls(this.response, responseUrl, classify)) notify("json", mediaUrl);
        } else if (rt === "arraybuffer" && this.response instanceof ArrayBuffer) {
          const prefix = new Uint8Array(this.response).subarray(0, 512);
          const magic = sniffManifestMagic(new TextDecoder().decode(prefix));
          if (magic) {
            notify("xhr", responseUrl, magic === "hls-manifest" ? "application/vnd.apple.mpegurl" : "application/dash+xml");
          }
          if (magic === "hls-manifest" && this.response.byteLength <= MAX_SNIFF_READ) {
            emitHlsPlaylist(new TextDecoder().decode(this.response), responseUrl);
          }
        }
      }

      const ids = watchedRuleIds(url);
      if (ids.length > 0) {
        if (rt === "" || rt === "text") reportBody(url, ids, this.responseText);
        else if (rt === "json" && this.response && typeof this.response === "object") {
          reportBody(url, ids, JSON.stringify(this.response));
        }
      }
    } catch {
      /* ignore */
    }
  }

  // ===== Response / Blob / Stream 来源传播 =====

  for (const key of ["arrayBuffer", "bytes"]) {
    wrapMethod(Response.prototype, key, (orig) =>
      function (this: Response, ...args: unknown[]) {
        const p = orig.apply(this, args) as Promise<ArrayBuffer | Uint8Array>;
        try {
          const url = this.url;
          if (!url || !(p instanceof Promise)) return p;
          const ct = this.headers.get("content-type") || "";
          return p.then((buf) => {
            try {
              tagResult(buf, url, ct, buf.byteLength);
            } catch {
              /* ignore */
            }
            return buf;
          });
        } catch {
          return p;
        }
      });
  }

  wrapMethod(Response.prototype, "blob", (orig) =>
    function (this: Response, ...args: unknown[]) {
      const p = orig.apply(this, args) as Promise<Blob>;
      try {
        const url = this.url;
        if (!url || !(p instanceof Promise)) return p;
        const ct = this.headers.get("content-type") || "";
        return p.then((blob) => {
          try {
            tagResult(blob, url, ct, blob.size);
          } catch {
            /* ignore */
          }
          return blob;
        });
      } catch {
        return p;
      }
    });

  wrapMethod(Blob.prototype, "arrayBuffer", (orig) =>
    function (this: Blob, ...args: unknown[]) {
      const p = orig.apply(this, args) as Promise<ArrayBuffer>;
      try {
        if (!hasAnyTag() || !tagsOf(this)) return p;
        return p.then((buf) => {
          try {
            propagate(this, buf);
          } catch {
            /* ignore */
          }
          return buf;
        });
      } catch {
        return p;
      }
    });

  wrapMethod(Blob.prototype, "slice", (orig) =>
    function (this: Blob, ...args: unknown[]) {
      const out = orig.apply(this, args);
      try {
        if (hasAnyTag()) propagate(this, out);
      } catch {
        /* ignore */
      }
      return out;
    });

  wrapMethod(ReadableStream.prototype, "getReader", (orig) =>
    function (this: ReadableStream, ...args: unknown[]) {
      const reader = orig.apply(this, args);
      try {
        const src = streamSource.get(this);
        if (src) readerSource.set(reader as object, { ...src, pushed: false });
      } catch {
        /* ignore */
      }
      return reader;
    });

  const readerProto =
    typeof ReadableStreamDefaultReader !== "undefined" ? ReadableStreamDefaultReader.prototype : undefined;
  wrapMethod(readerProto, "read", (orig) =>
    function (this: object, ...args: unknown[]) {
      const p = orig.apply(this, args) as Promise<ReadableStreamReadResult<Uint8Array>>;
      try {
        const src = readerSource.get(this);
        if (!src) return p;
        return p.then((res) => {
          try {
            if (res && res.value && res.value.buffer) {
              tag(res.value, src.url);
              if (!src.pushed && src.media) {
                src.pushed = true;
                recent.push(src.url, res.value.byteLength, Date.now());
              }
            }
          } catch {
            /* ignore */
          }
          return res;
        });
      } catch {
        return p;
      }
    });

  // ===== 拷贝路径传播：slice / set =====

  wrapMethod(ArrayBuffer.prototype, "slice", (orig) =>
    function (this: ArrayBuffer, ...args: unknown[]) {
      const out = orig.apply(this, args);
      try {
        if (hasAnyTag()) propagate(this, out);
      } catch {
        /* ignore */
      }
      return out;
    });

  const typedArrayProto = Object.getPrototypeOf(Uint8Array.prototype) as object | undefined;
  wrapMethod(typedArrayProto, "slice", (orig) =>
    function (this: ArrayBufferView, ...args: unknown[]) {
      const out = orig.apply(this, args);
      try {
        if (hasAnyTag()) propagate(this, out);
      } catch {
        /* ignore */
      }
      return out;
    });
  wrapMethod(typedArrayProto, "set", (orig) =>
    function (this: ArrayBufferView, ...args: unknown[]) {
      const r = orig.apply(this, args);
      try {
        if (hasAnyTag()) propagate(args[0], this);
      } catch {
        /* ignore */
      }
      return r;
    });

  // ===== MSE：createObjectURL / 元素绑定 / SourceBuffer =====

  wrapMethod(URL, "createObjectURL", (orig) =>
    function (this: unknown, ...args: unknown[]) {
      const url = orig.apply(this, args) as string;
      try {
        const obj = args[0];
        if (typeof MediaSource !== "undefined" && obj instanceof MediaSource) {
          msByObjectUrl.set(url, getMsInfo(obj));
          if (msByObjectUrl.size > 256) {
            const oldest = msByObjectUrl.keys().next().value;
            if (oldest !== undefined) msByObjectUrl.delete(oldest);
          }
        } else if (typeof Blob !== "undefined" && obj instanceof Blob && /^(?:video|audio)\//i.test(obj.type || "video/")) {
          // 整段下载后再交给 <video src=blob:>：Blob 带着来源标记时，把 blob URL 还原成原始地址
          const sources = tagsOf(obj);
          if (sources && sources.size === 1) {
            const [source] = sources;
            dispatchReport({ t: "blob", blobUrl: url, url: source });
          }
        }
      } catch {
        /* ignore */
      }
      return url;
    });

  if (typeof HTMLMediaElement !== "undefined") {
    wrapSetter(HTMLMediaElement.prototype, "src", linkBySrc);
    wrapSetter(HTMLMediaElement.prototype, "srcObject", linkBySrc);
  }
  wrapMethod(Element.prototype, "setAttribute", (orig) =>
    function (this: Element, ...args: unknown[]) {
      const r = orig.apply(this, args);
      try {
        if (typeof args[0] === "string" && args[0].toLowerCase() === "src" && isMediaElement(this)) {
          linkBySrc(this, String(args[1]));
        }
      } catch {
        /* ignore */
      }
      return r;
    });

  if (typeof MediaSource !== "undefined") {
    wrapMethod(MediaSource.prototype, "addSourceBuffer", (orig) =>
      function (this: MediaSource, ...args: unknown[]) {
        const sb = orig.apply(this, args) as object;
        try {
          const mime = String(args[0] ?? "");
          const ms = getMsInfo(this);
          const lower = mime.toLowerCase();
          const info: SbInfo = {
            ms,
            sbId: nextSbId++,
            mime,
            kind: lower.startsWith("video") ? "video" : lower.startsWith("audio") ? "audio" : "unknown",
            exact: [],
            temporal: new Map(),
            epoch: configEpoch,
          };
          ms.sbs.push(info);
          sbInfo.set(sb, info);
        } catch {
          /* ignore */
        }
        return sb;
      });
  }
  if (typeof SourceBuffer !== "undefined") {
    wrapMethod(SourceBuffer.prototype, "appendBuffer", (orig) =>
      function (this: SourceBuffer, ...args: unknown[]) {
        try {
          onAppend(this, args[0]);
        } catch {
          /* 归因失败不影响 append */
        }
        return orig.apply(this, args);
      });
  }

  // ===== Service Worker 作用域：识别 SW 合成的下载 =====
  // 流式保存、网盘、网页 IM 等下载可由页面 SW 在浏览器内合成 http(s) 响应，引擎按 URL
  // 重放只会拿到 404 / 网页。上报本源 SW 作用域，background 据此要求作用域内的下载必须
  // 有真实网络请求才接管。与嗅探开关无关：它服务于下载拦截的正确性。
  const reportedScopes = new Set<string>();
  function reportScopes(result: unknown): void {
    for (const reg of Array.isArray(result) ? result : [result]) {
      const scope = (reg as { scope?: unknown } | null | undefined)?.scope;
      if (typeof scope !== "string" || !isHttpUrl(scope) || reportedScopes.has(scope)) continue;
      reportedScopes.add(scope);
      dispatchReport({ t: "swScope", scope });
    }
  }
  if (typeof ServiceWorkerContainer !== "undefined") {
    for (const key of ["register", "getRegistration", "getRegistrations"]) {
      wrapMethod(ServiceWorkerContainer.prototype, key, (orig) =>
        function (this: ServiceWorkerContainer, ...args: unknown[]) {
          const p = orig.apply(this, args);
          if (!(p instanceof Promise)) return p;
          // 返回镜像原结果的派生 Promise：成功值与拒绝原因不变，页面未处理的拒绝照旧上抛
          return p.then((result: unknown) => {
            try {
              reportScopes(result);
            } catch {
              /* ignore */
            }
            return result;
          });
        });
    }
    try {
      // 已受控的文档（常见于再次访问：页面不会再调 register）：主动查一次本文档的注册
      const container = navigator.serviceWorker;
      if (container?.controller) {
        container.getRegistration().then(reportScopes, () => {
          /* 查询失败：不上报 */
        });
      }
    } catch {
      /* 沙箱 iframe 读取 navigator.serviceWorker 会抛 SecurityError */
    }
  }
}
