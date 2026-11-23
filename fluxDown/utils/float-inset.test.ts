import { describe, expect, test } from "bun:test";
import { floatInsetFor, insetFloatTop } from "./float-inset";

describe("insetFloatTop", () => {
  test("贴顶部浮钮下移到避让区以下", () => {
    expect(insetFloatTop(200, 400, 28, 52, 8)).toBe(252);
  });

  test("避让小于常规边距时仍保留边距", () => {
    expect(insetFloatTop(200, 400, 28, 4, 8)).toBe(208);
  });

  test("元素剩余高度放不下浮钮时返回 null（隐藏，不遮挡原生控件）", () => {
    expect(insetFloatTop(200, 288, 28, 52, 8)).toBe(252);
    expect(insetFloatTop(200, 287, 28, 52, 8)).toBeNull();
  });
});

describe("floatInsetFor", () => {
  const el = {
    closest(selector: string) {
      if (selector === "bad[") throw new SyntaxError("invalid selector");
      return selector === ".mini" || selector === ".player" ? {} : null;
    },
  } as unknown as Pick<Element, "closest">;

  test("取命中规则里最大的避让，未命中与非法选择器不计", () => {
    expect(floatInsetFor(el, [{ selector: ".player", top: 20 }, { selector: ".mini", top: 52 }, { selector: ".other", top: 90 }])).toBe(52);
    expect(floatInsetFor(el, [{ selector: "bad[", top: 90 }, { selector: ".player", top: 20 }])).toBe(20);
    expect(floatInsetFor(el, [{ selector: ".other", top: 52 }])).toBe(0);
  });
});
