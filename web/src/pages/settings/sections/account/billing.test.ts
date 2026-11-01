import { describe, expect, test } from 'bun:test'
import type { CloudPlan } from '../../../../lib/rpc'
import {
  cardPlans,
  lifetimePlans,
  localDate,
  campaignProgress,
  effectivePriceMinor,
  formatCountdown,
  formatMinor,
  normalizeReferralCode,
  ownedPriceMinor,
  pageCount,
  payableMinor,
  planCardState,
  planIconName,
  priceDisplay,
  purchasablePlans,
  referralReasonKey,
  remainingSecs,
  validCreateCode,
} from './billing'

const plan = (code: string, priceMinor: number, extra: Partial<CloudPlan> = {}): CloudPlan => ({
  code,
  name: code,
  description: '',
  badge: null,
  icon: 'crown',
  color: '#336699',
  badgeStyle: 'plain',
  badgeColor: '#336699',
  badgeNumbered: false,
  badgeNumberDigits: 1,
  priceMinor,
  currency: 'CNY',
  highlights: [],
  entitlements: {},
  sort: 0,
  campaign: null,
  purchasable: true,
  ...extra,
})

const campaign = (effectivePriceMinor: number) => ({
  name: 'launch',
  endAt: null,
  stages: [
    { label: '早鸟', priceMinor: effectivePriceMinor, quota: 100 },
    { label: '常规', priceMinor: 9900, quota: null },
  ],
  soldTotal: 40,
  stageSold: [40, 0],
  currentStageIndex: 0,
  effectivePriceMinor,
})

describe('formatMinor', () => {
  test('CNY 与其它币种', () => {
    expect(formatMinor(1230, 'CNY')).toBe('¥12.30')
    expect(formatMinor(1230, 'cny')).toBe('¥12.30')
    expect(formatMinor(1230, 'usd')).toBe('USD 12.30')
    expect(formatMinor(5, 'CNY')).toBe('¥0.05')
    expect(formatMinor(0, 'CNY')).toBe('¥0.00')
    expect(formatMinor(100, 'EUR')).toBe('EUR 1.00')
  })
  test('负数前置 -', () => {
    expect(formatMinor(-1230, 'CNY')).toBe('-¥12.30')
    expect(formatMinor(-5, 'usd')).toBe('-USD 0.05')
    expect(formatMinor(123456789, 'CNY')).toBe('¥1234567.89')
  })
})

describe('价格与列表', () => {
  test('生效价取活动价', () => {
    expect(effectivePriceMinor(plan('a', 9900))).toBe(9900)
    expect(effectivePriceMinor(plan('a', 9900, { campaign: campaign(4900) }))).toBe(4900)
  })
  test('可购买过滤 + sort 稳定升序', () => {
    const list = purchasablePlans([
      plan('free', 0),
      plan('b', 2000, { sort: 2 }),
      plan('a', 1000, { sort: 1 }),
      plan('c', 3000, { sort: 2 }),
      plan('hidden', 1000, { purchasable: false }),
      plan('zero', 5000, { campaign: campaign(0) }),
    ])
    expect(list.map((p) => p.code)).toEqual(['a', 'b', 'c'])
  })
  test('已持有基准价', () => {
    const catalog = [plan('free', 0), plan('pro', 3000), plan('max', 6000)]
    expect(ownedPriceMinor(catalog, 'pro')).toBe(3000)
    expect(ownedPriceMinor(catalog, 'free')).toBe(0)
    expect(ownedPriceMinor(catalog, 'gone')).toBe(0)
    expect(ownedPriceMinor(catalog, null)).toBe(0)
  })
})

describe('卡片状态', () => {
  const pro = plan('pro', 3000)
  const max = plan('max', 6000)
  test('优先级 Current > TierBlocked > CreditBlocked > Available', () => {
    expect(planCardState(pro, 'pro', 3000, 3000)).toBe('current')
    expect(planCardState(pro, 'max', 6000, 6000)).toBe('tierBlocked')
    expect(planCardState(max, 'pro', 3000, 6000)).toBe('creditBlocked')
    expect(planCardState(max, 'pro', 3000, 3000)).toBe('available')
    expect(planCardState(max, null, 0, 0)).toBe('available')
  })
  test('价格相等也算 TierBlocked；活动价 <= credit 为 CreditBlocked', () => {
    expect(planCardState(plan('x', 3000), 'pro', 3000, 0)).toBe('tierBlocked')
    expect(planCardState(plan('y', 9900, { campaign: campaign(2000) }), null, 0, 2000)).toBe('creditBlocked')
  })
})

describe('应付与价格区', () => {
  const max = plan('max', 6000)
  test('应付', () => {
    expect(payableMinor(max, 0)).toBe(6000)
    expect(payableMinor(max, 2000)).toBe(4000)
    expect(payableMinor(plan('y', 9900, { campaign: campaign(4900) }), 1000)).toBe(3900)
  })
  test('价格区：抵扣 > 活动 > 普通', () => {
    expect(priceDisplay(max, 'available', 2000)).toEqual({ payable: 4000, strike: 6000, creditApplied: true, stageLabel: null })
    const promo = plan('y', 9900, { campaign: campaign(4900) })
    expect(priceDisplay(promo, 'available', 0)).toEqual({ payable: 4900, strike: 9900, creditApplied: false, stageLabel: '早鸟' })
    expect(priceDisplay(promo, 'available', 1000)).toEqual({ payable: 3900, strike: 4900, creditApplied: true, stageLabel: null })
    expect(priceDisplay(max, 'available', 0)).toEqual({ payable: 6000, strike: null, creditApplied: false, stageLabel: null })
    // 当前 / 不可选套餐不套用抵扣：显示原本价格。
    for (const state of ['current', 'tierBlocked', 'creditBlocked'] as const) {
      expect(priceDisplay(max, state, 2000)).toEqual({ payable: 6000, strike: null, creditApplied: false, stageLabel: null })
    }
  })
  test('活动进度与截止日期', () => {
    expect(campaignProgress(plan('y', 9900, { campaign: campaign(4900) }))).toEqual({ sold: 40, quota: 100, ratio: 0.4 })
    expect(campaignProgress(max)).toBeNull()
    const open = { ...campaign(4900), currentStageIndex: 1 }
    expect(campaignProgress(plan('z', 9900, { campaign: open }))).toBeNull()
    expect(localDate(null)).toBeNull()
    expect(localDate('bad')).toBe('bad')
    expect(/^\d{4}-\d{2}-\d{2}$/.test(localDate('2026-10-17T12:00:00Z') ?? '')).toBe(true)
  })
})

describe('倒计时', () => {
  test('格式', () => {
    expect(formatCountdown(-5)).toBe('00:00')
    expect(formatCountdown(0)).toBe('00:00')
    expect(formatCountdown(65)).toBe('01:05')
    expect(formatCountdown(3599)).toBe('59:59')
    expect(formatCountdown(3600)).toBe('1:00:00')
    expect(formatCountdown(3725)).toBe('1:02:05')
    expect(formatCountdown(7199)).toBe('1:59:59')
    expect(formatCountdown(3661)).toBe('1:01:01')
  })
  test('剩余秒；解析失败为 0', () => {
    const now = Date.parse('2026-10-17T00:00:00Z')
    expect(remainingSecs('2026-10-17T00:01:00Z', now)).toBe(60)
    expect(remainingSecs('2026-10-16T23:59:00Z', now)).toBe(-60)
    expect(remainingSecs('2026-10-17T00:15:00Z', now + 500)).toBe(899)
    expect(remainingSecs('2026-10-17T08:15:00+08:00', now)).toBe(900)
    expect(remainingSecs('', now)).toBe(0)
    expect(remainingSecs('nope', now)).toBe(0)
  })
})

describe('推荐码', () => {
  test('规范化与校验', () => {
    expect(normalizeReferralCode('  ab12 ')).toBe('AB12')
    expect(validCreateCode('')).toBe(true)
    expect(validCreateCode('   ')).toBe(true)
    expect(validCreateCode('abc')).toBe(false)
    expect(validCreateCode('abcd')).toBe(true)
    expect(validCreateCode('A'.repeat(16))).toBe(true)
    expect(validCreateCode('A'.repeat(17))).toBe(false)
    expect(validCreateCode('ab-cd')).toBe(false)
    expect(validCreateCode('ab cd')).toBe(false)
    expect(validCreateCode('中文中文')).toBe(false)
    expect(validCreateCode(' abc123 ')).toBe(true)
  })
  test('validate reason → 键', () => {
    expect(referralReasonKey('self_use')).toBe('accountPlanErrorReferralSelfUse')
    expect(referralReasonKey('already_used')).toBe('accountPlanErrorReferralAlreadyUsed')
    expect(referralReasonKey('not_found')).toBe('accountPlanErrorReferralNotFound')
    expect(referralReasonKey('plan_excluded')).toBe('accountPlanErrorReferralPlanExcluded')
    expect(referralReasonKey('feature_disabled')).toBe('accountPlanErrorReferralInvalid')
    expect(referralReasonKey(null)).toBe('accountPlanErrorReferralInvalid')
  })
})

describe('其它', () => {
  test('图标名映射，未知回退 crown', () => {
    expect(planIconName('Rocket')).toBe('rocket')
    expect(planIconName('sparkles')).toBe('sparkles')
    expect(planIconName('???')).toBe('crown')
  })
  test('总页数', () => {
    expect(pageCount(0, 20)).toBe(1)
    expect(pageCount(20, 20)).toBe(1)
    expect(pageCount(21, 20)).toBe(2)
  })
})

describe('subscription cards', () => {
  const card = (code: string, sort: number) => plan(code, 1000, { sort, billingPeriodDays: 30 })
  const sub = { planCode: 'c1', planName: 'c1', startsAt: '2026-01-01T00:00:00Z', endsAt: '2026-02-01T00:00:00Z' }
  test('same card renews, other cards blocked while active', () => {
    expect(planCardState(card('c1', 5), 'free', 0, 0, sub, 0)).toBe('renewable')
    expect(planCardState(card('c2', 9), 'free', 0, 0, sub, 0)).toBe('tierBlocked')
  })
  test('without active card, sort must exceed base plan sort', () => {
    expect(planCardState(card('c1', 2), 'pro', 3000, 0, null, 2)).toBe('tierBlocked')
    expect(planCardState(card('c1', 3), 'pro', 3000, 0, null, 2)).toBe('available')
  })
  test('cards never take credit and split from lifetime plans', () => {
    const c = card('c1', 5)
    expect(priceDisplay(c, 'available', 500).creditApplied).toBe(false)
    expect(planCardState(c, 'pro', 3000, 3000)).toBe('available')
    const list = purchasablePlans([c, plan('pro', 3000, { sort: 2 })])
    expect(cardPlans(list).map((p) => p.code)).toEqual(['c1'])
    expect(lifetimePlans(list).map((p) => p.code)).toEqual(['pro'])
  })
})
