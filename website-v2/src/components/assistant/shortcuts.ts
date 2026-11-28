import { useEffect, useRef } from "react";

/** 助手区域根节点标记：整页工作区与悬浮面板各一个；输入框只在焦点位于其中（或无焦点）时自动抢焦点。 */
export const AI_ROOT_ATTR = "data-ai-root";

/** `aria-keyshortcuts` 里对外宣传的「新对话」组合键（只宣传浏览器一定能送达页面的那个）。 */
export const NEW_CHAT_ARIA_SHORTCUT = "Alt+N";

const isMac = (): boolean => typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);

/** 「新对话」快捷键的显示文案：macOS 显示 ⌥N，其余 Alt+N。 */
export const newChatShortcutLabel = (): string => (isMac() ? "⌥N" : "Alt+N");

/**
 * Alt+N（macOS 的 Option+N，用物理键位 `code` 判定，免得被输入法 / 死键改写成 ñ）；macOS 上 ⌘N 也认（只认 Meta，
 * 不认 Ctrl：Ctrl+N 是文本框里 emacs 风格的「下一行」），但它多数浏览器保留给「新窗口」，所以不对外宣传。
 * 不带 Shift，也不是组字过程中。
 */
function isNewChatKey(event: KeyboardEvent): boolean {
  if (event.code !== "KeyN" || event.shiftKey || event.isComposing) return false;
  if (event.altKey) return !event.ctrlKey && !event.metaKey;
  return isMac() && event.metaKey && !event.ctrlKey;
}

/**
 * Alt+N 新建对话。`scope: "root"` 只在焦点位于助手区域内时生效（悬浮面板，不劫持页面其余部分）；
 * `"page"` 整页都生效（助手整页）。Ctrl+N / Cmd+N 被浏览器保留给「新窗口」，页面收不到，所以不用它做主快捷键。
 */
export function useNewChatShortcut(onNewChat: () => void, scope: "root" | "page"): void {
  const handler = useRef(onNewChat);
  handler.current = onNewChat;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isNewChatKey(event)) return;
      if (scope === "root" && !(document.activeElement?.closest(`[${AI_ROOT_ATTR}]`))) return;
      event.preventDefault();
      handler.current();
    };
    window.addEventListener("keydown", onKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", onKeyDown, { capture: true });
  }, [scope]);
}
