import { useState, useEffect, useCallback } from "react";
import type { ReactNode } from "react";
import { motion, AnimatePresence } from "framer-motion";
import type { Messages } from "@/lib/locales";
import { useLocale } from "@/lib/i18n";
import IssueDetailModal from "./IssueDetailModal";

type RoadmapStatus = "planned" | "in-progress" | "done";

interface RoadmapLabel {
  name: string;
  color: string;
}

interface RoadmapItem {
  id: number;
  title: string;
  description: string;
  status: RoadmapStatus;
  url: string;
  labels: RoadmapLabel[];
  createdAt: string;
  updatedAt: string;
  comments: number;
}

interface RoadmapColumn {
  status: RoadmapStatus;
  items: RoadmapItem[];
}

interface RoadmapData {
  columns: RoadmapColumn[];
  counts: Record<RoadmapStatus, number>;
  total: number;
  updatedAt: string;
}

/** Per-status accent + copy. Universal semantics: neutral → warm → green. */
const STATUS_META: Record<
  RoadmapStatus,
  { color: string; labelKey: keyof Messages; icon: ReactNode }
> = {
  planned: {
    color: "#8B8B93",
    labelKey: "roadmap.status.planned",
    icon: (
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="10" strokeDasharray="3 3" />
        <path d="M12 7v5l3 2" />
      </svg>
    ),
  },
  "in-progress": {
    color: "#F5C518",
    labelKey: "roadmap.status.inProgress",
    icon: (
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21 12a9 9 0 1 1-6.219-8.56" />
      </svg>
    ),
  },
  done: {
    color: "#22C55E",
    labelKey: "roadmap.status.done",
    icon: (
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21.801 10A10 10 0 1 1 17 3.335" />
        <path d="m9 11 3 3L22 4" />
      </svg>
    ),
  },
};

/** Compact locale-aware date, e.g. "Jul 16, 2026" / "2026年7月16日". */
function formatDate(dateStr: string, locale: string): string {
  const d = new Date(dateStr);
  if (Number.isNaN(d.getTime())) return "";
  try {
    return new Intl.DateTimeFormat(locale === "zh" ? "zh-CN" : locale, {
      year: "numeric",
      month: "short",
      day: "numeric",
    }).format(d);
  } catch {
    return d.toISOString().slice(0, 10);
  }
}

/** Readable text color for a GitHub hex label background. */
function labelTextColor(hex: string): string {
  if (!/^[0-9a-fA-F]{6}$/.test(hex)) return "currentColor";
  const r = parseInt(hex.slice(0, 2), 16);
  const g = parseInt(hex.slice(2, 4), 16);
  const b = parseInt(hex.slice(4, 6), 16);
  const luminance = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
  return luminance > 0.6 ? "#18181b" : "#fafafa";
}

function RoadmapCard({
  item,
  index,
  color,
  onOpen,
  locale,
  t,
}: {
  item: RoadmapItem;
  index: number;
  color: string;
  onOpen: (id: number) => void;
  locale: string;
  t: (key: keyof Messages, params?: Record<string, string>) => string;
}) {
  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 12 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.35, delay: Math.min(0.04 * index, 0.3) }}
      role="button"
      tabIndex={0}
      onClick={() => onOpen(item.id)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen(item.id);
        }
      }}
      className="group relative cursor-pointer rounded-xl border border-dark-border bg-dark-surface1/60 p-4 transition-all duration-200 hover:-translate-y-0.5 hover:bg-dark-surface1 hover:shadow-lg hover:border-dark-text-muted/40"
      style={{ ["--accent" as string]: color }}
    >
      {/* status-colored accent bar */}
      <span
        className="absolute left-0 top-4 bottom-4 w-[3px] rounded-full opacity-70 transition-opacity group-hover:opacity-100"
        style={{ backgroundColor: color }}
      />

      <div className="pl-2.5">
        <h3
          className="text-sm font-semibold text-dark-text leading-snug transition-colors group-hover:text-[var(--accent)]"
        >
          {item.title}
        </h3>

        {item.description && (
          <p className="mt-1.5 text-xs text-dark-text-muted line-clamp-2 leading-relaxed">
            {item.description}
          </p>
        )}

        {item.labels.length > 0 && (
          <div className="mt-2.5 flex flex-wrap gap-1.5">
            {item.labels.slice(0, 4).map((label) => (
              <span
                key={label.name}
                className="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium leading-none"
                style={
                  /^[0-9a-fA-F]{6}$/.test(label.color)
                    ? {
                        backgroundColor: `#${label.color}`,
                        color: labelTextColor(label.color),
                      }
                    : {
                        backgroundColor: "var(--color-dark-surface3)",
                        color: "var(--color-dark-text-secondary)",
                      }
                }
              >
                {label.name}
              </span>
            ))}
          </div>
        )}

        <div className="mt-2.5 flex items-center gap-3 text-[11px] text-dark-text-muted/80">
          <span className="inline-flex items-center gap-1">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M8 2v4" />
              <path d="M16 2v4" />
              <rect width="18" height="18" x="3" y="4" rx="2" />
              <path d="M3 10h18" />
            </svg>
            {formatDate(item.updatedAt, locale)}
          </span>
          {item.comments > 0 && (
            <span className="inline-flex items-center gap-1">
              <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z" />
              </svg>
              {item.comments} {t("issueDetail.replies")}
            </span>
          )}
        </div>
      </div>
    </motion.div>
  );
}

export default function RoadmapSection() {
  const { t, locale } = useLocale();
  const [data, setData] = useState<RoadmapData | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [detailIssue, setDetailIssue] = useState<number | null>(null);

  const openDetail = useCallback((id: number) => setDetailIssue(id), []);

  useEffect(() => {
    fetch("/api/roadmap")
      .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
      .then((fresh: RoadmapData) => setData(fresh))
      .catch(() => setLoadError(true))
      .finally(() => setLoading(false));
  }, []);

  return (
    <section className="pt-10 sm:pt-12 pb-16 sm:pb-20">
      <div className="mx-auto max-w-6xl px-4 sm:px-6 lg:px-8">
        {/* ── Header ── */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.5 }}
          className="text-center mb-10 sm:mb-14"
        >
          <span className="inline-flex items-center gap-2 rounded-full border border-dark-border bg-dark-surface1/50 px-4 py-1.5 text-xs font-medium text-dark-text-secondary backdrop-blur-sm mb-6">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-brand-sky">
              <path d="M14.106 5.553a2 2 0 0 0 1.788 0l3.659-1.83A1 1 0 0 1 21 4.619v12.764a1 1 0 0 1-.553.894l-4.553 2.277a2 2 0 0 1-1.788 0l-4.212-2.106a2 2 0 0 0-1.788 0l-3.659 1.83A1 1 0 0 1 3 19.381V6.618a1 1 0 0 1 .553-.894l4.553-2.277a2 2 0 0 1 1.788 0z" />
              <path d="M15 5.764v15" />
              <path d="M9 3.236v15" />
            </svg>
            {t("roadmap.badge")}
          </span>

          <h1 className="text-4xl sm:text-5xl font-bold tracking-tight leading-tight">
            <span className="text-dark-text">{t("roadmap.title")}</span>
            <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">{t("roadmap.titleHighlight")}</span>
          </h1>

          <p className="mt-4 text-base sm:text-lg text-dark-text-secondary max-w-2xl mx-auto leading-relaxed">
            {t("roadmap.subtitle")}
          </p>
        </motion.div>

        {/* ── Loading / error ── */}
        {loading && (
          <div className="flex items-center justify-center py-20">
            <div className="flex items-center gap-3 text-dark-text-muted">
              <svg className="w-5 h-5 animate-spin" viewBox="0 0 24 24" fill="none">
                <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
              </svg>
              <span className="text-sm">{t("roadmap.loading")}</span>
            </div>
          </div>
        )}

        {loadError && (
          <div className="flex items-center justify-center py-20">
            <span className="text-sm text-danger">{t("roadmap.loadError")}</span>
          </div>
        )}

        {/* ── Board ── */}
        {!loading && !loadError && data && (
          data.total === 0 ? (
            <div className="text-center py-16 text-sm text-dark-text-muted">
              {t("roadmap.empty")}
            </div>
          ) : (
            <div className="grid grid-cols-1 lg:grid-cols-3 gap-5">
              {data.columns.map((column, colIdx) => {
                const meta = STATUS_META[column.status];
                return (
                  <motion.div
                    key={column.status}
                    initial={{ opacity: 0, y: 16 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ duration: 0.4, delay: 0.1 * colIdx }}
                    className="flex flex-col rounded-2xl border border-dark-border bg-dark-surface1/30 backdrop-blur-sm"
                  >
                    {/* Column header */}
                    <div
                      className="flex items-center justify-between gap-2 rounded-t-2xl border-b border-dark-border px-4 py-3.5"
                      style={{
                        background: `linear-gradient(180deg, ${meta.color}14 0%, transparent 100%)`,
                      }}
                    >
                      <div className="flex items-center gap-2 font-semibold text-sm" style={{ color: meta.color }}>
                        {meta.icon}
                        <span className="text-dark-text">{t(meta.labelKey)}</span>
                      </div>
                      <span
                        className="inline-flex items-center justify-center min-w-6 h-6 px-1.5 rounded-full text-xs font-bold tabular-nums"
                        style={{
                          color: meta.color,
                          backgroundColor: `${meta.color}1F`,
                        }}
                      >
                        {column.items.length}
                      </span>
                    </div>

                    {/* Column body */}
                    <div className="flex-1 p-3 space-y-3 min-h-[120px]">
                      {column.items.length === 0 ? (
                        <div className="flex items-center justify-center h-full py-10 text-xs text-dark-text-muted/70">
                          {t("roadmap.columnEmpty")}
                        </div>
                      ) : (
                        <AnimatePresence initial={false}>
                          {column.items.map((item, i) => (
                            <RoadmapCard
                              key={item.id}
                              item={item}
                              index={i}
                              color={meta.color}
                              onOpen={openDetail}
                              locale={locale}
                              t={t}
                            />
                          ))}
                        </AnimatePresence>
                      )}
                    </div>
                  </motion.div>
                );
              })}
            </div>
          )
        )}
      </div>

      <IssueDetailModal issueNumber={detailIssue} onClose={() => setDetailIssue(null)} />
    </section>
  );
}
