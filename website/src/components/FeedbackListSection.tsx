import { useState, useEffect, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  ListFilter,
  MessageSquare,
  CircleDot,
  CircleCheck,
  CircleSlash,
  Copy,
  Clock,
  Loader2,
  AlertCircle,
  ChevronLeft,
  ChevronRight,
  Lightbulb,
  Bug,
  MessageCircle,
  Tag,
} from "lucide-react";
import { useLocale } from "@/lib/i18n";

// ── 类型 ──

interface IssueLabel {
  name: string;
  color: string;
}

type CloseReason = "completed" | "not_planned" | "duplicate" | null;

interface IssueItem {
  number: number;
  title: string;
  state: string;
  close_reason: CloseReason;
  labels: IssueLabel[];
  created_at: string;
  updated_at: string;
  comments: number;
  user: { login: string; avatar_url: string };
  body_preview: string;
}

interface IssuesResponse {
  issues: IssueItem[];
  page: number;
  per_page: number;
  has_more: boolean;
  total_shown: number;
}

type StateFilter = "all" | "open" | "closed";
type LabelFilter = "" | "enhancement" | "bug" | "feedback";

// ── 标签配置 ──

const LABEL_CONFIG: { value: LabelFilter; icon: typeof Lightbulb; colorClass: string }[] = [
  { value: "", icon: Tag, colorClass: "text-dark-text-secondary" },
  { value: "enhancement", icon: Lightbulb, colorClass: "text-warning" },
  { value: "bug", icon: Bug, colorClass: "text-danger" },
  { value: "feedback", icon: MessageCircle, colorClass: "text-brand-cyan" },
];

// ── 工具函数 ──

function formatTimeAgo(dateStr: string, locale: string): string {
  const now = Date.now();
  const date = new Date(dateStr).getTime();
  const diff = now - date;
  const seconds = Math.floor(diff / 1000);
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);
  const days = Math.floor(hours / 24);
  const months = Math.floor(days / 30);

  const isZh = locale === "zh-CN";

  if (months > 0) return isZh ? `${months} 个月前` : `${months}mo ago`;
  if (days > 0) return isZh ? `${days} 天前` : `${days}d ago`;
  if (hours > 0) return isZh ? `${hours} 小时前` : `${hours}h ago`;
  if (minutes > 0) return isZh ? `${minutes} 分钟前` : `${minutes}m ago`;
  return isZh ? "刚刚" : "just now";
}

function getLabelDisplayName(name: string, t: (key: string) => string): string {
  switch (name) {
    case "enhancement": return t("fbList.label.enhancement");
    case "bug": return t("fbList.label.bug");
    case "feedback": return t("fbList.label.feedback");
    default: return name;
  }
}

function getLabelColor(label: IssueLabel): string {
  // 使用 GitHub 标签颜色
  if (label.color) {
    return `#${label.color}`;
  }
  switch (label.name) {
    case "enhancement": return "#f59e0b";
    case "bug": return "#ef4444";
    case "feedback": return "#06b6d4";
    default: return "#71717a";
  }
}

// ── 组件 ──

interface FeedbackListSectionProps {
  onIssueClick?: (issueNumber: number) => void;
}

export default function FeedbackListSection({ onIssueClick }: FeedbackListSectionProps) {
  const { t, locale } = useLocale();
  const [issues, setIssues] = useState<IssueItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [stateFilter, setStateFilter] = useState<StateFilter>("open");
  const [labelFilter, setLabelFilter] = useState<LabelFilter>("");
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  const [totalShown, setTotalShown] = useState(0);

  const PER_PAGE = 15;

  const fetchIssues = useCallback(async (state: StateFilter, label: LabelFilter, p: number) => {
    setLoading(true);
    setError("");

    try {
      const params = new URLSearchParams({
        state,
        page: String(p),
        per_page: String(PER_PAGE),
      });
      if (label) params.set("label", label);

      const res = await fetch(`/api/issues?${params}`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }

      const data: IssuesResponse = await res.json();
      setIssues(data.issues);
      setHasMore(data.has_more);
      setTotalShown(data.total_shown);
    } catch {
      setError(t("fbList.error"));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    fetchIssues(stateFilter, labelFilter, page);
  }, [stateFilter, labelFilter, page, fetchIssues]);

  const handleStateChange = (state: StateFilter) => {
    setStateFilter(state);
    setPage(1);
  };

  const handleLabelChange = (label: LabelFilter) => {
    setLabelFilter(label);
    setPage(1);
  };

  const totalPages = Math.ceil(totalShown / PER_PAGE);

  return (
    <section className="relative py-16 sm:py-20 overflow-hidden bg-dark-bg">
      <div className="mx-auto max-w-4xl px-4 sm:px-6 lg:px-8 relative z-10">
        {/* Header */}
        <motion.div
          className="text-center max-w-2xl mx-auto mb-10"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5 }}
        >
          <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-brand-blue/10 text-brand-blue border border-brand-blue/20 uppercase tracking-widest">
            <ListFilter className="w-3 h-3" />
            {t("fbList.badge")}
          </span>
          <h2 className="mt-6 text-2xl sm:text-3xl lg:text-4xl font-bold tracking-tight text-dark-text">
            {t("fbList.title")}
            <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("fbList.titleHighlight")}
            </span>
          </h2>
          <p className="mt-3 text-dark-text-secondary text-base">
            {t("fbList.subtitle")}
          </p>
        </motion.div>

        {/* Filters */}
        <motion.div
          className="mb-6 flex flex-col sm:flex-row gap-3 sm:items-center sm:justify-between"
          initial={{ opacity: 0, y: 10 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.4, delay: 0.1 }}
        >
          {/* State tabs */}
          <div className="flex items-center gap-1 p-1 rounded-lg bg-dark-surface1 border border-dark-border">
            {(["open", "closed", "all"] as StateFilter[]).map((state) => (
              <button
                key={state}
                onClick={() => handleStateChange(state)}
                className={`relative flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-all duration-200 cursor-pointer ${
                  stateFilter === state
                    ? "text-dark-text"
                    : "text-dark-text-secondary hover:text-dark-text-muted"
                }`}
              >
                {state === "open" && <CircleDot className="w-3 h-3 text-success" />}
                {state === "closed" && <CircleCheck className="w-3 h-3 text-purple-400" />}
                {t(`fbList.state.${state}`)}
                {stateFilter === state && (
                  <motion.div
                    layoutId="state-filter-bg"
                    className="absolute inset-0 rounded-md bg-dark-surface2 border border-dark-border -z-10"
                    transition={{ type: "spring", bounce: 0.15, duration: 0.4 }}
                  />
                )}
              </button>
            ))}
          </div>

          {/* Label filter */}
          <div className="flex items-center gap-1 p-1 rounded-lg bg-dark-surface1 border border-dark-border">
            {LABEL_CONFIG.map(({ value, icon: Icon, colorClass }) => (
              <button
                key={value}
                onClick={() => handleLabelChange(value)}
                className={`relative flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-all duration-200 cursor-pointer ${
                  labelFilter === value
                    ? "text-dark-text"
                    : "text-dark-text-secondary hover:text-dark-text-muted"
                }`}
              >
                <Icon className={`w-3 h-3 ${colorClass}`} />
                {t(`fbList.labelFilter.${value || "all"}`)}
                {labelFilter === value && (
                  <motion.div
                    layoutId="label-filter-bg"
                    className="absolute inset-0 rounded-md bg-dark-surface2 border border-dark-border -z-10"
                    transition={{ type: "spring", bounce: 0.15, duration: 0.4 }}
                  />
                )}
              </button>
            ))}
          </div>
        </motion.div>

        {/* Count */}
        {!loading && !error && (
          <div className="mb-4 text-xs text-dark-text-muted">
            {t("fbList.showing").replace("{count}", String(totalShown))}
          </div>
        )}

        {/* Issue List */}
        <div className="space-y-3">
          <AnimatePresence mode="wait">
            {loading ? (
              <motion.div
                key="loading"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="flex items-center justify-center py-16"
              >
                <Loader2 className="w-6 h-6 animate-spin text-dark-text-secondary" />
                <span className="ml-2 text-sm text-dark-text-secondary">{t("fbList.loading")}</span>
              </motion.div>
            ) : error ? (
              <motion.div
                key="error"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="flex items-center justify-center py-16 text-danger"
              >
                <AlertCircle className="w-5 h-5 mr-2" />
                <span className="text-sm">{error}</span>
              </motion.div>
            ) : issues.length === 0 ? (
              <motion.div
                key="empty"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                className="flex flex-col items-center justify-center py-16 text-dark-text-secondary"
              >
                <MessageSquare className="w-10 h-10 mb-3 opacity-30" />
                <span className="text-sm">{t("fbList.empty")}</span>
              </motion.div>
            ) : (
              <motion.div
                key={`list-${stateFilter}-${labelFilter}-${page}`}
                initial={{ opacity: 0, y: 10 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -10 }}
                transition={{ duration: 0.3 }}
              >
                {issues.map((issue, idx) => (
                  <motion.div
                    key={issue.number}
                    initial={{ opacity: 0, y: 10 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ duration: 0.3, delay: idx * 0.03 }}
                  >
                    <button
                      type="button"
                      onClick={() => onIssueClick?.(issue.number)}
                      className="w-full text-left group rounded-lg border border-dark-border bg-dark-surface1 p-4 sm:p-5 mb-3 hover:border-dark-text-muted/30 hover:bg-dark-surface2/50 transition-all duration-200 cursor-pointer"
                    >
                      <div className="flex items-start gap-3">
                        {/* State icon */}
                        <div className="mt-0.5 shrink-0" title={
                          issue.state === "open"
                            ? t("issueDetail.open")
                            : issue.close_reason === "not_planned"
                              ? t("issueDetail.notPlanned")
                              : issue.close_reason === "duplicate"
                                ? t("issueDetail.duplicate")
                                : t("issueDetail.completed")
                        }>
                          {issue.state === "open" ? (
                            <CircleDot className="w-4 h-4 text-success" />
                          ) : issue.close_reason === "not_planned" ? (
                            <CircleSlash className="w-4 h-4 text-zinc-400" />
                          ) : issue.close_reason === "duplicate" ? (
                            <Copy className="w-4 h-4 text-zinc-400" />
                          ) : (
                            <CircleCheck className="w-4 h-4 text-purple-400" />
                          )}
                        </div>

                        {/* Content */}
                        <div className="flex-1 min-w-0">
                          {/* Title + labels */}
                          <div className="flex flex-wrap items-center gap-2 mb-1.5">
                            <h3 className="text-sm font-semibold text-dark-text group-hover:text-brand-sky transition-colors truncate max-w-full">
                              {issue.title}
                            </h3>
                            {issue.labels.map((label) => (
                              <span
                                key={label.name}
                                className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-medium border shrink-0"
                                style={{
                                  color: getLabelColor(label),
                                  borderColor: `${getLabelColor(label)}40`,
                                  backgroundColor: `${getLabelColor(label)}15`,
                                }}
                              >
                                {getLabelDisplayName(label.name, t)}
                              </span>
                            ))}
                          </div>

                          {/* Preview */}
                          {issue.body_preview && (
                            <p className="text-xs text-dark-text-secondary line-clamp-2 mb-2">
                              {issue.body_preview}
                            </p>
                          )}

                          {/* Meta */}
                          <div className="flex items-center gap-3 text-[11px] text-dark-text-muted">
                            <span>#{issue.number}</span>
                            <span className="flex items-center gap-1">
                              <Clock className="w-3 h-3" />
                              {formatTimeAgo(issue.created_at, locale)}
                            </span>
                            {issue.comments > 0 && (
                              <span className="flex items-center gap-1">
                                <MessageSquare className="w-3 h-3" />
                                {issue.comments}
                              </span>
                            )}
                          </div>
                        </div>
                      </div>
                    </button>
                  </motion.div>
                ))}
              </motion.div>
            )}
          </AnimatePresence>
        </div>

        {/* Pagination */}
        {!loading && !error && totalPages > 1 && (
          <motion.div
            className="mt-6 flex items-center justify-center gap-2"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            transition={{ delay: 0.3 }}
          >
            <button
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page <= 1}
              className="flex items-center gap-1 px-3 py-1.5 rounded-md text-xs font-medium border border-dark-border bg-dark-surface1 text-dark-text-secondary hover:text-dark-text hover:bg-dark-surface2 disabled:opacity-30 disabled:cursor-not-allowed transition-all cursor-pointer"
            >
              <ChevronLeft className="w-3 h-3" />
              {t("fbList.prev")}
            </button>
            <span className="text-xs text-dark-text-muted px-2">
              {page} / {totalPages}
            </span>
            <button
              onClick={() => setPage((p) => p + 1)}
              disabled={!hasMore}
              className="flex items-center gap-1 px-3 py-1.5 rounded-md text-xs font-medium border border-dark-border bg-dark-surface1 text-dark-text-secondary hover:text-dark-text hover:bg-dark-surface2 disabled:opacity-30 disabled:cursor-not-allowed transition-all cursor-pointer"
            >
              {t("fbList.next")}
              <ChevronRight className="w-3 h-3" />
            </button>
          </motion.div>
        )}
      </div>
    </section>
  );
}
