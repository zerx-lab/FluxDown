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
import { en, zhCN } from "./locales";
import type { Messages } from "./locales";

export type Locale = "en" | "zh";

const localeMap: Record<string, Messages> = {
  en,
  zh: zhCN,
};

const STORAGE_KEY = "fluxdown-locale";

/** 检测浏览器语言 */
export function detectLocale(): Locale {
  if (typeof navigator === "undefined") return "en";
  const langs = navigator.languages ?? [navigator.language];
  for (const lang of langs) {
    if (lang.startsWith("zh")) return "zh";
    if (lang.startsWith("en")) return "en";
  }
  return "en";
}

/** 从 localStorage 加载或自动检测 */
export function loadLocale(): Locale {
  if (typeof window === "undefined") return detectLocale();
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === "en" || saved === "zh") return saved;
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
}

/** 获取翻译消息 */
export function getMessages(locale: Locale): Messages {
  return localeMap[locale] ?? en;
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
 */
export function useLocale() {
  const [locale, setLocaleState] = useState<Locale>(() => loadLocale());
  const [messages, setMessages] = useState<Messages>(() => getMessages(loadLocale()));

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
    document.documentElement.lang = loc === "zh" ? "zh-CN" : "en";
    window.dispatchEvent(new CustomEvent("locale-change", { detail: { locale: loc } }));
  }, []);

  const tt = useCallback(
    (key: keyof Messages, params?: Record<string, string>) => t(messages, key, params),
    [messages],
  );

  return { locale, messages, setLocale, t: tt };
}
