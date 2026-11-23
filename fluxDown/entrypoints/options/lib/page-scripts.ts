/**
 * 页面脚本面板的纯逻辑：状态 → 文案键 / 色调、浏览器开关引导（Chrome 138+ 详情页开关 / <138 开发者模式）。
 */
import type { PageScriptErrorCode, PluginViewState } from "@/background/page-scripts/model";
import { CHROME_USER_SCRIPTS_TOGGLE_VERSION, chromeMajorVersion } from "@/background/page-scripts/availability";
import type { MessageKey } from "@/utils/i18n";

type Tone = "neutral" | "accent" | "success" | "warning" | "danger";

export const STATE_LABEL: Record<PluginViewState, MessageKey> = {
  disabled: "pageScripts.state.disabled",
  enabled: "pageScripts.state.enabled",
  error: "pageScripts.state.error",
  "desktop-offline": "pageScripts.state.desktopOffline",
  "permission-missing": "pageScripts.state.permissionMissing",
  "switch-off": "pageScripts.state.switchOff",
  "browser-unsupported": "pageScripts.state.browserUnsupported",
};

export const STATE_TONE: Record<PluginViewState, Tone> = {
  disabled: "neutral",
  enabled: "success",
  error: "danger",
  "desktop-offline": "warning",
  "permission-missing": "warning",
  "switch-off": "warning",
  "browser-unsupported": "danger",
};

export const ERROR_LABEL: Record<PageScriptErrorCode, MessageKey> = {
  fetch_failed: "pageScripts.error.fetch_failed",
  sha_mismatch: "pageScripts.error.sha_mismatch",
  source_too_large: "pageScripts.error.source_too_large",
  register_failed: "pageScripts.error.register_failed",
};

/** 打开浏览器开关的引导：Chrome 138+ 在扩展详情页，<138 要开开发者模式；Firefox 只需权限请求。 */
export type SwitchGuide = "details-page" | "developer-mode" | "none";

export function switchGuide(userAgent: string): SwitchGuide {
  const chrome = chromeMajorVersion(userAgent);
  if (chrome === null) return "none";
  return chrome >= CHROME_USER_SCRIPTS_TOGGLE_VERSION ? "details-page" : "developer-mode";
}

/** 扩展详情页地址：Edge 用 `edge://`，其余 Chromium 用 `chrome://`。 */
export function extensionDetailsUrl(userAgent: string, extensionId: string): string {
  const scheme = /\bEdg\//.test(userAgent) ? "edge" : "chrome";
  return `${scheme}://extensions/?id=${extensionId}`;
}

/**
 * 匹配规则 → 去重后的站点名（保持声明顺序），给普通用户看的摘要用：
 * `*://*.lanzoub.com/*` → `lanzoub.com`，`https://pan.baidu.com/s/*` → `pan.baidu.com`。
 * 主机为 `*` 或无法解析的规则原样保留，不吞掉信息。
 */
export function matchHosts(patterns: readonly string[]): string[] {
  const hosts = patterns.map((pattern) => {
    const host = /^[^:/]+:\/\/([^/]+)/.exec(pattern)?.[1];
    if (!host || host === "*") return pattern;
    return host.startsWith("*.") ? host.slice(2) : host;
  });
  return [...new Set(hosts)];
}
