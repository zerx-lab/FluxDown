/**
 * 自动拦截发送的短窗去重（#1048）。
 *
 * 部分 Chromium 分支对一次点击会产生两个 download item（第二个是无 Referer 的 `Sec-Fetch-Dest: empty`
 * 重发），按 download id 的互斥拦不住，于是同一 URL 被投递两次：第二个任务只剩默认 UA，
 * 对需要来源页鉴权的站点会下到错误页。
 *
 * 同一 key 在「发送进行中」或「成功后窗口期内」再次发送，直接复用第一次的结果：调用方
 * 照常取消这个多余的下载项，不再发第二个请求。失败结果不缓存，后续发送可以重试。
 */

interface Entry<T> {
  promise: Promise<T>;
  /** 成功落定的时间；undefined = 仍在发送中。 */
  settledAt?: number;
}

export interface SendDeduper<T extends { success: boolean }> {
  /** 复用窗口内的发送结果，否则调用 `send`。`reused` 表示命中了去重。 */
  run(key: string, send: () => Promise<T>): { promise: Promise<T>; reused: boolean };
}

export function createSendDeduper<T extends { success: boolean }>(
  windowMs: number,
  now: () => number = Date.now,
): SendDeduper<T> {
  const entries = new Map<string, Entry<T>>();

  const prune = (at: number): void => {
    for (const [key, entry] of entries) {
      if (entry.settledAt !== undefined && at - entry.settledAt >= windowMs) entries.delete(key);
    }
  };

  return {
    run(key, send) {
      const at = now();
      prune(at);
      const hit = entries.get(key);
      if (hit) return { promise: hit.promise, reused: true };

      const entry = { promise: undefined as unknown as Promise<T> } as Entry<T>;
      const forget = (): void => {
        if (entries.get(key) === entry) entries.delete(key);
      };
      // send() 同步段内就占位：onCreated / onDeterminingFilename 同一 tick 到达的第二次调用也能命中。
      entry.promise = send().then(
        (outcome) => {
          if (outcome.success) entry.settledAt = now();
          else forget();
          return outcome;
        },
        (error: unknown) => {
          forget();
          throw error;
        },
      );
      entries.set(key, entry);
      return { promise: entry.promise, reused: false };
    },
  };
}

/**
 * 去重键 = 方法 + URL + 请求体。同一端点（OA / 导出接口）的不同表单 POST 是不同下载，
 * 只按 URL 去重会把第二个下载当成重复、复用第一次的成功结果，调用方随即取消浏览器下载项，
 * 用户的第二个下载被静默丢弃。`record` 取自 `lookupRequestRecord`（GET 不回传 method）。
 */
export function interceptDedupeKey(url: string, record: { method?: string; body?: unknown }): string {
  return `${record.method ?? "GET"} ${url} ${JSON.stringify(record.body ?? null)}`;
}
