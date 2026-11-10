/**
 * MAIN world（page/hooks.ts）与 ISOLATED world（sensor.content.ts）之间的事件契约。
 *
 * 所有 `detail` 一律是 JSON 字符串：Chrome / Firefox 的 world 边界都只能可靠地传递
 * 原始字符串（Firefox 的 Xray 包装会让对象 detail 不可读）。接收方 `try/JSON.parse`，
 * 失败即丢弃。
 */

import type { ClassifyRule } from "@/rules/engine";
import type { HlsPlaylistInfo } from "@/utils/hls-playlist";

/** MAIN → ISOLATED，在 document 上派发。 */
export const EV_REPORT = "__fluxdown_report";
/** ISOLATED → MAIN，在 document 上派发。 */
export const EV_CONFIG = "__fluxdown_config";
/** MAIN 在媒体元素上派发（composed: true，bubbles: false）；无元素时在 document 上派发并带 elementless。 */
export const EV_MEDIA = "__fluxdown_media";
/**
 * MAIN 在 shadow 宿主上派发（bubbles + composed）：页面刚给已连接的元素挂了 shadow root，
 * ISOLATED 据此观察其中的媒体元素（closed root 由内容脚本 API 读取）。detail 为空串。
 */
export const EV_SHADOW = "__fluxdown_shadow";

export type ReportDetail =
  | { t: "resource"; url: string; mime?: string; size?: number; via: "fetch" | "xhr" | "json" }
  | { t: "manifest"; manifestUrl: string; manifest: unknown }
  | { t: "body"; url: string; ruleIds: string[]; text: string }
  | { t: "preempt"; url: string; filename?: string }
  | { t: "pageUrl"; kind: "push" | "replace" | "pop"; from: string }
  | { t: "hls"; url: string; playlist: HlsPlaylistInfo }
  /** `URL.createObjectURL(blob)` 的 blob 来自某个响应（来源标记唯一）：blob URL ↔ 原始地址。 */
  | { t: "blob"; blobUrl: string; url: string }
  /** 本源已注册的 Service Worker 作用域（识别 SW 合成的下载，见 utils/download-source.ts）。 */
  | { t: "swScope"; scope: string }
  /** 钩子对 sensor 握手（EV_CONFIG 带新 sid）的应答；协议 0 的旧钩子不应答。 */
  | { t: "hooks"; protocol: number; hid: string; late: boolean };

/**
 * MAIN 钩子协议版本。只在事件契约发生不兼容变化时递增；新增字段不递增。
 * 0 = 只有布尔安装标记、不应答握手的旧钩子（事件结构与 1 相同）。
 */
export const HOOKS_PROTOCOL = 1;

export interface SourceBufferBinding {
  sbId: number;
  mime: string;
  kind: "video" | "audio" | "unknown";
  /** appendBuffer 的数据按来源打标命中的响应 URL（去重，最近 64 个）。 */
  exact: string[];
  /** 无标记时按时间窗口归因的响应 URL 计数（count 最高的 16 个）。 */
  temporal: { url: string; count: number }[];
}

export interface MediaDetail {
  msId: number;
  /** 钩子实例 id（协议 ≥ 1）；与 msId 一起唯一标识一次 MediaSource 播放会话。 */
  hid?: string;
  sourceBuffers: SourceBufferBinding[];
  /** MediaSource 尚未关联任何元素时为 true（在 document 上派发）。 */
  elementless?: boolean;
}

export interface PageConfig {
  sniff: boolean;
  bodyWatch: { ruleId: string; url?: string[]; host?: string[]; pathRegex?: string }[];
  dom: { ruleId: string; selector: string; attr: string }[];
  floatButton: boolean;
  floatTrigger: "hover" | "always";
  /** 浮钮贴在播放元素的哪个角。 */
  floatPosition: "top-right" | "top-left" | "bottom-right" | "bottom-left";
  /** page 规则：播放元素标题来源（取离元素最近的匹配节点）。 */
  titles: { selector: string; attr?: string }[];
  /** page 规则：这些容器内的媒体元素不显示浮钮。 */
  floatExclude: string[];
  pageDot: boolean;
  frameIsTop: boolean;
  /** 是否接管本页 magnet 链接点击：settings.enabled && settings.interceptMagnet && 站点 intercept !== "never"。 */
  interceptMagnet: boolean;
  /** 跳过接管修饰键：settings.enabled 关闭时为 "off"（没有接管可跳过）。 */
  bypassKey: "alt" | "shift" | "off";
  /** 按下链接时上报以预热 NMH：settings.enabled && 站点 intercept !== "never"。 */
  warmOnLinkPress: boolean;
  /** settings.sniffImages：DOM 扫描是否把图片链接算作可下载。 */
  sniffImages: boolean;
  /** settings.sniffMaxResources：单页 DOM 嗅探最多上报的资源数（fetch / XHR 嗅探不计入）。 */
  sniffMaxResources: number;
  /**
   * 用户实际生效的 classify 规则（含覆盖 / 吊销 / 本地与插件规则，求值顺序）。页面 / 内容脚本据此
   * 用 `createClassifier` 判定类型；未收到配置前退回随包规则。
   */
  classify: ClassifyRule[];
}

/**
 * sensor 下发给 MAIN 的配置：PageConfig + 会话身份。
 * 导航代数由 MAIN 持有：同一 sid 的 `nav` 增大才换代；新 sid（sensor 重注入）只握手、不换代，
 * 否则重注入会把仍有效的 SourceBuffer 证据当作上一页的清掉。
 */
export interface HookConfig extends PageConfig {
  sid: string;
  /** sensor 本地的 SPA 换页计数。 */
  nav: number;
}

/** 编码 detail；接收方用 {@link decodeDetail}。 */
export function encodeDetail(value: ReportDetail | MediaDetail | HookConfig): string {
  return JSON.stringify(value);
}

/** 解码 CustomEvent.detail；非字符串或 JSON 失败返回 null。形状由调用方按判别字段检查。 */
export function decodeDetail(detail: unknown): unknown {
  if (typeof detail !== "string") return null;
  try {
    return JSON.parse(detail) as unknown;
  } catch {
    return null;
  }
}
