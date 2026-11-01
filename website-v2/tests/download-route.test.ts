import { afterAll, beforeEach, expect, mock, test } from "bun:test";
import { bustApiCaches } from "../src/lib/api-cache";
import { markReleaseViewer, recordUpdateAssets, resetReleaseViewers } from "../src/lib/download-gate";
import { DOWNLOAD_PASS_COOKIE, issueDownloadPass } from "../src/lib/download-pass";

let configured = true;
let cdnConfigured = true;
const MAINLAND_IP = "114.114.114.114";
const MAINLAND_IPV6 = "2408:8207:78ae:7a51:f55e:4f2f:261d:ccc9";
/** 反代按边缘国家码（ESA Ali-Ip-Country / CF-IPCountry）写入的 X-Client-Country。 */
const COUNTRY: Record<string, string> = {
  [MAINLAND_IP]: "CN", [MAINLAND_IPV6]: "CN", "8.8.8.8": "US", "1.1.1.1": "HK",
};
const signatures: string[] = [];
const PASS_SECRET = "pass-secret";
// 与 bt-guard-proxy.test.ts / download-pass.test.ts 共用同一进程的模块表：bun 的 mock.module 首次注册后导出名集合即固定，此处必须含全部 astro:env/server 导出。
mock.module("astro:env/server", () => ({
  GITHUB_TOKEN: "", GITHUB_REPO: "owner/repo", FLUXCLOUD_API_BASE: "", DOWNLOAD_PASS_SECRET: PASS_SECRET,
}));
const ossMock = () => ({
  ossConfigured: configured,
  cdnConfigured,
  cdnDownloadUrl: (key: string) => {
    signatures.push("CDN");
    return `https://cdn.example/${key}?auth_key=signed`;
  },
  releaseObjectKey: (tag: string, name: string) => `${tag}/${name}`,
  presignOssHeadUrl: (key: string) => {
    signatures.push("HEAD");
    return `https://oss.example/${key}`;
  },
});
mock.module("../src/lib/oss", ossMock);
let pan123Configured = false;
let pan123Link: string | null = "https://pan.example/app.zip";
const pan123Mock = () => ({
  pan123Configured,
  pan123DownloadUrl: async (key: string, size: number) => {
    signatures.push(`PAN123 ${key} ${size}`);
    return pan123Link;
  },
});
mock.module("../src/lib/pan123", pan123Mock);
const { GET } = await import("../src/pages/api/download/[filename]");
const originalFetch = globalThis.fetch;
let draft = false;
let prerelease = false;
let status = 200;
let headSize = "12";
let requests: string[] = [];
let sequence = 0;

beforeEach(() => {
  bustApiCaches();
  resetReleaseViewers();
  markReleaseViewer(MAINLAND_IP);
  configured = true;
  cdnConfigured = true;
  mock.module("../src/lib/oss", ossMock);
  pan123Configured = false;
  pan123Link = "https://pan.example/app.zip";
  mock.module("../src/lib/pan123", pan123Mock);
  prerelease = false;
  status = 200;
  headSize = "12";
  signatures.length = 0;
  requests = [];
  sequence++;
  globalThis.fetch = (async (input, init) => {
    const url = String(input);
    requests.push(url);
    if (init?.method === "HEAD") return new Response(null, { headers: { "content-length": headSize } });
    if (status !== 200) return new Response(null, { status });
    const release = { tag_name: `v${sequence}`, draft, prerelease, assets: [{
      name: "app.zip", size: 12,
      browser_download_url: `https://github.com/owner/repo/releases/download/v${sequence}/app.zip`,
    }] };
    return Response.json(url.includes("/tags/") ? release : [release]);
  }) as typeof fetch;
});
afterAll(() => { globalThis.fetch = originalFetch; mock.restore(); });

/** 签发给 `ip` 的验证码通行证 Cookie（浏览器经 POST /api/download/pass 换取）。 */
function pass(ip = MAINLAND_IP, now = Date.now()): string {
  return `${DOWNLOAD_PASS_COOKIE}=${issueDownloadPass(PASS_SECRET, ip, now)}`;
}

/** 缺省模拟读过发布清单的大陆访客；clientIp 传空串时不带 X-Client-IP（按 TCP 对端 10.0.0.1）。 */
async function get(
  source = "", tag = `v${sequence}`, filename = "app.zip", clientIp = MAINLAND_IP, userAgent = "", cookie = "",
) {
  const url = new URL(`https://site.example/api/download/${filename}`);
  if (source) url.searchParams.set("source", source);
  if (tag) url.searchParams.set("tag", tag);
  const headers: Record<string, string> = {};
  if (clientIp) headers["x-client-ip"] = clientIp;
  if (COUNTRY[clientIp]) headers["x-client-country"] = COUNTRY[clientIp];
  if (userAgent) headers["user-agent"] = userAgent;
  if (cookie) headers.cookie = cookie;
  const request = new Request(url, { headers });
  return GET({ params: { filename }, url, request, clientAddress: "10.0.0.1" } as Parameters<typeof GET>[0]);
}

test("mainland is the edge's country code, so IPv6 visitors count and HK / unknown do not", async () => {
  for (const [ip, expected] of [[MAINLAND_IPV6, "cdn"], ["1.1.1.1", "github"], ["10.9.9.9", "github"]]) {
    markReleaseViewer(ip);
    const response = await get("auto", `v${sequence}`, "app.zip", ip, "Mozilla/5.0", pass(ip));
    expect(response.headers.get("X-Download-Source")).toBe(expected);
  }
});

test("github never signs or probes OSS, even for mainland visitors", async () => {
  const response = await get("github", `v${sequence}`, "app.zip", MAINLAND_IP);
  expect(response.status).toBe(302);
  expect(response.headers.get("X-Download-Source")).toBe("github");
  expect(signatures).toEqual([]);
  expect(requests.length).toBe(1);
});

test("visitors outside mainland China go straight to GitHub without probing", async () => {
  for (const source of ["auto", "", "unknown"]) {
    const response = await get(source, `v${sequence}`, "app.zip", "8.8.8.8");
    expect(response.headers.get("X-Download-Source")).toBe("github");
  }
  // 无 X-Client-IP 时按 TCP 对端判定，同样不是大陆。
  expect((await get("auto", `v${sequence}`, "app.zip", "")).headers.get("X-Download-Source")).toBe("github");
  expect(signatures).toEqual([]);
});

test("mainland visitors get a verified OSS asset through the signed CDN, else GitHub", async () => {
  for (const source of ["auto", ""]) {
    const response = await get(source, `v${sequence}`, "app.zip", MAINLAND_IP);
    expect(response.headers.get("X-Download-Source")).toBe("cdn");
    expect(response.headers.get("Location")).toBe(`https://cdn.example/v${sequence}/app.zip?auth_key=signed`);
  }
  // HEAD 结果缓存：同一资产只探测一次。
  expect(signatures).toEqual(["HEAD", "CDN", "CDN"]);
  sequence++;
  headSize = "11";
  signatures.length = 0;
  const missing = await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP);
  expect(missing.headers.get("X-Download-Source")).toBe("github");
  expect(signatures).toEqual(["HEAD"]);
});

test("CDN routing is off until both the CDN key and OSS credentials are configured", async () => {
  for (const [cdn, oss] of [[false, true], [true, false]]) {
    cdnConfigured = cdn;
    configured = oss;
    mock.module("../src/lib/oss", ossMock);
    bustApiCaches();
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP)).headers.get("X-Download-Source")).toBe("github");
    expect((await get("oss")).status).toBe(503);
  }
  expect(signatures).toEqual([]);
});

test("forced OSS outside mainland China goes to GitHub instead of the paid CDN", async () => {
  const response = await get("oss", `v${sequence}`, "app.zip", "8.8.8.8");
  expect(response.headers.get("X-Download-Source")).toBe("github");
  expect(signatures).toEqual([]);
});

test("mainland requests that never read the release manifest or come from scripts go to GitHub", async () => {
  resetReleaseViewers();
  for (const source of ["auto", "", "oss"]) {
    expect((await get(source)).headers.get("X-Download-Source")).toBe("github");
  }
  markReleaseViewer(MAINLAND_IP);
  for (const ua of ["curl/8.7.1", "python-requests/2.32", "Go-http-client/2.0", "Wget/1.21"]) {
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, ua)).headers.get("X-Download-Source")).toBe("github");
  }
  expect(signatures).toEqual([]);
  // 应用内更新（含不带 User-Agent 的旧客户端）与带验证码通行证的浏览器照常走 CDN。
  for (const ua of ["fluxdown-agent/0.5.9", "fluxdown-android/0.5.9", ""]) {
    expect((await get("", `v${sequence}`, "app.zip", MAINLAND_IP, ua)).headers.get("X-Download-Source")).toBe("cdn");
  }
  expect((await get("", `v${sequence}`, "app.zip", MAINLAND_IP, "Mozilla/5.0 (Windows NT 10.0)", pass()))
    .headers.get("X-Download-Source")).toBe("cdn");
});

test("browsers need a captcha pass bound to their IP for the mirror; updaters never do", async () => {
  const browser = "Mozilla/5.0 (Windows NT 10.0)";
  const tampered = pass().replace(/.$/, (c) => (c === "A" ? "B" : "A"));
  for (const cookie of ["", pass("8.8.8.8"), tampered, `${DOWNLOAD_PASS_COOKIE}=`, `other=${pass().split("=")[1]}`]) {
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, browser, cookie)).headers.get("X-Download-Source"))
      .toBe("github");
  }
  // 不认识的非更新器 User-Agent 同样要通行证。
  expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, "SomeDownloader/1.0")).headers.get("X-Download-Source"))
    .toBe("github");
  expect(signatures).toEqual([]);
  expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, browser, `a=1; ${pass()}`))
    .headers.get("X-Download-Source")).toBe("cdn");
  // 通行证过期后回落 GitHub。
  const expired = pass(MAINLAND_IP, Date.now() - 30 * 60 * 1000);
  expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, browser, expired)).headers.get("X-Download-Source"))
    .toBe("github");
  // 自升级不带 Cookie 也照常拿镜像。
  for (const ua of ["fluxdown-agent/0.6.0", "fluxdown-android/1.2.3", ""]) {
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, ua)).headers.get("X-Download-Source")).toBe("cdn");
  }
});

test("reading the manifest admits the visitor for 24 hours", async () => {
  const originalNow = Date.now;
  let now = originalNow();
  Date.now = () => now;
  try {
    resetReleaseViewers();
    markReleaseViewer(MAINLAND_IP, now);
    now += 24 * 3600 * 1000 - 1;
    expect((await get("auto")).headers.get("X-Download-Source")).toBe("cdn");
    now += 2;
    bustApiCaches();
    expect((await get("auto")).headers.get("X-Download-Source")).toBe("github");
  } finally {
    Date.now = originalNow;
  }
});

test("size mismatch: mainland falls back to GitHub, forced OSS returns 503 without Location", async () => {
  headSize = "11";
  expect((await get("")).headers.get("X-Download-Source")).toBe("github");
  const response = await get("oss");
  expect(response.status).toBe(503);
  expect(response.headers.has("Location")).toBe(false);
  expect(signatures).toEqual(["HEAD"]);
});

/** 开启 123，并把当前 `v${sequence}/app.zip` 登记为更新清单资产（/api/release 生成清单时登记）。 */
function enablePan123(link: string | null = "https://pan.example/app.zip") {
  pan123Configured = true;
  pan123Link = link;
  mock.module("../src/lib/pan123", pan123Mock);
  recordUpdateAssets("stable", [{ tag: `v${sequence}`, name: "app.zip" }]);
}

test("mainland: only >=0.6.0 updaters get 123pan; browsers and UA-less old updaters keep dl-cn", async () => {
  enablePan123();
  for (const [ua, cookie] of [["Mozilla/5.0 (Windows NT 10.0)", pass()], ["", ""]]) {
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, ua, cookie)).headers.get("X-Download-Source"))
      .toBe("cdn");
  }
  expect(signatures).toEqual(["HEAD", "CDN", "CDN"]);
  signatures.length = 0;
  const response = await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, "fluxdown-agent/0.6.0");
  expect(response.headers.get("X-Download-Source")).toBe("pan123");
  expect(response.headers.get("Location")).toBe("https://pan.example/app.zip");
  // 123 不可用（缺资产 / 大小不符 / 超时）时大陆更新器回落 dl-cn 镜像，镜像也不行才是 GitHub。
  enablePan123(null);
  expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, "fluxdown-agent/0.6.0")).headers.get("X-Download-Source"))
    .toBe("cdn");
  expect(signatures).toEqual([`PAN123 v${sequence}/app.zip 12`, `PAN123 v${sequence}/app.zip 12`, "CDN"]);
  headSize = "11";
  sequence++;
  enablePan123(null);
  expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, "fluxdown-agent/0.6.0")).headers.get("X-Download-Source"))
    .toBe("github");
  expect((await get("oss", `v${sequence}`, "app.zip", MAINLAND_IP, "fluxdown-agent/0.6.0")).status).toBe(503);
});

test("outside mainland: 123pan needs a strict >=0.6.0 updater User-Agent", async () => {
  enablePan123();
  markReleaseViewer("8.8.8.8");
  for (const ua of ["fluxdown-agent/0.6.0", "fluxdown-android/0.6.0", "fluxdown-agent/0.6.0-rc.1", "fluxdown-android/1.0.0"]) {
    expect((await get("", `v${sequence}`, "app.zip", "8.8.8.8", ua)).headers.get("X-Download-Source")).toBe("pan123");
  }
  signatures.length = 0;
  for (const ua of [
    "", "Mozilla/5.0", "fluxdown-agent/0.5.9", "fluxdown-cli/0.6.0", "fluxdown-test/0.6.0",
    "fluxdown-agent/0.6", "fluxdown-agent/0.6.0 Mozilla/5.0", "Mozilla/5.0 fluxdown-agent/0.6.0",
  ]) {
    expect((await get("", `v${sequence}`, "app.zip", "8.8.8.8", ua)).headers.get("X-Download-Source")).toBe("github");
  }
  expect(signatures).toEqual([]);
  // 123 不可用时非大陆更新器直接 GitHub，不消耗付费镜像；source=oss 也不返回 503。
  enablePan123(null);
  expect((await get("oss", `v${sequence}`, "app.zip", "8.8.8.8", "fluxdown-agent/0.6.0")).headers.get("X-Download-Source"))
    .toBe("github");
  expect(signatures).toEqual([`PAN123 v${sequence}/app.zip 12`]);
});

test("123pan only signs assets in the current update manifest, within the per-IP quota", async () => {
  enablePan123();
  markReleaseViewer("8.8.8.8");
  const updater = "fluxdown-agent/0.6.0";
  // 历史版本 / 非更新组件不在清单：不签 123。
  expect((await get("", `v${sequence}`, "other.zip", "8.8.8.8", updater)).status).toBe(404);
  recordUpdateAssets("stable", [{ tag: `v${sequence + 100}`, name: "app.zip" }]);
  expect((await get("", `v${sequence}`, "app.zip", "8.8.8.8", updater)).headers.get("X-Download-Source")).toBe("github");
  expect(signatures).toEqual([]);
  // 另一渠道的清单同样有效。
  recordUpdateAssets("frontier", [{ tag: `v${sequence}`, name: "app.zip" }]);
  for (let i = 0; i < 10; i++) {
    expect((await get("", `v${sequence}`, "app.zip", "8.8.8.8", updater)).headers.get("X-Download-Source")).toBe("pan123");
  }
  // 同一 IP 24 小时内签满 10 条后回落：非大陆 GitHub，大陆 dl-cn。
  expect((await get("", `v${sequence}`, "app.zip", "8.8.8.8", updater)).headers.get("X-Download-Source")).toBe("github");
  expect((await get("", `v${sequence}`, "app.zip", MAINLAND_IP, updater)).headers.get("X-Download-Source")).toBe("pan123");
  expect(signatures.filter((s) => s.startsWith("PAN123")).length).toBe(11);
});

test("123pan obeys the download gate, source=github and stale metadata", async () => {
  const originalNow = Date.now;
  let now = originalNow();
  // 下载元数据缓存在首个请求时创建并捕获 Date.now：必须先替换。
  Date.now = () => now;
  try {
    resetReleaseViewers();
    enablePan123();
    const updater = "fluxdown-agent/0.6.0";
    expect((await get("", `v${sequence}`, "app.zip", "8.8.8.8", updater)).headers.get("X-Download-Source")).toBe("github");
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, updater)).headers.get("X-Download-Source")).toBe("github");
    markReleaseViewer(MAINLAND_IP, now);
    expect((await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, "curl/8.7.1")).headers.get("X-Download-Source"))
      .toBe("github");
    expect((await get("github", `v${sequence}`, "app.zip", MAINLAND_IP, updater)).headers.get("X-Download-Source"))
      .toBe("github");
    expect(signatures).toEqual([]);
    now += 120_001;
    status = 503;
    const fallback = await get("auto", `v${sequence}`, "app.zip", MAINLAND_IP, updater);
    expect(fallback.headers.get("X-Download-Source")).toBe("github");
    expect(fallback.headers.get("X-Download-Metadata")).toBe("stale");
    expect(signatures).toEqual([]);
  } finally {
    Date.now = originalNow;
  }
});

test("unconfigured OSS is explicit 503", async () => {
  configured = false;
  mock.module("../src/lib/oss", ossMock);
  expect((await get("oss")).status).toBe(503);
  expect(signatures).toEqual([]);
});

test("draft and missing assets never sign; explicit previews remain supported", async () => {
  draft = true;
  expect((await get("oss")).status).toBe(403);
  expect(signatures).toEqual([]);
  bustApiCaches();
  draft = false;
  prerelease = true;
  expect((await get("github")).status).toBe(302);
  expect((await get("oss", `v${sequence}`, "unknown.zip")).status).toBe(404);
  expect((await get("github", "")).status).toBe(404);
  expect(signatures).toEqual([]);
});

test("stale metadata only redirects to GitHub; webhook invalidation removes fallback", async () => {
  const originalNow = Date.now;
  let now = originalNow();
  Date.now = () => now;
  try {
    expect((await get("github")).status).toBe(302);
    now += 120_001;
    cdnConfigured = true;
    status = 503;
    const fallback = await get();
    expect(fallback.status).toBe(302);
    expect(fallback.headers.get("X-Download-Source")).toBe("github");
    expect(fallback.headers.get("X-Download-Metadata")).toBe("stale");
    expect((await get("auto")).headers.get("X-Download-Source")).toBe("github");
    expect((await get("oss")).status).toBe(503);
    expect(signatures).toEqual([]);
    bustApiCaches();
    expect((await get("github")).status).toBe(503);
  } finally {
    Date.now = originalNow;
  }
});

for (const failure of [
  new DOMException("body timed out", "TimeoutError"),
  new DOMException("body aborted", "AbortError"),
  new TypeError("terminated"),
]) {
  test(`response body ${failure.name} preserves stale GitHub fallback without OSS signing`, async () => {
    const originalNow = Date.now;
    let now = originalNow();
    Date.now = () => now;
    try {
      expect((await get("github")).status).toBe(302);
      now += 120_001;
      globalThis.fetch = (async () => new Response(new ReadableStream({
        start(controller) { controller.error(failure); },
      }))) as typeof fetch;
      const fallback = await get();
      expect(fallback.status).toBe(302);
      expect(fallback.headers.get("X-Download-Source")).toBe("github");
      expect(fallback.headers.get("X-Download-Metadata")).toBe("stale");
      expect((await get("oss")).status).toBe(503);
      expect(signatures).toEqual([]);
      globalThis.fetch = (async () => { throw new TypeError("network down"); }) as typeof fetch;
      expect((await get("github")).status).toBe(302);
      now += 480_000;
      expect((await get("github")).status).toBe(503);
    } finally {
      Date.now = originalNow;
    }
  });
}

for (const body of ["{broken JSON", JSON.stringify({ tag_name: "invalid" })]) {
  test(`complete invalid response does not use stale metadata: ${body}`, async () => {
    const originalNow = Date.now;
    let now = originalNow();
    Date.now = () => now;
    try {
      expect((await get("github")).status).toBe(302);
      now += 120_001;
      globalThis.fetch = (async () => new Response(body)) as typeof fetch;
      expect((await get("github")).status).toBe(500);
      globalThis.fetch = (async () => { throw new TypeError("network down"); }) as typeof fetch;
      expect((await get("github")).status).toBe(503);
      expect(signatures).toEqual([]);
    } finally {
      Date.now = originalNow;
    }
  });
}

test("canonical repository casing works for tag and latest without changing tag or filename casing", async () => {
  const tag = `V${sequence}`;
  const filename = "App.ZIP";
  const location = `https://github.com/Owner/Repo/releases/download/${tag}/${filename}`;
  globalThis.fetch = (async (input) => {
    const release = { tag_name: tag, draft: false, prerelease: false, assets: [{
      name: filename, size: 12, browser_download_url: location,
    }] };
    return Response.json(String(input).includes("/tags/") ? release : [release]);
  }) as typeof fetch;
  for (const requestedTag of [tag, ""]) {
    const response = await get("github", requestedTag, filename);
    expect(response.status).toBe(302);
    expect(response.headers.get("Location")).toBe(location);
  }
  expect((await get("github", tag, "app.zip")).status).toBe(404);
  expect((await get("github", tag.toLowerCase(), filename)).status).toBe(500);
  expect(signatures).toEqual([]);
});

test("404 remains authoritative and cold API outages cannot produce signatures", async () => {
  status = 404;
  expect((await get("oss")).status).toBe(404);
  bustApiCaches();
  status = 503;
  expect((await get("github")).status).toBe(503);
  expect((await get("oss")).status).toBe(503);
  expect(signatures).toEqual([]);
});
