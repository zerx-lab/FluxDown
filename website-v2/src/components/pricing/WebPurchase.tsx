import { useCallback, useEffect, useState } from "react";
import { CircleCheck, LoaderCircle } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { pricing } from "@/i18n/messages/pricing";
import Dialog from "@/components/pay/Dialog";
import { QrTile, WechatBadge } from "@/components/pay/WechatQr";
import { estimateUsdtMicros, formatMinor, formatUsdt } from "@/components/pay/money";
import type { PlanBrief } from "./types";
import { withBase } from "@/lib/base";

/** Web purchase from a plan card: account lookup → confirm (optional referral code) → WeChat QR → poll until paid. */

interface Identity {
  nickname: string;
  emailMasked: string;
  originId: number | null;
  planCode: string;
  planName: string;
  purchaseCreditMinor: number;
}
interface WebOrder {
  orderNo: string;
  planName: string;
  status: string;
  amountMinor: number;
  creditMinor: number;
  referralDiscountMinor: number;
  currency: string;
  codeUrl: string | null;
  payMethod: "wxpay" | "usdt" | "waffo";
  payAmountMicros: number | null;
  payReceivedMicros: number | null;
  payAddress: string | null;
  payNetworks: { key: string; name: string; chainId: number }[];
  expiresAt: string;
  /** Waffo hosted checkout URL (waffo orders only). */
  checkoutUrl: string | null;
}
/** `POST /api/cloud/order-referral` result; `code` is the normalized input it was checked for. */
interface ReferralCheck {
  code: string;
  valid: boolean;
  discountMinor: number;
  reason: string | null;
}

type Step = "account" | "confirm" | "pay" | "success";
type Errors = (typeof pricing)["en"]["webbuy"]["err"];

/** FluxCloud error code → message key (unknown codes fall back to the network message). */
const ERROR_KEYS: Record<string, keyof Errors> = {
  not_found: "notFound",
  validation_error: "invalidAccount",
  rate_limited: "rateLimited",
  payment_disabled: "paymentDisabled",
  plan_not_purchasable: "notPurchasable",
  plan_already_owned: "alreadyOwned",
  not_an_upgrade: "notAnUpgrade",
  plan_tier_not_higher: "tierNotHigher",
  subscription_conflict: "subscriptionConflict",
  gateway_error: "gateway",
  upstream_unreachable: "gateway",
  referral_code_invalid: "referralInvalid",
  referral_already_used: "referralAlreadyUsed",
  referral_plan_excluded: "referralPlanExcluded",
  usdt_disabled: "usdtDisabled",
  usdt_not_available: "usdtUnavailable",
  waffo_disabled: "waffoDisabled",
  waffo_not_available: "waffoUnavailable",
};

/** Referral validate `reason` (valid=false) → message key. */
const REFERRAL_REASON_KEYS: Record<string, keyof Errors> = {
  not_found: "referralInvalid",
  feature_disabled: "referralDisabled",
  plan_excluded: "referralPlanExcluded",
  already_used: "referralAlreadyUsed",
};

/** Same normalization as FluxCloud `referral::normalize_code`. */
const normalizeCode = (raw: string) => raw.trim().toUpperCase();

const POLL_INTERVAL = 3000;
const USDT_EXPIRED_POLL_MS = 30 * 60 * 1000;

async function errorCode(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { code?: string };
    return body.code ?? "network";
  } catch {
    return "network";
  }
}

const STEP_INDEX: Record<Step, number> = { account: 0, confirm: 1, pay: 2, success: 3 };

type PayMethod = WebOrder["payMethod"];

/** USD cents → `US$12.99`. */
const formatUsd = (cents: number) => `US$${(cents / 100).toFixed(2)}`;

function openCheckout(url: string) {
  window.open(url, "_blank", "noopener,noreferrer");
}

export default function WebPurchase({ plan, lang }: { plan: PlanBrief; lang: Lang }) {
  const t = pricing[lang].webbuy;
  const [open, setOpen] = useState(false);
  const [step, setStep] = useState<Step>("account");
  const [account, setAccount] = useState("");
  const [referralCode, setReferralCode] = useState("");
  const [referralCheck, setReferralCheck] = useState<ReferralCheck | null>(null);
  const [referralBusy, setReferralBusy] = useState(false);
  const [referralError, setReferralError] = useState<string | null>(null);
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [order, setOrder] = useState<WebOrder | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [payMethod, setPayMethod] = useState<PayMethod>("wxpay");

  const reset = useCallback(() => {
    setStep("account");
    setIdentity(null);
    setOrder(null);
    setError(null);
    setBusy(false);
    // A referral check is account-specific (already-used, plan gates): drop it with the account.
    setReferralCheck(null);
    setReferralError(null);
  }, []);

  const close = useCallback(() => {
    setOpen(false);
    reset();
    setAccount("");
    setReferralCode("");
  }, [reset]);

  async function lookup() {
    const trimmed = account.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    setError(null);
    try {
      const res = await fetch(withBase("/api/cloud/order-lookup"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ account: trimmed }),
      });
      if (!res.ok) {
        setError(t.err[ERROR_KEYS[await errorCode(res)] ?? "network"]);
        return;
      }
      setIdentity((await res.json()) as Identity);
      setStep("confirm");
    } catch {
      setError(t.err.network);
    } finally {
      setBusy(false);
    }
  }

  async function checkReferral() {
    const code = normalizeCode(referralCode);
    if (!code || referralBusy) return;
    setReferralBusy(true);
    setReferralError(null);
    setReferralCheck(null);
    try {
      const res = await fetch(withBase("/api/cloud/order-referral"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ account: account.trim(), planCode: plan.code, code }),
      });
      if (!res.ok) {
        setReferralError(t.err[ERROR_KEYS[await errorCode(res)] ?? "network"]);
        return;
      }
      const body = (await res.json()) as Omit<ReferralCheck, "code">;
      if (!body.valid) {
        setReferralError(t.err[REFERRAL_REASON_KEYS[body.reason ?? ""] ?? "referralInvalid"]);
        return;
      }
      setReferralCheck({ ...body, code });
    } catch {
      setReferralError(t.err.network);
    } finally {
      setReferralBusy(false);
    }
  }

  async function createOrder() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch(withBase("/api/cloud/order"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          account: account.trim(),
          planCode: plan.code,
          referralCode: normalizeCode(referralCode) || undefined,
          payMethod: method,
        }),
      });
      if (!res.ok) {
        setError(t.err[ERROR_KEYS[await errorCode(res)] ?? "network"]);
        return;
      }
      const created = (await res.json()) as WebOrder;
      setOrder(created);
      // Try once to open the hosted checkout; if the popup is blocked, the pay-step button is the fallback.
      if (created.payMethod === "waffo" && created.checkoutUrl) openCheckout(created.checkoutUrl);
      setStep("pay");
    } catch {
      setError(t.err.network);
    } finally {
      setBusy(false);
    }
  }

  // Poll every 3 s: paid → success; failed → back to confirm. WeChat `expired` → back to confirm;
  // USDT `expired` stays on the pay step (late deposits within grace may still be credited) and keeps polling for 30 min.
  // Waffo `expired` / `refunded` stay on the pay step (late payments are still honoured, so expired keeps polling).
  useEffect(() => {
    if (step !== "pay" || !order) return;
    const query = withBase(`/api/cloud/order?orderNo=${encodeURIComponent(order.orderNo)}&account=${encodeURIComponent(account.trim())}`);
    let expiredSince: number | null = order.payMethod === "usdt" && order.status === "expired" ? Date.now() : null;
    const timer = window.setInterval(async () => {
      if (expiredSince != null && Date.now() - expiredSince > USDT_EXPIRED_POLL_MS) {
        window.clearInterval(timer);
        return;
      }
      try {
        const res = await fetch(query);
        if (!res.ok) return; // transient failure / rate limit: wait for the next tick
        const fresh = (await res.json()) as WebOrder;
        if (fresh.status === "paid") {
          setOrder(fresh);
          setStep("success");
        } else if (fresh.status === "expired" && (fresh.payMethod === "usdt" || fresh.payMethod === "waffo")) {
          expiredSince ??= Date.now();
          setOrder((prev) => (prev && prev.status === "expired" && prev.payReceivedMicros === fresh.payReceivedMicros ? prev : fresh));
        } else if (fresh.status === "refunded") {
          setOrder(fresh);
          window.clearInterval(timer);
        } else if (fresh.status === "expired" || fresh.status === "failed") {
          setError(fresh.status === "expired" ? t.err.expired : t.err.failed);
          setOrder(null);
          setStep("confirm");
        } else if (fresh.payMethod === "usdt") {
          setOrder(fresh); // refresh received amount
        }
      } catch {
        // network blip: next tick
      }
    }, POLL_INTERVAL);
    return () => window.clearInterval(timer);
  }, [step, order?.orderNo, order?.payMethod, account, t]);

  const money = (minor: number, currency = plan.currency) => formatMinor(minor, currency, lang);
  const estimate = Math.max(0, plan.priceMinor - (identity?.purchaseCreditMinor ?? 0));
  const referralDiscount = referralCheck ? Math.min(referralCheck.discountMinor, estimate) : 0;
  // Waffo charges the fixed USD list price: campaign prices, upgrade credit and referral codes are rejected server-side.
  const waffoBlocked =
    plan.priceMinor !== plan.basePriceMinor
      ? t.waffoNoCampaign
      : (identity?.purchaseCreditMinor ?? 0) > 0
        ? t.waffoNoUpgrade
        : normalizeCode(referralCode)
          ? t.waffoNoReferral
          : null;
  const methods: PayMethod[] = ["wxpay"];
  if (plan.priceUsdtMicros != null) methods.push("usdt");
  if (plan.waffoPriceMinor != null) methods.push("waffo");
  // Typing a referral code after selecting Waffo falls back to WeChat so the order stays valid.
  const method: PayMethod = payMethod === "waffo" && waffoBlocked ? "wxpay" : payMethod;
  const methodLabel = (m: PayMethod) => (m === "wxpay" ? t.payWechat : m === "usdt" ? t.payUsdt : t.payWaffo);

  return (
    <>
      <button type="button" onClick={() => setOpen(true)} className="btn btn-primary w-full">
        {t.button}
      </button>

      <Dialog open={open} onClose={close} title={t.title(plan.name)} closeLabel={t.close} dismissible={step !== "pay"}>
        {step !== "success" && (
          <ol className="mb-6 flex items-center gap-2 font-mono text-[10.5px] uppercase tracking-[0.1em]">
            {t.steps.map((label, i) => {
              const current = STEP_INDEX[step];
              return (
                <li
                  key={label}
                  aria-current={i === current ? "step" : undefined}
                  className={`flex items-center gap-2 last:flex-none ${i < t.steps.length - 1 ? "flex-1" : ""} ${
                    i > current ? "text-subtle" : i === current ? "text-accent-ink" : "text-muted"
                  }`}
                >
                  <span className="num">{String(i + 1).padStart(2, "0")}</span>
                  <span className="truncate">{label}</span>
                  {i < t.steps.length - 1 && (
                    <span aria-hidden className={`h-px min-w-3 flex-1 ${i < current ? "bg-accent" : "bg-line-strong"}`} />
                  )}
                </li>
              );
            })}
          </ol>
        )}

        {step === "account" && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void lookup();
            }}
          >
            <label htmlFor={`webbuy-${plan.code}`} className="block text-[13px] font-medium">
              {t.accountLabel}
            </label>
            <input
              id={`webbuy-${plan.code}`}
              value={account}
              onChange={(e) => setAccount(e.target.value)}
              placeholder={t.accountPlaceholder}
              autoComplete="username"
              autoFocus
              aria-invalid={error ? true : undefined}
              aria-describedby={`webbuy-${plan.code}-help`}
              className="field mt-2"
            />
            <p id={`webbuy-${plan.code}-help`} className="mt-2.5 text-[12.5px] leading-relaxed text-muted">
              {t.accountHelp}
            </p>
            <ErrorLine text={error} />
            <button type="submit" disabled={busy || !account.trim()} className="btn btn-primary mt-5 w-full disabled:cursor-not-allowed disabled:opacity-50">
              {busy && <LoaderCircle size={15} className="animate-spin" aria-hidden />}
              {busy ? t.loading : t.next}
            </button>
          </form>
        )}

        {step === "confirm" && identity && (
          <div>
            <p className="text-[13px] leading-relaxed text-muted">{t.confirmDesc}</p>
            <dl className="mt-4 divide-y divide-line border-y border-line">
              <Row label={t.fieldNickname} value={identity.nickname} />
              <Row label={t.fieldEmail} value={identity.emailMasked} />
              {identity.originId != null && <Row label={t.fieldOrigin} value={String(identity.originId)} mono />}
              <Row label={t.fieldPlan} value={identity.planName} />
            </dl>
            <form
              className="mt-4"
              onSubmit={(e) => {
                e.preventDefault();
                void checkReferral();
              }}
            >
              <label htmlFor={`webbuy-${plan.code}-referral`} className="block text-[13px] font-medium">
                {t.referralLabel}
              </label>
              <div className="mt-2 flex gap-2">
                <input
                  id={`webbuy-${plan.code}-referral`}
                  value={referralCode}
                  onChange={(e) => {
                    // Any edit invalidates the previous check: the discount shown must match the code sent.
                    setReferralCode(e.target.value);
                    setReferralCheck(null);
                    setReferralError(null);
                  }}
                  placeholder={t.referralPlaceholder}
                  autoComplete="off"
                  autoCapitalize="characters"
                  spellCheck={false}
                  maxLength={32}
                  aria-invalid={referralError ? true : undefined}
                  aria-describedby={`webbuy-${plan.code}-referral-help`}
                  className="field num min-w-0 flex-1 font-mono uppercase"
                />
                <button
                  type="submit"
                  disabled={referralBusy || !normalizeCode(referralCode)}
                  className="btn btn-secondary shrink-0 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {referralBusy && <LoaderCircle size={15} className="animate-spin" aria-hidden />}
                  {referralBusy ? t.loading : t.referralApply}
                </button>
              </div>
              <p
                id={`webbuy-${plan.code}-referral-help`}
                role={referralError ? "alert" : referralCheck ? "status" : undefined}
                className={`mt-2 text-[12.5px] leading-relaxed ${
                  referralError ? "text-danger" : referralCheck ? "text-accent-ink" : "text-muted"
                }`}
              >
                {referralError ??
                  (referralCheck
                    ? referralDiscount > 0
                      ? t.referralValid(money(referralDiscount))
                      : t.referralNoDiscount
                    : t.referralHelp)}
              </p>
            </form>
            {methods.length > 1 && (
              <div className="mt-4">
                <p className="text-[13px] font-medium">{t.payMethod}</p>
                <div
                  role="radiogroup"
                  aria-label={t.payMethod}
                  className={`mt-2 grid gap-2 ${methods.length === 3 ? "grid-cols-1 sm:grid-cols-3" : "grid-cols-2"}`}
                >
                  {methods.map((m) => {
                    const disabled = m === "waffo" && waffoBlocked != null;
                    return (
                      <button
                        key={m}
                        type="button"
                        role="radio"
                        aria-checked={method === m}
                        disabled={disabled}
                        title={disabled ? (waffoBlocked ?? undefined) : undefined}
                        onClick={() => setPayMethod(m)}
                        className={`btn ${method === m ? "btn-primary" : "btn-secondary"} disabled:cursor-not-allowed disabled:opacity-50`}
                      >
                        {methodLabel(m)}
                      </button>
                    );
                  })}
                </div>
                {plan.waffoPriceMinor != null && waffoBlocked && (
                  <p className="mt-2 text-[12.5px] leading-relaxed text-muted">{waffoBlocked}</p>
                )}
              </div>
            )}
            {/* Estimate: plan price − current-plan credit − checked referral discount; the order amount is authoritative. */}
            <dl className="mt-4 divide-y divide-line border-y border-line">
              <Row label={t.pricePlan} value={money(plan.priceMinor)} mono />
              {identity.purchaseCreditMinor > 0 && (
                <Row label={t.priceCredit} value={`− ${money(Math.min(identity.purchaseCreditMinor, plan.priceMinor))}`} mono />
              )}
              {referralDiscount > 0 && method !== "waffo" && <Row label={t.priceReferral} value={`− ${money(referralDiscount)}`} mono />}
              {method !== "waffo" && (
                <Row label={t.pricePayable} value={money(estimate - referralDiscount)} mono strong={method === "wxpay"} />
              )}
              {method === "waffo" && plan.waffoPriceMinor != null && (
                <Row label={t.waffoPayable} value={formatUsd(plan.waffoPriceMinor)} mono strong />
              )}
              {method === "usdt" && plan.priceUsdtMicros != null && (
                <Row
                  label={t.usdtEstimate}
                  value={`≈ ${formatUsdt(estimateUsdtMicros(estimate - referralDiscount, plan.basePriceMinor, plan.priceUsdtMicros))}`}
                  mono
                  strong
                />
              )}
            </dl>
            <ErrorLine text={error} />
            <div className="mt-6 grid grid-cols-2 gap-3">
              <button type="button" onClick={reset} className="btn btn-secondary">
                {t.notMe}
              </button>
              <button type="button" disabled={busy} onClick={() => void createOrder()} className="btn btn-primary disabled:opacity-50">
                {busy && <LoaderCircle size={15} className="animate-spin" aria-hidden />}
                {busy ? t.loading : t.confirmPay}
              </button>
            </div>
          </div>
        )}

        {step === "pay" && order && order.payMethod === "usdt" && order.status === "expired" && (
          <div className="flex flex-col items-center text-center" role="status">
            <h3 className="text-base font-semibold">{t.usdtExpiredTitle}</h3>
            <p className="mt-2 text-[13px] leading-relaxed text-muted">{t.usdtExpiredDesc}</p>
            <p className="num mt-4 font-mono text-[11px] text-subtle">
              {t.orderNo} · {order.orderNo}
            </p>
            <button type="button" onClick={close} className="btn btn-secondary mt-5 w-full">
              {t.close}
            </button>
          </div>
        )}

        {step === "pay" && order && order.payMethod === "usdt" && order.status !== "expired" && <UsdtPayPanel order={order} t={t} />}

        {step === "pay" && order && order.payMethod === "waffo" && (
          <div className="flex flex-col items-center text-center" role="status">
            {order.status === "refunded" ? (
              <>
                <h3 className="text-base font-semibold">{t.waffoRefundedTitle}</h3>
                <p className="mt-2 text-[13px] leading-relaxed text-muted">{t.waffoRefundedDesc}</p>
              </>
            ) : order.status === "expired" ? (
              <>
                <h3 className="text-base font-semibold">{t.usdtExpiredTitle}</h3>
                <p className="mt-2 text-[13px] leading-relaxed text-muted">{t.waffoExpiredDesc}</p>
              </>
            ) : (
              <>
                <p className="text-[13px] leading-relaxed text-muted">{t.waffoDesc}</p>
                {order.payAmountMicros != null && (
                  <div className="num mt-5 text-3xl font-semibold tracking-tight">{formatUsd(order.payAmountMicros / 10_000)}</div>
                )}
                {order.checkoutUrl ? (
                  <button type="button" onClick={() => openCheckout(order.checkoutUrl!)} className="btn btn-primary mt-5 w-full">
                    {t.waffoGo}
                  </button>
                ) : (
                  <ErrorLine text={t.err.gateway} />
                )}
                <p className="mt-4 inline-flex items-center gap-2 text-xs text-muted">
                  <LoaderCircle size={13} className="animate-spin text-accent" aria-hidden />
                  {t.waiting}
                </p>
              </>
            )}
            <p className="num mt-4 font-mono text-[11px] text-subtle">
              {t.orderNo} · {order.orderNo}
            </p>
            {(order.status === "expired" || order.status === "refunded") && (
              <div className="mt-5 grid w-full grid-cols-2 gap-3">
                <button type="button" onClick={close} className="btn btn-secondary">
                  {t.close}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setOrder(null);
                    setError(null);
                    setStep("confirm");
                  }}
                  className="btn btn-primary"
                >
                  {t.reorder}
                </button>
              </div>
            )}
          </div>
        )}

        {step === "pay" && order && order.payMethod === "wxpay" && (
          <div className="flex flex-col items-center text-center">
            <WechatBadge label={t.wechatBadge} />
            <p className="mt-3 text-[13px] text-muted">{t.scanTitle}</p>
            {order.codeUrl ? (
              <div className="mt-5">
                <QrTile value={order.codeUrl} size={188} wechat />
              </div>
            ) : (
              <ErrorLine text={t.err.gateway} />
            )}
            <div className="num mt-5 text-3xl font-semibold tracking-tight">{money(order.amountMinor, order.currency)}</div>
            {order.creditMinor > 0 && (
              <p className="mt-1 text-xs text-accent-ink">{t.credit(money(order.creditMinor, order.currency))}</p>
            )}
            {order.referralDiscountMinor > 0 && (
              <p className="mt-1 text-xs text-accent-ink">{t.referralApplied(money(order.referralDiscountMinor, order.currency))}</p>
            )}
            <p className="mt-4 inline-flex items-center gap-2 text-xs text-muted" role="status">
              <LoaderCircle size={13} className="animate-spin text-accent" aria-hidden />
              {t.waiting}
            </p>
            <p className="num mt-2 font-mono text-[11px] text-subtle">
              {t.orderNo} · {order.orderNo}
            </p>
          </div>
        )}

        {step === "success" && order && (
          <div className="flex flex-col items-center py-2 text-center" role="status">
            <span className="grid size-12 place-items-center rounded-full bg-accent-soft text-accent-ink">
              <CircleCheck size={24} aria-hidden />
            </span>
            <h3 className="mt-4 text-base font-semibold">{t.paidTitle}</h3>
            <p className="mt-2 text-[13px] leading-relaxed text-muted">{t.paidDesc}</p>
            <button type="button" onClick={close} className="btn btn-primary mt-6 w-full">
              {t.close}
            </button>
          </div>
        )}
      </Dialog>
    </>
  );
}

function Row({ label, value, mono, strong }: { label: string; value: string; mono?: boolean; strong?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-4 py-2.5">
      <dt className="text-xs text-muted">{label}</dt>
      <dd className={`text-right text-sm ${mono ? "num font-mono" : ""} ${strong ? "font-semibold text-fg" : "font-medium"}`}>
        {value}
      </dd>
    </div>
  );
}

function ErrorLine({ text }: { text: string | null }) {
  if (!text) return null;
  return (
    <p role="alert" className="mt-3 text-[13px] text-danger">
      {text}
    </p>
  );
}

/** Copy-to-clipboard button; briefly flips its label to "copied". */
function CopyButton({ text, label, doneLabel }: { text: string; label: string; doneLabel: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="btn btn-secondary shrink-0 px-2.5 py-1 text-xs"
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => {
          setDone(true);
          window.setTimeout(() => setDone(false), 1500);
        });
      }}
    >
      {done ? doneLabel : label}
    </button>
  );
}

function formatRemaining(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const mm = String(Math.floor(s / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return `${mm}:${ss}`;
}

/** Logos for the ops chain keys (`public/chains/`); unknown keys render name only. */
const CHAIN_LOGOS: Record<string, string> = {
  polygon: "/chains/polygon.png",
  arbitrum: "/chains/arbitrum.png",
  bsc: "/chains/bsc.png",
  xlayer: "/chains/xlayer.svg",
};

/** USDT payment view: deposit address QR, exact amount, networks, countdown and received progress. */
function UsdtPayPanel({ order, t }: { order: WebOrder; t: (typeof pricing)["en"]["webbuy"] }) {
  const expiresMs = Date.parse(order.expiresAt);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const address = order.payAddress ?? "";
  const amount = order.payAmountMicros ?? 0;
  const amountText = (amount / 1e6).toFixed(2);
  return (
    <div className="flex flex-col items-center text-center">
      <p className="text-[13px] text-muted">{t.payUsdt}</p>
      {address && (
        <div className="mt-4">
          <QrTile value={address} size={188} />
        </div>
      )}
      <div className="mt-4 w-full text-left">
        <p className="text-xs text-muted">{t.usdtAddress}</p>
        <div className="mt-1 flex items-center gap-2">
          <code className="num min-w-0 flex-1 break-all font-mono text-[12px]">{address}</code>
          <CopyButton text={address} label={t.copy} doneLabel={t.copied} />
        </div>
        <p className="mt-3 text-xs text-muted">{t.usdtAmount}</p>
        <div className="mt-1 flex items-center gap-2">
          <span className="num flex-1 text-2xl font-semibold tracking-tight">{formatUsdt(amount)}</span>
          <CopyButton text={amountText} label={t.copy} doneLabel={t.copied} />
        </div>
        {order.payNetworks.length > 0 && (
          <>
            <p className="mt-3 text-xs text-muted">{t.usdtNetworks}</p>
            <ul className="mt-1.5 flex flex-wrap gap-2">
              {order.payNetworks.map((n) => (
                <li
                  key={n.key}
                  className="inline-flex items-center gap-1.5 rounded-full border border-line px-2.5 py-1 text-[13px] font-medium"
                >
                  {CHAIN_LOGOS[n.key] && (
                    <img src={CHAIN_LOGOS[n.key]} alt="" width={18} height={18} className="size-[18px] rounded-full" />
                  )}
                  {n.name}
                </li>
              ))}
            </ul>
          </>
        )}
        <p className="num mt-3 text-xs text-muted">
          {t.usdtExpiresIn}: {Number.isNaN(expiresMs) ? "--:--" : formatRemaining(expiresMs - now)}
        </p>
        <p className="num mt-1 text-xs text-muted">
          {t.usdtReceived}: {formatUsdt(order.payReceivedMicros ?? 0)} / {formatUsdt(amount)}
        </p>
      </div>
      <ul className="mt-4 w-full list-disc pl-5 text-left text-xs leading-relaxed text-danger">
        <li>{t.usdtWarnExact}</li>
        <li>{t.usdtWarnNetwork}</li>
        <li>{t.usdtWarnLate}</li>
      </ul>
      <p className="mt-4 inline-flex items-center gap-2 text-xs text-muted" role="status">
        <LoaderCircle size={13} className="animate-spin text-accent" aria-hidden />
        {t.waiting}
      </p>
      <p className="num mt-2 font-mono text-[11px] text-subtle">
        {t.orderNo} · {order.orderNo}
      </p>
    </div>
  );
}
