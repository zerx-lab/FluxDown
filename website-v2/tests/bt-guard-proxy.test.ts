import { afterAll, beforeEach, expect, mock, test } from "bun:test";
import {
  btGuardErrorKey, formatExpiry, MAX_APPEAL_CONTACT, MAX_APPEAL_DETAIL, parseAppealRequest, parseSelf,
} from "../src/lib/bt-guard";

let base = "http://cloud.test";
const configure = () => mock.module("astro:env/server", () => ({
  GITHUB_TOKEN: "", GITHUB_REPO: "owner/repo", FLUXCLOUD_API_BASE: base, DOWNLOAD_PASS_SECRET: undefined,
}));
configure();
// 动态 import：路由模块须在 astro:env/server 被 mock 之后才加载。
const { GET } = await import("../src/pages/api/cloud/bt-guard/self");
const { POST } = await import("../src/pages/api/cloud/bt-guard/appeal");
const originalFetch = globalThis.fetch;

const SELF = {
  listed: true,
  ipGroup: "2001:db8::/64",
  entries: [{ reason: "多方共识", category: "consensus", action: "ban", expiresAt: 1763100000 }],
  turnstileSiteKey: "0x4AAA",
  appealPending: false,
};

interface Seen { url: string; method: string; headers: Record<string, string>; body: string }
let seen: Seen[] = [];
let reply: () => Response | Promise<Response> = () => Response.json(SELF);

beforeEach(() => {
  base = "http://cloud.test";
  configure();
  seen = [];
  reply = () => Response.json(SELF);
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    seen.push({
      url: String(input),
      method: init?.method ?? "GET",
      headers: Object.fromEntries(new Headers(init?.headers).entries()),
      body: typeof init?.body === "string" ? init.body : "",
    });
    return reply();
  }) as typeof fetch;
});
afterAll(() => { globalThis.fetch = originalFetch; mock.restore(); });

const self = (ip = "203.0.113.7") =>
  GET({
    request: new Request("https://site.example/api/cloud/bt-guard/self", { headers: { "x-client-ip": ip } }),
    clientAddress: "10.0.0.1",
  } as unknown as Parameters<typeof GET>[0]);

const appeal = (body: string, headers: Record<string, string> = {}) =>
  POST({
    request: new Request("https://site.example/api/cloud/bt-guard/appeal", {
      method: "POST", body, headers: { "x-client-ip": "203.0.113.7", ...headers },
    }),
    clientAddress: "10.0.0.1",
  } as unknown as Parameters<typeof POST>[0]);

const expectPrivate = (response: Response) => {
  expect(response.headers.get("X-Robots-Tag")).toBe("noindex, nofollow");
  expect(response.headers.get("Cache-Control")).toBe("no-store");
  expect(response.headers.get("Content-Type")).toContain("application/json");
};

test("self: success is forwarded with the visitor ip and trimmed to known fields", async () => {
  reply = () => Response.json({ ...SELF, internalNote: "secret", entries: [{ ...SELF.entries[0], evidence: "x" }] });
  const response = await self("198.51.100.9");
  expect(response.status).toBe(200);
  expectPrivate(response);
  expect(await response.json()).toEqual(SELF);
  expect(seen).toHaveLength(1);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/bt-guard/public/self");
  expect(seen[0]?.method).toBe("GET");
  expect(seen[0]?.headers["x-forwarded-for"]).toBe("198.51.100.9");
});

test("self: falls back to the socket address when no client ip header is present", async () => {
  await GET({
    request: new Request("https://site.example/api/cloud/bt-guard/self"),
    clientAddress: "10.0.0.1",
  } as unknown as Parameters<typeof GET>[0]);
  expect(seen[0]?.headers["x-forwarded-for"]).toBe("10.0.0.1");
});

test("self: not listed is a plain answer without entries", async () => {
  reply = () => Response.json({ listed: false, ipGroup: "203.0.113.7", entries: [{ junk: 1 }].slice(1) });
  const body = await (await self()).json();
  expect(body).toEqual({ listed: false, ipGroup: "203.0.113.7", entries: [], appealPending: false });
});

test("self: FluxCloud errors pass through with the same status and code", async () => {
  reply = () => Response.json({ code: "rate_limited", message: "slow down" }, { status: 429, headers: { "Retry-After": "30" } });
  const limited = await self();
  expect(limited.status).toBe(429);
  expect(limited.headers.get("Retry-After")).toBe("30");
  expect(await limited.json()).toEqual({ code: "rate_limited", message: "slow down" });

  reply = () => new Response("<html>bad gateway</html>", { status: 503 });
  const down = await self();
  expect(down.status).toBe(503);
  expect(await down.json()).toEqual({ code: "upstream_error" });
});

test("self: malformed upstream success, unreachable upstream and missing config are 502", async () => {
  for (const bad of [
    { listed: "yes" },
    { listed: true, entries: [] },
    { listed: true, entries: [{ reason: "r", category: "x", action: "ban", expiresAt: 1 }] },
    { listed: true, entries: [{ reason: "r", category: "community", action: "ban", expiresAt: "soon" }] },
    { listed: false, entries: "nope" },
    [],
  ]) {
    reply = () => Response.json(bad);
    const response = await self();
    expect(response.status).toBe(502);
    expect(await response.json()).toEqual({ code: "upstream_invalid" });
  }

  reply = () => new Response("not json", { status: 200 });
  expect((await self()).status).toBe(502);

  reply = () => { throw new Error("connect refused"); };
  const unreachable = await self();
  expect(unreachable.status).toBe(502);
  expect(await unreachable.json()).toEqual({ code: "upstream_unreachable" });

  base = "";
  configure();
  seen = [];
  const unconfigured = await self();
  expect(unconfigured.status).toBe(502);
  expect(await unconfigured.json()).toEqual({ code: "not_configured" });
  expect(seen).toHaveLength(0);
});

test("parseSelf: caps entries and reason length, drops empty optional fields", () => {
  const entries = Array.from({ length: 30 }, () => ({ ...SELF.entries[0], reason: "x".repeat(500) }));
  const parsed = parseSelf({ listed: true, entries, turnstileSiteKey: "", ipGroup: "" });
  expect(parsed?.entries).toHaveLength(20);
  expect(parsed?.entries[0]?.reason).toHaveLength(300);
  expect(parsed).not.toHaveProperty("turnstileSiteKey");
  expect(parsed).not.toHaveProperty("ipGroup");
  expect(parseSelf({ listed: false, appealPending: true, entries: SELF.entries })?.entries).toEqual([]);
  expect(parseSelf({ listed: true, entries: [{ ...SELF.entries[0], expiresAt: -1 }] })).toBeNull();
  expect(parseSelf({ listed: true, entries: [{ ...SELF.entries[0], expiresAt: 1.5 }] })).toBeNull();
});

test("appeal: forwards detail, contact and the turnstile token; 204 passes through", async () => {
  reply = () => new Response(null, { status: 204 });
  const response = await appeal(JSON.stringify({
    detail: "  shared campus network  ", contact: "me@example.com", turnstileToken: "tok", extra: "ignored",
  }));
  expect(response.status).toBe(204);
  expect(response.headers.get("X-Robots-Tag")).toBe("noindex, nofollow");
  expect(response.headers.get("Cache-Control")).toBe("no-store");
  expect(seen).toHaveLength(1);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/bt-guard/public/appeal");
  expect(seen[0]?.method).toBe("POST");
  expect(seen[0]?.headers["x-forwarded-for"]).toBe("203.0.113.7");
  expect(seen[0]?.headers["content-type"]).toBe("application/json");
  expect(JSON.parse(seen[0]?.body ?? "")).toEqual({
    detail: "shared campus network", contact: "me@example.com", turnstileToken: "tok",
  });
});

test("appeal: optional fields that are blank are not sent", async () => {
  reply = () => new Response(null, { status: 204 });
  await appeal(JSON.stringify({ detail: "mistake", contact: "   ", turnstileToken: "" }));
  expect(JSON.parse(seen[0]?.body ?? "")).toEqual({ detail: "mistake" });
});

test("appeal: FluxCloud errors pass through (not listed, pending, turnstile, rate limit)", async () => {
  const cases: Array<[number, string]> = [
    [404, "not_listed"], [409, "appeal_pending"], [403, "turnstile_required"], [400, "turnstile_failed"], [429, "rate_limited"],
  ];
  for (const [status, code] of cases) {
    reply = () => Response.json({ code, message: "m" }, { status });
    const response = await appeal(JSON.stringify({ detail: "d" }));
    expect(response.status).toBe(status);
    expectPrivate(response);
    expect(await response.json()).toEqual({ code, message: "m" });
  }
  reply = () => { throw new Error("down"); };
  const down = await appeal(JSON.stringify({ detail: "d" }));
  expect(down.status).toBe(502);
  expect(await down.json()).toEqual({ code: "upstream_unreachable" });
});

test("appeal: bad input is rejected with 422 before FluxCloud is called", async () => {
  const bad = [
    "not json",
    "[]",
    "null",
    JSON.stringify({}),
    JSON.stringify({ detail: "" }),
    JSON.stringify({ detail: "   " }),
    JSON.stringify({ detail: 5 }),
    JSON.stringify({ detail: "d".repeat(MAX_APPEAL_DETAIL + 1) }),
    JSON.stringify({ detail: "d", contact: "not-an-email" }),
    JSON.stringify({ detail: "d", contact: `${"a".repeat(MAX_APPEAL_CONTACT)}@example.com` }),
    JSON.stringify({ detail: "d", contact: 7 }),
    JSON.stringify({ detail: "d", turnstileToken: "t".repeat(2049) }),
    JSON.stringify({ detail: "d", turnstileToken: 1 }),
  ];
  for (const body of bad) {
    const response = await appeal(body);
    expect(response.status).toBe(422);
    expect(await response.json()).toEqual({ code: "validation_error" });
  }
  expect(seen).toHaveLength(0);
});

const streamed = (chunks: string[], extra: Record<string, string> = {}) => {
  const encoder = new TextEncoder();
  let sent = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      const chunk = chunks[sent];
      if (chunk === undefined) controller.close();
      else controller.enqueue(encoder.encode(chunk));
      sent += 1;
    },
  });
  const request = new Request("https://site.example/api/cloud/bt-guard/appeal", {
    method: "POST", body: stream, headers: { "x-client-ip": "203.0.113.7", ...extra },
    duplex: "half",
  } as RequestInit);
  return { request, sent: () => sent };
};

test("appeal: an oversized chunked body without content-length is aborted with 413", async () => {
  const { request, sent } = streamed(Array.from({ length: 1000 }, () => "x".repeat(4096)));
  expect(request.headers.get("content-length")).toBeNull();
  const response = await POST({ request, clientAddress: "10.0.0.1" } as unknown as Parameters<typeof POST>[0]);
  expect(response.status).toBe(413);
  expect(await response.json()).toEqual({ code: "payload_too_large" });
  // 读到上限附近就取消，而不是把 4 MiB 全读完。
  expect(sent()).toBeLessThan(100);
  expect(seen).toHaveLength(0);
});

test("appeal: a lying content-length does not bypass the cap, and small chunked bodies work", async () => {
  const big = await appeal("x".repeat(20000), { "content-length": "10" });
  expect(big.status).toBe(413);

  reply = () => new Response(null, { status: 204 });
  const { request } = streamed(['{"detail":', '"split ', 'across chunks"}']);
  const ok = await POST({ request, clientAddress: "10.0.0.1" } as unknown as Parameters<typeof POST>[0]);
  expect(ok.status).toBe(204);
  expect(JSON.parse(seen[0]?.body ?? "")).toEqual({ detail: "split across chunks" });
});

test("appeal/self: 503 from FluxCloud (turnstile not configured) passes through and maps to a friendly key", async () => {
  reply = () => Response.json({ code: "appeal_unavailable", message: "m" }, { status: 503 });
  const response = await appeal(JSON.stringify({ detail: "d" }));
  expect(response.status).toBe(503);
  expect(await response.json()).toEqual({ code: "appeal_unavailable", message: "m" });
  expect(btGuardErrorKey(503, "appeal_unavailable")).toBe("unavailable");
  expect(btGuardErrorKey(503, undefined)).toBe("unavailable");
  expect(btGuardErrorKey(503, "upstream_error")).toBe("unavailable");
});

test("appeal: boundary lengths are accepted", async () => {
  reply = () => new Response(null, { status: 204 });
  const contact = `${"a".repeat(MAX_APPEAL_CONTACT - "@e.co".length)}@e.co`;
  expect(contact).toHaveLength(MAX_APPEAL_CONTACT);
  const response = await appeal(JSON.stringify({
    detail: "d".repeat(MAX_APPEAL_DETAIL), contact, turnstileToken: "t".repeat(2048),
  }));
  expect(response.status).toBe(204);
  expect(parseAppealRequest(JSON.stringify({ detail: "d" }))).toEqual({ ok: true, detail: "d" });
});

test("btGuardErrorKey: code wins over status, status is the fallback", () => {
  expect(btGuardErrorKey(200, "not_listed")).toBe("notListed");
  expect(btGuardErrorKey(500, "appeal_pending")).toBe("appealPending");
  expect(btGuardErrorKey(403, "turnstile_required")).toBe("turnstileRequired");
  expect(btGuardErrorKey(403, "turnstile_failed")).toBe("turnstileFailed");
  expect(btGuardErrorKey(500, "rate_limited")).toBe("rateLimited");
  expect(btGuardErrorKey(404, undefined)).toBe("notListed");
  expect(btGuardErrorKey(409, "weird")).toBe("appealPending");
  expect(btGuardErrorKey(429, undefined)).toBe("rateLimited");
  expect(btGuardErrorKey(502, "upstream_invalid")).toBe("generic");
});

test("formatExpiry: formats unix seconds, rejects out-of-range values", () => {
  expect(formatExpiry(1763100000, "en")).toContain("2025");
  expect(formatExpiry(1763100000, "zh")).toContain("2025");
  expect(formatExpiry(Number.MAX_SAFE_INTEGER, "en")).toBe("—");
});
