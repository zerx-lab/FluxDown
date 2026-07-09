/**
 * 进程内 API 响应缓存（Astro node standalone 单进程，模块单例跨路由共享）。
 *
 * /api/release 与 /api/changelog 用它缓存 GitHub API 响应以保护配额；
 * /api/webhooks/github 在收到 GitHub `release` 事件时调用 bustApiCaches()
 * 立即失效，保证发版后官网下载地址即时更新（无需等 TTL 过期）。
 */

interface CacheEntry {
  data: unknown;
  timestamp: number;
}

const store = new Map<string, CacheEntry>();

/** 读取缓存；不存在或超过 ttlMs 时返回 null */
export function getCached<T>(key: string, ttlMs: number): T | null {
  const entry = store.get(key);
  if (!entry || Date.now() - entry.timestamp > ttlMs) return null;
  return entry.data as T;
}

/** 写入缓存（记录当前时间戳） */
export function setCached(key: string, data: unknown): void {
  store.set(key, { data, timestamp: Date.now() });
}

/** 清空全部 API 缓存（GitHub release webhook 触发） */
export function bustApiCaches(): void {
  store.clear();
}
