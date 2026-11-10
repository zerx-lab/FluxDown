import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { SiteProfile } from "./site-profiles";
import { createStorageArea } from "./storage-mock.test-util";

const local = createStorageArea();
const sync = createStorageArea();
mock.module("wxt/browser", () => ({
  browser: { storage: { local, sync } },
}));
// wxt/browser 必须先 mock 再加载被测模块（与 i18n.test.ts 同一约定），故此处动态导入。
const { DEFAULT_SITE_PROFILE, loadSiteProfiles, normalizeSiteKey, profileFor, saveSiteProfile } = await import(
  "./site-profiles"
);
const { loadSettings } = await import("./settings");

function profile(overrides: Partial<SiteProfile> = {}): SiteProfile {
  return { ...DEFAULT_SITE_PROFILE, updatedAt: 1, ...overrides };
}

beforeEach(() => {
  local.data = {};
  sync.data = {};
});

describe("profileFor", () => {
  test("取最长的后缀命中项", () => {
    const profiles = {
      "example.com": profile({ intercept: "never" }),
      "video.example.com": profile({ intercept: "always" }),
    };
    expect(profileFor(profiles, "cdn.video.example.com").key).toBe("video.example.com");
    expect(profileFor(profiles, "www.example.com").key).toBe("example.com");
    expect(profileFor(profiles, "video.example.com").profile.intercept).toBe("always");
  });

  test("按标签边界匹配，notexample.com 不命中；无命中返回默认", () => {
    const profiles = { "example.com": profile({ intercept: "never" }) };
    const miss = profileFor(profiles, "notexample.com");
    expect(miss.key).toBeNull();
    expect(miss.profile).toEqual(DEFAULT_SITE_PROFILE);
  });

  test("key 不使用 = 前缀语义：=example.com 不会被当作仅本域", () => {
    const profiles = { "=example.com": profile({ intercept: "never" }) };
    expect(profileFor(profiles, "example.com").key).toBeNull();
  });

  test("IPv4 CIDR 命中网段内主机，网段外不命中", () => {
    const profiles = { "10.0.0.0/8": profile({ intercept: "never" }) };
    expect(profileFor(profiles, "10.1.2.3").key).toBe("10.0.0.0/8");
    expect(profileFor(profiles, "10.255.255.255").profile.intercept).toBe("never");
    expect(profileFor(profiles, "11.0.0.1").key).toBeNull();
    expect(profileFor(profiles, "example.com").key).toBeNull();
  });

  test("IPv4 网段边界：172.16.0.0/12 覆盖 172.16–172.31", () => {
    const profiles = { "172.16.0.0/12": profile({ intercept: "never" }) };
    expect(profileFor(profiles, "172.16.0.1").key).toBe("172.16.0.0/12");
    expect(profileFor(profiles, "172.31.255.254").key).toBe("172.16.0.0/12");
    expect(profileFor(profiles, "172.15.255.255").key).toBeNull();
    expect(profileFor(profiles, "172.32.0.1").key).toBeNull();
  });

  test("IPv6 CIDR 匹配方括号形态的 host；地址族不同不互相命中", () => {
    const profiles = {
      "fd00::/8": profile({ intercept: "never" }),
      "0.0.0.0/0": profile({ intercept: "always" }),
    };
    expect(profileFor(profiles, "[fd12:3456::1]").key).toBe("fd00::/8");
    expect(profileFor(profiles, "[fe80::1]").key).toBeNull();
    expect(profileFor(profiles, "8.8.8.8").key).toBe("0.0.0.0/0");
  });

  test("优先级：精确 host > 最长前缀网段 > 短前缀网段", () => {
    const profiles = {
      "10.0.0.0/8": profile({ intercept: "never" }),
      "10.1.0.0/16": profile({ intercept: "always" }),
      "10.1.2.3": profile({ saveDir: "/exact" }),
    };
    expect(profileFor(profiles, "10.1.2.3").key).toBe("10.1.2.3");
    expect(profileFor(profiles, "10.1.9.9").key).toBe("10.1.0.0/16");
    expect(profileFor(profiles, "10.2.0.1").key).toBe("10.0.0.0/8");
  });

  test("IP host 上网段优先于恰好是后缀的域名 key", () => {
    const profiles = {
      "2.3": profile({ intercept: "always" }),
      "1.0.0.0/8": profile({ intercept: "never" }),
    };
    expect(profileFor(profiles, "1.2.2.3").key).toBe("1.0.0.0/8");
  });

  test("原型链属性名不会被当作已存在的 key", () => {
    expect(profileFor({}, "constructor").key).toBeNull();
  });
});

describe("normalizeSiteKey", () => {
  test("CIDR 归一化为小写、清零主机位的规范写法", () => {
    expect(normalizeSiteKey(" 10.1.2.3/8 ")).toBe("10.0.0.0/8");
    expect(normalizeSiteKey("192.168.0.0/16")).toBe("192.168.0.0/16");
    expect(normalizeSiteKey("FD00:0:0::/8")).toBe("fd00::/8");
    expect(normalizeSiteKey("[fd00::1]/64")).toBe("fd00::/64");
    expect(normalizeSiteKey("::ffff:10.0.0.1/104")).toBe("::ffff:a00:0/104");
    expect(normalizeSiteKey("0.0.0.0/0")).toBe("0.0.0.0/0");
  });

  test("形如 CIDR 但非法时返回 null，不退化为域名", () => {
    expect(normalizeSiteKey("10.0.0.0/33")).toBeNull();
    expect(normalizeSiteKey("10.0.0/8")).toBeNull();
    expect(normalizeSiteKey("300.0.0.0/8")).toBeNull();
    expect(normalizeSiteKey("fd00::/129")).toBeNull();
    expect(normalizeSiteKey("10.0.0.0/")).toBe("10.0.0.0");
    // 数字点分样式仍判非法，不退化成 `normalizeDomain` 解析出的主机 1.2.0.3。
    expect(normalizeSiteKey("1.2.3/8")).toBeNull();
  });

  test("普通域名与带路径 / scheme 的输入保持原有行为", () => {
    expect(normalizeSiteKey("*.Example.com")).toBe("example.com");
    expect(normalizeSiteKey("https://Example.com/a/b")).toBe("example.com");
    expect(normalizeSiteKey("http://10.0.0.1/8")).toBe("10.0.0.1");
    expect(normalizeSiteKey("example.com/8")).toBe("example.com");
    // 由十六进制字母组成的域名带数字路径不是 CIDR。
    expect(normalizeSiteKey("add.de/12")).toBe("add.de");
    expect(normalizeSiteKey("dead.beef/1")).toBe("dead.beef");
  });
});

describe("存取", () => {
  test("host 经归一化存储，null 删除", async () => {
    await saveSiteProfile("https://Example.com/path", profile({ saveDir: " /data " }));
    const loaded = await loadSiteProfiles();
    expect(Object.keys(loaded)).toEqual(["example.com"]);
    expect(loaded["example.com"].saveDir).toBe("/data");
    await saveSiteProfile("example.com", null);
    expect(await loadSiteProfiles()).toEqual({});
  });

  test("CIDR key 存取与加载保留规范写法", async () => {
    await saveSiteProfile("10.1.2.3/8", profile({ intercept: "never" }));
    local.data.fluxdown_site_profiles = {
      ...(local.data.fluxdown_site_profiles as object),
      "172.16.0.0/99": profile(),
    };
    expect(Object.keys(await loadSiteProfiles())).toEqual(["10.0.0.0/8"]);
    await saveSiteProfile("10.0.0.0/8", null);
    expect(await loadSiteProfiles()).toEqual({});
  });
});

describe("旧 excludeDomains 迁移", () => {
  test("迁移幂等且不覆盖已有站点配置", async () => {
    sync.data.settings = { enabled: true, excludeDomains: ["A.com", "b.com"] };
    local.data.fluxdown_site_profiles = {
      "a.com": profile({ intercept: "always", updatedAt: 9 }),
    };

    await loadSettings();
    const first = await loadSiteProfiles();
    expect(first["a.com"].intercept).toBe("always");
    expect(first["b.com"].intercept).toBe("never");
    expect(first["b.com"].sniff).toBe(true);

    await loadSettings();
    expect(await loadSiteProfiles()).toEqual(first);
    expect("excludeDomains" in (sync.data.settings as object)).toBe(false);
  });
});
