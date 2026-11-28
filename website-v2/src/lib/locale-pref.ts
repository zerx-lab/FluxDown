/**
 * 用户显式选择的站点语言(语言切换器 / 命令面板写入)。
 *
 * 同一个键同时写 localStorage 与 cookie:
 * - Layout.astro 首帧前的内联脚本读它,决定无前缀(英文)页面是否跳到对应语言页;
 * - SSR 的 /docs/ 入口(pages/docs/index.ts)读 cookie;
 * - 键名沿用旧站,已保存的选择继续有效。
 * 键名在 Layout.astro 内联脚本里有一份副本(内联脚本无法 import)。
 */
import type { Lang } from "@/i18n/config";

export const LOCALE_KEY = "fluxdown-locale";

export function rememberLocale(lang: Lang): void {
  try {
    localStorage.setItem(LOCALE_KEY, lang);
  } catch {
    // 隐私模式下 localStorage 不可用,仍写 cookie
  }
  const secure = location.protocol === "https:" ? "; Secure" : "";
  document.cookie = `${LOCALE_KEY}=${lang}; Path=/; Max-Age=31536000; SameSite=Lax${secure}`;
}
