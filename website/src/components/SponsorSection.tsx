import { useState, useEffect, useMemo } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  Heart,
  ExternalLink,
  Zap,
  Rocket,
  Gem,
  Flame,
  Users,
  Sparkles,
  Crown,
  Star,
} from "lucide-react";
import { useLocale } from "@/lib/i18n";
import type { Messages } from "@/lib/locales";

/* ============================================================
   SponsorSection — Afdian (爱发电) Dynamic Integration
   - Dynamically fetches plans from /api/sponsors
   - Parses Markdown descriptions (bold, bullets)
   - Shows sponsor wall from Open API
   - Graceful degradation when API unavailable
   ============================================================ */

const AFDIAN_URL = "https://ifdian.net/u/7b862392211611f1942a52540025c377";

// ── Types ──────────────────────────────────────────────────

interface AfdianPlan {
  planId: string;
  name: string;
  price: string;
  desc: string;
  payMonth: number;
  sponsorCount: number;
  independent: boolean;
  permanent: boolean;
}

interface AfdianProfile {
  userId: string;
  name: string;
  avatar: string;
  doing: string;
  detail: string;
  category: string;
}

interface Sponsor {
  name: string;
  avatar: string;
  amount: string;
  plan: string;
}

interface SponsorsPayload {
  profile: AfdianProfile | null;
  plans: AfdianPlan[];
  sponsors: Sponsor[];
  totalSponsors: number;
  updatedAt: number;
}

interface SponsorSectionProps {
  /** When rendered as a dedicated page, adds extra top padding for the navbar */
  fullPage?: boolean;
}

// ── Tier styling config ────────────────────────────────────

interface TierStyle {
  icon: React.ReactNode;
  gradient: string;
  borderHover: string;
  iconBg: string;
  glowColor: string;
}

const TIER_STYLES: TierStyle[] = [
  {
    icon: <Flame className="w-5 h-5" />,
    gradient: "from-amber-500/10 to-orange-500/10",
    borderHover: "hover:border-amber-500/40",
    iconBg: "bg-amber-500/15 text-amber-400",
    glowColor: "amber",
  },
  {
    icon: <Zap className="w-5 h-5" />,
    gradient: "from-brand-sky/10 to-brand-cyan/10",
    borderHover: "hover:border-brand-sky/40",
    iconBg: "bg-brand-sky/15 text-brand-sky",
    glowColor: "sky",
  },
  {
    icon: <Rocket className="w-5 h-5" />,
    gradient: "from-purple-500/10 to-indigo-500/10",
    borderHover: "hover:border-purple-500/40",
    iconBg: "bg-purple-500/15 text-purple-400",
    glowColor: "purple",
  },
  {
    icon: <Gem className="w-5 h-5" />,
    gradient: "from-pink-500/10 to-rose-500/10",
    borderHover: "hover:border-pink-500/40",
    iconBg: "bg-pink-500/15 text-pink-400",
    glowColor: "pink",
  },
  {
    icon: <Crown className="w-5 h-5" />,
    gradient: "from-yellow-500/10 to-amber-500/10",
    borderHover: "hover:border-yellow-500/40",
    iconBg: "bg-yellow-500/15 text-yellow-400",
    glowColor: "yellow",
  },
  {
    icon: <Star className="w-5 h-5" />,
    gradient: "from-emerald-500/10 to-teal-500/10",
    borderHover: "hover:border-emerald-500/40",
    iconBg: "bg-emerald-500/15 text-emerald-400",
    glowColor: "emerald",
  },
];

function getTierStyle(index: number): TierStyle {
  return TIER_STYLES[index % TIER_STYLES.length]!;
}

// ── Simple Markdown parser ─────────────────────────────────
// Handles: **bold**, - bullet lists, newlines

function parseMarkdownDesc(desc: string): React.ReactNode[] {
  if (!desc || !desc.trim()) return [];

  const lines = desc.split("\n").filter((l) => l.trim());
  const elements: React.ReactNode[] = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!.trim();
    const isBullet = line.startsWith("- ");
    const text = isBullet ? line.slice(2) : line;

    // Parse **bold** segments
    const parts: React.ReactNode[] = [];
    const boldRegex = /\*\*(.+?)\*\*/g;
    let lastIndex = 0;
    let match: RegExpExecArray | null;

    while ((match = boldRegex.exec(text)) !== null) {
      if (match.index > lastIndex) {
        parts.push(text.slice(lastIndex, match.index));
      }
      parts.push(
        <span
          key={`b-${i}-${match.index}`}
          className="font-semibold text-dark-text"
        >
          {match[1]}
        </span>,
      );
      lastIndex = match.index + match[0].length;
    }
    if (lastIndex < text.length) {
      parts.push(text.slice(lastIndex));
    }

    if (isBullet) {
      elements.push(
        <li key={i} className="flex items-start gap-2.5">
          <span className="mt-1.5 w-1.5 h-1.5 rounded-full bg-brand-sky/60 flex-shrink-0" />
          <span className="text-[13px] text-dark-text-secondary leading-relaxed">
            {parts}
          </span>
        </li>,
      );
    } else {
      elements.push(
        <p
          key={i}
          className="text-[13px] text-dark-text-secondary leading-relaxed"
        >
          {parts}
        </p>,
      );
    }
  }

  return elements;
}

// ── Plan card ──────────────────────────────────────────────

function PlanCard({
  plan,
  tierIndex,
  totalPlans,
  index,
  locale,
}: {
  plan: AfdianPlan;
  tierIndex: number;
  totalPlans: number;
  index: number;
  locale: string;
}) {
  const style = getTierStyle(tierIndex);
  // Mark 2nd tier as popular when there are 3+ plans
  const isPopular = totalPlans >= 3 && tierIndex === 1;

  const descElements = useMemo(() => parseMarkdownDesc(plan.desc), [plan.desc]);
  const hasBullets = plan.desc.includes("- ");
  const priceLabel =
    locale === "zh"
      ? `¥${plan.price} / ${plan.permanent ? "永久" : "月"}`
      : `¥${plan.price} / ${plan.permanent ? "lifetime" : "mo"}`;

  return (
    <motion.div
      initial={{ opacity: 0, y: 24 }}
      whileInView={{ opacity: 1, y: 0 }}
      viewport={{ once: true, amount: 0.3 }}
      transition={{ duration: 0.5, delay: 0.08 * index, ease: "easeOut" }}
      className="relative group"
    >
      {/* Popular badge */}
      {isPopular && (
        <div className="absolute -top-3 left-1/2 -translate-x-1/2 z-10">
          <span className="inline-flex items-center gap-1 px-3 py-0.5 rounded-full text-[10px] font-semibold uppercase tracking-wider bg-brand-sky text-white shadow-lg shadow-brand-sky/25">
            <Sparkles className="w-3 h-3" />
            {locale === "zh" ? "推荐" : "Popular"}
          </span>
        </div>
      )}

      <div
        className={`relative h-full rounded-xl border border-dark-border/60 bg-gradient-to-b ${style.gradient} backdrop-blur-sm p-6 transition-all duration-300 ${style.borderHover} hover:shadow-lg hover:shadow-dark-bg/50 ${isPopular ? "border-brand-sky/30 ring-1 ring-brand-sky/10" : ""}`}
      >
        {/* Header: icon + name + price */}
        <div className="flex items-center gap-3 mb-4">
          <div
            className={`flex items-center justify-center w-10 h-10 rounded-lg ${style.iconBg} transition-transform duration-300 group-hover:scale-110`}
          >
            {style.icon}
          </div>
          <div className="min-w-0 flex-1">
            <h3 className="text-sm font-semibold text-dark-text truncate">
              {plan.name}
            </h3>
            <p className="text-lg font-bold bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {priceLabel}
            </p>
          </div>
        </div>

        {/* Description / perks */}
        {descElements.length > 0 &&
          (hasBullets ? (
            <ul className="space-y-2.5 mt-5">{descElements}</ul>
          ) : (
            <div className="space-y-2 mt-5">{descElements}</div>
          ))}

        {/* Sponsor count (if any) */}
        {plan.sponsorCount > 0 && (
          <div className="mt-5 pt-4 border-t border-dark-border/30">
            <span className="text-[11px] text-dark-text-muted">
              <Users className="w-3 h-3 inline-block mr-1 -mt-px" />
              {plan.sponsorCount} {locale === "zh" ? "人已赞助" : "sponsors"}
            </span>
          </div>
        )}
      </div>
    </motion.div>
  );
}

// ── Sponsor avatar ─────────────────────────────────────────

function SponsorAvatar({
  sponsor,
  index,
}: {
  sponsor: Sponsor;
  index: number;
}) {
  return (
    <motion.div
      initial={{ opacity: 0, scale: 0.6 }}
      whileInView={{ opacity: 1, scale: 1 }}
      viewport={{ once: true }}
      transition={{ duration: 0.3, delay: Math.min(index * 0.04, 0.8) }}
      className="group relative"
    >
      <div className="relative w-11 h-11 rounded-full overflow-hidden border-2 border-dark-border/60 transition-all duration-200 group-hover:border-brand-sky/50 group-hover:scale-110 group-hover:shadow-lg group-hover:shadow-brand-sky/10">
        <img
          src={sponsor.avatar}
          alt={sponsor.name}
          className="w-full h-full object-cover"
          loading="lazy"
          onError={(e) => {
            (e.target as HTMLImageElement).src =
              `data:image/svg+xml,${encodeURIComponent(
                `<svg xmlns="http://www.w3.org/2000/svg" width="44" height="44" viewBox="0 0 44 44"><rect fill="%23232326" width="44" height="44" rx="22"/><text x="22" y="27" text-anchor="middle" fill="%2338bdf8" font-size="16" font-family="sans-serif">${sponsor.name.charAt(0).toUpperCase()}</text></svg>`,
              )}`;
          }}
        />
      </div>
      {/* Tooltip */}
      <div className="absolute -top-9 left-1/2 -translate-x-1/2 px-2 py-1 rounded-md bg-dark-surface2 border border-dark-border/60 text-[11px] text-dark-text whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity duration-150 pointer-events-none z-10 shadow-lg">
        {sponsor.name}
        {sponsor.plan && (
          <span className="ml-1 text-dark-text-muted">· {sponsor.plan}</span>
        )}
        <div className="absolute top-full left-1/2 -translate-x-1/2 -mt-px w-2 h-2 bg-dark-surface2 border-r border-b border-dark-border/60 rotate-45" />
      </div>
    </motion.div>
  );
}

// ── Skeleton loaders ───────────────────────────────────────

function PlanCardSkeleton({ index }: { index: number }) {
  return (
    <div
      className="rounded-xl border border-dark-border/40 bg-dark-surface1/50 p-6 animate-pulse"
      style={{ animationDelay: `${index * 100}ms` }}
    >
      <div className="flex items-center gap-3 mb-4">
        <div className="w-10 h-10 rounded-lg bg-dark-surface2" />
        <div className="space-y-2 flex-1">
          <div className="h-4 w-20 rounded bg-dark-surface2" />
          <div className="h-5 w-16 rounded bg-dark-surface2" />
        </div>
      </div>
      <div className="space-y-3 mt-5">
        <div className="h-3 w-full rounded bg-dark-surface2/60" />
        <div className="h-3 w-4/5 rounded bg-dark-surface2/60" />
        <div className="h-3 w-3/5 rounded bg-dark-surface2/60" />
      </div>
    </div>
  );
}

// ── Grid columns helper ────────────────────────────────────

function planGridCols(count: number): string {
  if (count <= 0) return "";
  if (count === 1) return "grid-cols-1 max-w-sm mx-auto";
  if (count === 2) return "grid-cols-1 sm:grid-cols-2 max-w-2xl mx-auto";
  if (count === 3) return "grid-cols-1 sm:grid-cols-3";
  return "grid-cols-1 sm:grid-cols-2 lg:grid-cols-4";
}

// ── Main component ─────────────────────────────────────────

export default function SponsorSection({
  fullPage = false,
}: SponsorSectionProps) {
  const { t, locale } = useLocale();
  const [data, setData] = useState<SponsorsPayload | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetch("/api/sponsors")
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((payload: SponsorsPayload) => setData(payload))
      .catch(() => setData(null))
      .finally(() => setLoading(false));
  }, []);

  const plans = data?.plans ?? [];
  const sponsors = data?.sponsors ?? [];
  const totalSponsors = data?.totalSponsors ?? 0;

  return (
    <section
      id="sponsor"
      className={`relative bg-dark-bg overflow-hidden ${fullPage ? "pt-32 sm:pt-40 pb-20 sm:pb-28" : "py-20 sm:py-28"}`}
    >
      {/* Background decorative elements */}
      <div className="absolute inset-0 pointer-events-none">
        <div className="absolute top-1/4 left-0 w-72 h-72 bg-pink-500/[0.03] blur-[100px] rounded-full" />
        <div className="absolute bottom-1/4 right-0 w-72 h-72 bg-brand-sky/[0.03] blur-[100px] rounded-full" />
      </div>

      <div className="relative mx-auto max-w-5xl px-4 sm:px-6 lg:px-8">
        {/* ── Section header ─────────────────────────────── */}
        <motion.div
          className="text-center mb-14"
          initial={{ opacity: 0, y: 24 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, amount: 0.3 }}
          transition={{ duration: 0.5 }}
        >
          {/* Badge */}
          <motion.div
            className="inline-flex items-center gap-2 px-3.5 py-1.5 rounded-full border border-pink-500/20 bg-pink-500/5 mb-6"
            initial={{ opacity: 0, scale: 0.9 }}
            whileInView={{ opacity: 1, scale: 1 }}
            viewport={{ once: true }}
            transition={{ duration: 0.4 }}
          >
            <Heart className="w-3.5 h-3.5 text-pink-400" />
            <span className="text-xs font-medium text-pink-400 tracking-wide">
              {t("sponsor.badge")}
            </span>
          </motion.div>

          {/* Title */}
          <h2 className="text-3xl sm:text-4xl font-bold tracking-tight text-dark-text">
            {t("sponsor.title")}
            <span className="bg-gradient-to-r from-pink-400 via-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("sponsor.titleHighlight")}
            </span>
          </h2>

          {/* Subtitle */}
          <p className="mt-4 text-sm sm:text-base text-dark-text-secondary max-w-2xl mx-auto leading-relaxed">
            {t("sponsor.subtitle")}
          </p>

          {/* Creator profile (dynamic) */}
          {data?.profile && (
            <motion.div
              className="mt-5 inline-flex items-center gap-2.5 px-4 py-2 rounded-full bg-dark-surface1 border border-dark-border/40"
              initial={{ opacity: 0, y: 8 }}
              whileInView={{ opacity: 1, y: 0 }}
              viewport={{ once: true }}
              transition={{ duration: 0.4, delay: 0.2 }}
            >
              <img
                src={data.profile.avatar}
                alt={data.profile.name}
                className="w-6 h-6 rounded-full object-cover"
              />
              <span className="text-xs text-dark-text-secondary">
                {data.profile.doing}
                {data.profile.category && (
                  <span className="ml-1.5 text-dark-text-muted">
                    · {data.profile.category}
                  </span>
                )}
              </span>
            </motion.div>
          )}
        </motion.div>

        {/* ── Plan cards ─────────────────────────────────── */}
        <AnimatePresence mode="wait">
          {loading ? (
            <motion.div
              key="skeleton"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-5 mb-12"
            >
              {[0, 1, 2, 3].map((i) => (
                <PlanCardSkeleton key={i} index={i} />
              ))}
            </motion.div>
          ) : plans.length > 0 ? (
            <motion.div
              key="plans"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              className={`grid gap-5 mb-12 ${planGridCols(plans.length)}`}
            >
              {plans.map((plan, i) => (
                <PlanCard
                  key={plan.planId}
                  plan={plan}
                  tierIndex={i}
                  totalPlans={plans.length}
                  index={i}
                  locale={locale}
                />
              ))}
            </motion.div>
          ) : null}
        </AnimatePresence>

        {/* ── CTA button ─────────────────────────────────── */}
        <motion.div
          className="text-center mb-16"
          initial={{ opacity: 0, y: 16 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5, delay: 0.3 }}
        >
          <a
            href={AFDIAN_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="group inline-flex items-center gap-2.5 px-7 py-3.5 rounded-xl bg-gradient-to-r from-pink-500 to-rose-500 text-white font-semibold text-sm tracking-wide shadow-lg shadow-pink-500/20 hover:shadow-xl hover:shadow-pink-500/30 hover:from-pink-600 hover:to-rose-600 transition-all duration-300 hover:scale-[1.03] active:scale-[0.98]"
          >
            <Heart className="w-4 h-4 transition-transform duration-300 group-hover:scale-110" />
            {t("sponsor.cta")}
            <ExternalLink className="w-3.5 h-3.5 opacity-60 transition-transform duration-300 group-hover:translate-x-0.5 group-hover:-translate-y-0.5" />
          </a>

          <p className="mt-3 text-xs text-dark-text-muted">
            {t("sponsor.ctaHint")}
          </p>
        </motion.div>

        {/* ── Sponsor wall ───────────────────────────────── */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, amount: 0.2 }}
          transition={{ duration: 0.5, delay: 0.15 }}
        >
          {/* Divider with label */}
          <div className="flex items-center gap-4 mb-8">
            <div className="flex-1 h-px bg-gradient-to-r from-transparent to-dark-border/60" />
            <div className="flex items-center gap-2 text-dark-text-muted">
              <Users className="w-4 h-4" />
              <span className="text-xs font-medium tracking-wide uppercase">
                {t("sponsor.sponsors")}
              </span>
              {totalSponsors > 0 && (
                <span className="inline-flex items-center justify-center min-w-[20px] h-5 px-1.5 rounded-full bg-brand-sky/10 text-brand-sky text-[10px] font-bold">
                  {totalSponsors}
                </span>
              )}
            </div>
            <div className="flex-1 h-px bg-gradient-to-l from-transparent to-dark-border/60" />
          </div>

          {/* Sponsor avatars or empty state */}
          <AnimatePresence mode="wait">
            {loading ? (
              <motion.div
                key="loading"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="flex justify-center py-8"
              >
                <div className="flex gap-3">
                  {[0, 1, 2, 3, 4].map((i) => (
                    <div
                      key={i}
                      className="w-11 h-11 rounded-full bg-dark-surface2 animate-pulse"
                      style={{ animationDelay: `${i * 100}ms` }}
                    />
                  ))}
                </div>
              </motion.div>
            ) : sponsors.length > 0 ? (
              <motion.div
                key="sponsors"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="flex flex-wrap justify-center gap-3"
              >
                {sponsors.map((sponsor, i) => (
                  <SponsorAvatar
                    key={`${sponsor.name}-${i}`}
                    sponsor={sponsor}
                    index={i}
                  />
                ))}
              </motion.div>
            ) : (
              <motion.div
                key="empty"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="text-center py-8"
              >
                <div className="inline-flex items-center justify-center w-14 h-14 rounded-full bg-dark-surface2 border border-dark-border/40 mb-3">
                  <Heart className="w-6 h-6 text-dark-text-muted" />
                </div>
                <p className="text-sm text-dark-text-muted">
                  {t("sponsor.beFirst")}
                </p>
              </motion.div>
            )}
          </AnimatePresence>
        </motion.div>
      </div>
    </section>
  );
}
