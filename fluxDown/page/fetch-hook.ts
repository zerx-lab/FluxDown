/**
 * MAIN world 的 `window.fetch` 包装（纯函数，依赖全部注入，便于单测）。
 *
 * 铁律：传给下游 fetch 的参数与调用方完全一致（#1051）。下游可能是页面自己或油猴脚本的前置包装，
 * 它们只认 `(url, init)` 约定；把入参改写成单个 `Request` 会让这类包装拿到意外形态，
 * 传入 `Request` 时 `new Request(request)` 还会占用其 body。URL 只从第一个参数旁路只读推导。
 */

/** 注入的依赖：全部只读旁路，异常一律被吞，绝不影响页面请求。 */
export interface FetchHookDeps {
  /** 当前页面基准地址（`document.baseURI`），用于解析相对 URL。 */
  baseUrl: () => string;
  /** 嗅探开着：需要读 URL 并上报。 */
  sniffOn: () => boolean;
  /** 仅媒体事件还需要响应来源打标（浮钮）。 */
  mediaEventsOn: () => boolean;
  /** 请求发出时的 URL 通知（只在 sniffOn 时调用）。 */
  onRequest: (url: string) => void;
  /** 响应到达（只在 sniffOn 或 mediaEventsOn 时调用）。 */
  onResponse: (response: Response, url: string) => void;
}

/**
 * 只读推导 fetch 第一个参数对应的绝对 URL；无法确定返回空串。
 * 不调用任意对象的 `toString`（避免页面自定义对象的副作用）。
 */
export function requestUrlOf(input: unknown, baseUrl: string): string {
  try {
    if (typeof input === "string") return new URL(input, baseUrl).href;
    if (input && typeof input === "object") {
      const { url, href } = input as { url?: unknown; href?: unknown };
      if (typeof url === "string") return url; // Request：已是绝对地址
      if (typeof href === "string") return href; // URL
    }
  } catch {
    /* 非法 URL 交给原生 fetch 报错 */
  }
  return "";
}

/**
 * 包装 `originalFetch`：原样透传全部参数，只在旁路读取 URL / 响应。
 *
 * 原生 fetch 以绑定函数作为微任务回调发起，调用栈上没有本脚本的帧：页面自己被 CSP 拦下的请求
 * （connect-src 违规等）在控制台的来源不再是 page-hooks.js，Chrome 不会把它记进扩展错误列表。
 * 直接 `originalFetch.apply` 时栈顶是本包装函数，这些页面错误全都算到扩展头上。
 */
export function wrapFetch(originalFetch: typeof fetch, deps: FetchHookDeps): typeof fetch {
  return function (this: unknown, ...args: Parameters<typeof fetch>) {
    const sniff = safe(deps.sniffOn);
    const observe = sniff || safe(deps.mediaEventsOn);
    let url = "";
    if (observe) {
      try {
        url = requestUrlOf(args[0], deps.baseUrl());
        if (sniff && url) deps.onRequest(url);
      } catch {
        /* 嗅探绝不打断页面请求 */
      }
    }

    const start = originalFetch.bind(this, ...args) as () => Promise<Response>;
    const pending = Promise.resolve().then(start);
    if (!observe) return pending;
    return pending.then((response) => {
      try {
        if (response && typeof response === "object") deps.onResponse(response, url);
      } catch {
        /* 不干扰原始响应 */
      }
      return response;
    });
  } as typeof fetch;
}

function safe(fn: () => boolean): boolean {
  try {
    return fn();
  } catch {
    return false;
  }
}
