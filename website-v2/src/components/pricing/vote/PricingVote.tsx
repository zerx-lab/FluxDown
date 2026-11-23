import { useEffect, useState } from "react";
import { LoaderCircle } from "lucide-react";
import type { Lang } from "@/i18n/config";
import { pricingVote } from "@/i18n/messages/pricingVote";
import { BrandIcon } from "@/components/icons/brand";
import PollCard, { type Plan } from "./PollCard";
import { Avatar, type PollComment, type Viewer } from "./Discussion";
import { withBase } from "@/lib/base";

interface PricingData {
  results: Record<Plan, Record<string, number>>;
  totals: Record<Plan, number>;
  comments: PollComment[];
  issueUrl: string;
  viewer: (Viewer & { votes: Record<Plan, string | null> }) | null;
  loginEnabled: boolean;
}

const PLANS: Plan[] = ["lifetime", "subscription"];

interface Props {
  lang: Lang;
  /** `/api/auth/github?returnTo=…` for this page's language. */
  loginUrl: string;
  logoutUrl: string;
}

/** Price poll + discussion, backed by GET/POST /api/pricing-vote (GitHub OAuth session). */
export default function PricingVote({ lang, loginUrl, logoutUrl }: Props) {
  const t = pricingVote[lang];
  const [data, setData] = useState<PricingData | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [authError, setAuthError] = useState(false);
  const [submitting, setSubmitting] = useState<Plan | null>(null);
  const [status, setStatus] = useState<{ text: string; ok: boolean } | null>(null);

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("auth_error")) {
      setAuthError(true);
      window.history.replaceState(null, "", window.location.pathname);
    }
    fetch(withBase("/api/pricing-vote"))
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((d: PricingData) => setData(d))
      .catch(() => setLoadError(true));
  }, []);

  const viewer = data?.viewer ?? null;

  async function vote(plan: Plan, option: string) {
    if (!viewer) {
      window.location.href = loginUrl;
      return;
    }
    if (viewer.votes[plan] || submitting) return;
    setSubmitting(plan);
    setStatus(null);
    try {
      const res = await fetch(withBase("/api/pricing-vote"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: "vote", plan, option }),
      });
      if (res.status === 401) {
        window.location.href = loginUrl;
        return;
      }
      if (!res.ok) {
        setStatus({ text: res.status === 429 ? t.rateLimited : t.error, ok: false });
        return;
      }
      const result = (await res.json()) as { message?: string; option?: string };
      const already = result.message === "already_voted";
      const finalOption = already ? (result.option ?? option) : option;
      setData((prev) => {
        if (!prev?.viewer) return prev;
        const next: PricingData = {
          ...prev,
          viewer: { ...prev.viewer, votes: { ...prev.viewer.votes, [plan]: finalOption } },
        };
        if (result.message === "voted") {
          next.results = {
            ...prev.results,
            [plan]: { ...prev.results[plan], [option]: (prev.results[plan][option] || 0) + 1 },
          };
          next.totals = { ...prev.totals, [plan]: prev.totals[plan] + 1 };
        }
        return next;
      });
      setStatus({ text: already ? t.alreadyVoted : t.success, ok: true });
    } catch {
      setStatus({ text: t.error, ok: false });
    } finally {
      setSubmitting(null);
    }
  }

  return (
    <>
      <div className="section-tight">
        {authError && (
          <p role="alert" className="mb-6 border-l-2 border-danger bg-sunken px-4 py-3 text-sm font-medium text-danger">
            {t.authError}
          </p>
        )}

        {!data && !loadError && (
          <p role="status" className="flex items-center justify-center gap-3 py-20 text-sm text-muted">
            <LoaderCircle size={18} className="animate-spin text-accent" aria-hidden />
            {t.loading}
          </p>
        )}
        {loadError && (
          <p role="alert" className="py-20 text-center text-sm text-danger">
            {t.loadError}
          </p>
        )}

        {data && (
          <>
            <div className="mb-6 flex flex-wrap items-center justify-between gap-3">
              {viewer ? (
                <p className="chip h-8 gap-2.5 pl-1 pr-3 text-[13px]">
                  <Avatar src={viewer.avatar} size={24} />
                  <span>
                    {t.signedInAs} <strong className="font-semibold text-fg">{viewer.login}</strong>
                  </span>
                  <a href={logoutUrl} className="link text-xs">
                    {t.logout}
                  </a>
                </p>
              ) : (
                <a href={loginUrl} className="btn btn-secondary btn-sm">
                  <BrandIcon name="github" size={14} />
                  {t.signInToVote}
                </a>
              )}
              <p
                role="status"
                className={`text-sm font-medium ${status?.ok ? "text-ok" : "text-danger"}`}
              >
                {status?.text}
              </p>
            </div>
            <div className="cells border border-line md:grid-cols-2">
              {PLANS.map((plan, i) => (
                <PollCard
                  key={plan}
                  lang={lang}
                  plan={plan}
                  index={i}
                  results={data.results[plan]}
                  total={data.totals[plan]}
                  chosen={viewer?.votes[plan] ?? null}
                  busy={submitting !== null}
                  onVote={(p, o) => void vote(p, o)}
                />
              ))}
            </div>
          </>
        )}
      </div>
    </>
  );
}
