/**
 * AI 助手入口（顶栏链接、移动菜单条目、悬浮挂件）的显隐，浏览器侧。
 *
 * 是否显示由 FluxCloud 判定（`ai.mode` + 账号是否内部账号），官网不复刻规则，只经 BFF `GET /api/account/ai/status`
 * 取结果。唯一状态是 `<html data-ai-visible>`：缺省没有（入口隐藏），有则显示。
 *
 * - 首帧前：`Layout.astro` 的内联脚本读 localStorage 里上次的结果并立即写这个属性。
 * - 首帧后：`refreshAiEntryVisibility`（由 `Header.astro` 调用）请求最新结果，更新缓存与属性，属性变化时派发
 *   `AI_ENTRY_EVENT`（顶栏据此重算按宽度收起的步骤，悬浮挂件据此挂载 / 卸载）。请求失败保留上次已知值。
 *
 * 结果与登录态有关（`internal` 模式只对内部账号显示），所以缓存带着取值时的登录态（`in` / `out`，来自提示 Cookie
 * `fd_signed_in`）：登录态与缓存不一致时首帧一律按隐藏，等刷新结果，不会把上一个身份的结果套在当前身份上。
 * 同一登录态下的缓存只管 `CACHE_TTL_MS` 内免重复请求。
 *
 * 缓存键 / 格式、属性名、事件名与 `Layout.astro`（首帧内联脚本）、`Header.astro`（`data-compact` 脚本）同值：
 * 内联脚本无法 import，改动要一起改。
 */
import { accountApi, hasSignedInHint } from "./client";
import type { AiStatus } from "./types";

/** localStorage 键；值 `<in|out>:<0|1>:<取值时刻 ms>`，首帧脚本只读前两段。 */
export const AI_ENTRY_CACHE_KEY = "fd_ai_visible";
/** `<html>` 上的显示标记。 */
export const AI_ENTRY_ATTR = "data-ai-visible";
/** 标记变化后在 `window` 上派发。 */
export const AI_ENTRY_EVENT = "fd:ai-visible";
const CACHE_TTL_MS = 60_000;

type Auth = "in" | "out";

/** 读缓存：形状不对（或存储不可用）一律当没有。 */
function readCache(): { auth: Auth; at: number } | null {
  try {
    const [auth, visible, at] = (localStorage.getItem(AI_ENTRY_CACHE_KEY) ?? "").split(":");
    const time = Number(at);
    if ((auth !== "in" && auth !== "out") || (visible !== "0" && visible !== "1") || !Number.isFinite(time)) return null;
    return { auth, at: time };
  } catch {
    return null;
  }
}

function writeCache(auth: Auth, visible: boolean): void {
  try {
    localStorage.setItem(AI_ENTRY_CACHE_KEY, `${auth}:${visible ? 1 : 0}:${Date.now()}`);
  } catch {
    // 存储不可用：只是下次首帧不预先显示
  }
}

export const isAiEntryVisible = (): boolean => document.documentElement.hasAttribute(AI_ENTRY_ATTR);

/** 订阅标记变化；返回取消订阅函数。 */
export function subscribeAiEntry(onChange: () => void): () => void {
  window.addEventListener(AI_ENTRY_EVENT, onChange);
  return () => window.removeEventListener(AI_ENTRY_EVENT, onChange);
}

function apply(visible: boolean): void {
  if (isAiEntryVisible() === visible) return;
  document.documentElement.toggleAttribute(AI_ENTRY_ATTR, visible);
  window.dispatchEvent(new Event(AI_ENTRY_EVENT));
}

/** 同登录态且缓存还新鲜就不请求；失败（网络 / 上游异常 / 形状不对）不动缓存与标记。 */
export async function refreshAiEntryVisibility(): Promise<void> {
  const cached = readCache();
  const age = cached ? Date.now() - cached.at : Number.NaN;
  if (cached && cached.auth === (hasSignedInHint() ? "in" : "out") && age >= 0 && age < CACHE_TTL_MS) return;

  const res = await accountApi<AiStatus>("ai/status");
  if (!res.ok || typeof res.data?.visible !== "boolean") return;
  // 登录态以响应回来之后为准：会话在这次请求里被判失效时 BFF 已清掉提示 Cookie，结果是匿名口径
  writeCache(hasSignedInHint() ? "in" : "out", res.data.visible);
  apply(res.data.visible);
}
