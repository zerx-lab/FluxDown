/**
 * FluxDown i18n 国际化模块
 *
 * - 默认跟随浏览器语言
 * - 支持中文(zh)和英文(en)
 * - 不支持的语言回退到英文
 */

import { browser } from 'wxt/browser';
import zhCN from './locales/zh-CN';
import en from './locales/en';
import type { MessageKey } from './locales/zh-CN';

type LocaleMessages = Record<MessageKey, string>;

const locales: Record<string, LocaleMessages> = {
  'zh': zhCN,
  'zh-CN': zhCN,
  'zh-TW': zhCN,
  'zh-HK': zhCN,
  'en': en,
};

const FALLBACK_LOCALE = 'en';
const STORAGE_KEY = 'fluxdown_locale';

let currentMessages: LocaleMessages = en;
let currentLocale: string = FALLBACK_LOCALE;

/**
 * 检测浏览器语言，返回匹配的 locale key
 */
function detectBrowserLocale(): string {
  const langs = navigator.languages ?? [navigator.language];
  for (const lang of langs) {
    // 精确匹配 (如 zh-CN)
    if (locales[lang]) return lang;
    // 语言前缀匹配 (如 zh-TW → zh)
    const prefix = lang.split('-')[0];
    if (locales[prefix]) return prefix;
  }
  return FALLBACK_LOCALE;
}

/**
 * 初始化 i18n，应在应用启动时调用
 *
 * 优先级：用户手动选择 > 浏览器语言 > 英文(fallback)
 */
export async function initI18n(): Promise<string> {
  let locale: string;

  try {
    const result = await browser.storage.local.get(STORAGE_KEY);
    locale = result[STORAGE_KEY] || detectBrowserLocale();
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
  // 精确匹配
  if (locales[locale]) {
    currentLocale = locale;
    currentMessages = locales[locale];
    return;
  }
  // 语言前缀匹配
  const prefix = locale.split('-')[0];
  if (locales[prefix]) {
    currentLocale = prefix;
    currentMessages = locales[prefix];
    return;
  }
  // 回退到英文
  currentLocale = FALLBACK_LOCALE;
  currentMessages = locales[FALLBACK_LOCALE];
}

/**
 * 保存用户选择的语言
 */
export async function saveLocale(locale: string): Promise<void> {
  setLocale(locale);
  try {
    await browser.storage.local.set({ [STORAGE_KEY]: locale });
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
    return result[STORAGE_KEY] || null;
  } catch {
    return null;
  }
}

/**
 * 获取当前语言
 */
export function getLocale(): string {
  return currentLocale;
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
      message = message.replace(`{${k}}`, v);
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
  const langMap: Record<string, string> = {
    'zh': 'zh-CN',
    'zh-CN': 'zh-CN',
    'zh-TW': 'zh-CN',
    'zh-HK': 'zh-CN',
    'en': 'en',
  };
  document.documentElement.lang = langMap[currentLocale] || 'en';
}

export type { MessageKey };
