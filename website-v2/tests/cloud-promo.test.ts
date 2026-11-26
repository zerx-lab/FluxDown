import { describe, expect, test } from "bun:test";
import { heroPromoView, pickHeroPromo } from "../src/lib/cloud-promo";

const NOW = Date.parse("2026-12-08T00:00:00Z");
const DAY = 24 * 60 * 60 * 1000;

function plan(over: Record<string, unknown> = {}, campaign: Record<string, unknown> | null = {}) {
  return {
    code: "pro",
    name: "专业版",
    priceMinor: 12900,
    currency: "CNY",
    purchasable: true,
    campaign:
      campaign &&
      {
        name: "双十二年终特惠",
        endAt: null,
        stages: [
          { label: "预热早鸟价", priceMinor: 10300, quota: 200 },
          { label: "常规特惠", priceMinor: 11900, quota: null },
        ],
        soldTotal: 100,
        stageSold: [100, 0],
        currentStageIndex: 0,
        effectivePriceMinor: 10300,
        ...campaign,
      },
    ...over,
  };
}

describe("pickHeroPromo", () => {
  test("picks the first discounted, purchasable campaign plan", () => {
    const free = { code: "free", name: "免费版", priceMinor: 0, currency: "CNY", campaign: null };
    expect(pickHeroPromo([free, plan()], NOW)).toEqual({
      campaignName: "双十二年终特惠",
      planName: "专业版",
      saleMinor: 10300,
      listMinor: 12900,
      currency: "CNY",
      stageLeft: 100,
      endAt: null,
    });
  });

  test("no campaign, no discount, or delisted plan shows nothing", () => {
    expect(pickHeroPromo([plan({}, null)], NOW)).toBeNull();
    expect(pickHeroPromo([plan({}, { effectivePriceMinor: 12900 })], NOW)).toBeNull();
    expect(pickHeroPromo([plan({ purchasable: false })], NOW)).toBeNull();
    expect(pickHeroPromo({ error: "x" }, NOW)).toBeNull();
  });

  test("a stale cached campaign past its end is ignored", () => {
    expect(pickHeroPromo([plan({}, { endAt: new Date(NOW - 1000).toISOString() })], NOW)).toBeNull();
  });

  test("an exhausted stage is skipped until the catalog advances", () => {
    expect(pickHeroPromo([plan({}, { stageSold: [200, 0] })], NOW)).toBeNull();
  });

  test("unlimited last stage has no quota", () => {
    const p = pickHeroPromo([plan({}, { currentStageIndex: 1, effectivePriceMinor: 11900 })], NOW);
    expect(p?.stageLeft).toBeNull();
  });
});

describe("heroPromoView", () => {
  test("zh uses campaign and plan names with remaining quota", () => {
    const p = pickHeroPromo([plan()], NOW);
    expect(p).not.toBeNull();
    if (!p) return;
    const v = heroPromoView(p, "zh", NOW);
    expect(v.title).toBe("双十二年终特惠 · 专业版");
    expect(v.sale).toBe("¥103");
    expect(v.list).toBe("¥129");
    expect(v.meta).toBe("本价位余 100 份");
  });

  test("en never shows the Chinese-only catalog names", () => {
    const p = pickHeroPromo([plan()], NOW);
    if (!p) throw new Error("expected promo");
    expect(heroPromoView(p, "en", NOW).title).toBe("FluxCloud limited offer");
  });

  test("deadline is mentioned only in the last week", () => {
    const unlimited = { currentStageIndex: 1, effectivePriceMinor: 11900 };
    const far = pickHeroPromo([plan({}, { ...unlimited, endAt: new Date(NOW + 20 * DAY).toISOString() })], NOW);
    const near = pickHeroPromo([plan({}, { ...unlimited, endAt: "2026-12-12T15:59:59Z" })], NOW);
    if (!far || !near) throw new Error("expected promos");
    expect(heroPromoView(far, "zh", NOW).meta).toBeNull();
    expect(heroPromoView(near, "zh", NOW).meta).toBe("12月12日截止");
    expect(heroPromoView(near, "en", NOW).meta).toBe("ends Dec 12");
  });
});
