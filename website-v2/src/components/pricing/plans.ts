import type { CloudPlan, PlanBrief } from "./types";

/** 生效价（分）：进行中的活动价优先，否则标价。 */
export const effectivePrice = (plan: CloudPlan): number => plan.campaign?.effectivePriceMinor ?? plan.priceMinor;

/** 周期会员卡：计费周期为正数（缺省 / null = 永久套餐）。 */
export function isCard(plan: CloudPlan): boolean {
  return typeof plan.billingPeriodDays === "number" && plan.billingPeriodDays > 0;
}

/** 可在官网购买：未下架（缺省视为可购买）且生效价为正。 */
export const isBuyablePlan = (plan: CloudPlan): boolean => plan.purchasable !== false && effectivePrice(plan) > 0;

/** 购买弹窗需要的套餐摘要。官网不提供 USDT（大陆备案站点政策），与定价页一致。 */
export function planBrief(plan: CloudPlan): PlanBrief {
  return {
    code: plan.code,
    name: plan.name,
    priceMinor: effectivePrice(plan),
    currency: plan.currency,
    basePriceMinor: plan.priceMinor,
    priceUsdtMicros: null,
    waffoPriceMinor: plan.waffoPriceMinor ?? null,
  };
}
