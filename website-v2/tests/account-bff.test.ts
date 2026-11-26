import { afterAll, beforeEach, expect, mock, test } from "bun:test";
import type { CookieJar, CookieOptions } from "../src/lib/account/session";

// 动态 import：base.ts 在模块加载时读 import.meta.env.BASE_URL（Bun 下即 process.env），
// astro:env/server 也须先 mock，二者都要在路由模块加载之前就位。
process.env.BASE_URL = "/";
mock.module("astro:env/server", () => ({ FLUXCLOUD_API_BASE: "http://cloud.test" }));
const { handleAccountRequest } = await import("../src/lib/account/routes");
const { resetRefreshCache, ACCESS_COOKIE, DEVICE_COOKIE, REFRESH_COOKIE, SIGNED_IN_COOKIE } = await import(
  "../src/lib/account/session"
);

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

interface Seen { url: string; method: string; headers: Record<string, string>; body: unknown }
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
    };
    seen.push(call);
    return reply(call);
  }) as typeof fetch;
});
afterAll(() => {
  globalThis.fetch = originalFetch;
  mock.restore();
});

const AUTH = (n: number) => ({
  accessToken: `at-${n}`,
  refreshToken: `rt-${n}`,
  expiresIn: 900,
  user: { id: "u1" },
  device: { deviceId: "x" },
});

const ME = {
  id: "u1", email: "a@example.com", nickname: "Ann", plan: "free", status: "active", originId: 10001,
  originIdChanged: false, membershipOrdinal: null, isInternal: true, hasPassword: true,
  createdAt: "2026-01-01T00:00:00Z", lastLoginAt: null, entitlements: { maxDevices: 10 }, currentPlan: null,
  purchaseCreditMinor: 0, activeSubscription: null,
};

function call(
  path: string,
  { method = "GET", body, jar = new Jar(), headers = {} }: { method?: string; body?: unknown; jar?: Jar; headers?: Record<string, string> } = {},
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
  });
  return handleAccountRequest(request, path, jar, "203.0.113.7");
}

const signedIn = () => new Jar({ [ACCESS_COOKIE]: "at-0", [REFRESH_COOKIE]: "rt-0", [SIGNED_IN_COOKIE]: "1" });

test("csrf: requests without the custom header or from another site are refused before any upstream call", async () => {
  const noHeader = new Request("https://site.example/api/account/me", { headers: { "Sec-Fetch-Site": "same-origin" } });
  expect((await handleAccountRequest(noHeader, "me", signedIn(), "ip")).status).toBe(403);
  const crossSite = await call("me", { jar: signedIn(), headers: { "Sec-Fetch-Site": "cross-site" } });
  expect(crossSite.status).toBe(403);
  expect(await crossSite.json()).toEqual({ code: "forbidden" });
  expect(seen).toHaveLength(0);
});

test("routing: unknown paths are 404 and known paths with another method are 405", async () => {
  expect((await call("sync/items")).status).toBe(404);
  expect((await call("me", { method: "DELETE" })).status).toBe(405);
  expect(seen).toHaveLength(0);
});

test("login: server injects the web device, writes HttpOnly session cookies and never returns tokens", async () => {
  reply = () => Response.json({ status: "ok", auth: AUTH(1) });
  const jar = new Jar();
  const res = await call("login", {
    method: "POST",
    jar,
    body: { account: " a@example.com ", password: " secret pw ", deviceId: "spoofed", devicePlatform: "windows" },
  });
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ status: "ok" });

  expect(seen).toHaveLength(1);
  expect(seen[0]?.url).toBe("http://cloud.test/api/v1/auth/login");
  expect(seen[0]?.headers["x-forwarded-for"]).toBe("203.0.113.7");
  const sent = seen[0]?.body as Record<string, string>;
  expect(sent.account).toBe("a@example.com");
  expect(sent.password).toBe(" secret pw ");
  expect(sent.devicePlatform).toBe("web");
  expect(sent.deviceId).toMatch(/^[0-9a-f-]{36}$/);
  expect(sent.deviceId).toBe(jar.get(DEVICE_COOKIE)?.value as string);

  expect(jar.store.get(REFRESH_COOKIE)?.value).toBe("rt-1");
  expect(jar.store.get(REFRESH_COOKIE)?.options).toMatchObject({ httpOnly: true, sameSite: "strict", path: "/api/account" });
  expect(jar.store.get(ACCESS_COOKIE)?.options?.maxAge).toBe(840);
  expect(jar.store.get(SIGNED_IN_COOKIE)?.options).toMatchObject({ httpOnly: false, path: "/" });
});

test("login: a new browser gets the verify step and keeps the same device id for login/verify", async () => {
  reply = () => Response.json({ status: "deviceVerificationRequired", ttlSeconds: 600, willReplaceDevices: false });
  const jar = new Jar();
  const first = await call("login", { method: "POST", jar, body: { account: "10001", password: "pw123456" } });
  expect(await first.json()).toEqual({ status: "verify", ttlSeconds: 600 });
  expect(jar.get(REFRESH_COOKIE)).toBeUndefined();

  reply = () => Response.json(AUTH(2));
  const verified = await call("login/verify", { method: "POST", jar, body: { account: "10001", password: "pw123456", code: "123456" } });
  expect(await verified.json()).toEqual({ status: "ok" });
  const [loginBody, verifyBody] = seen.map((s) => s.body as Record<string, string>);
  expect(verifyBody?.deviceId).toBe(loginBody?.deviceId as string);
  expect(seen[1]?.url).toBe("http://cloud.test/api/v1/auth/login/verify");
  expect(jar.get(REFRESH_COOKIE)?.value).toBe("rt-2");
});

test("me: forwards the bearer token and trims the profile to known fields", async () => {
  reply = () => Response.json(ME);
  const res = await call("me", { jar: signedIn() });
  expect(res.status).toBe(200);
  expect(res.headers.get("Cache-Control")).toBe("no-store");
  const body = await res.json();
  expect(body.isInternal).toBeUndefined();
  expect(body.email).toBe("a@example.com");
  expect(seen[0]?.headers.authorization).toBe("Bearer at-0");
});

test("refresh: concurrent requests with an expired access token share one refresh", async () => {
  reply = (c) => (c.url.endsWith("/auth/refresh") ? Response.json(AUTH(7)) : Response.json(ME));
  const jars = [new Jar({ [REFRESH_COOKIE]: "rt-0" }), new Jar({ [REFRESH_COOKIE]: "rt-0" }), new Jar({ [REFRESH_COOKIE]: "rt-0" })];
  const results = await Promise.all(jars.map((jar) => call("me", { jar })));
  expect(results.map((r) => r.status)).toEqual([200, 200, 200]);
  expect(seen.filter((s) => s.url.endsWith("/auth/refresh"))).toHaveLength(1);
  expect(seen.find((s) => s.url.endsWith("/auth/refresh"))?.body).toEqual({ refreshToken: "rt-0" });
  for (const jar of jars) expect(jar.get(REFRESH_COOKIE)?.value).toBe("rt-7");
  expect(seen.filter((s) => s.url.endsWith("/api/v1/me")).every((s) => s.headers.authorization === "Bearer at-7")).toBe(true);
});

test("refresh: an upstream 401 unauthorized is retried once with renewed tokens", async () => {
  let meCalls = 0;
  reply = (c) => {
    if (c.url.endsWith("/auth/refresh")) return Response.json(AUTH(3));
    meCalls += 1;
    return meCalls === 1 ? Response.json({ code: "unauthorized" }, { status: 401 }) : Response.json(ME);
  };
  const jar = signedIn();
  const res = await call("me", { jar });
  expect(res.status).toBe(200);
  expect(seen.map((s) => s.headers.authorization ?? "refresh")).toEqual(["Bearer at-0", "refresh", "Bearer at-3"]);
  expect(jar.get(ACCESS_COOKIE)?.value).toBe("at-3");
});

test("refresh: a revoked session clears the cookies and reports unauthorized", async () => {
  reply = () => Response.json({ code: "unauthorized" }, { status: 401 });
  const jar = signedIn();
  const res = await call("me", { jar });
  expect(res.status).toBe(401);
  expect(await res.json()).toEqual({ code: "unauthorized" });
  expect(jar.get(REFRESH_COOKIE)).toBeUndefined();
  expect(jar.get(SIGNED_IN_COOKIE)).toBeUndefined();
});

test("business 401s such as invalid_code pass through without touching the session", async () => {
  reply = () => Response.json({ code: "invalid_code" }, { status: 401 });
  const jar = signedIn();
  const res = await call("me/password", { method: "POST", jar, body: { newPassword: "new-password", code: "000000" } });
  expect(res.status).toBe(401);
  expect(await res.json()).toEqual({ code: "invalid_code" });
  expect(seen).toHaveLength(1);
  expect(jar.get(REFRESH_COOKIE)?.value).toBe("rt-0");
});

test("me/password: exactly one of currentPassword or code is required", async () => {
  const both = await call("me/password", { method: "POST", jar: signedIn(), body: { newPassword: "new-password", currentPassword: "old", code: "1" } });
  const neither = await call("me/password", { method: "POST", jar: signedIn(), body: { newPassword: "new-password" } });
  expect([both.status, neither.status]).toEqual([400, 400]);
  expect(seen).toHaveLength(0);
});

test("logout: revokes upstream then clears; an unreachable upstream keeps the session for a retry", async () => {
  reply = () => new Response(null, { status: 204 });
  const jar = signedIn();
  expect((await call("logout", { method: "POST", jar })).status).toBe(204);
  expect(seen[0]?.body).toEqual({ refreshToken: "rt-0" });
  expect(jar.get(REFRESH_COOKIE)).toBeUndefined();

  reply = () => Response.json({ code: "upstream_error" }, { status: 503 });
  const kept = signedIn();
  expect((await call("logout", { method: "POST", jar: kept })).status).toBe(503);
  expect(kept.get(REFRESH_COOKIE)?.value).toBe("rt-0");
});

test("password/reset: success clears this browser's session because all sessions were revoked", async () => {
  reply = () => new Response(null, { status: 204 });
  const jar = signedIn();
  const res = await call("password/reset", { method: "POST", jar, body: { email: "a@example.com", code: "123456", newPassword: "another-pass" } });
  expect(res.status).toBe(204);
  expect(seen[0]?.body).toEqual({ email: "a@example.com", code: "123456", newPassword: "another-pass" });
  expect(jar.get(REFRESH_COOKIE)).toBeUndefined();
});

test("devices: list is trimmed and the row id is validated before deletion", async () => {
  reply = () => Response.json({ devices: [{ id: "d1", deviceId: "secret", name: "PC", defaultSaveDir: "C:\\Users\\x", isOnline: true }] });
  const list = await call("devices", { jar: signedIn() });
  expect(await list.json()).toEqual({ devices: [{ id: "d1", name: "PC", isOnline: true }] });

  const bad = await call("devices/..%2Fme", { method: "DELETE", jar: signedIn() });
  expect(bad.status).toBe(400);
  expect(seen).toHaveLength(1);
});
