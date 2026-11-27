import { useEffect, useRef } from "react";

/** 助手区域根节点标记：整页工作区与悬浮面板各一个；输入框只在焦点位于其中（或无焦点）时自动抢焦点。 */
export const AI_ROOT_ATTR = "data-ai-root";

const isMac = (): boolean => typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);

/** 「新对话」快捷键的显示文案：macOS 显示 ⌘N，其余 Ctrl+N。 */
export const newChatShortcutLabel = (): string => (isMac() ? "⌘N" : "Ctrl+N");

/** Ctrl+N 或 Cmd+N（不带 Shift / Alt，也不是两者同按）。 */
function isNewChatKey(event: KeyboardEvent): boolean {
  return (
    event.key.toLowerCase() === "n" &&
    event.ctrlKey !== event.metaKey &&
    !event.shiftKey &&
    !event.altKey &&
    !event.isComposing
  );
}

/**
 * Ctrl+N / Cmd+N 新建对话。`scope: "root"` 只在焦点位于助手区域内时生效（悬浮面板，不劫持页面其余部分）；
 * `"page"` 整页都生效（助手整页）。注意 Chrome 等浏览器把 Ctrl+N / Cmd+N 保留给「新窗口」，
 * 页面收不到这次按键时快捷键不会触发（浏览器行为，页面无法覆盖）。
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
