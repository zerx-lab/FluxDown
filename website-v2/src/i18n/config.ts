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

export const LANGS = ["en", "zh", "zh-tw", "es", "ru", "tr"] as const;
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

/** `<html lang>` 用的 BCP 47 标签。 */
export const HTML_LANG: Record<Lang, string> = {
  en: "en",
  zh: "zh-CN",
  "zh-tw": "zh-TW",
  es: "es",
  ru: "ru",
  tr: "tr",
};
/** `Intl.*` 日期 / 数字 / 相对时间格式化用的 locale(与 `HTML_LANG` 同源,en 固定美式)。 */
export const INTL_LOCALE: Record<Lang, string> = {
  en: "en-US",
  zh: "zh-CN",
  "zh-tw": "zh-TW",
  es: "es-ES",
  ru: "ru-RU",
  tr: "tr-TR",
};
/** hreflang 码(Google 现行规范:ISO 639-1,繁中带地区)。 */
export const HREFLANG: Record<Lang, string> = {
  en: "en",
  zh: "zh",
  "zh-tw": "zh-TW",
  es: "es",
  ru: "ru",
  tr: "tr",
};
export const OG_LOCALE: Record<Lang, string> = {
  en: "en_US",
  zh: "zh_CN",
  "zh-tw": "zh_TW",
  es: "es_ES",
  ru: "ru_RU",
  tr: "tr_TR",
};
/** 语言切换器显示名(各自语言自称)。 */
export const LANG_LABEL: Record<Lang, string> = {
  en: "English",
  zh: "简体中文",
  "zh-tw": "繁體中文",
  es: "Español",
  ru: "Русский",
  tr: "Türkçe",
};

export function isLang(value: unknown): value is Lang {
  return typeof value === "string" && (LANGS as readonly string[]).includes(value);
}

/** 浏览器 / Accept-Language 语言标签 → 站点语言;无匹配返回 null。繁中:显式 Hant 或 TW/HK/MO 地区。 */
export function matchLangTag(tag: string): Lang | null {
  const lower = tag.trim().toLowerCase();
  if (lower.startsWith("zh")) {
    if (/(^|-)hans(-|$)/.test(lower)) return "zh";
    if (/(^|-)hant(-|$)/.test(lower) || /-(tw|hk|mo)(-|$)/.test(lower)) return "zh-tw";
    return "zh";
  }
  const primary = lower.split("-")[0];
  return isLang(primary) ? primary : null;
}
