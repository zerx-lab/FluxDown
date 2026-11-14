/**
 * 扩展备份：导出 / 导入全部可迁移的用户数据。
 * 格式 `{format:"fluxdown-extension-backup", version:1, …}`；remoteToken 与 remoteVerified 不进备份。
 */
import { browser } from "wxt/browser";
import type { RuleOverrides, SniffRuleV1 } from "@/rules/schema";
import { PackRejectedError, validatePack } from "@/rules/validate";
import {
  DEFAULT_SETTINGS,
  loadSettings,
  migrateSettings,
  saveSettings,
  type FluxDownSettings,
} from "@/utils/settings";
import {
  loadSiteProfiles,
  parseSiteProfiles,
  saveSiteProfiles,
  type SiteProfile,
} from "@/utils/site-profiles";
import {
  LOCAL_RULES_KEY,
  RULE_OVERRIDES_KEY,
  buildLocalPack,
  parseLocalRules,
  parseOverrides,
} from "./local-rules";
import { isRecord } from "./guards";

export const BACKUP_FORMAT = "fluxdown-extension-backup";
export const BACKUP_VERSION = 1;

const DOT_POS_KEY = "fluxdown_dot_pos";
const THEME_KEY = "theme";
const LOCALE_KEY = "fluxdown_locale";
const LOCALES = ["zh-CN", "zh-TW", "en", "ru"] as const;

export interface ExtensionBackup {
  format: typeof BACKUP_FORMAT;
  version: typeof BACKUP_VERSION;
  exportedAt: string;
  settings: Omit<FluxDownSettings, "remoteToken" | "remoteVerified">;
  siteProfiles: Record<string, SiteProfile>;
  localRules: SniffRuleV1[];
  ruleOverrides: RuleOverrides;
  dotPos: unknown;
  theme: "light" | "dark" | null;
  locale: (typeof LOCALES)[number] | null;
}

/** 导入前已校验、可直接落盘的备份内容。 */
export interface ParsedBackup {
  exportedAt: string;
  settings: Partial<FluxDownSettings>;
  siteProfiles: Record<string, SiteProfile>;
  localRules: SniffRuleV1[];
  rejectedRules: { id: string; reason: string }[];
  ruleOverrides: RuleOverrides;
  dotPos: unknown;
  theme: "light" | "dark" | null;
  locale: (typeof LOCALES)[number] | null;
}

export type BackupParseError = "json" | "format" | "version";

const ENUM_FIELDS: Record<string, readonly string[]> = {
  interceptMode: ["smart", "all", "extension"],
  floatButtonTrigger: ["hover", "always"],
  floatButtonPosition: ["top-right", "top-left", "bottom-right", "bottom-left"],
  remoteMode: ["off", "fallback", "always"],
};

const SECRET_FIELDS: Record<string, true> = { remoteToken: true, remoteVerified: true };

/**
 * 只保留 DEFAULT_SETTINGS 中已知且类型 / 取值合法的字段：未知字段、旧字段（excludeDomains 等）
 * 与密钥字段一律丢弃，剩余缺省值由 migrateSettings 用默认值补齐。
 */
export function sanitizeSettings(raw: unknown): Partial<FluxDownSettings> {
  if (!isRecord(raw)) return {};
  const source = raw;
  const out: Record<string, unknown> = {};
  for (const [key, fallback] of Object.entries(DEFAULT_SETTINGS)) {
    const value = source[key];
    if (value === undefined || SECRET_FIELDS[key]) continue;
    if (Array.isArray(fallback)) {
      if (Array.isArray(value) && value.every((v) => typeof v === "string")) out[key] = value;
      continue;
    }
    if (typeof value !== typeof fallback) continue;
    const allowed = ENUM_FIELDS[key];
    if (allowed && !allowed.includes(value as string)) continue;
    if (typeof value === "number" && !(Number.isFinite(value) && value >= 0)) continue;
    out[key] = value;
  }
  return out;
}

export function parseBackup(
  text: string,
): { ok: true; backup: ParsedBackup } | { ok: false; error: BackupParseError } {
  let json: unknown;
  try {
    json = JSON.parse(text);
  } catch {
    return { ok: false, error: "json" };
  }
  if (!isRecord(json) || json.format !== BACKUP_FORMAT) {
    return { ok: false, error: "format" };
  }
  const raw = json;
  if (raw.version !== BACKUP_VERSION) return { ok: false, error: "version" };

  // 本地规则重新过 validatePack，不合格项剔除并回报。
  let localRules: SniffRuleV1[] = [];
  let rejectedRules: { id: string; reason: string }[] = [];
  const candidates = parseLocalRules(raw.localRules);
  if (candidates.length > 0) {
    try {
      const checked = validatePack(buildLocalPack(candidates, Date.now()), "local");
      localRules = checked.pack.rules;
      rejectedRules = checked.rejected;
    } catch (e) {
      if (!(e instanceof PackRejectedError)) throw e;
      rejectedRules = candidates.map((r) => ({ id: r.id, reason: e.message }));
    }
  }

  return {
    ok: true,
    backup: {
      exportedAt: typeof raw.exportedAt === "string" ? raw.exportedAt : "",
      settings: sanitizeSettings(raw.settings),
      siteProfiles: parseSiteProfiles(raw.siteProfiles),
      localRules,
      rejectedRules,
      ruleOverrides: parseOverrides(raw.ruleOverrides),
      dotPos: raw.dotPos ?? null,
      theme: raw.theme === "light" || raw.theme === "dark" ? raw.theme : null,
      locale: (LOCALES as readonly unknown[]).includes(raw.locale) ? (raw.locale as ExtensionBackup["locale"]) : null,
    },
  };
}

/** 备份文件名：`fluxdown-extension-backup-YYYY-MM-DD.json`（本地日期）。 */
export function backupFileName(now: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `fluxdown-extension-backup-${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}.json`;
}

/** 读取当前全部数据组装成备份对象（不含 remoteToken / remoteVerified）。 */
export async function collectBackup(now: Date): Promise<ExtensionBackup> {
  const [current, siteProfiles, stored] = await Promise.all([
    loadSettings(),
    loadSiteProfiles(),
    browser.storage.local.get([LOCAL_RULES_KEY, RULE_OVERRIDES_KEY, DOT_POS_KEY, THEME_KEY, LOCALE_KEY]),
  ]);
  const { remoteToken: _token, remoteVerified: _verified, ...settings } = current;
  const theme = stored[THEME_KEY];
  const locale = stored[LOCALE_KEY];
  return {
    format: BACKUP_FORMAT,
    version: BACKUP_VERSION,
    exportedAt: now.toISOString(),
    settings,
    siteProfiles,
    localRules: parseLocalRules(stored[LOCAL_RULES_KEY]),
    ruleOverrides: parseOverrides(stored[RULE_OVERRIDES_KEY]),
    dotPos: stored[DOT_POS_KEY] ?? null,
    theme: theme === "light" || theme === "dark" ? theme : null,
    locale: (LOCALES as readonly unknown[]).includes(locale) ? (locale as ExtensionBackup["locale"]) : null,
  };
}

/**
 * 整体覆盖当前数据。settings 经 sanitize + migrateSettings 合并默认值，且不触碰本机的
 * remoteToken（备份里没有）；站点配置、本地规则、覆盖表整表替换；dotPos / theme / locale 为空则清除。
 */
export async function applyBackup(backup: ParsedBackup): Promise<void> {
  const { remoteToken: _token, remoteVerified: _verified, ...patch } = migrateSettings(backup.settings);
  await saveSettings(patch);
  await saveSiteProfiles(backup.siteProfiles);

  const toSet: Record<string, unknown> = {
    [LOCAL_RULES_KEY]: backup.localRules,
    [RULE_OVERRIDES_KEY]: backup.ruleOverrides,
  };
  const toRemove: string[] = [];
  const optional: [string, unknown][] = [
    [DOT_POS_KEY, backup.dotPos],
    [THEME_KEY, backup.theme],
    [LOCALE_KEY, backup.locale],
  ];
  for (const [key, value] of optional) {
    if (value === null || value === undefined) toRemove.push(key);
    else toSet[key] = value;
  }
  await browser.storage.local.set(toSet);
  if (toRemove.length > 0) await browser.storage.local.remove(toRemove);
}
