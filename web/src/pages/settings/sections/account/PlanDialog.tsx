// 套餐选择 / 购买对话框（GPUI `crates/account/src/pages/plan_dialog.rs` 的 Web 对应）：
// select（目录 + 抵扣 + 邀请码）→ paying（微信二维码 + 轮询）→ success。关闭 / 卸载即停止轮询与倒计时。

import { Check, CheckCheck, Copy, Crown, Diamond, Gem, Rocket, Shield, Sparkles, Star, TriangleAlert, Zap } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { useT } from '../../../../i18n'
import { cn } from '../../../../lib/cn'
import { copyText } from '../../../../lib/copy'
import { RpcError, rpc } from '../../../../lib/rpc'
import type { AgentSessionDto, CloudOrder, CloudPlan, CloudSubscription } from '../../../../lib/rpc'
import { Badge, Button, ConfirmFooter, Dialog, DialogFooter, Icon, Input, ProgressBar, Spinner } from '../../../../ui'
import {
  localDate,
  campaignProgress,
  formatCountdown,
  formatMinor,
  normalizeReferralCode,
  ownedPriceMinor,
  payableMinor,
  baseSortOf,
  cardPlans,
  isSubscriptionPlan,
  lifetimePlans,
  planCardState,
  planIconName,
  priceDisplay,
  purchasablePlans,
  referralReasonKey,
  remainingSecs,
} from './billing'
import type { PlanCardState, PlanIconName } from './billing'
import { accountErrorKey } from './errorText'
import { PlanBadge } from './PlanBadge'
import { parseBadgeColor } from './planBadgeText'
import { encodeQrChallengeImage } from '../extensions/challengeImage'

const ICONS: Record<PlanIconName, LucideIcon> = { crown: Crown, rocket: Rocket, star: Star, gem: Gem, diamond: Diamond, zap: Zap, shield: Shield, sparkles: Sparkles }
const POLL_INTERVAL_MS = 3000
const FALLBACK_COLOR = 'var(--fx-colors-accent-foreground, currentColor)'

type Load =
  | { status: 'loading' }
  | { status: 'error' }
  | { status: 'ready'; catalog: CloudPlan[]; credit: number; currentCode: string | null; subscription: CloudSubscription | null }

type Step = { kind: 'select' } | { kind: 'paying'; order: CloudOrder; plan: CloudPlan } | { kind: 'success'; planName: string }

/** 传输层瞬断 / 服务端声明可重试的错误：轮询继续下一轮。 */
function pollRetryable(error: unknown): boolean {
  return error instanceof RpcError && (error.retryable || error.appCode === 'unavailable' || error.appCode === 'timeout')
}

/** 支付结果刷新资料：失败只记录，不阻断主流程。 */
async function refreshProfileLogged(): Promise<void> {
  try {
    await rpc.agent.auth.refreshProfile()
  } catch (error) {
    console.warn('[plan] refreshProfile failed', error)
  }
}

function planColor(plan: CloudPlan): string {
  return parseBadgeColor(plan.color) ?? FALLBACK_COLOR
}

function Banner({ tone, children }: { tone: 'danger' | 'info'; children: string }) {
  return (
    <div
      role={tone === 'danger' ? 'alert' : 'status'}
      className={cn(
        'flex items-start gap-2 rounded-md border px-3 py-2 text-xs',
        tone === 'danger' ? 'border-destructive/30 bg-destructive/8 text-destructive' : 'border-warning/30 bg-warning/8 text-warning',
      )}
    >
      <Icon icon={TriangleAlert} className="mt-px shrink-0" />
      <span className="min-w-0 flex-1 break-words">{children}</span>
    </div>
  )
}

function PlanCard({ plan, state, credit, selected, onSelect, ordinal, conflict }: { plan: CloudPlan; state: PlanCardState; credit: number; selected: boolean; onSelect: () => void; ordinal: number | null; conflict: boolean }) {
  const t = useT()
  const color = planColor(plan)
  const price = priceDisplay(plan, state, credit)
  const progress = campaignProgress(plan)
  const endDate = localDate(plan.campaign?.endAt)
  const available = state === 'available' || state === 'renewable'
  const Glyph = ICONS[planIconName(plan.icon)]
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={!available}
      onClick={onSelect}
      className={cn(
        'flex w-full flex-wrap items-start gap-3 rounded-lg border p-3 text-left transition-colors coarse:min-h-touch',
        selected ? 'border-accent-text bg-accent-text/8' : 'border-hairline bg-surface hover:bg-nav-hover',
        !available && 'cursor-not-allowed opacity-60 hover:bg-surface',
      )}
      title={conflict ? t('errReasonSubscriptionConflict') : undefined}
    >
      <span className="flex size-10 shrink-0 items-center justify-center rounded-lg" style={{ background: `color-mix(in srgb, ${color} 14%, transparent)`, color }}>
        <Glyph className="size-5" strokeWidth={1.75} />
      </span>
      <span className="flex min-w-0 flex-1 basis-48 flex-col gap-1">
        <span className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-semibold text-foreground">{plan.name}</span>
          <PlanBadge plan={plan} ordinal={ordinal} />
          {state === 'current' || state === 'renewable' ? <Badge tone="accent">{t('accountPlanCurrent')}</Badge> : null}
          {state === 'tierBlocked' || state === 'creditBlocked' ? <Badge>{t('accountPlanLowerTier')}</Badge> : null}
        </span>
        {isSubscriptionPlan(plan) ? <span className="text-xs text-muted-foreground tabular">{t('accountPeriodDays', { days: plan.billingPeriodDays ?? 0 })}</span> : null}
        {plan.description ? <span className="text-xs text-muted-foreground">{plan.description}</span> : null}
        {plan.highlights.length > 0 ? (
          <ul className="mt-0.5 flex flex-col gap-0.5">
            {plan.highlights.map((item, index) => (
              <li key={`${index}-${item}`} className="flex items-start gap-1.5 text-xs text-foreground">
                <Icon icon={Check} className="mt-px shrink-0 text-success" />
                <span className="min-w-0 break-words">{item}</span>
              </li>
            ))}
          </ul>
        ) : null}
        {progress ? (
          <span className="mt-1 flex flex-col gap-1">
            <ProgressBar value={progress.ratio} />
            <span className="text-caption text-muted-foreground tabular">{t('accountPlanLimitedSold', { sold: progress.sold, quota: progress.quota })}</span>
          </span>
        ) : null}
        {endDate ? <span className="text-caption text-muted-foreground tabular">{t('accountPlanCampaignEnds', { date: endDate })}</span> : null}
      </span>
      <span className="flex shrink-0 flex-col items-end gap-0.5 text-right">
        <span className="text-title font-semibold text-foreground tabular">{formatMinor(price.payable, plan.currency)}</span>
        {price.strike !== null ? <span className="text-xs text-text-tertiary tabular line-through">{formatMinor(price.strike, plan.currency)}</span> : null}
        {price.creditApplied ? <span className="text-caption text-success">{t('accountPlanCreditApplied', { amount: formatMinor(credit, plan.currency) })}</span> : null}
        {price.stageLabel ? <Badge tone="accent">{price.stageLabel}</Badge> : null}
      </span>
    </button>
  )
}

function SkeletonCards() {
  return (
    <div className="flex flex-col gap-2" aria-busy>
      {[0, 1].map((index) => (
        <div key={index} className="flex animate-pulse items-start gap-3 rounded-lg border border-hairline p-3">
          <div className="size-10 rounded-lg bg-muted" />
          <div className="flex flex-1 flex-col gap-2">
            <div className="h-3.5 w-1/3 rounded bg-muted" />
            <div className="h-3 w-2/3 rounded bg-muted" />
            <div className="h-3 w-1/2 rounded bg-muted" />
          </div>
          <div className="h-5 w-16 rounded bg-muted" />
        </div>
      ))}
    </div>
  )
}

function PayingStep({ order, plan, onPaid, onBack }: { order: CloudOrder; plan: CloudPlan; onPaid: (order: CloudOrder) => void; onBack: (error: { expired: true } | { key: string }) => void }) {
  const t = useT()
  const [qr, setQr] = useState<string | null>(null)
  const [left, setLeft] = useState(() => remainingSecs(order.expiresAt, Date.now()))
  const [copied, setCopied] = useState(false)
  const handlers = useRef({ onPaid, onBack })
  handlers.current = { onPaid, onBack }

  useEffect(() => {
    let alive = true
    setQr(null)
    if (order.codeUrl) {
      void encodeQrChallengeImage(order.codeUrl).then((url) => {
        if (alive) setQr(url)
      })
    }
    return () => {
      alive = false
    }
  }, [order.codeUrl])

  useEffect(() => {
    const timer = window.setInterval(() => setLeft(remainingSecs(order.expiresAt, Date.now())), 1000)
    return () => window.clearInterval(timer)
  }, [order.expiresAt])

  useEffect(() => {
    if (!copied) return
    const timer = window.setTimeout(() => setCopied(false), 2000)
    return () => window.clearTimeout(timer)
  }, [copied])

  useEffect(() => {
    let stopped = false
    let timer: number | undefined
    const tick = async () => {
      try {
        const next = await rpc.agent.order.get({ orderNo: order.orderNo })
        if (stopped) return
        if (next.status === 'paid') {
          await refreshProfileLogged()
          if (!stopped) handlers.current.onPaid(next)
          return
        }
        if (next.status === 'expired' || next.status === 'failed') {
          handlers.current.onBack({ expired: true })
          return
        }
      } catch (error) {
        if (stopped) return
        if (!pollRetryable(error)) {
          handlers.current.onBack({ key: accountErrorKey(error) })
          return
        }
      }
      if (!stopped) timer = window.setTimeout(() => void tick(), POLL_INTERVAL_MS)
    }
    timer = window.setTimeout(() => void tick(), POLL_INTERVAL_MS)
    return () => {
      stopped = true
      window.clearTimeout(timer)
    }
  }, [order.orderNo])

  const currency = order.currency || plan.currency
  return (
    <div className="flex flex-col items-center gap-3 py-2 text-center">
      <div className="flex size-[248px] max-w-full items-center justify-center rounded-xl border border-hairline bg-white p-1">
        {qr ? <img src={qr} alt={t('accountPlanScanHint')} className="size-full" /> : order.codeUrl ? <Spinner /> : null}
      </div>
      <div className="flex flex-col items-center gap-0.5">
        <div className="text-[28px] leading-9 font-semibold text-foreground tabular">{formatMinor(order.amountMinor, currency)}</div>
        <div className="text-sm text-muted-foreground">{order.planName || plan.name}</div>
        {order.creditMinor > 0 ? <div className="text-xs text-success">{t('accountPlanCreditApplied', { amount: formatMinor(order.creditMinor, currency) })}</div> : null}
        {order.referralDiscountMinor > 0 ? <div className="text-xs text-success">{t('accountPlanReferralDiscountApplied', { amount: formatMinor(order.referralDiscountMinor, currency) })}</div> : null}
      </div>
      <div className="text-xs text-foreground">{t('accountPlanScanHint')}</div>
      <div className="text-xs text-muted-foreground tabular">{t('accountPlanExpiresIn', { time: formatCountdown(left) })}</div>
      <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Spinner size="sm" />
        {t('accountPlanWaitingPayment')}
      </div>
      <button
        type="button"
        className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-caption text-text-tertiary hover:text-foreground coarse:min-h-touch"
        onClick={() => {
          copyText(order.orderNo)
          setCopied(true)
        }}
      >
        <span className="select-text tabular">{t('accountPlanOrderNo', { orderNo: order.orderNo })}</span>
        <Icon icon={copied ? Check : Copy} />
      </button>
    </div>
  )
}

export function PlanDialog({ session, onClose }: { session: AgentSessionDto; onClose: () => void }) {
  const t = useT()
  const [load, setLoad] = useState<Load>({ status: 'loading' })
  const [step, setStep] = useState<Step>({ kind: 'select' })
  const [selected, setSelected] = useState<string | null>(null)
  const [referral, setReferral] = useState('')
  const [notice, setNotice] = useState<{ tone: 'danger' | 'info'; text: string } | null>(null)
  const [busy, setBusy] = useState(false)
  const active = useRef(true)
  const [version, setVersion] = useState(0)
  const sessionPlanCode = useRef<string | null>(null)
  // 当前套餐以用户 `plan` 为准（`currentPlan` 只是展示配置，可能缺失）。
  sessionPlanCode.current = session.user.plan || null
  const deviceId = session.device.deviceId

  useEffect(() => {
    active.current = true
    return () => {
      active.current = false
    }
  }, [])

  const reload = useCallback(() => setVersion((value) => value + 1), [])

  useEffect(() => {
    let alive = true
    setLoad({ status: 'loading' })
    void (async () => {
      const [catalog, profile] = await Promise.allSettled([rpc.agent.plan.list(), rpc.agent.auth.refreshProfile()])
      if (!alive) return
      if (catalog.status === 'rejected') {
        console.warn('[plan] plan.list failed', catalog.reason)
        setLoad({ status: 'error' })
        return
      }
      if (profile.status === 'rejected') console.warn('[plan] refreshProfile failed; credit treated as 0', profile.reason)
      const credit = profile.status === 'fulfilled' ? Math.max(0, profile.value.purchaseCreditMinor) : 0
      const currentCode = profile.status === 'fulfilled' ? profile.value.plan || null : sessionPlanCode.current
      setLoad({ status: 'ready', catalog: catalog.value, credit, currentCode, subscription: profile.status === 'fulfilled' ? (profile.value.activeSubscription ?? null) : null })
    })()
    return () => {
      alive = false
    }
  }, [version])

  const buy = async (plan: CloudPlan) => {
    if (busy) return
    setBusy(true)
    setNotice(null)
    try {
      const code = normalizeReferralCode(referral)
      if (code !== '') {
        const checked = await rpc.agent.referral.validate({ code, planCode: plan.code })
        if (!active.current) return
        if (!checked.valid) {
          setNotice({ tone: 'danger', text: t(referralReasonKey(checked.reason)) })
          setBusy(false)
          return
        }
      }
      const order = await rpc.agent.order.create({ planCode: plan.code, deviceId, ...(code !== '' ? { referralCode: code } : {}) })
      if (!active.current) return
      setStep({ kind: 'paying', order, plan })
      setBusy(false)
    } catch (error) {
      if (!active.current) return
      setNotice({ tone: 'danger', text: t(accountErrorKey(error)) })
      setBusy(false)
    }
  }

  const backToSelect = useCallback(
    (reason: { expired: true } | { key: string }) => {
      setStep({ kind: 'select' })
      setSelected(null)
      setNotice({ tone: 'danger', text: 'expired' in reason ? t('accountPlanOrderExpired') : t(reason.key) })
      reload()
    },
    [reload, t],
  )

  const onPaid = useCallback(
    (order: CloudOrder) => setStep({ kind: 'success', planName: order.planName }),
    [],
  )

  const ready = load.status === 'ready' ? load : null
  const plans = ready ? purchasablePlans(ready.catalog) : []
  const owned = ready ? ownedPriceMinor(ready.catalog, ready.currentCode) : 0
  const baseSort = ready ? baseSortOf(ready.catalog, ready.currentCode) : null
  const stateOf = (plan: CloudPlan, r: NonNullable<typeof ready>): PlanCardState => planCardState(plan, r.currentCode, owned, r.credit, r.subscription, baseSort)
  const chosen = ready && selected ? plans.find((plan) => plan.code === selected) : undefined
  const chosenState = chosen && ready ? stateOf(chosen, ready) : null
  const chosenAvailable = chosenState === 'available' || chosenState === 'renewable'

  let title = t('accountPlanDialogTitle')
  let footer = null
  let body = null
  if (step.kind === 'paying') {
    title = t('accountPlanPayingTitle')
    footer = (
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>
          {t('close')}
        </Button>
      </DialogFooter>
    )
    body = <PayingStep order={step.order} plan={step.plan} onPaid={onPaid} onBack={backToSelect} />
  } else if (step.kind === 'success') {
    footer = (
      <DialogFooter>
        <Button variant="primary" onClick={onClose}>
          {t('close')}
        </Button>
      </DialogFooter>
    )
    body = (
      <div className="flex flex-col items-center gap-3 py-8 text-center">
        <div className="flex size-16 items-center justify-center rounded-full bg-success/15 text-success">
          <CheckCheck className="size-8" strokeWidth={1.75} />
        </div>
        <div className="text-title font-semibold text-foreground">{t('accountPlanPaidSuccess')}</div>
        {step.planName ? <div className="text-sm text-muted-foreground">{step.planName}</div> : null}
      </div>
    )
  } else {
    footer = (
      <ConfirmFooter
        cancelLabel={t('cancel')}
        okLabel={chosen && chosenAvailable && ready ? `${t(chosenState === 'renewable' ? 'accountRenew' : 'accountPlanBuy')} ${formatMinor(payableMinor(chosen, isSubscriptionPlan(chosen) ? 0 : ready.credit), chosen.currency)}` : t('accountPlanBuy')}
        onCancel={onClose}
        onOk={() => chosen && void buy(chosen)}
        okDisabled={!chosenAvailable}
        loading={busy}
        extra={<span className="text-xs text-muted-foreground">{t('accountPlanOneTime')}</span>}
      />
    )
    body = (
      <div className="flex flex-col gap-3">
        {load.status === 'loading' ? <SkeletonCards /> : null}
        {load.status === 'error' ? (
          <div className="flex flex-col items-center gap-2 py-8 text-center">
            <div className="text-sm text-muted-foreground">{t('accountPlanLoadFailed')}</div>
            <Button onClick={reload}>{t('accountDevicesRetry')}</Button>
          </div>
        ) : null}
        {ready && plans.length === 0 ? <div className="py-8 text-center text-sm text-muted-foreground">{t('accountPlanEmpty')}</div> : null}
        {ready && plans.length > 0 ? (
          <div role="radiogroup" className="flex flex-col gap-2">
            {[
              { key: 'lifetime', title: t('accountPurchaseGroupLifetime'), items: lifetimePlans(plans) },
              { key: 'cards', title: t('accountPurchaseGroupCards'), items: cardPlans(plans) },
            ].map((group) =>
              group.items.length === 0 ? null : (
                <div key={group.key} className="flex flex-col gap-2">
                  {cardPlans(plans).length > 0 ? <div className="text-xs font-medium text-muted-foreground">{group.title}</div> : null}
                  {group.items.map((plan) => {
                    const state = stateOf(plan, ready)
                    return (
                      <PlanCard
                        key={plan.code}
                        plan={plan}
                        state={state}
                        credit={ready.credit}
                        selected={selected === plan.code}
                        ordinal={state === 'current' ? session.user.membershipOrdinal : null}
                        conflict={state === 'tierBlocked' && isSubscriptionPlan(plan) && ready.subscription !== null}
                        onSelect={() => {
                          setSelected(plan.code)
                          setNotice(null)
                        }}
                      />
                    )
                  })}
                </div>
              ),
            )}
          </div>
        ) : null}
        {ready && plans.length > 0 ? (
          <Input
            value={referral}
            disabled={busy}
            placeholder={t('accountPlanReferralCodePlaceholder')}
            autoCapitalize="characters"
            spellCheck={false}
            onChange={(event) => {
              setReferral(event.target.value.toUpperCase())
              setNotice(null)
            }}
          />
        ) : null}
        {notice ? <Banner tone={notice.tone}>{notice.text}</Banner> : null}
      </div>
    )
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title={title} size="lg" modalLocked={busy} footer={footer}>
      {body}
    </Dialog>
  )
}
