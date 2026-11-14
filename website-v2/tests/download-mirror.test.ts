import { afterAll, beforeEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import edge from "../cdn/esa-dl-cn.js";
import {
  checkMirrorQuota,
  MIRROR_BYTES_PER_IP,
  MIRROR_FILES_PER_IP,
  MIRROR_REISSUES_PER_FILE,
  mirrorNetworkId,
  recordMirrorLink,
  resetReleaseViewers,
} from "../src/lib/download-gate";

const HOUR = 3600 * 1000;
/** 北京时间 2026-11-13 00:00 之后 1 小时。 */
const T0 = Date.UTC(2026, 10, 12, 17);

beforeEach(() => resetReleaseViewers());

test("network id is the IPv4 /24 in hex; IPv6 and malformed addresses have none", () => {
  expect(mirrorNetworkId("114.114.114.114")).toBe("727272");
  expect(mirrorNetworkId("1.2.3.255")).toBe("010203");
  expect(mirrorNetworkId("::ffff:10.0.255.1")).toBe("0a00ff");
  for (const ip of ["2408:8207::1", "256.1.1.1", "1.2.3", "unknown", ""]) expect(mirrorNetworkId(ip)).toBeNull();
});

test("per-IP quota caps distinct files and bytes; re-signing a granted file only counts reissues", () => {
  for (let i = 0; i < MIRROR_FILES_PER_IP; i++) {
    expect(checkMirrorQuota("1.1.1.1", `f${i}`, 10, false, 0, T0)).toBe("ok");
    recordMirrorLink("1.1.1.1", `f${i}`, 10, T0);
  }
  expect(checkMirrorQuota("1.1.1.1", "another", 10, false, 0, T0)).toBe("ip-quota");
  // 已签过的文件仍可续签，直到重签上限。
  for (let i = 1; i < MIRROR_REISSUES_PER_FILE; i++) {
    expect(checkMirrorQuota("1.1.1.1", "f0", 10, false, 0, T0)).toBe("ok");
    recordMirrorLink("1.1.1.1", "f0", 10, T0);
  }
  expect(checkMirrorQuota("1.1.1.1", "f0", 10, false, 0, T0)).toBe("ip-quota");
  // 24 小时窗口从首次签发算起，过期后重新计。
  expect(checkMirrorQuota("1.1.1.1", "another", 10, false, 0, T0 + 24 * HOUR)).toBe("ok");
  // 字节配额：单个超大文件或累计超额都挡。
  expect(checkMirrorQuota("2.2.2.2", "big", MIRROR_BYTES_PER_IP + 1, false, 0, T0)).toBe("ip-quota");
  recordMirrorLink("2.2.2.2", "half", MIRROR_BYTES_PER_IP / 2, T0);
  expect(checkMirrorQuota("2.2.2.2", "rest", MIRROR_BYTES_PER_IP / 2, false, 0, T0)).toBe("ok");
  expect(checkMirrorQuota("2.2.2.2", "over", MIRROR_BYTES_PER_IP / 2 + 1, false, 0, T0)).toBe("ip-quota");
});

test("daily budget counts first grants only, reserves 20% for browsers and resets at Beijing midnight", () => {
  recordMirrorLink("1.1.1.1", "a", 70, T0);
  recordMirrorLink("1.1.1.1", "a", 70, T0); // 重签不计预算
  expect(checkMirrorQuota("2.2.2.2", "b", 10, true, 100, T0)).toBe("ok");
  expect(checkMirrorQuota("2.2.2.2", "b", 11, true, 100, T0)).toBe("budget");
  expect(checkMirrorQuota("2.2.2.2", "b", 30, false, 100, T0)).toBe("ok");
  expect(checkMirrorQuota("2.2.2.2", "b", 31, false, 100, T0)).toBe("budget");
  // 北京时间次日 0 点（T0 + 23h）预算清零；UTC 0 点（T0 + 7h）不清。
  expect(checkMirrorQuota("2.2.2.2", "b", 31, false, 100, T0 + 7 * HOUR)).toBe("budget");
  expect(checkMirrorQuota("2.2.2.2", "b", 31, false, 100, T0 + 23 * HOUR)).toBe("ok");
});

const SECRET = "edge-secret";
const ENV = { CN_CDN_AUTH_KEY: SECRET, DOWNLOAD_URL_TTL_SEC: "10800" };
const PATH = "/FluxDownRelease/v0.6.7/app/FluxDown-0.6.7-windows-x64-setup.exe";
const originalFetch = globalThis.fetch;
afterAll(() => { globalThis.fetch = originalFetch; });

/** 与 src/lib/oss.ts::cdnDownloadUrl 同一签名串；网段取自 mirrorNetworkId，验证两端网段规则一致。 */
function signed(ip: string, issued = Math.floor(Date.now() / 1000)): string {
  const network = mirrorNetworkId(ip);
  const hash = createHash("md5").update(`${PATH}-${issued}-r4nd-${network}-${SECRET}`).digest("hex");
  return `https://dl-cn.fluxdown.com${PATH}?auth_key=${issued}-r4nd-${network}-${hash}`;
}

async function edgeStatus(url: string, clientIp: string | null, extra: Record<string, string> = {}): Promise<number> {
  globalThis.fetch = (async () => new Response("body", { status: 200 })) as unknown as typeof fetch;
  const headers: Record<string, string> = { ...extra };
  if (clientIp !== null) headers["ali-cdn-real-ip"] = clientIp;
  const response = await edge.fetch(new Request(url, { headers }), {}, ENV);
  return response.status;
}

test("edge accepts a link only inside the /24 it was signed for, by ESA's client IP header", async () => {
  const url = signed("114.114.114.114");
  expect(await edgeStatus(url, "114.114.114.114")).toBe(200);
  expect(await edgeStatus(url, "114.114.114.7")).toBe(200);
  expect(await edgeStatus(url, "114.114.115.114")).toBe(403);
  expect(await edgeStatus(url, "2408:8207::1")).toBe(403);
  expect(await edgeStatus(url, null)).toBe(403);
  // 访客可控的 ali-real-client-ip 不参与判定。
  expect(await edgeStatus(url, "8.8.8.8", { "ali-real-client-ip": "114.114.114.114" })).toBe(403);
  // 旧格式（uid 0）、改网段重签不出合法 md5、过期链接一律 403。
  expect(await edgeStatus(url.replace("-727272-", "-0-"), "114.114.114.114")).toBe(403);
  expect(await edgeStatus(url.replace("-727272-", "-727273-"), "114.114.115.114")).toBe(403);
  expect(await edgeStatus(signed("114.114.114.114", Math.floor(Date.now() / 1000) - 10801), "114.114.114.114")).toBe(403);
});
