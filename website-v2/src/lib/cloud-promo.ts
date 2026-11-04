/**
 * 官网云端专业版限时促销（展示层单一事实源）。
 *
 * 成交价仍以 FluxCloud 套餐 list + 销售活动为准；本文件只驱动首页 / 定价页 /
 * 公告条的文案与可见性。改价或改期只动这里，并同步后台：
 *   - 专业版 list = listPriceMinor
 *   - 活动一档 effective = salePriceMinor，endAt 对齐本文件
 */
import type { Lang } from "@/i18n/config";

export const CLOUD_PROMO = {
  id: "pro-list-129-sale-99",
  /** 专业版 list（分）。 */
  listPriceMinor: 12_900,
  /** 活动成交价（分）。 */
  salePriceMinor: 9_900,
  currency: "CNY",
  /**
   * 含 11 月 10 日当天（上海 23:59:59）。
   * 「11 月 10 号前」按营销惯例覆盖截止日当天。
   */
  endAt: "2026-11-10T15:59:59.000Z",
  /** 公告 / 埋点 id，与 announcements 条目共用。 */
  announcementId: "pro-promo-129-99",
  planPath: "/pricing",
} as const;

export type CloudPromo = typeof CLOUD_PROMO;

export function isCloudPromoActive(nowMs: number = Date.now()): boolean {
  const end = Date.parse(CLOUD_PROMO.endAt);
  return Number.isFinite(end) && nowMs <= end;
}

/** 分 → 展示价（CNY 无小数时不带 .00）。 */
export function formatPromoYuan(minor: number, lang: Lang): string {
  const yuan = minor / 100;
  const whole = Number.isInteger(yuan) ? String(yuan) : yuan.toFixed(2);
  return lang === "zh" ? `¥${whole}` : `CN¥${whole}`;
}

/** 截止日短标签（首页条 / 定价条）。 */
export function formatPromoEndLabel(lang: Lang): string {
  const d = new Date(CLOUD_PROMO.endAt);
  if (lang === "zh") {
    return `${d.getUTCMonth() + 1} 月 ${d.getUTCDate()} 日`;
  }
  return new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  }).format(d);
}

export const cloudPromoCopy = {
  en: {
    badge: "Limited",
    product: "Cloud Pro · permanent",
    /** 无障碍完整句（aria-label）。 */
    line: (sale: string, list: string, end: string) =>
      `Cloud Pro permanent ${sale} (list ${list}), ends ${end}`,
    cta: "Details",
    ctaBuy: "Buy",
    ends: (label: string) => `until ${label}`,
    heroBadge: "Pro permanent · CN¥99 · list CN¥129",
    annText: "Cloud Pro permanent: CN¥99 through Nov 10 (list CN¥129).",
    finalCta: "Pro · CN¥99 until Nov 10",
  },
  zh: {
    badge: "限时",
    product: "云端专业版 · 永久",
    line: (sale: string, list: string, end: string) =>
      `云端专业版永久 ${sale}（定价 ${list}），${end} 截止`,
    cta: "详情",
    ctaBuy: "购买",
    ends: (label: string) => `${label} 截止`,
    heroBadge: "专业版永久 · ¥99 · 定价 ¥129",
    annText: "云端专业版永久：¥99（定价 ¥129），11 月 10 日截止。",
    finalCta: "专业版 · 11 月 10 日前 ¥99",
  },
} as const;
