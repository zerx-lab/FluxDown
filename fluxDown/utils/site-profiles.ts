/**
 * 站点配置（按 host 的接管 / 嗅探 / 悬浮球 / 浮钮 / 保存目录）。
 *
 * 替代旧的全局 `excludeDomains`：后台拦截判定、PageConfig 计算、popup / 侧边栏
 * 的 SiteCard、设置页 `#sites` 共用本模块，保证同一 host 的解读一致。
 * 存储于 `storage.local` 的 `fluxdown_site_profiles`（`Record<host, SiteProfile>`）。
 */
import { browser } from "wxt/browser";
import { cidrContains, formatCidr, looksLikeCidr, parseCidr, parseIp } from "./cidr";
import type { Cidr } from "./cidr";
import { normalizeDomain } from "./domain-exclusion";

export const SITE_PROFILES_KEY = "fluxdown_site_profiles";

export interface SiteProfile {
  /** 下载接管：default = 跟随全局；never = 本站不接管；always = 跳过 smart 判定。 */
  intercept: "default" | "never" | "always";
  sniff: boolean;
  pageDot: boolean;
  floatButton: boolean;
  /** 目标设备上的保存目录，空串 = 用默认。 */
  saveDir: string;
  updatedAt: number;
}

export type SiteProfileFields = Omit<SiteProfile, "updatedAt">;

export const DEFAULT_SITE_PROFILE: SiteProfileFields = Object.freeze({
  intercept: "default",
  sniff: true,
  pageDot: true,
  floatButton: true,
  saveDir: "",
});

/** 宽松解析单条配置：字段类型不符回退默认值；非对象返回 null。 */
export function parseSiteProfile(value: unknown): SiteProfile | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const raw = value as Partial<Record<keyof SiteProfile, unknown>>;
  const intercept =
    raw.intercept === "never" || raw.intercept === "always" ? raw.intercept : "default";
  return {
    intercept,
    sniff: typeof raw.sniff === "boolean" ? raw.sniff : DEFAULT_SITE_PROFILE.sniff,
    pageDot: typeof raw.pageDot === "boolean" ? raw.pageDot : DEFAULT_SITE_PROFILE.pageDot,
    floatButton:
      typeof raw.floatButton === "boolean" ? raw.floatButton : DEFAULT_SITE_PROFILE.floatButton,
    saveDir: typeof raw.saveDir === "string" ? raw.saveDir.trim() : "",
    updatedAt: typeof raw.updatedAt === "number" ? raw.updatedAt : 0,
  };
}

/**
 * 站点配置 key 归一化：合法 CIDR（`10.0.0.0/8`、`fd00::/8`）转规范网络地址写法，
 * 其余按 normalizeDomain；形如 CIDR 但非法（`10.0.0.0/33`）返回 null，不退化成域名。
 */
export function normalizeSiteKey(input: string): string | null {
  const text = input.trim().toLowerCase();
  if (!text.includes("://") && looksLikeCidr(text)) {
    const cidr = parseCidr(text);
    return cidr ? formatCidr(cidr) : null;
  }
  return normalizeDomain(text);
}

/** 归一化整张表：key 经 normalizeSiteKey，非法项丢弃。 */
export function parseSiteProfiles(raw: unknown): Record<string, SiteProfile> {
  const out: Record<string, SiteProfile> = {};
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return out;
  for (const [key, value] of Object.entries(raw)) {
    const host = normalizeSiteKey(key);
    const profile = parseSiteProfile(value);
    if (host && profile) out[host] = profile;
  }
  return out;
}

export async function loadSiteProfiles(): Promise<Record<string, SiteProfile>> {
  const stored = await browser.storage.local.get(SITE_PROFILES_KEY);
  return parseSiteProfiles(stored[SITE_PROFILES_KEY]);
}

/** 写入（或 `null` 删除）一个 host / CIDR 网段的配置；key 经 normalizeSiteKey，非法时抛错。 */
export async function saveSiteProfile(host: string, p: SiteProfile | null): Promise<void> {
  const key = normalizeSiteKey(host);
  if (!key) throw new Error(`invalid host: ${host}`);
  const profiles = await loadSiteProfiles();
  if (p === null) {
    delete profiles[key];
  } else {
    profiles[key] = { ...p, saveDir: p.saveDir.trim() };
  }
  await browser.storage.local.set({ [SITE_PROFILES_KEY]: profiles });
}

/** 整表覆盖写入（迁移 / 备份导入用）。 */
export async function saveSiteProfiles(profiles: Record<string, SiteProfile>): Promise<void> {
  await browser.storage.local.set({ [SITE_PROFILES_KEY]: parseSiteProfiles(profiles) });
}

/**
 * 取 host 生效的配置。域名 host：所有满足 `h === k || h.endsWith("." + k)` 的 key 中最长者。
 * IP 字面量 host：精确 key > 包含它的最长前缀 CIDR key > 域名后缀 key。
 * 无命中返回默认配置与 `key: null`。
 */
export function profileFor(
  profiles: Record<string, SiteProfile>,
  host: string,
): { key: string | null; profile: SiteProfileFields } {
  const h = normalizeDomain(host);
  if (!h) return { key: null, profile: DEFAULT_SITE_PROFILE };
  const keys = Object.keys(profiles);
  let best: string | null = Object.hasOwn(profiles, h) ? h : null;
  const ip = best === null ? parseIp(h) : null;
  if (ip) {
    let bestCidr: Cidr | null = null;
    for (const k of keys) {
      const cidr = parseCidr(k);
      if (cidr && cidrContains(cidr, ip) && (bestCidr === null || cidr.prefix > bestCidr.prefix)) {
        best = k;
        bestCidr = cidr;
      }
    }
  }
  if (best === null) {
    for (const k of keys) {
      if (h.endsWith(`.${k}`) && (best === null || k.length > best.length)) best = k;
    }
  }
  if (best === null) return { key: null, profile: DEFAULT_SITE_PROFILE };
  const { updatedAt: _updatedAt, ...profile } = profiles[best];
  return { key: best, profile };
}
