import { describe, expect, test } from "bun:test";
import { requestUrlOf, wrapFetch, type FetchHookDeps } from "./fetch-hook";

const BASE = "https://example.com/dir/page.html";

function makeDeps(over: Partial<FetchHookDeps> = {}) {
  const requests: string[] = [];
  const responses: Array<[Response, string]> = [];
  const deps: FetchHookDeps = {
    baseUrl: () => BASE,
    sniffOn: () => true,
    mediaEventsOn: () => true,
    onRequest: (url) => requests.push(url),
    onResponse: (response, url) => responses.push([response, url]),
    ...over,
  };
  return { deps, requests, responses };
}

/** 只认 `(url, init)` 的前置包装（页面 / 油猴脚本常见写法）。 */
function legacyWrapper(calls: unknown[][]): typeof fetch {
  return (async (...args: unknown[]) => {
    calls.push(args);
    const [url, init] = args;
    if (typeof url !== "string") return undefined as unknown as Response;
    return new Response(`body:${url}:${(init as RequestInit | undefined)?.credentials ?? ""}`);
  }) as typeof fetch;
}

describe("wrapFetch 参数透传", () => {
  test("(url, init) 原样交给前置包装，响应可读", async () => {
    const calls: unknown[][] = [];
    const { deps } = makeDeps();
    const hooked = wrapFetch(legacyWrapper(calls), deps);
    const init = { credentials: "include" as const };

    const resp = await hooked("/api/x", init);

    expect(calls).toHaveLength(1);
    expect(calls[0][0]).toBe("/api/x");
    expect(calls[0][1]).toBe(init);
    expect(await resp.text()).toBe("body:/api/x:include");
  });

  test("Request 对象原样透传，body 未被占用", async () => {
    let received: unknown;
    const original = (async (input: unknown) => {
      received = input;
      return new Response("ok");
    }) as typeof fetch;
    const { deps } = makeDeps();
    const request = new Request("https://example.com/up", { method: "POST", body: "payload" });

    await wrapFetch(original, deps)(request);

    expect(received).toBe(request);
    expect(request.bodyUsed).toBe(false);
    expect(await request.text()).toBe("payload");
  });

  test("嗅探与媒体事件都关闭：不旁路读取，返回前置包装的响应", async () => {
    const calls: unknown[][] = [];
    const { deps, requests, responses } = makeDeps({ sniffOn: () => false, mediaEventsOn: () => false });
    const resp = await wrapFetch(legacyWrapper(calls), deps)("https://example.com/a", { credentials: "include" });

    expect(await resp.text()).toBe("body:https://example.com/a:include");
    expect(requests).toEqual([]);
    expect(responses).toEqual([]);
  });

  test("前置包装返回 undefined 时原样返回，旁路不抛", async () => {
    const calls: unknown[][] = [];
    const { deps, responses } = makeDeps();
    const resp = await wrapFetch(legacyWrapper(calls), deps)(new URL("https://example.com/a") as unknown as string);

    expect(resp).toBeUndefined();
    expect(responses).toEqual([]);
  });

  test("旁路回调抛错不影响返回的响应与请求", async () => {
    const { deps } = makeDeps({
      onRequest: () => {
        throw new Error("boom");
      },
      onResponse: () => {
        throw new Error("boom");
      },
    });
    const original = (async () => new Response("fine")) as typeof fetch;
    const resp = await wrapFetch(original, deps)("https://example.com/a");
    expect(await resp.text()).toBe("fine");
  });

  test("原 fetch 拒绝时原样拒绝", async () => {
    const failure = new TypeError("Failed to fetch");
    const original = (async () => {
      throw failure;
    }) as typeof fetch;
    const { deps } = makeDeps();
    await expect(wrapFetch(original, deps)("https://example.com/a")).rejects.toBe(failure);
  });

  test("this 与全部参数（含第三个及以后）原样传给原 fetch", async () => {
    let seenThis: unknown;
    let seenArgs: unknown[] = [];
    const original = (function (this: unknown, ...args: unknown[]) {
      seenThis = this;
      seenArgs = args;
      return Promise.resolve(new Response("ok"));
    }) as typeof fetch;
    const { deps } = makeDeps();
    const owner = { fetch: wrapFetch(original, deps) };
    await (owner.fetch as (...a: unknown[]) => Promise<Response>)("https://example.com/a", { method: "GET" }, "extra");

    expect(seenThis).toBe(owner);
    expect(seenArgs).toEqual(["https://example.com/a", { method: "GET" }, "extra"]);
  });
});

describe("wrapFetch 旁路上报", () => {
  test("相对地址按页面基准解析后上报，响应带同一 URL", async () => {
    const { deps, requests, responses } = makeDeps();
    const original = (async () => new Response("x")) as typeof fetch;
    const resp = await wrapFetch(original, deps)("../v/a.m3u8?x=1");

    expect(requests).toEqual(["https://example.com/v/a.m3u8?x=1"]);
    expect(responses).toEqual([[resp, "https://example.com/v/a.m3u8?x=1"]]);
  });

  test("仅媒体事件开启：不上报请求 URL，但仍打标响应", async () => {
    const { deps, requests, responses } = makeDeps({ sniffOn: () => false });
    const original = (async () => new Response("x")) as typeof fetch;
    await wrapFetch(original, deps)("https://example.com/a");

    expect(requests).toEqual([]);
    expect(responses).toHaveLength(1);
  });
});

describe("requestUrlOf", () => {
  test("string / URL / Request", () => {
    expect(requestUrlOf("a/b", BASE)).toBe("https://example.com/dir/a/b");
    expect(requestUrlOf(new URL("https://x.test/p?q=1"), BASE)).toBe("https://x.test/p?q=1");
    expect(requestUrlOf(new Request("https://x.test/r"), BASE)).toBe("https://x.test/r");
  });

  test("非法输入与未知对象返回空串，不调用 toString", () => {
    let touched = false;
    const weird = {
      toString() {
        touched = true;
        return "https://x.test/";
      },
    };
    expect(requestUrlOf("http://", BASE)).toBe("");
    expect(requestUrlOf(weird, BASE)).toBe("");
    expect(requestUrlOf(undefined, BASE)).toBe("");
    expect(touched).toBe(false);
  });
});
