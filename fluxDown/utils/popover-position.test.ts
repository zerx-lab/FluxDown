import { describe, expect, test } from "bun:test";
import { placePopover, type PopoverPlacementInput } from "./popover-position";

const BASE: PopoverPlacementInput = {
  anchor: { left: 400, top: 300, width: 80, height: 28 },
  size: { width: 240, height: 200 },
  viewport: { width: 1280, height: 720 },
  align: "end",
  prefer: "below",
  gap: 6,
  margin: 8,
  maxHeight: 280,
};

function place(patch: Partial<PopoverPlacementInput>) {
  return placePopover({ ...BASE, ...patch });
}

describe("placePopover 水平", () => {
  test("空间充足：按优先对齐（end = 右边缘对齐按钮）", () => {
    const p = place({});
    expect(p.left).toBe(400 + 80 - 240);
  });

  test("按钮贴左缘、右对齐会被左边裁掉：翻成左边缘对齐向右展开（回归：截图里的左半截不可见）", () => {
    const p = place({ anchor: { left: 12, top: 300, width: 80, height: 28 } });
    expect(p.left).toBe(12);
    expect(p.left).toBeGreaterThanOrEqual(8);
  });

  test("按钮贴右缘、start 对齐会溢出右边：翻成右边缘对齐", () => {
    const p = place({ align: "start", anchor: { left: 1180, top: 300, width: 80, height: 28 } });
    expect(p.left).toBe(1180 + 80 - 240);
    expect(p.left + 240).toBeLessThanOrEqual(1280 - 8);
  });

  test("按钮贴左缘且左边距小于留白：翻转后仍夹到 margin", () => {
    const p = place({ anchor: { left: 4, top: 300, width: 80, height: 28 } });
    expect(p.left).toBe(8);
  });

  test("视口比弹层窄：宽度上限 = 视口 - 2*margin，位置夹在 margin", () => {
    const p = place({ viewport: { width: 200, height: 720 }, anchor: { left: 100, top: 300, width: 80, height: 28 } });
    expect(p.maxWidth).toBe(184);
    expect(p.left).toBe(8);
  });

  test("两种对齐都放不下但视口足以容纳：夹紧到最近的合法位置", () => {
    // 视口 300、弹层 280：end 对齐左溢出、start 对齐右溢出，只能夹到 margin
    const p = place({
      viewport: { width: 300, height: 720 },
      size: { width: 280, height: 100 },
      anchor: { left: 150, top: 300, width: 80, height: 28 },
    });
    expect(p.left).toBe(8);
    expect(p.left + 280).toBeLessThanOrEqual(300 - 8);
  });
});

describe("placePopover 垂直", () => {
  test("下方空间足够：放在按钮下方", () => {
    const p = place({});
    expect(p.side).toBe("below");
    expect(p.top).toBe(300 + 28 + 6);
    expect(p.height).toBe(200);
  });

  test("下方放不下、上方放得下：翻到按钮上方且底边贴着按钮", () => {
    const p = place({ anchor: { left: 400, top: 600, width: 80, height: 28 } });
    expect(p.side).toBe("above");
    expect(p.height).toBe(200);
    expect(p.top + p.height).toBe(600 - 6);
  });

  test("优先上方但上方放不下、下方放得下：翻到下方", () => {
    const p = place({ prefer: "above", anchor: { left: 400, top: 100, width: 80, height: 28 } });
    expect(p.side).toBe("below");
    expect(p.top).toBe(100 + 28 + 6);
  });

  test("两侧都放不下：取空间更大的一侧并缩小高度，不越过视口边距", () => {
    const p = place({
      size: { width: 240, height: 500 },
      maxHeight: 500,
      viewport: { width: 1280, height: 400 },
      anchor: { left: 400, top: 250, width: 80, height: 28 },
    });
    // 下方: 400-8-(250+28+6)=108；上方: 250-6-8=236
    expect(p.side).toBe("above");
    expect(p.height).toBe(236);
    expect(p.top).toBe(8);
  });

  test("内容高度超过 maxHeight：高度被 maxHeight 限制（内部滚动）", () => {
    const p = place({ size: { width: 240, height: 900 }, maxHeight: 280 });
    expect(p.height).toBe(280);
  });

  test("按钮已在视口外下缘：不会产生负高度", () => {
    const p = place({ anchor: { left: 400, top: 740, width: 80, height: 28 }, viewport: { width: 1280, height: 720 } });
    expect(p.height).toBeGreaterThanOrEqual(0);
    expect(p.side).toBe("above");
  });
});
