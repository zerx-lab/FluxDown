/**
 * 站点语言契约(单一事实源)。
 *
 * - en 位于根路径(`/download`),其余语言位于 `/<lang>` 前缀(`/zh/download`、`/zh-tw/download`)。
 * - en + zh 是手写基线;`MT_LANGS` 由 qwen-mt 翻译流程(`.github/workflows/i18n-mt.yml`)生成:
 *   界面文案 `src/i18n/locales/<lang>/<ns>.ts`(由 `scripts/i18n-messages.ts generate` 产出),
 *   文档 `src/content/docs/<lang>/`。
 * - 每个页面都由服务端按 URL 直出目标语言;客户端不做切换重渲染。
 * - 无前缀 URL 同时是「自动语言」入口:Layout.astro 首帧前按显式选择(lib/locale-pref.ts)
 *   → 浏览器/系统语言判定跳到对应语言页;`DEFAULT_LANG` 只是无法判定时的回退。
 * - `astro.config.mjs` 与 `scripts/i18n-mt`(Python 按正则读取 `LANGS`)同契约,改语言集时同改。
 */

export const LANGS = [
  "en",
  "zh",
  "zh-tw",
  "ja",
  "ko",
  "es",
  "pt-br",
  "fr",
  "de",
  "it",
  "nl",
  "pl",
  "ru",
  "uk",
  "tr",
  "vi",
  "id",
  "th",
] as const;
export type Lang = (typeof LANGS)[number];
export const DEFAULT_LANG: Lang = "en";
/** 手写基线语言;其余语言由机器翻译生成。 */
export const BASELINE_LANGS = ["en", "zh"] as const satisfies readonly Lang[];
export type MtLang = Exclude<Lang, (typeof BASELINE_LANGS)[number]>;
export const MT_LANGS = LANGS.filter((lang): lang is MtLang => !(BASELINE_LANGS as readonly string[]).includes(lang));
/** 基线语言(手写文案);非 messages 模块里手写的双语数据(公告、促销文案等)只覆盖它们。 */
export type BaselineLang = (typeof BASELINE_LANGS)[number];
/** 手写双语数据未覆盖的语言(机器翻译语言)回退到 en。 */
export function toBaselineLang(lang: Lang): BaselineLang {
  return (BASELINE_LANGS as readonly Lang[]).includes(lang) ? (lang as BaselineLang) : "en";
}

/** 各语言的 BCP 47 / Intl / hreflang / OG / 自称,一处登记;下方各表由此派生。 */
const LANG_META = {
  en: { html: "en", intl: "en-US", hreflang: "en", og: "en_US", label: "English" },
  zh: { html: "zh-CN", intl: "zh-CN", hreflang: "zh", og: "zh_CN", label: "简体中文" },
  "zh-tw": { html: "zh-TW", intl: "zh-TW", hreflang: "zh-TW", og: "zh_TW", label: "繁體中文" },
  ja: { html: "ja", intl: "ja-JP", hreflang: "ja", og: "ja_JP", label: "日本語" },
  ko: { html: "ko", intl: "ko-KR", hreflang: "ko", og: "ko_KR", label: "한국어" },
  es: { html: "es", intl: "es-ES", hreflang: "es", og: "es_ES", label: "Español" },
  "pt-br": { html: "pt-BR", intl: "pt-BR", hreflang: "pt-BR", og: "pt_BR", label: "Português (Brasil)" },
  fr: { html: "fr", intl: "fr-FR", hreflang: "fr", og: "fr_FR", label: "Français" },
  de: { html: "de", intl: "de-DE", hreflang: "de", og: "de_DE", label: "Deutsch" },
  it: { html: "it", intl: "it-IT", hreflang: "it", og: "it_IT", label: "Italiano" },
  nl: { html: "nl", intl: "nl-NL", hreflang: "nl", og: "nl_NL", label: "Nederlands" },
  pl: { html: "pl", intl: "pl-PL", hreflang: "pl", og: "pl_PL", label: "Polski" },
  ru: { html: "ru", intl: "ru-RU", hreflang: "ru", og: "ru_RU", label: "Русский" },
  uk: { html: "uk", intl: "uk-UA", hreflang: "uk", og: "uk_UA", label: "Українська" },
  tr: { html: "tr", intl: "tr-TR", hreflang: "tr", og: "tr_TR", label: "Türkçe" },
  vi: { html: "vi", intl: "vi-VN", hreflang: "vi", og: "vi_VN", label: "Tiếng Việt" },
  id: { html: "id", intl: "id-ID", hreflang: "id", og: "id_ID", label: "Bahasa Indonesia" },
  th: { html: "th", intl: "th-TH", hreflang: "th", og: "th_TH", label: "ไทย" },
} as const satisfies Record<Lang, { html: string; intl: string; hreflang: string; og: string; label: string }>;

function pick(field: keyof (typeof LANG_META)["en"]): Record<Lang, string> {
  return Object.fromEntries(LANGS.map((lang) => [lang, LANG_META[lang][field]])) as Record<Lang, string>;
}

/** `<html lang>` 用的 BCP 47 标签。 */
export const HTML_LANG = pick("html");
/** `Intl.*` 日期 / 数字 / 相对时间格式化用的 locale(en 固定美式)。 */
export const INTL_LOCALE = pick("intl");
/** hreflang 码(Google 现行规范:ISO 639-1,繁中 / 巴葡带地区)。 */
export const HREFLANG = pick("hreflang");
export const OG_LOCALE = pick("og");
/** 语言切换器显示名(各自语言自称)。 */
export const LANG_LABEL = pick("label");

export function isLang(value: unknown): value is Lang {
  return typeof value === "string" && (LANGS as readonly string[]).includes(value);
}

/**
 * 浏览器 / Accept-Language 语言标签 → 站点语言;无匹配返回 null。
 * 繁中:显式 Hant 或 TW/HK/MO 地区;其余先整标签精确匹配(pt-br),再按主语言匹配(pt-PT → pt-br)。
 * Layout.astro 首帧内联脚本有一份同逻辑的 JS 副本,改这里同改那里。
 */
export function matchLangTag(tag: string): Lang | null {
  const lower = tag.trim().toLowerCase().replace(/_/g, "-");
  if (lower.startsWith("zh")) {
    if (/(^|-)hans(-|$)/.test(lower)) return "zh";
    if (/(^|-)hant(-|$)/.test(lower) || /-(tw|hk|mo)(-|$)/.test(lower)) return "zh-tw";
    return "zh";
  }
  if (isLang(lower)) return lower;
  // 旧版 Java / Android WebView 仍上报 ISO 639 废弃码 `in`(= id 印尼语)。
  const primary = lower.split("-")[0] === "in" ? "id" : lower.split("-")[0];
  return LANGS.find((lang) => lang.split("-")[0] === primary) ?? null;
}
