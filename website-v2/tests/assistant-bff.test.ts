import { afterAll, beforeEach, expect, mock, test } from "bun:test";
import { parseBlocks, parseInline } from "../src/components/assistant/md-parse";
import type { CookieJar, CookieOptions } from "../src/lib/account/session";

// 动态 import：base.ts 在模块加载时读 BASE_URL，astro:env/server 须先 mock，二者都要在路由模块加载之前就位。
process.env.BASE_URL = "/";
mock.module("astro:env/server", () => ({ FLUXCLOUD_API_BASE: "http://cloud.test" }));
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
  { method = "GET", body, signal, headers = {} }: { method?: string; body?: unknown; signal?: AbortSignal; headers?: Record<string, string> } = {},
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
  return handleAccountRequest(request, path.split("?")[0] ?? path, signedIn(), "203.0.113.7");
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
