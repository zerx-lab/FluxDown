/** FluxCloud public plan catalog entry (GET /api/cloud/plans, wire camelCase). */
export interface CloudCampaign {
  name: string;
  endAt: string | null;
  stages: { label: string; priceMinor: number; quota: number | null }[];
  soldTotal: number;
  stageSold: number[];
  currentStageIndex: number;
  effectivePriceMinor: number;
  effectivePriceUsdtMicros?: number | null;
}

export interface CloudPlan {
  code: string;
  name: string;
  description: string;
  badge: string | null;
  badgeStyle: string;
  badgeColor: string;
  badgeNumbered: boolean;
  badgeNumberDigits: number;
  icon: string;
  color: string;
  priceMinor: number;
  priceUsdtMicros?: number | null;
  currency: string;
  highlights: string[];
  /** Missing = purchasable (older catalog responses). Delisted-but-shown plans are false. */
  purchasable?: boolean;
  campaign: CloudCampaign | null;
  /** Membership card period in days; missing / null = permanent plan. */
  billingPeriodDays?: number | null;
}

/** What the web purchase dialog needs from a plan card. */
export interface PlanBrief {
  code: string;
  name: string;
  /** Price currently shown on the card (campaign price, minor units); the order is authoritative. */
  priceMinor: number;
  currency: string;
  /** Undiscounted list price (minor units); USDT is quoted proportionally to it. */
  basePriceMinor: number;
  /** USDT price at the list price (micros); null = USDT not offered. */
  priceUsdtMicros: number | null;
}
