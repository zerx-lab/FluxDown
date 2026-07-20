import { useState, useEffect, useRef, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { Heart, Loader2, CheckCircle2, X, RefreshCw } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import { useLocale } from "@/lib/i18n";

/* ============================================================
   SponsorSection — Free-amount payment (zerx pay gateway)
   - User picks a preset tier or enters a custom amount.
   - POST /api/pay/create -> WeChat codeUrl, rendered as QR.
   - Polls /api/pay/query until status === "paid".
   ============================================================ */

interface SponsorSectionProps {
  fullPage?: boolean;
}

// Preset amounts in yuan.
const PRESET_AMOUNTS = [5, 15, 30, 66, 128];

// WeChat Pay center logo for the QR code: white WeChat glyph on a
// brand-green rounded tile, inlined as an SVG data URI so it needs no asset.
const WECHAT_PAY_LOGO =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">' +
      '<rect width="48" height="48" rx="12" fill="#07C160"/>' +
      '<g fill="#ffffff" transform="translate(8 12)">' +
      '<path d="M11.6 0C5.2 0 0 4.4 0 9.9c0 3 1.6 5.7 4.2 7.6L3.1 21l3.9-2c1.3.3 2.6.5 4 .5h.6a8.4 8.4 0 0 1-.4-2.6c0-4.9 4.7-8.9 10.5-9h.6C21.2 3 16.9 0 11.6 0zM7.9 7.6a1.4 1.4 0 1 1 0-2.9 1.4 1.4 0 0 1 0 2.9zm7.4 0a1.4 1.4 0 1 1 0-2.9 1.4 1.4 0 0 1 0 2.9z"/>' +
      '<path d="M32 17.5c0-4.3-4.2-7.8-9.4-7.8s-9.4 3.5-9.4 7.8 4.2 7.8 9.4 7.8c1.1 0 2.1-.2 3.1-.5l3.1 1.7-.9-2.8c2.5-1.5 4-3.7 4-6.2zm-12.4-1.3a1.15 1.15 0 1 1 0-2.3 1.15 1.15 0 0 1 0 2.3zm6.2 0a1.15 1.15 0 1 1 0-2.3 1.15 1.15 0 0 1 0 2.3z"/>' +
      "</g></svg>",
  );

type PayState =
  | { phase: "idle" }
  | { phase: "creating" }
  | { phase: "pending"; codeUrl: string; outTradeNo: string }
  | { phase: "paid" }
  | { phase: "error"; message: string };

// Poll config.
const POLL_INTERVAL = 2500;
const POLL_TIMEOUT = 5 * 60 * 1000; // 5 minutes

// Public sponsor-wall issue (comments = wall entries).
const SPONSOR_WALL_URL = "https://github.com/zerx-lab/FluxDown/issues/3";

interface WallSponsor {
  name: string;
  avatar: string | null;
  amountCents: number;
  date: string;
  message: string | null;
}

// Deterministic fallback tile color for sponsors without an avatar.
const AVATAR_COLORS = [
  "bg-pink-500/20 text-pink-300",
  "bg-sky-500/20 text-sky-300",
  "bg-emerald-500/20 text-emerald-300",
  "bg-amber-500/20 text-amber-300",
  "bg-violet-500/20 text-violet-300",
];

function avatarColor(name: string): string {
  let h = 0;
  for (let i = 0; i < name.length; i += 1) {
    h = (h * 31 + name.charCodeAt(i)) >>> 0;
  }
  return AVATAR_COLORS[h % AVATAR_COLORS.length]!;
}

function fmtCents(cents: number): string {
  const yuan = cents / 100;
  return Number.isInteger(yuan) ? `¥${yuan}` : `¥${yuan.toFixed(2)}`;
}

// "2024-05-01" → "2024.05.01"; falls back to the raw value.
function fmtDate(date: string): string {
  return /^\d{4}-\d{2}-\d{2}$/.test(date) ? date.replace(/-/g, ".") : date;
}

export default function SponsorSection({
  fullPage = false,
}: SponsorSectionProps) {
  const { t } = useLocale();

  const [selected, setSelected] = useState<number>(PRESET_AMOUNTS[1]!);
  const [custom, setCustom] = useState<string>("");
  const [name, setName] = useState<string>("");
  const [message, setMessage] = useState<string>("");
  const [wallQueued, setWallQueued] = useState(false);
  const [pay, setPay] = useState<PayState>({ phase: "idle" });

  // Mirror name/message into a ref so the long-lived poll closure
  // reads the latest values without re-arming timers.
  const wallInfo = useRef({ name: "", message: "" });
  useEffect(() => {
    wallInfo.current = { name, message };
  }, [name, message]);

  // Latest sponsors from the GitHub wall (newest first).
  const [sponsors, setSponsors] = useState<WallSponsor[]>([]);
  useEffect(() => {
    let alive = true;
    fetch("/api/sponsor/list")
      .then((r) => (r.ok ? r.json() : null))
      .then((d: { sponsors?: WallSponsor[] } | null) => {
        if (alive && Array.isArray(d?.sponsors)) setSponsors(d.sponsors);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  // Featured: top 3 by amount ("special thanks"); rest: newest first.
  const featuredIdx = sponsors
    .map((_, i) => i)
    .sort((a, b) => sponsors[b]!.amountCents - sponsors[a]!.amountCents)
    .slice(0, 3);
  const featuredSet = new Set(featuredIdx);
  const featured = featuredIdx.map((i) => sponsors[i]!);
  const rest = sponsors.filter((_, i) => !featuredSet.has(i));

  // Effective amount in yuan (custom overrides preset when valid).
  const customNum = parseFloat(custom);
  const amountYuan =
    custom.trim() !== "" && Number.isFinite(customNum) && customNum > 0
      ? customNum
      : selected;
  const amountValid = Number.isFinite(amountYuan) && amountYuan >= 1;

  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pollDeadline = useRef<number>(0);

  const stopPolling = useCallback(() => {
    if (pollTimer.current) {
      clearTimeout(pollTimer.current);
      pollTimer.current = null;
    }
  }, []);

  useEffect(() => () => stopPolling(), [stopPolling]);

  // Post to the sponsor wall once payment is confirmed. Name/message may be
  // empty — anonymous sponsors are still recorded (server uses a fallback name).
  // Fire-and-forget: the thank-you screen must not block on GitHub.
  const submitWall = useCallback((outTradeNo: string) => {
    const n = wallInfo.current.name.trim();
    const m = wallInfo.current.message.trim();
    setWallQueued(true);
    void fetch("/api/sponsor/wall", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ outTradeNo, name: n, message: m }),
    }).catch(() => {});
  }, []);

  const poll = useCallback(
    (outTradeNo: string) => {
      const tick = async () => {
        if (Date.now() > pollDeadline.current) {
          stopPolling();
          setPay({ phase: "error", message: t("sponsor.pay.timeout") });
          return;
        }
        try {
          const res = await fetch(
            `/api/pay/status?outTradeNo=${encodeURIComponent(outTradeNo)}`,
          );
          if (res.ok) {
            const data = (await res.json()) as { paid?: boolean };
            if (data.paid) {
              stopPolling();
              submitWall(outTradeNo);
              setPay({ phase: "paid" });
              return;
            }
          }
        } catch {
          // transient — keep polling
        }
        pollTimer.current = setTimeout(tick, POLL_INTERVAL);
      };
      pollTimer.current = setTimeout(tick, POLL_INTERVAL);
    },
    [stopPolling, submitWall, t],
  );

  const startPayment = useCallback(async () => {
    if (!amountValid) return;
    setWallQueued(false);
    setPay({ phase: "creating" });
    try {
      const res = await fetch("/api/pay/create", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          amountCents: Math.round(amountYuan * 100),
          subject: "Support FluxDown",
        }),
      });
      if (!res.ok) {
        const data = (await res.json().catch(() => ({}))) as { error?: string };
        setPay({
          phase: "error",
          message:
            res.status === 503
              ? t("sponsor.pay.unavailable")
              : data.error || t("sponsor.pay.failed"),
        });
        return;
      }
      const data = (await res.json()) as {
        codeUrl: string;
        outTradeNo: string;
      };
      if (!data.codeUrl || !data.outTradeNo) {
        setPay({ phase: "error", message: t("sponsor.pay.failed") });
        return;
      }
      pollDeadline.current = Date.now() + POLL_TIMEOUT;
      setPay({
        phase: "pending",
        codeUrl: data.codeUrl,
        outTradeNo: data.outTradeNo,
      });
      poll(data.outTradeNo);
    } catch {
      setPay({ phase: "error", message: t("sponsor.pay.failed") });
    }
  }, [amountValid, amountYuan, poll, t]);

  const closeModal = useCallback(() => {
    stopPolling();
    setPay({ phase: "idle" });
  }, [stopPolling]);

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

      <div className="relative mx-auto max-w-2xl px-4 sm:px-6 lg:px-8">
        {/* ── Section header ─────────────────────────────── */}
        <motion.div
          className="text-center mb-12"
          initial={{ opacity: 0, y: 24 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, amount: 0.3 }}
          transition={{ duration: 0.5 }}
        >
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

          <h2 className="text-3xl sm:text-4xl font-bold tracking-tight text-dark-text">
            {t("sponsor.title")}
            <span className="bg-gradient-to-r from-pink-400 via-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("sponsor.titleHighlight")}
            </span>
          </h2>

          <p className="mt-4 text-sm sm:text-base text-dark-text-secondary max-w-xl mx-auto leading-relaxed">
            {t("sponsor.subtitle")}
          </p>
        </motion.div>

        {/* ── Payment card ───────────────────────────────── */}
        <motion.div
          className="rounded-2xl border border-dark-border/50 bg-dark-surface1/60 p-6 sm:p-8 backdrop-blur-sm"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, amount: 0.2 }}
          transition={{ duration: 0.5, delay: 0.1 }}
        >
          {/* Preset amount tiers */}
          <div className="grid grid-cols-3 sm:grid-cols-5 gap-2.5 mb-5">
            {PRESET_AMOUNTS.map((amt) => {
              const active = custom.trim() === "" && selected === amt;
              return (
                <button
                  key={amt}
                  type="button"
                  onClick={() => {
                    setSelected(amt);
                    setCustom("");
                  }}
                  className={`py-3 rounded-xl border text-sm font-semibold transition-all duration-200 ${
                    active
                      ? "border-brand-sky bg-brand-sky/10 text-brand-sky"
                      : "border-dark-border/50 bg-dark-surface2/40 text-dark-text-secondary hover:border-dark-border hover:text-dark-text"
                  }`}
                >
                  ¥{amt}
                </button>
              );
            })}
          </div>

          {/* Custom amount */}
          <div className="relative mb-6">
            <span className="absolute left-4 top-1/2 -translate-y-1/2 text-dark-text-muted text-sm">
              ¥
            </span>
            <input
              type="number"
              min={1}
              step={1}
              inputMode="decimal"
              value={custom}
              onChange={(e) => setCustom(e.target.value)}
              placeholder={t("sponsor.pay.customPlaceholder")}
              className="w-full pl-8 pr-4 py-3 rounded-xl border border-dark-border/50 bg-dark-surface2/40 text-dark-text text-sm placeholder:text-dark-text-muted focus:outline-none focus:border-brand-sky/60 focus:ring-1 focus:ring-brand-sky/30 transition-all duration-200"
            />
          </div>

          {/* Sponsor wall: optional name + message */}
          <div className="space-y-3 mb-6">
            <input
              type="text"
              maxLength={30}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("sponsor.wall.namePlaceholder")}
              className="w-full px-4 py-3 rounded-xl border border-dark-border/50 bg-dark-surface2/40 text-dark-text text-sm placeholder:text-dark-text-muted focus:outline-none focus:border-brand-sky/60 focus:ring-1 focus:ring-brand-sky/30 transition-all duration-200"
            />
            <textarea
              rows={2}
              maxLength={300}
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              placeholder={t("sponsor.wall.messagePlaceholder")}
              className="w-full px-4 py-3 rounded-xl border border-dark-border/50 bg-dark-surface2/40 text-dark-text text-sm placeholder:text-dark-text-muted focus:outline-none focus:border-brand-sky/60 focus:ring-1 focus:ring-brand-sky/30 transition-all duration-200 resize-none"
            />
            <p className="text-xs text-dark-text-muted leading-relaxed">
              {t("sponsor.wall.hint")}{" "}
              <a
                href={SPONSOR_WALL_URL}
                target="_blank"
                rel="noopener noreferrer"
                className="text-brand-sky hover:underline"
              >
                {t("sponsor.wall.link")}
              </a>
            </p>
          </div>

          {/* Pay button */}
          <button
            type="button"
            onClick={startPayment}
            disabled={!amountValid || pay.phase === "creating"}
            className="group w-full inline-flex items-center justify-center gap-2.5 px-7 py-3.5 rounded-xl bg-gradient-to-r from-pink-500 to-rose-500 text-white font-semibold text-sm tracking-wide shadow-lg shadow-pink-500/20 hover:shadow-xl hover:shadow-pink-500/30 hover:from-pink-600 hover:to-rose-600 transition-all duration-300 hover:scale-[1.02] active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:scale-100"
          >
            {pay.phase === "creating" ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <Heart className="w-4 h-4 transition-transform duration-300 group-hover:scale-110" />
            )}
            {t("sponsor.pay.cta")}
            {amountValid && (
              <span className="opacity-80">· ¥{amountYuan}</span>
            )}
          </button>

          <p className="mt-3 text-center text-xs text-dark-text-muted">
            {t("sponsor.ctaHint")}
          </p>
        </motion.div>

        {/* ── Latest sponsors (from the GitHub wall) ─────── */}
        {sponsors.length > 0 && (
          <motion.div
            className="mt-10"
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true, amount: 0.2 }}
            transition={{ duration: 0.5, delay: 0.15 }}
          >
            <h3 className="text-center text-sm font-semibold text-dark-text mb-4">
              {t("sponsor.list.title")}
            </h3>
            {featured.length > 0 && (
              <>
                <p className="text-center text-xs text-dark-text-muted mb-2.5">
                  {t("sponsor.list.featured")}
                </p>
                <div className="grid grid-cols-3 gap-2.5 sm:gap-3 mb-4">
                  {featured.map((s, i) => (
                    <div
                      key={`feat-${s.name}-${s.date}-${i}`}
                      className="flex flex-col items-center text-center rounded-2xl border border-dark-border/40 bg-gradient-to-b from-pink-500/[0.05] to-dark-surface1/40 px-3 pt-4 pb-4"
                    >
                      {s.avatar ? (
                        <img
                          src={s.avatar}
                          alt=""
                          loading="lazy"
                          referrerPolicy="no-referrer"
                          className="w-14 h-14 rounded-full object-cover ring-2 ring-pink-400/25"
                        />
                      ) : (
                        <span
                          className={`w-14 h-14 rounded-full inline-flex items-center justify-center text-base font-semibold ring-2 ring-pink-400/25 ${avatarColor(s.name)}`}
                        >
                          {s.name.slice(0, 1).toUpperCase()}
                        </span>
                      )}
                      <span
                        className="mt-2 max-w-full truncate text-xs sm:text-sm font-medium text-dark-text"
                        title={s.name}
                      >
                        {s.name}
                      </span>
                      <div className="mt-1 flex flex-wrap items-center justify-center gap-x-1.5 text-[11px] text-dark-text-muted">
                        {s.amountCents > 0 && (
                          <span className="font-semibold text-pink-300/90">
                            {fmtCents(s.amountCents)}
                          </span>
                        )}
                        <span>{fmtDate(s.date)}</span>
                      </div>
                      {s.message && (
                        <p
                          className="mt-2 w-full whitespace-pre-line break-words text-[11px] leading-relaxed text-dark-text-secondary line-clamp-3"
                          title={s.message}
                        >
                          “{s.message}”
                        </p>
                      )}
                    </div>
                  ))}
                </div>
              </>
            )}

            {rest.length > 0 && (
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
                {rest.map((s, i) => (
                  <div
                    key={`${s.name}-${s.date}-${i}`}
                    className="flex flex-col gap-2 px-3.5 py-2.5 rounded-xl border border-dark-border/40 bg-dark-surface1/40"
                  >
                    <div className="flex items-center gap-3">
                      {s.avatar ? (
                        <img
                          src={s.avatar}
                          alt=""
                          loading="lazy"
                          referrerPolicy="no-referrer"
                          className="w-8 h-8 rounded-full object-cover shrink-0"
                        />
                      ) : (
                        <span
                          className={`w-8 h-8 rounded-full shrink-0 inline-flex items-center justify-center text-xs font-semibold ${avatarColor(s.name)}`}
                        >
                          {s.name.slice(0, 1).toUpperCase()}
                        </span>
                      )}
                      <div className="flex-1 min-w-0">
                        <span
                          className="block truncate text-sm text-dark-text-secondary"
                          title={s.name}
                        >
                          {s.name}
                        </span>
                        <span className="block text-[11px] text-dark-text-muted">
                          {fmtDate(s.date)}
                        </span>
                      </div>
                      {s.amountCents > 0 && (
                        <span className="shrink-0 text-xs font-semibold text-pink-300/90">
                          {fmtCents(s.amountCents)}
                        </span>
                      )}
                    </div>
                    {s.message && (
                      <p
                        className="pl-11 whitespace-pre-line break-words text-xs leading-relaxed text-dark-text-muted line-clamp-2"
                        title={s.message}
                      >
                        “{s.message}”
                      </p>
                    )}
                  </div>
                ))}
              </div>
            )}
            <p className="mt-4 text-center">
              <a
                href={SPONSOR_WALL_URL}
                target="_blank"
                rel="noopener noreferrer"
                className="text-xs text-brand-sky hover:underline"
              >
                {t("sponsor.wall.link")} →
              </a>
            </p>
          </motion.div>
        )}
      </div>

      {/* ── Payment modal (QR + status) ──────────────────── */}
      <AnimatePresence>
        {(pay.phase === "pending" ||
          pay.phase === "paid" ||
          pay.phase === "error") && (
          <motion.div
            className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/60 backdrop-blur-sm"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={closeModal}
          >
            <motion.div
              className="relative w-full max-w-sm rounded-2xl border border-dark-border/60 bg-dark-surface1 p-7 text-center"
              initial={{ opacity: 0, scale: 0.95, y: 12 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={{ opacity: 0, scale: 0.95, y: 12 }}
              transition={{ duration: 0.2 }}
              onClick={(e) => e.stopPropagation()}
            >
              <button
                type="button"
                onClick={closeModal}
                className="absolute right-4 top-4 text-dark-text-muted hover:text-dark-text transition-colors"
                aria-label="Close"
              >
                <X className="w-4 h-4" />
              </button>

              {pay.phase === "pending" && (
                <>
                  <div className="inline-flex items-center gap-1.5 px-2.5 py-1 mb-3 rounded-full bg-[#07C160]/10 border border-[#07C160]/30">
                    <svg
                      viewBox="0 0 24 24"
                      className="w-3.5 h-3.5"
                      fill="#07C160"
                      aria-hidden="true"
                    >
                      <path d="M8.7 3C4.5 3 1 5.9 1 9.5c0 2 1.1 3.8 2.9 5l-.7 2.2 2.6-1.3c.9.2 1.8.4 2.7.4h.5a5.6 5.6 0 0 1-.3-1.8c0-3.3 3.2-6 7.1-6h.5C16.3 4.9 12.9 3 8.7 3zM6.2 8.1a.95.95 0 1 1 0-1.9.95.95 0 0 1 0 1.9zm5 0a.95.95 0 1 1 0-1.9.95.95 0 0 1 0 1.9zM23 14.4c0-2.9-2.9-5.3-6.5-5.3s-6.5 2.4-6.5 5.3 2.9 5.3 6.5 5.3c.8 0 1.5-.1 2.2-.3l2.1 1.1-.6-1.8c1.7-1 2.8-2.5 2.8-4.3zm-8.6-.9a.8.8 0 1 1 0-1.6.8.8 0 0 1 0 1.6zm4.3 0a.8.8 0 1 1 0-1.6.8.8 0 0 1 0 1.6z" />
                    </svg>
                    <span className="text-[11px] font-semibold text-[#07C160]">
                      {t("sponsor.pay.wechatBadge")}
                    </span>
                  </div>
                  <h3 className="text-base font-semibold text-dark-text mb-1">
                    {t("sponsor.pay.scanTitle")}
                  </h3>
                  <p className="text-xs text-dark-text-muted mb-5">
                    {t("sponsor.pay.scanHint")}
                  </p>
                  <div className="inline-flex p-4 rounded-xl bg-white mb-5">
                    <QRCodeSVG
                      value={pay.codeUrl}
                      size={200}
                      level="H"
                      imageSettings={{
                        src: WECHAT_PAY_LOGO,
                        height: 40,
                        width: 40,
                        excavate: true,
                      }}
                    />
                  </div>
                  <div className="flex items-center justify-center gap-2 text-xs text-dark-text-secondary">
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    {t("sponsor.pay.waiting")}
                  </div>
                </>
              )}

              {pay.phase === "paid" && (
                <div className="py-4">
                  <div className="inline-flex items-center justify-center w-16 h-16 rounded-full bg-emerald-500/10 mb-4">
                    <CheckCircle2 className="w-9 h-9 text-emerald-400" />
                  </div>
                  <h3 className="text-lg font-semibold text-dark-text mb-1">
                    {t("sponsor.pay.thanksTitle")}
                  </h3>
                  <p className="text-sm text-dark-text-secondary">
                    {t("sponsor.pay.thanksBody")}
                  </p>
                  {wallQueued && (
                    <p className="mt-2 text-xs text-dark-text-muted">
                      {t("sponsor.wall.thanksNote")}
                    </p>
                  )}
                  <a
                    href={SPONSOR_WALL_URL}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="mt-4 inline-flex items-center gap-1 text-xs text-brand-sky hover:underline"
                  >
                    {t("sponsor.wall.link")} →
                  </a>
                </div>
              )}

              {pay.phase === "error" && (
                <div className="py-4">
                  <h3 className="text-base font-semibold text-dark-text mb-2">
                    {t("sponsor.pay.errorTitle")}
                  </h3>
                  <p className="text-sm text-dark-text-secondary mb-5">
                    {pay.message}
                  </p>
                  <button
                    type="button"
                    onClick={() => {
                      setPay({ phase: "idle" });
                      startPayment();
                    }}
                    className="inline-flex items-center gap-2 px-5 py-2.5 rounded-lg border border-dark-border/60 text-sm text-dark-text hover:bg-dark-surface2 transition-colors"
                  >
                    <RefreshCw className="w-3.5 h-3.5" />
                    {t("sponsor.pay.retry")}
                  </button>
                </div>
              )}
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </section>
  );
}
