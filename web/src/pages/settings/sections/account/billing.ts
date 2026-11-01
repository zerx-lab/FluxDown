// 套餐购买 / 推介的纯逻辑（镜像 GPUI `crates/account/src/billing.rs`，两端用同一组用例）。
// 金额一律为「分」整数；格式化不经浮点。

import type { CloudPlan, CloudSubscription } from '../../../../lib/rpc'

/** 每人最多推荐码个数。 */
export const MAX_REFERRAL_CODES = 10
/** 推介分页大小。 */
export const REFERRAL_PAGE_SIZE = 20

/** 分 → `¥12.30`（CNY）/ `USD 12.30`（其余，代码大写）；负数前置 `-`。 */
export function formatMinor(minor: number, currency: string): string {
  const negative = minor < 0
  const abs = Math.abs(Math.trunc(minor))
  const body = `${Math.trunc(abs / 100)}.${String(abs % 100).padStart(2, '0')}`
  const code = currency.trim().toUpperCase()
  const text = code === 'CNY' ? `¥${body}` : `${code} ${body}`
  return negative ? `-${text}` : text
}

/** 生效价：有活动取活动生效价，否则标价。 */
export function effectivePriceMinor(plan: CloudPlan): number {
  return plan.campaign ? plan.campaign.effectivePriceMinor : plan.priceMinor
}

/** 周期会员卡：`billingPeriodDays` 非空且 > 0；缺失 / null 为永久套餐。 */
export function isSubscriptionPlan(plan: CloudPlan): boolean {
  return typeof plan.billingPeriodDays === 'number' && plan.billingPeriodDays > 0
}

/** 永久套餐：可购买会员中非周期卡。 */
export function lifetimePlans(plans: readonly CloudPlan[]): CloudPlan[] {
  return plans.filter((plan) => !isSubscriptionPlan(plan))
}

/** 会员卡：可购买会员中的周期卡。 */
export function cardPlans(plans: readonly CloudPlan[]): CloudPlan[] {
  return plans.filter(isSubscriptionPlan)
}

/** 已持有永久套餐的 sort：在完整目录里找；找不到返回 null（不据此拦截会员卡）。 */
export function baseSortOf(catalog: readonly CloudPlan[], currentCode: string | null | undefined): number | null {
  return catalog.find((plan) => plan.code === (currentCode || 'free'))?.sort ?? null
}

/** 可购买会员列表：membership、purchasable、非 free、生效价 > 0，按 sort 升序稳定排序。 */
export function purchasablePlans(catalog: readonly CloudPlan[]): CloudPlan[] {
  return catalog
    .filter((plan) => plan.purchasable && plan.code !== 'free' && effectivePriceMinor(plan) > 0)
    .map((plan, index) => ({ plan, index }))
    .sort((a, b) => a.plan.sort - b.plan.sort || a.index - b.index)
    .map(({ plan }) => plan)
}

/** 已持有套餐基准价：在完整目录里找当前套餐标价；找不到或 free → 0。 */
export function ownedPriceMinor(catalog: readonly CloudPlan[], currentCode: string | null | undefined): number {
  if (!currentCode || currentCode === 'free') return 0
  return catalog.find((plan) => plan.code === currentCode)?.priceMinor ?? 0
}

export type PlanCardState = 'current' | 'renewable' | 'tierBlocked' | 'creditBlocked' | 'available'

/**
 * 卡片状态优先级：Current > TierBlocked > CreditBlocked > Available。
 * 周期会员卡：同卡 → Renewable；有其他生效卡 → TierBlocked；sort ≤ 永久套餐 sort → TierBlocked；否则 Available；不抵扣。
 */
export function planCardState(
  plan: CloudPlan,
  currentCode: string | null | undefined,
  ownedPrice: number,
  creditMinor: number,
  activeSubscription: CloudSubscription | null | undefined = null,
  baseSort: number | null = null,
): PlanCardState {
  if (isSubscriptionPlan(plan)) {
    if (activeSubscription) return activeSubscription.planCode === plan.code ? 'renewable' : 'tierBlocked'
    if (baseSort !== null && plan.sort <= baseSort) return 'tierBlocked'
    return 'available'
  }
  if (currentCode && plan.code === currentCode) return 'current'
  if (ownedPrice > 0 && plan.priceMinor <= ownedPrice) return 'tierBlocked'
  if (creditMinor > 0 && effectivePriceMinor(plan) <= creditMinor) return 'creditBlocked'
  return 'available'
}

/** 应付：有抵扣时 生效价 − credit，否则生效价。 */
export function payableMinor(plan: CloudPlan, creditMinor: number): number {
  const effective = effectivePriceMinor(plan)
  return creditMinor > 0 ? effective - creditMinor : effective
}

export interface PriceDisplay {
  /** 主价（应付）。 */
  payable: number
  /** 划线价；无则 null。 */
  strike: number | null
  /** 是否显示「已抵扣当前套餐」。 */
  creditApplied: boolean
  /** 无抵扣且有活动时的当前档位名。 */
  stageLabel: string | null
}

/** 抵扣额只作用于可购买（available）卡片：当前 / 不可选套餐用已付金额去减会把「已付」误显示成「已抵扣」。 */
export function priceDisplay(plan: CloudPlan, state: PlanCardState, creditMinor: number): PriceDisplay {
  if (state !== 'available' || isSubscriptionPlan(plan)) creditMinor = 0
  const payable = payableMinor(plan, creditMinor)
  if (creditMinor > 0) return { payable, strike: effectivePriceMinor(plan), creditApplied: true, stageLabel: null }
  if (plan.campaign) {
    const label = plan.campaign.stages[plan.campaign.currentStageIndex]?.label ?? ''
    return { payable, strike: plan.priceMinor, creditApplied: false, stageLabel: label === '' ? null : label }
  }
  return { payable, strike: null, creditApplied: false, stageLabel: null }
}

export interface CampaignProgress {
  sold: number
  quota: number
  /** 0..1。 */
  ratio: number
}

/** 活动当前档位有 quota 时的限量进度；`sold = stageSold[currentStageIndex]`。 */
export function campaignProgress(plan: CloudPlan): CampaignProgress | null {
  const campaign = plan.campaign
  if (!campaign) return null
  const quota = campaign.stages[campaign.currentStageIndex]?.quota
  if (quota === null || quota === undefined) return null
  const sold = campaign.stageSold[campaign.currentStageIndex] ?? 0
  return { sold, quota, ratio: quota > 0 ? Math.min(1, Math.max(0, sold / quota)) : 1 }
}

/** RFC3339 → 本地日期 YYYY-MM-DD（活动截止 / 记录创建日）；空 → null，解析失败原样返回。 */
export function localDate(endAt: string | null | undefined): string | null {
  if (!endAt) return null
  const time = Date.parse(endAt)
  if (Number.isNaN(time)) return endAt
  const date = new Date(time)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}

/** 倒计时：负数按 0；< 3600 → `mm:ss`，否则 `h:mm:ss`。 */
export function formatCountdown(secs: number): string {
  const total = Math.max(0, Math.floor(secs))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  const pad = (value: number) => String(value).padStart(2, '0')
  return total < 3600 ? `${pad(minutes)}:${pad(seconds)}` : `${hours}:${pad(minutes)}:${pad(seconds)}`
}

/** 剩余秒 = floor((expiresAt − now) / 1000)，不钳制（formatCountdown 负数按 0）；解析失败按 0。 */
export function remainingSecs(expiresAt: string, nowMs: number): number {
  const time = Date.parse(expiresAt)
  if (Number.isNaN(time)) return 0
  return Math.floor((time - nowMs) / 1000)
}

/** 推荐码规范化：trim + 大写。 */
export function normalizeReferralCode(input: string): string {
  return input.trim().toUpperCase()
}

/** 创建码校验：空（随机生成）或 4–16 位大写字母数字。 */
export function validCreateCode(input: string): boolean {
  const code = normalizeReferralCode(input)
  return code === '' || /^[A-Z0-9]{4,16}$/.test(code)
}

/** `agent.referral.validate` 返回 `valid=false` 时的 reason → i18n 键。 */
export function referralReasonKey(reason: string | null | undefined): string {
  switch (reason) {
    case 'self_use':
      return 'accountPlanErrorReferralSelfUse'
    case 'already_used':
      return 'accountPlanErrorReferralAlreadyUsed'
    case 'not_found':
      return 'accountPlanErrorReferralNotFound'
    case 'plan_excluded':
      return 'accountPlanErrorReferralPlanExcluded'
    default:
      return 'accountPlanErrorReferralInvalid'
  }
}

export type PlanIconName = 'crown' | 'rocket' | 'star' | 'gem' | 'diamond' | 'zap' | 'shield' | 'sparkles'

const PLAN_ICON_NAMES: readonly PlanIconName[] = ['crown', 'rocket', 'star', 'gem', 'diamond', 'zap', 'shield', 'sparkles']

/** 套餐图标名：未知 → crown。 */
export function planIconName(icon: string): PlanIconName {
  const name = icon.trim().toLowerCase()
  return PLAN_ICON_NAMES.find((candidate) => candidate === name) ?? 'crown'
}

/** 总页数（至少 1）。 */
export function pageCount(total: number, pageSize: number): number {
  return Math.max(1, Math.ceil(Math.max(0, total) / pageSize))
}

/** 推介记录状态 → 徽标语气与文案键。 */
export function recordStatus(status: string): { tone: 'warning' | 'success' | 'neutral'; key: string } {
  switch (status) {
    case 'paid':
      return { tone: 'success', key: 'accountReferralStatusPaid' }
    case 'revoked':
      return { tone: 'neutral', key: 'accountReferralStatusRevoked' }
    default:
      return { tone: 'warning', key: 'accountReferralStatusPending' }
  }
}
