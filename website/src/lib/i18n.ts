/**
 * FluxDown Website i18n
 *
 * - 默认跟随浏览器语言
 * - 支持中文(zh)和英文(en)
 * - 不支持的语言回退到英文
 * - 每个 Astro island 通过 useLocale() 独立管理状态
 * - 组件间通过 locale-change 自定义事件同步
 */

import { useState, useEffect, useCallback } from "react";
import { en, localeRegistry, htmlLang } from "./locales";
import type { Messages } from "./locales";

/** locale 代码（"en"、"zh"、"ja"…），可用集合由 locales/*.json 自动发现 */
export type Locale = string;

const STORAGE_KEY = "fluxdown-locale";

/** 检测浏览器语言 */
export function detectLocale(): Locale {
  if (typeof navigator === "undefined") return "en";
  const langs = navigator.languages ?? [navigator.language];
  const available = Object.keys(localeRegistry);
  for (const lang of langs) {
    const lower = lang.toLowerCase();
    // 精确匹配（如 pt-br），其次主语言前缀匹配（如 zh-TW → zh、ja-JP → ja）
    const exact = available.find((c) => c === lower);
    if (exact) return exact;
    const prefix = available.find((c) => c === lower.split("-")[0]);
    if (prefix) return prefix;
  }
  return "en";
}

/** 从 localStorage 加载或自动检测 */
export function loadLocale(): Locale {
  if (typeof window === "undefined") return detectLocale();
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved && saved in localeRegistry) return saved;
  } catch {
    // localStorage 不可用（SSR / 隐私模式）
  }
  return detectLocale();
}

/** 持久化语言选择 */
export function saveLocale(locale: Locale): void {
  if (typeof window === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // localStorage 不可用
  }
  try {
    document.cookie = `${STORAGE_KEY}=${locale}; Path=/; Max-Age=31536000; SameSite=Lax; Secure`;
  } catch {
    // document.cookie 不可用（极端隐私模式）
  }
}

/**
 * —— SSR 语言注入(固定语言页面:/zh/、/ja/、docs)——
 * Astro 构建默认串行预渲染(build.concurrency=1),Layout frontmatter 在其
 * islands SSR 之前执行,因此模块级变量按页生效、无并发竞争。
 * 客户端不读该值:hydration 前从 <html data-fixed-lang lang> 同步还原,
 * 保证 SSR/CSR 首帧一致(避免 React #418 hydration mismatch)。
 */
let ssrLocale: Locale = "en";

export function setSSRLocale(locale: Locale): void {
  ssrLocale = locale in localeRegistry ? locale : "en";
}

/** html lang 属性 → locale 代码(htmlLang 的逆映射) */
function localeFromHtmlLang(lang: string): Locale {
  if (lang === "zh-CN") return "zh";
  return lang in localeRegistry ? lang : "en";
}

/** 首帧 locale:服务端取注入值;客户端仅固定语言页取 html lang,其余保持 en */
function initialLocale(): Locale {
  if (typeof document === "undefined") return ssrLocale;
  const html = document.documentElement;
  if (html.hasAttribute("data-fixed-lang")) return localeFromHtmlLang(html.lang);
  return "en";
}

/** 获取翻译消息 */
export function getMessages(locale: Locale): Messages {
  return localeRegistry[locale] ?? en;
}

/** 翻译函数 */
export function t(messages: Messages, key: keyof Messages, params?: Record<string, string>): string {
  let msg: string = messages[key] ?? en[key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      msg = msg.replace(`{${k}}`, v);
    }
  }
  return msg;
}

/**
 * 独立 i18n hook — 适用于 Astro island 架构
 * 每个 React island 独立管理 locale 状态，通过 CustomEvent 同步切换
 *
 * SSR 安全：初始值固定为 "en"（与服务端一致），useEffect 中再更新为实际语言。
 * 这样避免 React hydration mismatch（error #418）。
 */
export function useLocale() {
  // 首帧与服务端渲染严格一致:固定语言页 = 页面语言,其余 = "en"
  const [locale, setLocaleState] = useState<Locale>(initialLocale);
  const [messages, setMessages] = useState<Messages>(() => getMessages(initialLocale()));

  // 客户端挂载后更新为实际语言(读取 localStorage / navigator.languages)。
  // 固定语言页(/zh/、/ja/、docs)跳过:内容语言由 URL 决定,切换靠导航。
  useEffect(() => {
    if (document.documentElement.hasAttribute("data-fixed-lang")) return;
    const actual = loadLocale();
    setLocaleState(actual);
    setMessages(getMessages(actual));
  }, []);

  // 监听其他 island 的语言切换事件
  useEffect(() => {
    const onLocaleChange = (e: CustomEvent<{ locale: Locale }>) => {
      setLocaleState(e.detail.locale);
      setMessages(getMessages(e.detail.locale));
    };
    window.addEventListener("locale-change", onLocaleChange as EventListener);
    return () => window.removeEventListener("locale-change", onLocaleChange as EventListener);
  }, []);

  // 切换语言：更新本地状态 + 持久化 + 广播事件
  const setLocale = useCallback((loc: Locale) => {
    setLocaleState(loc);
    setMessages(getMessages(loc));
    saveLocale(loc);
    document.documentElement.lang = htmlLang(loc);
    window.dispatchEvent(new CustomEvent("locale-change", { detail: { locale: loc } }));
  }, []);

  const tt = useCallback(
    (key: keyof Messages, params?: Record<string, string>) => t(messages, key, params),
    [messages],
  );

  return { locale, messages, setLocale, t: tt };
}
