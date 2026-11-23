/** page 规则 `player` `float: "inset"` 的一条避让：`selector` 容器内的媒体元素，贴顶部的浮钮离元素顶部至少 `top` px。 */
export interface FloatInset {
  selector: string;
  top: number;
}

/** 媒体元素命中的 inset 规则里最大的顶部避让（px）；都未命中返回 0。非法选择器视为未命中。 */
export function floatInsetFor(el: Pick<Element, "closest">, insets: readonly FloatInset[]): number {
  let top = 0;
  for (const inset of insets) {
    if (inset.top <= top) continue;
    try {
      if (el.closest(inset.selector) !== null) top = inset.top;
    } catch {
      /* 非法选择器：视为未命中（同 MediaTracker.isFloatExcluded） */
    }
  }
  return top;
}

/**
 * 贴顶部浮钮在 inset 规则下的纵坐标（视口 Y，与 getBoundingClientRect 同系）：离媒体元素顶部至少
 * `inset` px（且不小于常规边距 `margin`），避开播放器自带的顶部控件条。元素剩余高度放不下浮钮时
 * 返回 null：宁可隐藏浮钮，也不遮挡原生控件。
 */
export function insetFloatTop(
  mediaTop: number,
  mediaBottom: number,
  badgeHeight: number,
  inset: number,
  margin: number,
): number | null {
  const top = mediaTop + Math.max(inset, margin);
  return top + badgeHeight + margin <= mediaBottom ? top : null;
}
