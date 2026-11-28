/**
 * 浏览器下载的来源判定（纯模块，零浏览器依赖，可在 bun 下测试）。
 *
 * FluxDown 引擎只能拿 URL 重新向服务器请求。字节只存在于浏览器内部的下载必须留给浏览器：
 * - 本地 scheme：`blob:` / `data:` / `filesystem:` / `chrome-extension:` 等（页面内存、
 *   沙箱文件系统或扩展包内资源，外部进程无从读取）；
 * - Service Worker 合成：流式保存、网盘 / 网页 IM 的流式下载，地址是普通 http(s)，
 *   但响应由页面 SW 在浏览器内拼出，服务器上并不存在，按 URL 重放只会拿到 404 / 网页。
 *
 * SW 合成没有直接标记，按两条证据联合判定，只在 SW 作用域内收紧，避免误伤普通下载：
 * 1. 下载地址落在页面上报的 SW 作用域内；
 * 2. 没有真实发往网络的请求（Chrome 下 SW 自行响应的导航不触发 webRequest；SW 直通
 *    `fetch(event.request)` 会触发），或该响应被明确判为合成（Firefox：无服务器 IP 且非缓存）。
 * 误判方向只会是「交给浏览器下载」，不会把不可重放的地址交给引擎。
 */

const REPLAYABLE_PROTOCOLS: Record<string, true> = { "http:": true, "https:": true, "ftp:": true };

/** 引擎能独立按 URL 重放的地址（http / https / ftp）。 */
export function isReplayableUrl(url: string): boolean {
  try {
    return REPLAYABLE_PROTOCOLS[new URL(url).protocol] === true;
  } catch {
    return false;
  }
}

/** Matching hint from a browser-observed HTTP(S) redirect; never a replay URL. */
export function observedBrowserOriginalUrl(url: string, finalUrl?: string): string | undefined {
  if (!finalUrl || url === finalUrl) return undefined;
  try {
    const original = new URL(url);
    const final = new URL(finalUrl);
    if (
      (original.protocol === "http:" || original.protocol === "https:") &&
      (final.protocol === "http:" || final.protocol === "https:") &&
      original.href !== final.href
    ) {
      return url;
    }
  } catch {
    // An invalid URL cannot establish a browser redirect.
  }
  return undefined;
}

/** 规范化 SW 作用域：仅接受 http(s)，去掉 query / hash。非法返回 null。 */
export function normalizeScope(scope: string): string | null {
  let parsed: URL;
  try {
    parsed = new URL(scope);
  } catch {
    return null;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
  return `${parsed.origin}${parsed.pathname}`;
}

/**
 * 作用域是否属于上报它的帧：SW 只能注册在同源路径下，跨源上报一律拒绝，
 * 防止页面借此让其它站点的下载失去接管。
 */
export function scopeBelongsTo(scope: string, frameUrl: string): boolean {
  const normalized = normalizeScope(scope);
  if (!normalized) return false;
  try {
    return new URL(normalized).origin === new URL(frameUrl).origin;
  } catch {
    return false;
  }
}

/** 已知的 Service Worker 作用域（前缀匹配，插入序淘汰）。 */
export class ServiceWorkerScopes {
  private readonly scopes = new Set<string>();

  constructor(private readonly capacity = 256) {}

  /** 恢复持久化的列表；非字符串 / 非法项跳过。 */
  static from(stored: unknown, capacity?: number): ServiceWorkerScopes {
    const registry = new ServiceWorkerScopes(capacity);
    if (Array.isArray(stored)) {
      for (const item of stored) if (typeof item === "string") registry.add(item);
    }
    return registry;
  }

  /** 记录作用域；返回是否新增（调用方据此决定是否持久化）。 */
  add(scope: string): boolean {
    const normalized = normalizeScope(scope);
    if (!normalized || this.scopes.has(normalized)) return false;
    if (this.scopes.size >= this.capacity) {
      const oldest = this.scopes.values().next().value;
      if (oldest !== undefined) this.scopes.delete(oldest);
    }
    this.scopes.add(normalized);
    return true;
  }

  /** `url` 是否落在任一作用域内。 */
  covers(url: string): boolean {
    if (this.scopes.size === 0) return false;
    for (const scope of this.scopes) {
      if (url.startsWith(scope)) return true;
    }
    return false;
  }

  toJSON(): string[] {
    return [...this.scopes];
  }
}

/**
 * 某个 URL 的网络证据：
 * - `network`：浏览器确实把请求发往了网络（SW 直通也算）；
 * - `synthetic`：响应被明确判为浏览器内合成；
 * - `none`：没有观察到任何网络请求。
 */
export type NetworkEvidence = "network" | "synthetic" | "none";

/** 只能由浏览器完成的下载原因；`null` = 可交给引擎。 */
export type BrowserOnlyReason = "local-scheme" | "service-worker";

/**
 * 判定下载是否只能由浏览器完成。`urls` 为下载项的原始地址与重定向后的地址（可含空串）。
 */
export function browserOnlyReason(
  urls: readonly (string | undefined)[],
  scopes: ServiceWorkerScopes,
  evidenceOf: (url: string) => NetworkEvidence,
): BrowserOnlyReason | null {
  const list = urls.filter((u): u is string => typeof u === "string" && u.length > 0);
  if (list.length === 0 || list.some((u) => !isReplayableUrl(u))) return "local-scheme";
  if (!list.some((u) => scopes.covers(u))) return null;
  let network = false;
  for (const u of list) {
    const evidence = evidenceOf(u);
    if (evidence === "synthetic") return "service-worker";
    if (evidence === "network") network = true;
  }
  return network ? null : "service-worker";
}
