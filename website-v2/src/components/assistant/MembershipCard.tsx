import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Crown } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { account } from "@/i18n/messages/account";
import type { AssistantMessages } from "@/i18n/messages/assistant";
import { pricing } from "@/i18n/messages/pricing";
import { href } from "@/i18n/routing";
import { accountApi, accountErrorText } from "@/lib/account/client";
import type { AccountMe } from "@/lib/account/types";
import { withBase } from "@/lib/base";
import { LoadFailed, Skeleton } from "@/components/account/dashboard/ui";
import type { Guard } from "@/components/account/dashboard/resource";
import { formatMinor } from "@/components/pay/money";
import { effectivePrice, isBuyablePlan, isCard, planBrief } from "@/components/pricing/plans";
import type { CloudPlan } from "@/components/pricing/types";
import WebPurchase, { signedInFromMe } from "@/components/pricing/WebPurchase";

type Data = { state: "loading" } | { state: "error"; text: string } | { state: "ready"; plans: CloudPlan[]; me: AccountMe };

/** 公开套餐目录（与定价页同一接口）；形状不对按失败处理。 */
async function fetchPlans(): Promise<CloudPlan[] | null> {
  try {
    const res = await fetch(withBase("/api/cloud/plans"), { headers: { Accept: "application/json" } });
    if (!res.ok) return null;
    const plans: unknown = await res.json();
    return Array.isArray(plans) ? (plans as CloudPlan[]) : null;
  } catch {
    return null;
  }
}

/**
 * 助手回答后的「开通会员」卡片：不带套餐数据，挂载时实时取在售套餐与当前账户（避免历史卡片显示过期价格），
 * 每个在售套餐一行，购买走定价页同一个 `WebPurchase`（选套餐 → 扫码 → 成功）。窄面板里按钮折到下一行。
 */
export default function MembershipCard({ lang, t, guard }: { lang: Lang; t: AssistantMessages; guard: Guard }) {
  const ta = account[lang].membership.plans;
  const tp = pricing[lang];
  const [data, setData] = useState<Data>({ state: "loading" });
  const seq = useRef(0);
  const guardRef = useRef(guard);
  guardRef.current = guard;

  const load = useCallback(async () => {
    const id = ++seq.current;
    setData((prev) => (prev.state === "ready" ? prev : { state: "loading" }));
    const [plans, me] = await Promise.all([fetchPlans(), accountApi<AccountMe>("me")]);
    if (id !== seq.current) return;
    if (!me.ok) {
      if (!guardRef.current(me)) setData({ state: "error", text: accountErrorText(lang, me.code) });
      return;
    }
    setData(plans ? { state: "ready", plans, me: me.data } : { state: "error", text: ta.loadFailed });
  }, [lang, ta.loadFailed]);

  useEffect(() => {
    void load();
    return () => {
      seq.current += 1;
    };
  }, [load]);

  const rows = useMemo(() => (data.state === "ready" ? data.plans.filter(isBuyablePlan) : []), [data]);
  const signedIn = useMemo(() => (data.state === "ready" ? signedInFromMe(data.me) : null), [data]);

  return (
    <section aria-label={t.membership.title} className="rounded-xl border border-line bg-elev p-4">
      <div className="flex items-start gap-3">
        <span aria-hidden className="grid size-8 shrink-0 place-items-center rounded-lg bg-accent-soft text-accent-ink">
          <Crown size={16} />
        </span>
        <div className="min-w-0 flex-1">
          <h3 className="text-[14.5px] font-semibold">{t.membership.title}</h3>
          <p className="mt-1 text-[13px] leading-relaxed text-muted">{t.membership.body}</p>
        </div>
      </div>

      <div className="mt-3">
        {data.state === "loading" && (
          <div role="status" aria-busy="true" aria-label={account[lang].common.loading} className="space-y-2">
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-12 w-full" />
          </div>
        )}
        {data.state === "error" && (
          <LoadFailed text={data.text} retryLabel={t.sidebar.retry} onRetry={() => void load()} />
        )}
        {data.state === "ready" && signedIn && (
          rows.length === 0 ? (
            <p className="text-[13px] text-muted">{ta.empty}</p>
          ) : (
            <ul aria-label={t.membership.plansLabel} className="divide-y divide-line rounded-lg border border-line">
              {rows.map((plan) => {
                const isCurrent = plan.code === data.me.plan;
                const renewable = data.me.activeSubscription?.planCode === plan.code;
                return (
                  <li key={plan.code} className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 px-3 py-2.5">
                    <div className="min-w-0 flex-1 basis-32">
                      <p className="truncate text-[13.5px] font-medium" title={plan.name}>
                        {plan.name}
                      </p>
                      <p className="num text-[12.5px] text-muted">
                        {formatMinor(effectivePrice(plan), plan.currency, lang)}
                        {" · "}
                        {isCard(plan) ? tp.cards.period(plan.billingPeriodDays ?? 0) : tp.plans.oneTime}
                      </p>
                    </div>
                    <div className="w-28 shrink-0">
                      {!isCurrent || renewable ? (
                        <WebPurchase
                          lang={lang}
                          plan={planBrief(plan)}
                          signedIn={signedIn}
                          onPaid={() => void load()}
                          buttonLabel={renewable ? ta.renew : ta.buy}
                        />
                      ) : (
                        <button type="button" disabled className="btn btn-secondary w-full disabled:cursor-not-allowed disabled:opacity-60">
                          {ta.current}
                        </button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )
        )}
      </div>

      <p className="mt-3 text-[12.5px]">
        <a href={href("/pricing", lang)} className="link">
          {account[lang].membership.footer.pricing}
        </a>
      </p>
    </section>
  );
}
