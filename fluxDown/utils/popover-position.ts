/**
 * 弹层定位（纯函数，坐标全部是视口坐标）。
 *
 * 水平：优先按 `align` 与锚点对齐（start = 左边缘对齐向右展开，end = 右边缘对齐向左展开）；
 *   放不下则翻到另一种对齐；两种都放不下（视口比弹层还窄）就把优先对齐的位置夹进视口。
 * 垂直：优先 `prefer` 一侧；放不下翻到另一侧；两侧都放不下则选空间更大的一侧并缩小高度（调用方滚动）。
 */

export interface PopRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface PopSize {
  width: number;
  height: number;
}

export interface PopoverPlacementInput {
  /** 锚点（按钮）的视口矩形。 */
  anchor: PopRect;
  /** 弹层自然尺寸（不受 max-height 限制时的内容尺寸）。 */
  size: PopSize;
  /** 视口可用尺寸（不含滚动条）。 */
  viewport: PopSize;
  /** 优先水平对齐方向。 */
  align: "start" | "end";
  /** 优先垂直方向。 */
  prefer: "below" | "above";
  /** 弹层与锚点的间距。 */
  gap: number;
  /** 弹层与视口边缘的最小留白。 */
  margin: number;
  /** 弹层高度上限（再大也滚动）。 */
  maxHeight: number;
}

export interface PopoverPlacement {
  /** 弹层左上角的视口坐标。 */
  left: number;
  top: number;
  /** 弹层最终宽度上限 / 高度（内容超出时由调用方滚动）。 */
  maxWidth: number;
  height: number;
  side: "below" | "above";
}

export function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(value, Math.max(min, max)));
}

export function placePopover(input: PopoverPlacementInput): PopoverPlacement {
  const { anchor, size, viewport, align, prefer, gap, margin } = input;

  const maxWidth = Math.max(0, viewport.width - margin * 2);
  const width = Math.min(size.width, maxWidth);
  const startX = anchor.left;
  const endX = anchor.left + anchor.width - width;
  const fits = (x: number): boolean => x >= margin && x + width <= viewport.width - margin;
  const [first, second] = align === "start" ? [startX, endX] : [endX, startX];
  const left = fits(first)
    ? first
    : fits(second)
      ? second
      : clamp(first, margin, viewport.width - margin - width);

  const wanted = Math.min(size.height, input.maxHeight);
  const below = viewport.height - margin - (anchor.top + anchor.height + gap);
  const above = anchor.top - gap - margin;
  const room = (side: "below" | "above"): number => Math.max(0, side === "below" ? below : above);
  const other = prefer === "below" ? "above" : "below";
  const side: "below" | "above" = wanted <= room(prefer)
    ? prefer
    : wanted <= room(other)
      ? other
      : room(prefer) >= room(other)
        ? prefer
        : other;
  const height = Math.min(wanted, room(side));
  const top = side === "below" ? anchor.top + anchor.height + gap : anchor.top - gap - height;

  return { left, top, maxWidth, height, side };
}
