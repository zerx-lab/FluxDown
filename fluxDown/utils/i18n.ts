/**
 * FluxDown i18n 国际化模块
 *
 * - 默认跟随浏览器语言
 * - 支持简体中文、繁体中文（台湾）、俄语和英文
 * - 不支持的语言回退到英文
 */

import { browser } from 'wxt/browser';
import zhCN from './locales/zh-CN';
import zhTW from './locales/zh-TW';
import en from './locales/en';
import ru from './locales/ru';
import type { MessageKey } from './locales/zh-CN';

type LocaleMessages = Record<MessageKey, string>;

type Locale = 'zh-CN' | 'zh-TW' | 'en' | 'ru';

const locales: Record<Locale, LocaleMessages> = {
  'zh-CN': zhCN,
  'zh-TW': zhTW,
  'en': en,
  'ru': ru,
};

const FALLBACK_LOCALE = 'en';
const STORAGE_KEY = 'fluxdown_locale';

let currentMessages: LocaleMessages = en;
let currentLocale: Locale = FALLBACK_LOCALE;

function resolveLocale(locale: unknown): Locale | null {
  if (typeof locale !== 'string') return null;
  const [language, scriptOrRegion] = locale.trim().toLowerCase().replaceAll('_', '-').split('-');
  if (language === 'en') return 'en';
  if (language === 'ru') return 'ru';
  if (language !== 'zh') return null;
  if (scriptOrRegion === 'hant') return 'zh-TW';
  if (scriptOrRegion === 'hans') return 'zh-CN';
  // 仅地区子标签决定无显式书写系统的中文；不读取扩展中的地区值。
  return scriptOrRegion === 'tw' || scriptOrRegion === 'hk' || scriptOrRegion === 'mo' ? 'zh-TW' : 'zh-CN';
}

/**
 * 检测浏览器语言，返回匹配的 locale key
 */
function detectBrowserLocale(): Locale {
  const langs = navigator.languages?.length ? navigator.languages : [navigator.language];
  for (const lang of langs) {
    const locale = resolveLocale(lang);
    if (locale) return locale;
  }
  return FALLBACK_LOCALE;
}

/**
 * 初始化 i18n，应在应用启动时调用
 *
 * 优先级：用户手动选择 > 浏览器语言 > 英文(fallback)
 */
export async function initI18n(): Promise<string> {
  let locale: Locale;

  try {
    const result = await browser.storage.local.get(STORAGE_KEY);
    locale = resolveLocale(result[STORAGE_KEY]) ?? detectBrowserLocale();
  } catch {
    // 非扩展环境(如测试)直接使用浏览器检测
    locale = detectBrowserLocale();
  }

  setLocale(locale);
  return currentLocale;
}

/**
 * 设置当前语言
 */
export function setLocale(locale: string): void {
  currentLocale = resolveLocale(locale) ?? FALLBACK_LOCALE;
  currentMessages = locales[currentLocale];
}

/**
 * 保存用户选择的语言
 */
export async function saveLocale(locale: string): Promise<void> {
  setLocale(locale);
  try {
    await browser.storage.local.set({ [STORAGE_KEY]: currentLocale });
  } catch {
    // 非扩展环境忽略
  }
}

/**
 * 清除用户手动选择的语言，恢复跟随浏览器语言。
 */
export async function clearLocale(): Promise<void> {
  try {
    await browser.storage.local.remove(STORAGE_KEY);
  } catch {
    // 非扩展环境忽略
  }
  setLocale(detectBrowserLocale());
}

/**
 * 读取用户手动保存的语言；未手动选择过（跟随浏览器）时返回 null。
 */
export async function getSavedLocale(): Promise<string | null> {
  try {
    const result = await browser.storage.local.get(STORAGE_KEY);
    return resolveLocale(result[STORAGE_KEY]);
  } catch {
    return null;
  }
}

/**
 * 获取当前语言
 */
export function getLocale(): Locale {
  return currentLocale;
}

/**
 * 弹出窗口依次切换简体中文、繁体中文、英文和俄语。
 */
export function getNextLocale(): Locale {
  return currentLocale === 'en' ? 'zh-CN' : currentLocale === 'zh-CN' ? 'zh-TW' : currentLocale === 'zh-TW' ? 'ru' : 'en';
}

/**
 * 翻译函数
 *
 * @param key - 翻译 key
 * @param params - 插值参数，如 { ext: '.pdf' }
 * @returns 翻译后的字符串
 *
 * @example
 * t('domain.removed', { domain: 'example.com' }) // → "已移除 example.com"
 * t('header.connected') // → "已连接"
 */
export function t(key: MessageKey, params?: Record<string, string>): string {
  let message = currentMessages[key] ?? zhCN[key] ?? key;

  if (params) {
    for (const [k, v] of Object.entries(params)) {
      message = message.replaceAll(`{${k}}`, v);
    }
  }

  return message;
}

/**
 * 应用 HTML 元素的 i18n 翻译
 *
 * 遍历所有带有 data-i18n 属性的元素，将 textContent 替换为翻译值。
 * 支持 data-i18n-title、data-i18n-placeholder 等属性翻译。
 */
export function applyI18nToDOM(): void {
  // textContent 翻译
  document.querySelectorAll<HTMLElement>('[data-i18n]').forEach((el) => {
    const key = el.getAttribute('data-i18n') as MessageKey;
    if (key) el.textContent = t(key);
  });

  // title 属性翻译
  document.querySelectorAll<HTMLElement>('[data-i18n-title]').forEach((el) => {
    const key = el.getAttribute('data-i18n-title') as MessageKey;
    if (key) el.title = t(key);
  });

  // placeholder 属性翻译
  document.querySelectorAll<HTMLInputElement>('[data-i18n-placeholder]').forEach((el) => {
    const key = el.getAttribute('data-i18n-placeholder') as MessageKey;
    if (key) el.placeholder = t(key);
  });

  // 更新 html lang 属性
  document.documentElement.lang = currentLocale;
}

export type { MessageKey };
