import { afterAll, beforeEach, expect, mock, test } from "bun:test";
import { parseBlocks, parseInline } from "../src/components/assistant/md-parse";
import type { CookieJar, CookieOptions } from "../src/lib/account/session";

// 动态 import：base.ts 在模块加载时读 BASE_URL，astro:env/server 须先 mock，二者都要在路由模块加载之前就位。
process.env.BASE_URL = "/";
// 与 download-route / download-pass / bt-guard-proxy 测试共用同一进程的模块表：bun 的 mock.module 首次注册后导出名集合即固定，须含全部 astro:env/server 导出。
mock.module("astro:env/server", () => ({
  GITHUB_TOKEN: "", GITHUB_REPO: "owner/repo", FLUXCLOUD_API_BASE: "http://cloud.test", DOWNLOAD_PASS_SECRET: undefined,
  CN_CDN_DAILY_BUDGET_GB: 0,
}));
const { handleAccountRequest } = await import("../src/lib/account/routes");
const { resetRefreshCache, ACCESS_COOKIE, REFRESH_COOKIE } = await import("../src/lib/account/session");

const originalFetch = globalThis.fetch;

class Jar implements CookieJar {
  store = new Map<string, { value: string; options?: CookieOptions }>();
  constructor(initial: Record<string, string> = {}) {
    for (const [name, value] of Object.entries(initial)) this.store.set(name, { value });
  }
  get(name: string) {
    const entry = this.store.get(name);
    return entry ? { value: entry.value } : undefined;
  }
  set(name: string, value: string, options: CookieOptions) {
    this.store.set(name, { value, options });
  }
  delete(name: string) {
    this.store.delete(name);
  }
}

interface Seen { url: string; method: string; headers: Record<string, string>; body: unknown; signal?: AbortSignal | null }
let seen: Seen[] = [];
let reply: (call: Seen) => Response | Promise<Response> = () => Response.json({});

beforeEach(() => {
  resetRefreshCache();
  seen = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const call: Seen = {
      url: String(input),
      method: init?.method ?? "GET",
      headers: Object.fromEntries(new Headers(init?.headers).entries()),
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
      signal: init?.signal,
    };
    seen.push(call);
    return reply(call);
  }) as typeof fetch;
});
afterAll(() => {
  globalThis.fetch = originalFetch;
  mock.restore();
});

const SESSION_ID = "3f2c8a8e-1d7b-4b8e-9a51-0c6f3e2d1a90";
const signedIn = () => new Jar({ [ACCESS_COOKIE]: "at-0", [REFRESH_COOKIE]: "rt-0" });

function call(
  path: string,
  { method = "GET", body, signal, headers = {}, jar = signedIn() }: { method?: string; body?: unknown; signal?: AbortSignal; headers?: Record<string, string>; jar?: Jar } = {},
) {
  const request = new Request(`https://site.example/api/account/${path}`, {
    method,
    headers: {
      "X-Fluxdown-Account": "1",
      "Sec-Fetch-Site": "same-origin",
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      ...headers,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal,
  });
  return handleAccountRequest(request, path.split("?")[0] ?? path, jar, "203.0.113.7");
}

/** 把若干文本块依次作为上游 SSE 正文返回（块边界可以切在帧中间）。 */
function sse(chunks: string[]): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
  return new Response(body, { headers: { "Content-Type": "text/event-stream" } });
}

const frame = (event: unknown) => `data: ${JSON.stringify(event)}\n\n`;

function framesOf(text: string): unknown[] {
  return text
    .split("\n\n")
    .filter((block) => block.startsWith("data: "))
    .map((block) => JSON.parse(block.slice(6)));
}

/** 1×1 PNG 的 data URL。 */
const PNG_URL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

test("ask: forwards only whitelisted events and fields, even when frames are split across chunks", async () => {
  const issue = {
    id: "i1", sessionId: SESSION_ID, kind: "bug", title: "Crash", body: "steps", labels: ["bug"], status: "draft",
    githubNumber: null, githubUrl: null, error: "internal", reviewNote: null, createdAt: "t", updatedAt: "t",
  };
  const upstream = [
    frame({ type: "run", runId: "r1", sessionId: SESSION_ID }),
    frame({ type: "triage", intent: "usage", action: "pass", confidence: 0.9 }),
    frame({ type: "routed", profile: "general", profileName: "General", route: "default" }),
    frame({ type: "reasoning", delta: "secret thoughts" }),
    frame({ type: "clientToolCall", requestId: "q1", name: "fd_get_logs", arguments: {} }),
    frame({ type: "issueDraft", issue }),
    ": ka\n\n",
    frame({ type: "text", delta: "Hel" }),
    frame({ type: "done", status: "completed", messageId: "m1", text: "Hello", inputTokens: 10, outputTokens: 5 }),
  ].join("");
  const cut = upstream.indexOf("Hel") + 1;
  reply = () => sse([upstream.slice(0, cut), upstream.slice(cut)]);

  const res = await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "  hi  ", images: [PNG_URL] } });
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toContain("text/event-stream");
  const text = await res.text();
  expect(text).toContain(": ka\n\n");
  expect(framesOf(text)).toEqual([
    { type: "run", runId: "r1", sessionId: SESSION_ID },
    {
      type: "issueDraft",
      issue: {
        id: "i1", sessionId: SESSION_ID, kind: "bug", title: "Crash", body: "steps", labels: ["bug"], status: "draft",
        githubNumber: null, githubUrl: null, reviewNote: null, createdAt: "t", updatedAt: "t",
      },
    },
    { type: "text", delta: "Hel" },
    { type: "done", status: "completed", messageId: "m1", text: "Hello" },
  ]);

  expect(seen).toHaveLength(1);
  expect(seen[0]?.url).toBe(`http://cloud.test/api/v1/ai/sessions/${SESSION_ID}/messages`);
  expect(seen[0]?.headers.authorization).toBe("Bearer at-0");
  expect(seen[0]?.body).toEqual({ text: "hi", images: [PNG_URL] });
});

test("ask: refusals before the stream starts pass through as JSON errors", async () => {
  reply = () => Response.json({ code: "ai_quota_exceeded", message: "今日提问次数已用完" }, { status: 403 });
  const res = await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "hi" } });
  expect(res.status).toBe(403);
  expect(await res.json()).toEqual({ code: "ai_quota_exceeded", message: "今日提问次数已用完" });
});

test("ask: a visitor disconnect aborts the upstream request", async () => {
  reply = () => sse([frame({ type: "run", runId: "r1", sessionId: SESSION_ID })]);
  const visitor = new AbortController();
  await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "hi" }, signal: visitor.signal });
  visitor.abort();
  expect(seen[0]?.signal?.aborted).toBe(true);
});

test("ask: accepts questions longer than the default 16 KiB body limit and rejects bad ids", async () => {
  reply = () => sse([]);
  const long = "问".repeat(8000);
  expect((await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: long } })).status).toBe(200);
  expect((await call("ai/sessions/..%2Fx/messages", { method: "POST", body: { text: "hi" } })).status).toBe(400);
  expect(seen).toHaveLength(1);
});

test("ask: images must be at most 4 bitmap data URLs; bad ones never reach FluxCloud", async () => {
  reply = () => sse([]);
  const ask = (images: unknown) => call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "hi", images } });
  expect((await ask(Array(5).fill(PNG_URL))).status).toBe(400);
  expect((await ask(["data:image/svg+xml;base64,PHN2Zz4="])).status).toBe(400);
  expect((await ask(["https://example.com/a.png"])).status).toBe(400);
  expect((await ask("not-a-list")).status).toBe(400);
  expect(seen).toHaveLength(0);

  // 4 张约 1 MB 的图片：远超普通 16 KiB 上限，仍在提问上限内
  const big = `data:image/jpeg;base64,${"A".repeat(1024 * 1024)}`;
  expect((await ask(Array(4).fill(big))).status).toBe(200);
  expect(seen).toHaveLength(1);
});

test("ask: forwards the current site locale; malformed locales never reach FluxCloud", async () => {
  reply = () => sse([]);
  const ask = (locale: unknown) => call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "hi", locale } });
  expect((await ask("pt-BR")).status).toBe(200);
  expect(seen[0]?.body).toEqual({ text: "hi", locale: "pt-BR" });
  for (const bad of ["english", "en US", 42, "en-"]) expect((await ask(bad)).status).toBe(400);
  expect(seen).toHaveLength(1);
});

test("attachments: plain <img> GETs are proxied with a locked-down image response", async () => {
  const png = Uint8Array.from(atob(PNG_URL.split(",")[1] ?? ""), (c) => c.charCodeAt(0));
  reply = () => new Response(png, { headers: { "Content-Type": "image/png" } });
  const res = await call("ai/attachments/a1", { headers: { "X-Fluxdown-Account": "" } });
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toBe("image/png");
  expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  expect(new Uint8Array(await res.arrayBuffer())).toEqual(png);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/attachments/a1");
  expect(seen[0]?.headers.authorization).toBe("Bearer at-0");

  // 上游给出非位图类型（如 SVG）不转发；跨站请求在到达上游前拒绝
  reply = () => new Response("<svg/>", { headers: { "Content-Type": "image/svg+xml" } });
  expect((await call("ai/attachments/a1")).status).toBe(502);
  expect((await call("ai/attachments/a1", { headers: { "X-Fluxdown-Account": "", "Sec-Fetch-Site": "cross-site" } })).status).toBe(403);
  expect(seen).toHaveLength(2);
});

test("config: carries the entry visibility next to availability; internals stay out", async () => {
  reply = () =>
    Response.json({
      available: false, reason: "plan_disabled", visible: true, maxMessageChars: 0, dailyLimit: 0, usedToday: 0,
      imagesSupported: false, maxImages: 0, maxImageBytes: 0, provider: "secret",
    });
  const res = await call("ai/config");
  expect(await res.json()).toEqual({
    available: false, reason: "plan_disabled", visible: true, maxMessageChars: 0, dailyLimit: 0, usedToday: 0,
    imagesSupported: false, maxImages: 0, maxImageBytes: 0,
  });
});

test("status: a signed-in visitor gets the verdict for their own account from ai/config (plan limits don't hide the entry)", async () => {
  reply = () => Response.json({ available: false, reason: "plan_disabled", visible: true, usedToday: 3 });
  const res = await call("ai/status");
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ visible: true });
  expect(seen).toHaveLength(1);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/config");
  expect(seen[0]?.headers.authorization).toBe("Bearer at-0");

  reply = () => Response.json({ available: false, reason: "internal_only", visible: false });
  expect(await (await call("ai/status")).json()).toEqual({ visible: false });
});

test("status: without a session the public ai/status answers, with no credentials sent", async () => {
  reply = () => Response.json({ visible: true });
  const res = await call("ai/status", { jar: new Jar() });
  expect(await res.json()).toEqual({ visible: true });
  expect(seen).toHaveLength(1);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/status");
  expect(seen[0]?.headers.authorization).toBeUndefined();
});

test("status: a dead session (401) falls back to the public verdict and the session is cleared", async () => {
  reply = (c) =>
    c.url.endsWith("/ai/config") ? Response.json({ code: "unauthorized" }, { status: 401 }) : Response.json({ visible: false });
  const jar = new Jar({ [ACCESS_COOKIE]: "at-0" });
  const res = await call("ai/status", { jar });
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ visible: false });
  expect(seen.map((s) => s.url)).toEqual(["http://cloud.test/api/v1/ai/config", "http://cloud.test/api/v1/ai/status"]);
  expect(seen[1]?.headers.authorization).toBeUndefined();
  expect(jar.get(ACCESS_COOKIE)).toBeUndefined();
});

test("status: other upstream failures and malformed bodies surface as failures, never as a guessed verdict", async () => {
  reply = () => Response.json({ code: "internal_error" }, { status: 500 });
  const signed = await call("ai/status");
  expect(signed.status).toBe(500);
  expect(seen).toHaveLength(1);

  reply = () => Response.json({ code: "internal_error" }, { status: 503 });
  expect((await call("ai/status", { jar: new Jar() })).status).toBe(503);

  // 上游还没带 visible（FluxCloud 未升级）/ 类型不对：502，客户端保留上次已知值
  reply = () => Response.json({ available: true });
  expect((await call("ai/status")).status).toBe(502);
  reply = () => Response.json({ visible: "yes" });
  expect((await call("ai/status", { jar: new Jar() })).status).toBe(502);
});

test("status: GET only, behind the same CSRF gate as the rest of the account BFF", async () => {
  reply = () => Response.json({ visible: true });
  expect((await call("ai/status", { headers: { "X-Fluxdown-Account": "" } })).status).toBe(403);
  expect((await call("ai/status", { method: "POST", body: {} })).status).toBe(405);
  expect(seen).toHaveLength(0);
});

test("sessions: created as website channel without client tools; kind is validated", async () => {
  reply = () => Response.json({ id: SESSION_ID, channel: "website", kind: "feedback", title: "", messageCount: 0, rating: null, active: false, createdAt: "t", updatedAt: "t", userId: "u1" });
  const res = await call("ai/sessions", { method: "POST", body: { kind: "feedback", locale: "zh-CN", channel: "desktop" } });
  expect(res.status).toBe(200);
  expect(await res.json()).not.toHaveProperty("userId");
  expect(seen[0]?.body).toEqual({ channel: "website", kind: "feedback", client: { clientTools: [], locale: "zh-CN" } });

  expect((await call("ai/sessions", { method: "POST", body: { kind: "admin" } })).status).toBe(400);
  expect(seen).toHaveLength(1);
});

test("sessions: list clamps paging and detail drops tool messages but keeps image ids", async () => {
  reply = () => Response.json({ total: 0, items: [] });
  await call("ai/sessions?limit=500&offset=-3");
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/sessions?limit=100&offset=0");

  reply = () =>
    Response.json({
      session: { id: SESSION_ID, channel: "website", kind: "ask", title: "Q", messageCount: 4, rating: null, active: false, createdAt: "t", updatedAt: "t" },
      messages: [
        { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", images: ["a1", "a2"], createdAt: "t", toolName: null },
        { id: "m2", runId: "r1", seq: 2, role: "tool_call", content: "{}", images: [], createdAt: "t", toolName: "source_read" },
        { id: "m3", runId: "r1", seq: 3, role: "tool_result", content: "file body", images: [], createdAt: "t", toolName: "source_read" },
        { id: "m4", runId: "r1", seq: 4, role: "assistant", content: "A", images: [], createdAt: "t", toolName: null },
      ],
      issues: [],
    });
  const res = await call(`ai/sessions/${SESSION_ID}`);
  const detail = (await res.json()) as { messages: unknown[] };
  expect(detail.messages).toEqual([
    { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", createdAt: "t", images: ["a1", "a2"] },
    { id: "m4", runId: "r1", seq: 4, role: "assistant", content: "A", createdAt: "t", images: [] },
  ]);
});

test("rating: PUT per answer forwards to the message route; the old per-session route is gone", async () => {
  reply = () => new Response(null, { status: 204 });
  const rate = (messageId: string, body: unknown) => call(`ai/sessions/${SESSION_ID}/messages/${messageId}/rating`, { method: "PUT", body });

  expect((await rate("m4", { rating: "up", comment: " too long ", extra: 1 })).status).toBe(204);
  expect(seen[0]?.method).toBe("PUT");
  expect(seen[0]?.url).toBe(`http://cloud.test/api/v1/ai/sessions/${SESSION_ID}/messages/m4/rating`);
  expect(seen[0]?.body).toEqual({ rating: "up", comment: "too long" });

  // 清除评价：不转发评论；踩不带评论时只有 rating
  expect((await rate("m4", { rating: null, comment: "x" })).status).toBe(204);
  expect(seen[1]?.body).toEqual({ rating: null });
  expect((await rate("m4", { rating: "down" })).status).toBe(204);
  expect(seen[2]?.body).toEqual({ rating: "down" });

  // 非法评价 / 评论过长 / 非法消息 ID 都到不了 FluxCloud
  expect((await rate("m4", { rating: "meh" })).status).toBe(400);
  expect((await rate("m4", {})).status).toBe(400);
  expect((await rate("m4", { rating: "up", comment: "x".repeat(501) })).status).toBe(400);
  expect((await rate("m4%2F..", { rating: "up" })).status).toBe(400);
  expect(seen).toHaveLength(3);

  // 旧的会话级评价路由已删除：不会被转发
  const old = await call(`ai/sessions/${SESSION_ID}/rating`, { method: "PUT", body: { rating: "up" } });
  expect(old.status).toBeGreaterThanOrEqual(400);
  expect(old.status).toBeLessThan(500);
  expect(seen).toHaveLength(3);
});

test("sessions: detail keeps per-answer rating fields on assistant messages only", async () => {
  reply = () =>
    Response.json({
      session: { id: SESSION_ID, channel: "website", kind: "ask", title: "Q", messageCount: 3, rating: "down", active: false, createdAt: "t", updatedAt: "t" },
      messages: [
        { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", images: [], createdAt: "t" },
        { id: "m2", runId: "r1", seq: 2, role: "assistant", content: "A", images: [], rating: "down", ratingComment: "wrong", createdAt: "t", secret: 1 },
        { id: "m3", runId: "r2", seq: 3, role: "assistant", content: "B", images: [], rating: null, ratingComment: null, createdAt: "t" },
      ],
      issues: [],
    });
  const detail = (await (await call(`ai/sessions/${SESSION_ID}`)).json()) as { messages: unknown[] };
  expect(detail.messages).toEqual([
    { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", createdAt: "t", images: [] },
    { id: "m2", runId: "r1", seq: 2, role: "assistant", content: "A", rating: "down", ratingComment: "wrong", createdAt: "t", images: [] },
    { id: "m3", runId: "r2", seq: 3, role: "assistant", content: "B", rating: null, ratingComment: null, createdAt: "t", images: [] },
  ]);
});

test("markdown: model output can't produce unsafe links and an unclosed fence runs to the end", () => {
  expect(parseInline("[x](javascript:void) <b>hi</b>")).toEqual([{ kind: "text", text: "x <b>hi</b>" }]);
  expect(parseInline("see https://fluxdown.com/docs.")).toEqual([
    { kind: "text", text: "see " },
    { kind: "link", href: "https://fluxdown.com/docs", children: [{ kind: "text", text: "https://fluxdown.com/docs" }] },
    { kind: "text", text: "." },
  ]);
  expect(parseInline("snake_case_name stays")).toEqual([{ kind: "text", text: "snake_case_name stays" }]);
  expect(parseBlocks("Run:\n```bash\ncargo build\n")).toEqual([
    { kind: "para", lines: ["Run:"] },
    { kind: "code", lang: "bash", text: "cargo build\n" },
  ]);
  expect(parseBlocks("1. one\n   - nested\n2. two")).toEqual([
    {
      kind: "list",
      ordered: true,
      start: 1,
      items: [
        [{ kind: "para", lines: ["one"] }, { kind: "list", ordered: false, start: 1, items: [[{ kind: "para", lines: ["nested"] }]] }],
        [{ kind: "para", lines: ["two"] }],
      ],
    },
  ]);
});

test("sessions: detail turns a successful offer_membership result into a membership_offer message", async () => {
  const offer = (id: string, seq: number, extra: Record<string, unknown>) => ({
    id, runId: "r1", seq, role: "tool_result", content: "plans: secret summary", images: [], createdAt: "t", toolName: "offer_membership", isError: false, ...extra,
  });
  reply = () =>
    Response.json({
      session: { id: SESSION_ID, channel: "website", kind: "ask", title: "Q", messageCount: 6, rating: null, active: false, createdAt: "t", updatedAt: "t" },
      messages: [
        { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", images: [], createdAt: "t", toolName: null },
        { id: "m2", runId: "r1", seq: 2, role: "tool_call", content: "{}", images: [], createdAt: "t", toolName: "offer_membership" },
        offer("m3", 3, {}),
        offer("m4", 4, { isError: true }),
        offer("m5", 5, { toolName: "source_read" }),
        offer("m6", 6, { runId: null }),
        { id: "m7", runId: "r1", seq: 7, role: "assistant", content: "A", images: [], createdAt: "t", toolName: null },
      ],
      issues: [],
    });
  const detail = (await (await call(`ai/sessions/${SESSION_ID}`)).json()) as { messages: unknown[] };
  expect(detail.messages).toEqual([
    { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", createdAt: "t", images: [] },
    { id: "m3", runId: "r1", seq: 3, role: "membership_offer", content: "", createdAt: "t", images: [] },
    { id: "m7", runId: "r1", seq: 7, role: "assistant", content: "A", createdAt: "t", images: [] },
  ]);
});

test("sessions: list forwards a trimmed search term; over-long terms never reach FluxCloud", async () => {
  reply = () => Response.json({ total: 0, items: [] });
  const res = await call(`ai/sessions?limit=10&q=${encodeURIComponent("  下载 失败 ")}`);
  expect(res.status).toBe(200);
  expect(seen[0]?.url).toBe(`http://cloud.test/api/v1/ai/sessions?limit=10&offset=0&q=${encodeURIComponent("下载 失败")}`);

  seen = [];
  const long = await call(`ai/sessions?q=${"x".repeat(101)}`);
  expect(long.status).toBe(400);
  expect(seen).toHaveLength(0);
});

test("sessions: clear-all, rename and export forward to FluxCloud with validated input", async () => {
  reply = () => Response.json({ cleared: 3, secret: 1 });
  const cleared = await call("ai/sessions", { method: "DELETE" });
  expect(await cleared.json()).toEqual({ cleared: 3 });
  expect(seen[0]?.method).toBe("DELETE");
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/sessions");

  seen = [];
  reply = () => Response.json({ id: SESSION_ID, channel: "website", kind: "ask", title: "New name", messageCount: 1, rating: null, active: false, createdAt: "t", updatedAt: "t" });
  const renamed = await call(`ai/sessions/${SESSION_ID}`, { method: "PATCH", body: { title: "  New name  " } });
  expect(renamed.status).toBe(200);
  expect(seen[0]?.method).toBe("PATCH");
  expect(seen[0]?.body).toEqual({ title: "New name" });
  expect((await call(`ai/sessions/${SESSION_ID}`, { method: "PATCH", body: { title: "   " } })).status).toBe(400);
  expect((await call(`ai/sessions/${SESSION_ID}`, { method: "PATCH", body: { title: "x".repeat(81) } })).status).toBe(400);

  seen = [];
  reply = () => Response.json({ fileName: "a.md", mime: "text/markdown", content: "# hi", extra: 1 });
  const exported = await call(`ai/sessions/${SESSION_ID}/export?format=markdown`);
  expect(await exported.json()).toEqual({ fileName: "a.md", mime: "text/markdown", content: "# hi" });
  expect(seen[0]?.url).toBe(`http://cloud.test/api/v1/ai/sessions/${SESSION_ID}/export?format=markdown`);
  expect((await call(`ai/sessions/${SESSION_ID}/export?format=pdf`)).status).toBe(400);
});

test("ask: replaceFrom / keepImages are validated and forwarded", async () => {
  reply = () => sse([frame({ type: "run", runId: "r1", sessionId: SESSION_ID })]);
  const ok = await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "again", replaceFrom: "m1", keepImages: true } });
  expect(ok.status).toBe(200);
  await ok.text();
  expect(seen[0]?.body).toEqual({ text: "again", replaceFrom: "m1", keepImages: true });

  seen = [];
  // keepImages 与新图片互斥，且必须带 replaceFrom
  const withImages = await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "x", replaceFrom: "m1", keepImages: true, images: [PNG_URL] } });
  expect(withImages.status).toBe(400);
  const orphan = await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "x", keepImages: true } });
  expect(orphan.status).toBe(400);
  const badId = await call(`ai/sessions/${SESSION_ID}/messages`, { method: "POST", body: { text: "x", replaceFrom: "../x" } });
  expect(badId.status).toBe(400);
  expect(seen).toHaveLength(0);
});

test("runs: resume proxies events after a sequence number and forwards only integer event ids", async () => {
  reply = () =>
    sse([
      `id: 4\ndata: ${JSON.stringify({ type: "text", delta: "Hel", secret: 1 })}\n\n`,
      `id: abc\ndata: ${JSON.stringify({ type: "text", delta: "lo" })}\n\n`,
      `data: ${JSON.stringify({ type: "triage", intent: "usage" })}\n\n`,
      `id: 6\ndata: ${JSON.stringify({ type: "done", status: "completed", messageId: "m1", text: "Hello", inputTokens: 1 })}\n\n`,
    ]);
  const res = await call("ai/runs/r1/events?after=3");
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toContain("text/event-stream");
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/runs/r1/events?after=3");
  expect(seen[0]?.method).toBe("GET");
  const text = await res.text();
  expect(text).toContain('id: 4\ndata: {"type":"text","delta":"Hel"}');
  expect(text).toContain('data: {"type":"text","delta":"lo"}');
  expect(text).not.toContain("id: abc");
  expect(text).not.toContain("triage");
  expect(text).toContain('id: 6\ndata: {"type":"done","status":"completed","messageId":"m1","text":"Hello"}');

  seen = [];
  reply = () => Response.json({ code: "ai_run_gone", message: "gone" }, { status: 410 });
  const gone = await call("ai/runs/r1/events");
  expect(gone.status).toBe(410);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/ai/runs/r1/events?after=0");
  expect((await call("ai/runs/../x/events")).status).not.toBe(200);
});

test("sessions: detail keeps assistant_step messages and the interrupted flag", async () => {
  reply = () =>
    Response.json({
      session: { id: SESSION_ID, channel: "website", kind: "ask", title: "Q", messageCount: 3, rating: null, active: false, createdAt: "t", updatedAt: "t" },
      messages: [
        { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", images: [], createdAt: "t" },
        { id: "m2", runId: "r1", seq: 2, role: "assistant_step", content: "Let me look", images: [], createdAt: "t", secret: 1 },
        { id: "m3", runId: "r1", seq: 3, role: "assistant", content: "Partial", images: [], interrupted: true, createdAt: "t" },
      ],
      issues: [],
    });
  const detail = (await (await call(`ai/sessions/${SESSION_ID}`)).json()) as { messages: unknown[] };
  expect(detail.messages).toEqual([
    { id: "m1", runId: "r1", seq: 1, role: "user", content: "Q", createdAt: "t", images: [] },
    { id: "m2", runId: "r1", seq: 2, role: "assistant_step", content: "Let me look", createdAt: "t", images: [] },
    { id: "m3", runId: "r1", seq: 3, role: "assistant", content: "Partial", interrupted: true, createdAt: "t", images: [] },
  ]);
});
