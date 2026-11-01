import { useEffect, useState, type ReactNode } from "react";
import { ArrowRight, Check, Zap } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { pricing } from "@/i18n/messages/pricing";
import { BrandIcon } from "@/components/icons/brand";
import { formatDate, formatMinor } from "@/components/pay/money";
import { PlanBadge, PlanRibbon } from "./PlanBadge";
import WebPurchase from "./WebPurchase";
import type { CloudPlan } from "./types";
import { withBase } from "@/lib/base";

interface Props {
  lang: Lang;
  downloadHref: string;
  sourceHref: string;
}

type Catalog = { state: "loading" } | { state: "ready"; plans: CloudPlan[] } | { state: "error" };

/** Column counts per breakpoint for N plan cells (md is always 2). */
function lgCols(total: number): number {
  if (total <= 2) return 2;
  if (total === 4) return 4;
  return 3;
}

const LG_GRID: Record<number, string> = { 2: "lg:grid-cols-2", 3: "lg:grid-cols-3", 4: "lg:grid-cols-4" };

/** Periodic membership card: a membership with a positive billing period (missing / null = permanent). */
function isCard(plan: CloudPlan): boolean {
  return typeof plan.billingPeriodDays === "number" && plan.billingPeriodDays > 0;
}

/**
 * Plan columns: the always-free desktop app first, then the live FluxCloud catalog.
 * Catalog failures degrade silently to static placeholder cards (price TBD), as before.
 */
export default function PricingPlans({ lang, downloadHref, sourceHref }: Props) {
  const t = pricing[lang].plans;
  const [catalog, setCatalog] = useState<Catalog>({ state: "loading" });

  useEffect(() => {
    let alive = true;
    fetch(withBase("/api/cloud/plans"))
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((plans: unknown) => {
        if (!alive) return;
        setCatalog(
          Array.isArray(plans) && plans.length > 0 ? { state: "ready", plans: plans as CloudPlan[] } : { state: "error" },
        );
      })
      .catch(() => alive && setCatalog({ state: "error" }));
    return () => {
      alive = false;
    };
  }, []);

  const cells: ReactNode[] = [<AppCell key="app" lang={lang} downloadHref={downloadHref} sourceHref={sourceHref} />];
  if (catalog.state === "loading") {
    cells.push(<SkeletonCell key="s1" label={t.loading} />, <SkeletonCell key="s2" label={t.loading} />);
  } else if (catalog.state === "ready") {
    for (const plan of catalog.plans.filter((p) => !isCard(p))) cells.push(<CloudPlanCell key={plan.code} plan={plan} lang={lang} />);
  } else {
    cells.push(
      <PlanShell key="fb-free" tag={t.cloudTag} name={t.fallback.freeName} price={t.free} desc={t.fallback.freeDesc} />,
      <PlanShell
        key="fb-life"
        tag={t.cloudTag}
        name={t.fallback.lifetimeName}
        price={t.tbd}
        period={t.oneTime}
        desc={t.fallback.lifetimeDesc}
        features={t.fallback.lifetimeFeatures}
        featured
      />,
    );
  }

  const total = cells.length;
  const cols = lgCols(total);
  const mdFill = total % 2;
  const lgFill = (cols - (total % cols)) % cols;

  const cards = catalog.state === "ready" ? catalog.plans.filter(isCard) : [];
  const cardT = pricing[lang].cards;

  return (
    <>
    <div className="fig">
      <div className="fig-bar">
        <span>{t.figTag}</span>
        <span>{t.figCaption}</span>
      </div>
      <div className={`cells grid-cols-1 md:grid-cols-2 ${LG_GRID[cols]}`} aria-busy={catalog.state === "loading"}>
        {cells}
        {Array.from({ length: mdFill }, (_, i) => (
          <div key={`md${i}`} aria-hidden className="plan-fill hidden md:block lg:hidden" />
        ))}
        {Array.from({ length: lgFill }, (_, i) => (
          <div key={`lg${i}`} aria-hidden className="plan-fill hidden lg:block" />
        ))}
      </div>
      <p className="border-t border-line px-5 py-3.5 text-center text-[13px] text-muted">{t.freeNote}</p>
    </div>
    {cards.length > 0 && (
      <section className="mt-12" aria-label={cardT.title}>
        <h3 className="h3">{cardT.title}</h3>
        <p className="mt-2 max-w-2xl text-sm leading-relaxed text-muted">{cardT.lede}</p>
        <div className="fig mt-5">
          <div className="cells grid-cols-1 md:grid-cols-2 lg:grid-cols-3">
            {cards.map((plan) => (
              <CloudPlanCell key={plan.code} plan={plan} lang={lang} />
            ))}
          </div>
        </div>
      </section>
    )}
    </>
  );
}

function AppCell({ lang, downloadHref, sourceHref }: Props) {
  const t = pricing[lang].plans;
  return (
    <PlanShell
      tag={t.app.tag}
      name={t.app.name}
      price={t.app.price}
      period={t.app.period}
      desc={t.app.desc}
      features={t.app.features}
      footer={
        <div className="grid grid-cols-[1fr_auto] gap-2">
          <a href={downloadHref} className="btn btn-secondary">
            {t.app.download}
            <ArrowRight size={15} aria-hidden />
          </a>
          <a href={sourceHref} target="_blank" rel="noopener noreferrer" className="btn btn-ghost">
            <BrandIcon name="github" size={15} />
            {t.app.source}
          </a>
        </div>
      }
    />
  );
}

function CloudPlanCell({ plan, lang }: { plan: CloudPlan; lang: Lang }) {
  const t = pricing[lang].plans;
  const c = plan.campaign;
  const stage = c ? c.stages[c.currentStageIndex] : null;
  const stageLeft =
    c && stage && stage.quota != null ? Math.max(0, stage.quota - (c.stageSold[c.currentStageIndex] ?? 0)) : null;
  const priceMinor = c ? c.effectivePriceMinor : plan.priceMinor;
  const paid = plan.priceMinor > 0;

  return (
    <PlanShell
      tag={t.cloudTag}
      name={plan.name}
      badge={<PlanBadge plan={plan} />}
      ribbon={<PlanRibbon plan={plan} />}
      price={priceMinor === 0 ? t.free : formatMinor(priceMinor, plan.currency, lang)}
      strike={c != null && c.effectivePriceMinor < plan.priceMinor ? formatMinor(plan.priceMinor, plan.currency, lang) : undefined}
      period={priceMinor > 0 ? (isCard(plan) ? pricing[lang].cards.period(plan.billingPeriodDays ?? 0) : t.oneTime) : undefined}
      note={c?.endAt ? t.campaignEnds(formatDate(c.endAt, lang)) : undefined}
      campaign={
        c && (
          <div className="mb-5 mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 border border-accent/30 bg-accent-soft px-3 py-2 text-xs">
            <span className="inline-flex items-center gap-1.5 font-medium text-accent-ink">
              <Zap size={12} aria-hidden />
              {c.name}
            </span>
            {stage?.label && <span className="text-muted">{stage.label}</span>}
            {stageLeft != null && <span className="num ml-auto font-mono text-muted">{t.campaignLeft(stageLeft)}</span>}
          </div>
        )
      }
      desc={plan.description}
      features={plan.highlights}
      featured={paid}
      footer={
        paid &&
        (plan.purchasable !== false ? (
          <>
            {/* USDT checkout hidden on the mainland-filed site (crypto payments are not allowed in mainland China); FluxCloud still supports it. */}
            <WebPurchase plan={{ code: plan.code, name: plan.name, priceMinor, currency: plan.currency, basePriceMinor: plan.priceMinor, priceUsdtMicros: null }} lang={lang} />
            <p className="mt-2.5 text-center text-xs text-subtle">{t.buyInApp}</p>
          </>
        ) : (
          <p className="text-center text-sm text-muted">{t.unavailable}</p>
        ))
      }
    />
  );
}

interface ShellProps {
  tag: string;
  name: string;
  price: string;
  period?: string;
  strike?: string;
  note?: string;
  desc?: string;
  features?: readonly string[];
  badge?: ReactNode;
  ribbon?: ReactNode;
  campaign?: ReactNode;
  footer?: ReactNode;
  featured?: boolean;
}

function PlanShell(p: ShellProps) {
  return (
    <article className={`cell spot flex flex-col ${p.featured ? "plan-featured" : ""}`}>
      {p.ribbon}
      {p.campaign}
      <div className="flex min-h-[22px] flex-wrap items-center gap-2.5">
        <span className="eyebrow">{p.tag}</span>
        {p.badge}
      </div>
      <h3 className="h3 mt-4">{p.name}</h3>
      <div className="mt-5 flex flex-wrap items-baseline gap-x-2.5 gap-y-1">
        <span className="num text-[2.6rem] font-semibold leading-none tracking-[-0.03em]">{p.price}</span>
        {p.strike && <s className="num text-base text-subtle">{p.strike}</s>}
        {p.period && <span className="font-mono text-xs uppercase tracking-[0.08em] text-muted">/ {p.period}</span>}
      </div>
      {p.note && <p className="mt-2 text-xs text-muted">{p.note}</p>}
      {p.desc && <p className="mt-5 text-sm leading-relaxed text-muted">{p.desc}</p>}
      {p.features && p.features.length > 0 && (
        <ul className="mt-6 space-y-2.5 border-t border-dashed border-line pt-6">
          {p.features.map((f) => (
            <li key={f} className="flex items-start gap-2.5 text-sm">
              <Check size={15} className="mt-[3px] shrink-0 text-accent" aria-hidden />
              <span>{f}</span>
            </li>
          ))}
        </ul>
      )}
      {p.footer && <div className="mt-auto pt-8">{p.footer}</div>}
    </article>
  );
}

function SkeletonCell({ label }: { label: string }) {
  return (
    <div className="cell flex flex-col" role="status" aria-label={label}>
      <div className="h-3 w-16 animate-pulse rounded-sm bg-inset" />
      <div className="mt-5 h-6 w-32 animate-pulse rounded-sm bg-inset" />
      <div className="mt-6 h-10 w-28 animate-pulse rounded-sm bg-inset" />
      <div className="mt-6 space-y-3 border-t border-dashed border-line pt-6">
        {[80, 64, 72].map((w) => (
          <div key={w} className="h-3 animate-pulse rounded-sm bg-inset" style={{ width: `${w}%` }} />
        ))}
      </div>
    </div>
  );
}
