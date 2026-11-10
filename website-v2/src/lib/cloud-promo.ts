/**
 * 官网云端专业版限时促销（展示层）。
 *
 * 可见价一律跟 FluxCloud 公开目录里的专业版（`planCode`）走：
 *   list = plan.priceMinor
 *   sale = campaign?.effectivePriceMinor ?? list
 * 本文件只管：绑哪个套餐、促销条展示窗口、文案模板。
 * 改价只改后台套餐 / 销售活动，不必再改这里的数字。
 */
import type { Lang } from "@/i18n/config";

export const CLOUD_PROMO = {
  /** 与 FluxCloud catalog `code` 对齐。 */
  planCode: "pro",
  /**
   * 促销条 / Hero / CTA 展示窗口截止（含当天上海 23:59:59）。
   * 与成交价无关；成交价以 catalog 活动为准。
   */
  endAt: "2026-11-10T15:59:59.000Z",
  /** 公告 / 埋点 id，与 announcements 条目共用。 */
  announcementId: "pro-promo-129-99",
  planPath: "/pricing",
} as const;

export type CloudPromo = typeof CLOUD_PROMO;

export type PromoPrices = {
  listMinor: number;
  saleMinor: number;
  currency: string;
  hasDiscount: boolean;
};

/** 目录条目 → 促销展示价；非目标套餐或无效价返回 null。 */
export function promoPricesFromPlan(plan: {
  code: string;
  priceMinor: number;
  currency?: string | null;
  campaign?: { effectivePriceMinor: number } | null;
}): PromoPrices | null {
  if (plan.code !== CLOUD_PROMO.planCode) return null;
  if (!Number.isFinite(plan.priceMinor) || plan.priceMinor < 0) return null;
  const listMinor = plan.priceMinor;
  const rawSale = plan.campaign?.effectivePriceMinor;
  const saleMinor =
    rawSale != null && Number.isFinite(rawSale) && rawSale >= 0 ? rawSale : listMinor;
  return {
    listMinor,
    saleMinor,
    currency: (plan.currency && plan.currency.trim()) || "CNY",
    hasDiscount: saleMinor < listMinor,
  };
}

export function isCloudPromoActive(nowMs: number = Date.now()): boolean {
  const end = Date.parse(CLOUD_PROMO.endAt);
  return Number.isFinite(end) && nowMs <= end;
}

/** 分 → 展示价（与定价卡同口径：整元无小数）。 */
export function formatPromoYuan(minor: number, currency: string, lang: Lang): string {
  const cur = (currency || "CNY").toUpperCase();
  const yuan = minor / 100;
  const whole = Number.isInteger(yuan) ? String(yuan) : yuan.toFixed(2);
  if (cur === "CNY") return lang === "zh" ? `¥${whole}` : `CN¥${whole}`;
  return lang === "zh" ? `${whole} ${cur}` : `${cur} ${whole}`;
}

export const cloudPromoCopy = {
  en: {
    badge: "Limited",
    product: "Cloud Pro · permanent",
    line: (sale: string, list: string | null) =>
      list ? `Cloud Pro permanent ${sale} (list ${list})` : `Cloud Pro permanent ${sale}`,
    cta: "Details",
    ctaBuy: "Buy",
    heroBadge: (sale: string, list: string | null) =>
      list ? `Pro permanent · ${sale} · list ${list}` : `Pro permanent · ${sale}`,
    /** 公告列表不写死价，避免与目录脱节。 */
    annText: "Cloud Pro permanent is on a limited offer — see Pricing.",
    finalCta: (sale: string) => `Pro · ${sale}`,
  },
  zh: {
    badge: "限时",
    product: "云端专业版 · 永久",
    line: (sale: string, list: string | null) =>
      list ? `云端专业版永久 ${sale}（定价 ${list}）` : `云端专业版永久 ${sale}`,
    cta: "详情",
    ctaBuy: "购买",
    heroBadge: (sale: string, list: string | null) =>
      list ? `专业版永久 · ${sale} · 定价 ${list}` : `专业版永久 · ${sale}`,
    annText: "云端专业版永久限时优惠，详见定价页。",
    finalCta: (sale: string) => `专业版 · ${sale}`,
  },
} as const;

export function promoAriaLine(lang: Lang, sale: string, list: string | null): string {
  return cloudPromoCopy[lang].line(sale, list);
}
