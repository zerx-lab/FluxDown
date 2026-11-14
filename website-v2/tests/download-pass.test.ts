import { afterAll, beforeEach, expect, mock, test } from "bun:test";
import { DOWNLOAD_PASS_COOKIE, issueDownloadPass, verifyDownloadPass } from "../src/lib/download-pass";

const SECRET = "pass-secret";
const IP = "114.114.114.114";
let secret: string | undefined = SECRET;
// 与 download-route.test.ts / bt-guard-proxy.test.ts 共用同一进程的模块表：导出名集合须与它们一致。
const configure = () => mock.module("astro:env/server", () => ({
  GITHUB_TOKEN: "", GITHUB_REPO: "owner/repo", FLUXCLOUD_API_BASE: "", DOWNLOAD_PASS_SECRET: secret,
  CN_CDN_DAILY_BUDGET_GB: 0,
}));
configure();
// 动态 import：路由模块须在 astro:env/server 被 mock 之后才加载（静态 import 会先于 mock 求值）。
const { GET, POST } = await import("../src/pages/api/download/pass");

beforeEach(() => {
  secret = SECRET;
  configure();
});
afterAll(() => mock.restore());

interface Options {
  path?: string;
  edge?: string | null;
  param?: string | null;
  country?: string;
  cookie?: string;
}

function request(method: "GET" | "POST", { path = "/api/download/pass", edge = "1", param = "verified", country = "CN", cookie }: Options) {
  const url = new URL(`https://site.example${path}`);
  const headers: Record<string, string> = { "x-client-ip": IP, "x-client-country": country };
  if (edge !== null) headers["x-fluxdown-edge"] = edge;
  if (param !== null) headers["captcha-verify-param"] = param;
  if (cookie) headers.cookie = cookie;
  const context = { url, request: new Request(url, { method, headers }), clientAddress: "10.0.0.1" };
  return (method === "GET" ? GET : POST)(context as unknown as Parameters<typeof POST>[0]) as Promise<Response>;
}

test("edge-verified POST issues an IP-bound HttpOnly pass for the download path", async () => {
  const response = await request("POST", {});
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ expires_in: 1800 });
  const cookie = response.headers.get("set-cookie") ?? "";
  expect(cookie).toContain("Path=/api/download;");
  expect(cookie).toContain("HttpOnly");
  expect(cookie).toContain("SameSite=Lax");
  expect(cookie).toContain("Max-Age=1800");
  const token = /^fd_dl_pass=([^;]+);/.exec(cookie)?.[1] ?? null;
  expect(verifyDownloadPass(SECRET, token, IP)).toBe(true);
  expect(verifyDownloadPass(SECRET, token, "8.8.8.8")).toBe(false);
  expect(verifyDownloadPass("other-secret", token, IP)).toBe(false);
});

test("POST without edge verification, outside the captcha rule path or when disabled issues nothing", async () => {
  const rejected: [Options, number][] = [
    [{ edge: null }, 403],
    [{ edge: "0" }, 403],
    [{ param: null }, 403],
    [{ path: "/api/download/pass/" }, 404],
    [{ path: "/API/download/pass" }, 404],
  ];
  for (const [options, status] of rejected) {
    const response = await request("POST", options);
    expect(response.status).toBe(status);
    expect(response.headers.has("set-cookie")).toBe(false);
  }
  secret = undefined;
  configure();
  const disabled = await request("POST", {});
  expect(disabled.status).toBe(404);
  expect(disabled.headers.has("set-cookie")).toBe(false);
});

test("GET asks for the captcha only from mainland edge visitors without a valid pass", async () => {
  const cases: [Options, boolean][] = [
    [{}, true],
    [{ cookie: `${DOWNLOAD_PASS_COOKIE}=${issueDownloadPass(SECRET, IP)}` }, false],
    [{ cookie: `${DOWNLOAD_PASS_COOKIE}=${issueDownloadPass(SECRET, "8.8.8.8")}` }, true],
    [{ country: "US" }, false],
    [{ edge: null }, false],
  ];
  for (const [options, required] of cases) {
    expect(await (await request("GET", options)).json()).toEqual({ required });
  }
  secret = undefined;
  configure();
  expect(await (await request("GET", {})).json()).toEqual({ required: false });
});
