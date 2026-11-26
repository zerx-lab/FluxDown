import { useEffect, useMemo } from "react";
import { Check, Zap } from "lucide-react";
import { PlanBadge } from "@/components/pricing/PlanBadge";
import WebPurchase, { signedInFromMe, type SignedInAccount } from "@/components/pricing/WebPurchase";
import type { CloudPlan } from "@/components/pricing/types";
import { formatMinor } from "@/components/pay/money";
import { pricing } from "@/i18n/messages/pricing";
import { href } from "@/i18n/routing";
import type { DashboardCtx } from "./context";
import { daysUntil, formatDay } from "./format";
import OrdersList from "./OrdersList";
import { planLabel } from "./plan";
import { Clip, LoadFailed, Panel, RefreshButton, SectionHead, Skeleton, SubHead } from "./ui";

const effectivePrice = (plan: CloudPlan) => plan.campaign?.effectivePriceMinor ?? plan.priceMinor;
const isCard = (plan: CloudPlan) => typeof plan.billingPeriodDays === "number" && plan.billingPeriodDays > 0;

function CurrentPlan({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, lang } = ctx;
  const sub = me.activeSubscription;
  const left = sub ? daysUntil(sub.endsAt) : null;
  const currency = me.currentPlan?.currency ?? "CNY";
  return (
    <Panel className="p-5 sm:p-6">
      <p className="font-mono text-[11.5px] uppercase tracking-[0.14em] text-muted">{t.membership.current.heading}</p>
      <div className="mt-3 flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2">
        <h3 className="min-w-0 max-w-full text-2xl font-semibold tracking-tight">
          <Clip text={planLabel(me, t)} />
        </h3>
        {me.currentPlan && <PlanBadge plan={me.currentPlan} />}
      </div>
      <dl className="mt-5 divide-y divide-line border-y border-line">
        <div className="flex items-baseline justify-between gap-4 py-2.5">
          <dt className="text-xs text-muted">{t.membership.current.period}</dt>
          <dd className="num text-right font-mono text-sm">
            {sub ? (
              <>
                {t.membership.current.periodRange(formatDay(sub.startsAt, lang), formatDay(sub.endsAt, lang))}
                {left != null && (
                  <span className="block font-sans text-[12.5px] text-muted">
                    {left === 0 ? t.common.expiresToday : t.common.daysLeft(left)}
                  </span>
                )}
              </>
            ) : (
              <span className="font-sans text-muted">{t.membership.current.permanent}</span>
            )}
          </dd>
        </div>
        {me.purchaseCreditMinor > 0 && (
          <div className="flex items-baseline justify-between gap-4 py-2.5">
            <dt className="text-xs text-muted">{t.membership.current.credit}</dt>
            <dd className="num text-right font-mono text-sm">
              {formatMinor(me.purchaseCreditMinor, currency, lang)}
              <span className="block font-sans text-[12.5px] text-muted">{t.membership.current.creditHint}</span>
            </dd>
          </div>
        )}
      </dl>
    </Panel>
  );
}

function PlanCard({
  plan,
  ctx,
  signedIn,
}: {
  plan: CloudPlan;
  ctx: DashboardCtx;
  signedIn: SignedInAccount;
}) {
  const { me, t, lang } = ctx;
  const tp = pricing[lang];
  const price = effectivePrice(plan);
  const campaign = plan.campaign;
  const isCurrent = plan.code === me.plan;
  const renewable = me.activeSubscription?.planCode === plan.code;
  const buyable = !isCurrent || renewable;
  return (
    <div className="cell flex flex-col">
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
        <h4 className="min-w-0 max-w-full text-base font-semibold tracking-tight">
          <Clip text={plan.name} />
        </h4>
        <PlanBadge plan={plan} />
        {isCurrent && <span className="chip text-accent-ink">{t.membership.plans.current}</span>}
      </div>
      <p className="mt-4 flex flex-wrap items-baseline gap-x-2">
        <span className="num text-3xl font-semibold tracking-tight">{formatMinor(price, plan.currency, lang)}</span>
        {campaign && price < plan.priceMinor && (
          <span className="num text-sm text-subtle line-through">{formatMinor(plan.priceMinor, plan.currency, lang)}</span>
        )}
        <span className="text-[13px] text-muted">
          {isCard(plan) ? tp.cards.period(plan.billingPeriodDays ?? 0) : tp.plans.oneTime}
        </span>
      </p>
      {campaign && (
        <p className="mt-2 inline-flex items-center gap-1.5 text-xs font-medium text-accent-ink">
          <Zap size={12} aria-hidden />
          {campaign.name}
        </p>
      )}
      {plan.highlights.length > 0 && (
        <ul className="mt-4 flex flex-col gap-1.5 text-[13px] text-muted">
          {plan.highlights.slice(0, 5).map((line, i) => (
            <li key={i} className="flex items-start gap-2">
              <Check size={14} className="mt-0.5 shrink-0 text-accent-ink" aria-hidden />
              <span className="min-w-0">{line}</span>
            </li>
          ))}
        </ul>
      )}
      <div className="mt-auto pt-6">
        {buyable ? (
          <WebPurchase
            lang={lang}
            plan={{
              code: plan.code,
              name: plan.name,
              priceMinor: price,
              currency: plan.currency,
              basePriceMinor: plan.priceMinor,
              // 官网不提供 USDT(大陆备案站点政策),与定价页一致。
              priceUsdtMicros: null,
              waffoPriceMinor: plan.waffoPriceMinor ?? null,
            }}
            signedIn={signedIn}
            onPaid={() => {
              void ctx.refreshMe();
              ctx.orders.reload();
              ctx.notify.reloadIfLoaded();
            }}
            buttonLabel={renewable ? t.membership.plans.renew : t.membership.plans.buy}
          />
        ) : (
          <button type="button" disabled className="btn btn-secondary w-full disabled:cursor-not-allowed disabled:opacity-60">
            {t.membership.plans.current}
          </button>
        )}
      </div>
    </div>
  );
}

function PlanCardSkeleton() {
  return (
    <div className="cell flex min-h-[260px] flex-col">
      <Skeleton className="h-5 w-28" />
      <Skeleton className="mt-5 h-8 w-36" />
      <div className="mt-5 space-y-2.5">
        <Skeleton className="h-3.5 w-4/5" />
        <Skeleton className="h-3.5 w-2/3" />
        <Skeleton className="h-3.5 w-3/4" />
      </div>
      <Skeleton className="mt-auto h-10 w-full rounded-lg" />
    </div>
  );
}

export default function MembershipSection({ ctx }: { ctx: DashboardCtx }) {
  const { me, t, lang, plans, orders } = ctx;

  useEffect(() => {
    plans.load();
    orders.load();
  }, [plans.load, orders.load]);

  const signedIn = useMemo(() => signedInFromMe(me), [me]);
  const buyablePlans = useMemo(
    () => (plans.data ?? []).filter((p) => p.purchasable !== false && effectivePrice(p) > 0),
    [plans.data],
  );
  const refreshing = plans.refreshing || orders.refreshing || plans.status === "loading" || orders.status === "loading";

  return (
    <section aria-labelledby="section-heading">
      <SectionHead
        title={t.membership.title}
        lede={t.membership.lede}
        actions={
          <RefreshButton
            label={t.common.refresh}
            busy={refreshing}
            onClick={() => {
              void ctx.refreshMe();
              plans.reload();
              orders.reload();
            }}
          />
        }
      />

      <div className="mt-8 space-y-12">
        <CurrentPlan ctx={ctx} />

        <div className="space-y-5">
          <SubHead title={t.membership.plans.heading} lede={t.membership.plans.lede} />
          {plans.status === "error" ? (
            <LoadFailed text={t.membership.plans.loadFailed} retryLabel={t.common.retry} onRetry={plans.reload} />
          ) : plans.data == null ? (
            <div className="cells border border-line sm:grid-cols-2" role="status" aria-busy="true" aria-label={t.common.loading}>
              <PlanCardSkeleton />
              <PlanCardSkeleton />
            </div>
          ) : buyablePlans.length === 0 ? (
            <Panel className="px-6 py-10 text-center text-sm text-muted">{t.membership.plans.empty}</Panel>
          ) : (
            <div
              className={`cells border border-line ${buyablePlans.length > 1 ? "sm:grid-cols-2 sm:[&>*:last-child:nth-child(odd)]:col-span-2" : ""}`}
            >
              {buyablePlans.map((plan) => (
                <PlanCard key={plan.code} plan={plan} ctx={ctx} signedIn={signedIn} />
              ))}
            </div>
          )}
        </div>

        <OrdersList ctx={ctx} />

        <p className="flex flex-wrap gap-x-6 gap-y-2 text-[13px]">
          <a href={`${href("/terms", lang)}#refunds`} className="link">
            {t.membership.footer.refunds}
          </a>
          <a href={href("/pricing", lang)} className="link">
            {t.membership.footer.pricing}
          </a>
        </p>
      </div>
    </section>
  );
}
