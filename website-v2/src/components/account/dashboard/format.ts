import { INTL_LOCALE, type Lang } from "@/i18n/config";

/** 账户中心的格式化小件:日期 / 相对时间 / 用户数据边界(空值、emoji、超长)。 */

export const DASH = "-";

const dateFormats = new Map<string, Intl.DateTimeFormat>();
function dateFormat(lang: Lang, withTime: boolean): Intl.DateTimeFormat {
  const key = `${lang}:${withTime}`;
  let fmt = dateFormats.get(key);
  if (!fmt) {
    fmt = new Intl.DateTimeFormat(
      INTL_LOCALE[lang],
      withTime
        ? { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
        : { year: "numeric", month: "short", day: "numeric" },
    );
    dateFormats.set(key, fmt);
  }
  return fmt;
}

const relativeFormats = new Map<Lang, Intl.RelativeTimeFormat>();
function relativeFormat(lang: Lang): Intl.RelativeTimeFormat {
  let fmt = relativeFormats.get(lang);
  if (!fmt) {
    fmt = new Intl.RelativeTimeFormat(INTL_LOCALE[lang], { numeric: "auto" });
    relativeFormats.set(lang, fmt);
  }
  return fmt;
}

function parse(iso: string | null | undefined): Date | null {
  if (!iso) return null;
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? null : date;
}

/** `Nov 26, 2026`;空或无法解析 → `-`。 */
export function formatDay(iso: string | null | undefined, lang: Lang): string {
  const date = parse(iso);
  return date ? dateFormat(lang, false).format(date) : DASH;
}

/** `Nov 26, 2026, 03:04`;空或无法解析 → `-`。 */
export function formatDateTime(iso: string | null | undefined, lang: Lang): string {
  const date = parse(iso);
  return date ? dateFormat(lang, true).format(date) : DASH;
}

const RELATIVE_UNITS: readonly [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 31_536_000],
  ["month", 2_592_000],
  ["week", 604_800],
  ["day", 86_400],
  ["hour", 3_600],
  ["minute", 60],
];

/** `3 hours ago` / `3 小时前`;一分钟内 → `now` / `现在`。 */
export function formatRelative(iso: string | null | undefined, lang: Lang, now = Date.now()): string {
  const date = parse(iso);
  if (!date) return DASH;
  const diff = Math.round((date.getTime() - now) / 1000);
  const abs = Math.abs(diff);
  const fmt = relativeFormat(lang);
  for (const [unit, seconds] of RELATIVE_UNITS) {
    if (abs >= seconds) return fmt.format(Math.trunc(diff / seconds), unit);
  }
  return fmt.format(0, "second");
}

/** 距离 `iso` 还有几整天(向上取整,已过期 → 0);无法解析 → null。 */
export function daysUntil(iso: string | null | undefined, now = Date.now()): number | null {
  const date = parse(iso);
  if (!date) return null;
  return Math.max(0, Math.ceil((date.getTime() - now) / 86_400_000));
}

/** 空白 / 空值显示 `-`。 */
export function textOrDash(value: string | null | undefined): string {
  const trimmed = value?.trim();
  return trimmed ? trimmed : DASH;
}

/** 第一个字素簇(emoji / ZWJ 序列 / CJK 都完整保留),拉丁字母转大写;空串 → `?`。 */
export function firstGrapheme(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "?";
  let first: string | undefined;
  if (typeof Intl.Segmenter === "function") {
    for (const part of new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(trimmed)) {
      first = part.segment;
      break;
    }
  } else {
    first = Array.from(trimmed)[0];
  }
  return (first ?? "?").toLocaleUpperCase();
}

/** 按码点计数(与服务端 `chars().count()` 同口径),emoji 不会被算成两个字符。 */
export function charCount(value: string): number {
  return Array.from(value).length;
}

/** 会员编号 `No.0007`。 */
export function formatOrdinal(ordinal: number, digits: number): string {
  return `No.${String(ordinal).padStart(Math.max(digits, 4), "0")}`;
}

/** USD cents → `US$12.99`(与 WebPurchase 同形)。 */
export function formatUsdCents(cents: number): string {
  return `US$${(cents / 100).toFixed(2)}`;
}

/** 按 key 查文案表,表里没有 → 原样返回 key(后端新增枚举值时不留白)。 */
export function lookup(table: object, key: string): string {
  const value = (table as Record<string, string | undefined>)[key];
  return typeof value === "string" ? value : key;
}
