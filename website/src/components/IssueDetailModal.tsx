import { useState, useEffect, useCallback, useRef } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  X,
  CircleDot,
  CircleCheck,
  CircleSlash,
  Copy,
  Clock,
  MessageSquare,
  Loader2,
  AlertCircle,
  ThumbsUp,
  Heart,
  Rocket,
  Eye,
  Calendar,
  Mail,
  User,
  Send,
  CheckCircle2,
} from "lucide-react";
import { useLocale } from "@/lib/i18n";

// ── 类型 ──

interface IssueLabel {
  name: string;
  color: string;
}

interface Reactions {
  "+1": number;
  "-1": number;
  laugh: number;
  hooray: number;
  confused: number;
  heart: number;
  rocket: number;
  eyes: number;
}

interface ParsedMetadata {
  type: string | null;
  contact: string | null;
  source: string | null;
  submitted_at: string | null;
}

type CloseReason = "completed" | "not_planned" | "duplicate" | null;

interface IssueData {
  number: number;
  title: string;
  state: string;
  close_reason: CloseReason;
  labels: IssueLabel[];
  created_at: string;
  updated_at: string;
  comments_count: number;
  user: { login: string; avatar_url: string };
  description: string;
  body_raw: string;
  metadata: ParsedMetadata | null;
  is_feedback_format: boolean;
  reactions: Reactions;
}

interface CommentData {
  id: number;
  user: { login: string; avatar_url: string };
  body: string;
  created_at: string;
  updated_at: string;
  reactions: Reactions;
}

interface IssueDetailResponse {
  issue: IssueData;
  comments: CommentData[];
}

// ── Markdown 渲染 ──

function renderMarkdown(md: string): string {
  return md
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(
      /`([^`]+)`/g,
      '<code class="px-1.5 py-0.5 rounded bg-dark-surface3 text-brand-sky text-xs font-mono">$1</code>',
    )
    .replace(
      /^### (.+)$/gm,
      '<h4 class="text-sm font-semibold text-dark-text mt-4 mb-1.5">$1</h4>',
    )
    .replace(
      /^## (.+)$/gm,
      '<h3 class="text-base font-semibold text-dark-text mt-5 mb-2">$1</h3>',
    )
    .replace(
      /^- (.+)$/gm,
      '<li class="ml-4 pl-1.5 text-sm text-dark-text-secondary leading-relaxed list-disc">$1</li>',
    )
    .replace(
      /((?:<li[^>]*>.*<\/li>\n?)+)/g,
      '<ul class="space-y-1 my-2">$1</ul>',
    )
    .replace(
      /^(?!<[hul])((?!<\/)[^\n]+)$/gm,
      '<p class="text-sm text-dark-text-secondary leading-relaxed">$1</p>',
    )
    .replace(/\n{3,}/g, "\n\n");
}

// ── 工具函数 ──

function formatDate(dateStr: string, locale: string): string {
  const date = new Date(dateStr);
  return date.toLocaleDateString(locale === "zh-CN" ? "zh-CN" : "en-US", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

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

function getLabelColor(label: IssueLabel): string {
  if (label.color) return `#${label.color}`;
  switch (label.name) {
    case "enhancement": return "#f59e0b";
    case "bug": return "#ef4444";
    case "feedback": return "#06b6d4";
    default: return "#71717a";
  }
}

function getLabelDisplayName(name: string, t: (key: string) => string): string {
  switch (name) {
    case "enhancement": return t("fbList.label.enhancement");
    case "bug": return t("fbList.label.bug");
    case "feedback": return t("fbList.label.feedback");
    default: return name;
  }
}

// ── 子组件 ──

/** 状态徽章：open / completed / not_planned / duplicate */
function StateBadge({ state, closeReason, t }: { state: string; closeReason: CloseReason; t: (key: string) => string }) {
  if (state === "open") {
    return (
      <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-success/15 text-success border border-success/30 shrink-0">
        <CircleDot className="w-3 h-3" />
        {t("issueDetail.open")}
      </span>
    );
  }

  switch (closeReason) {
    case "not_planned":
      return (
        <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-zinc-500/15 text-zinc-400 border border-zinc-500/30 shrink-0">
          <CircleSlash className="w-3 h-3" />
          {t("issueDetail.notPlanned")}
        </span>
      );
    case "duplicate":
      return (
        <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-zinc-500/15 text-zinc-400 border border-zinc-500/30 shrink-0">
          <Copy className="w-3 h-3" />
          {t("issueDetail.duplicate")}
        </span>
      );
    default: // completed
      return (
        <span className="inline-flex items-center gap-1 px-2.5 py-0.5 rounded-full text-xs font-medium bg-purple-500/15 text-purple-400 border border-purple-500/30 shrink-0">
          <CircleCheck className="w-3 h-3" />
          {t("issueDetail.completed")}
        </span>
      );
  }
}

/** Reactions 行 */
function ReactionsBar({ reactions }: { reactions: Reactions }) {
  const items: { emoji: React.ReactNode; count: number; key: string }[] = [
    { emoji: <ThumbsUp className="w-3 h-3" />, count: reactions["+1"], key: "+1" },
    { emoji: <Heart className="w-3 h-3" />, count: reactions.heart, key: "heart" },
    { emoji: <Rocket className="w-3 h-3" />, count: reactions.rocket, key: "rocket" },
    { emoji: <Eye className="w-3 h-3" />, count: reactions.eyes, key: "eyes" },
    { emoji: "🎉", count: reactions.hooray, key: "hooray" },
    { emoji: "😄", count: reactions.laugh, key: "laugh" },
    { emoji: "😕", count: reactions.confused, key: "confused" },
    { emoji: "👎", count: reactions["-1"], key: "-1" },
  ];

  const visible = items.filter((item) => item.count > 0);
  if (visible.length === 0) return null;

  return (
    <div className="flex items-center gap-2 px-4 pb-3">
      {visible.map((item) => (
        <span
          key={item.key}
          className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] bg-dark-surface3 text-dark-text-secondary border border-dark-border"
        >
          {item.emoji}
          <span>{item.count}</span>
        </span>
      ))}
    </div>
  );
}

/** 结构化元数据展示（type / contact / submitted_at） */
function MetadataBar({ metadata, locale, t }: { metadata: ParsedMetadata; locale: string; t: (key: string) => string }) {
  const typeLabel = metadata.type
    ? t(`issueDetail.meta.type.${metadata.type}`)
    : null;

  const submittedLabel = metadata.submitted_at
    ? formatDate(metadata.submitted_at, locale)
    : null;

  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 px-4 py-2.5 border-t border-dark-border text-[11px] text-dark-text-muted">
      {typeLabel && (
        <span className="flex items-center gap-1">
          <span className="text-dark-text-secondary font-medium">{t("issueDetail.meta.typeLabel")}</span>
          {typeLabel}
        </span>
      )}
      {metadata.contact && (
        <span className="flex items-center gap-1">
          <Mail className="w-3 h-3" />
          {metadata.contact}
        </span>
      )}
      {submittedLabel && (
        <span className="flex items-center gap-1">
          <Calendar className="w-3 h-3" />
          {submittedLabel}
        </span>
      )}
    </div>
  );
}

// ── 主组件 ──

interface IssueDetailModalProps {
  issueNumber: number | null;
  onClose: () => void;
}

export default function IssueDetailModal({ issueNumber, onClose }: IssueDetailModalProps) {
  const { t, locale } = useLocale();
  const [data, setData] = useState<IssueDetailResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const overlayRef = useRef<HTMLDivElement>(null);

  const [replyBody, setReplyBody] = useState("");
  const [replyStatus, setReplyStatus] = useState<"idle" | "sending" | "success" | "error">("idle");
  const [replyError, setReplyError] = useState("");

  const fetchDetail = useCallback(async (num: number) => {
    setLoading(true);
    setError("");
    setData(null);

    try {
      const res = await fetch(`/api/issues/${num}`);
      if (!res.ok) {
        if (res.status === 404) {
          setError(t("issueDetail.notFound"));
          return;
        }
        throw new Error(`HTTP ${res.status}`);
      }
      const json: IssueDetailResponse = await res.json();
      setData(json);
    } catch {
      setError(t("issueDetail.error"));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    if (issueNumber !== null) {
      fetchDetail(issueNumber);
    }
    setReplyBody("");
    setReplyStatus("idle");
    setReplyError("");
  }, [issueNumber, fetchDetail]);

  const handleReplySubmit = useCallback(async () => {
    if (!replyBody.trim() || !issueNumber || replyStatus === "sending") return;

    setReplyStatus("sending");
    setReplyError("");

    try {
      const res = await fetch(`/api/issues/${issueNumber}/comments`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ body: replyBody.trim() }),
      });

      if (res.status === 429) {
        setReplyStatus("error");
        setReplyError(t("issueDetail.replyRateLimited"));
        return;
      }

      if (!res.ok) {
        setReplyStatus("error");
        setReplyError(t("issueDetail.replyError"));
        return;
      }

      setReplyStatus("success");
      setReplyBody("");
      fetchDetail(issueNumber);
      setTimeout(() => setReplyStatus("idle"), 3000);
    } catch {
      setReplyStatus("error");
      setReplyError(t("issueDetail.replyError"));
    }
  }, [replyBody, issueNumber, replyStatus, t, fetchDetail]);

  useEffect(() => {
    if (issueNumber === null) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [issueNumber, onClose]);

  // 锁定 body 滚动
  useEffect(() => {
    if (issueNumber !== null) {
      document.body.style.overflow = "hidden";
    } else {
      document.body.style.overflow = "";
    }
    return () => {
      document.body.style.overflow = "";
    };
  }, [issueNumber]);

  const handleOverlayClick = (e: React.MouseEvent) => {
    if (e.target === overlayRef.current) onClose();
  };

  // 决定渲染 body 内容：feedback 格式用 description，否则用 body_raw
  const bodyContent = data
    ? data.issue.is_feedback_format
      ? data.issue.description
      : data.issue.body_raw
    : "";

  return (
    <AnimatePresence>
      {issueNumber !== null && (
        <motion.div
          ref={overlayRef}
          className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] sm:pt-[8vh] px-4"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 }}
          onClick={handleOverlayClick}
        >
          {/* Backdrop */}
          <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />

          {/* Modal */}
          <motion.div
            className="relative w-full max-w-2xl max-h-[85vh] rounded-xl border border-dark-border bg-dark-bg overflow-hidden flex flex-col"
            initial={{ opacity: 0, y: 30, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 20, scale: 0.97 }}
            transition={{ type: "spring", bounce: 0.15, duration: 0.4 }}
          >
            {/* Header bar */}
            <div className="flex items-center justify-between px-5 py-3 border-b border-dark-border bg-dark-surface1 shrink-0">
              <div className="flex items-center gap-2 text-sm text-dark-text-muted min-w-0">
                <MessageSquare className="w-4 h-4 shrink-0" />
                <span className="truncate">
                  {data ? `#${data.issue.number}` : `#${issueNumber}`}
                </span>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={onClose}
                  className="p-1.5 rounded-md text-dark-text-muted hover:text-dark-text hover:bg-dark-surface2 transition-colors cursor-pointer"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>
            </div>

            {/* Content */}
            <div className="flex-1 overflow-y-auto px-5 py-5">
              {loading && (
                <div className="flex items-center justify-center py-16">
                  <Loader2 className="w-6 h-6 animate-spin text-dark-text-secondary" />
                </div>
              )}

              {error && !loading && (
                <div className="flex items-center justify-center py-16 text-danger">
                  <AlertCircle className="w-5 h-5 mr-2" />
                  <span className="text-sm">{error}</span>
                </div>
              )}

              {data && !loading && (
                <>
                  {/* Issue header */}
                  <div className="mb-6">
                    <div className="flex items-start gap-2 mb-3">
                      <div className="shrink-0 mt-0.5">
                        <StateBadge state={data.issue.state} closeReason={data.issue.close_reason} t={t} />
                      </div>
                      <h2 className="text-lg font-bold text-dark-text leading-snug">
                        {data.issue.title}
                      </h2>
                    </div>

                    {/* Labels */}
                    {data.issue.labels.length > 0 && (
                      <div className="flex flex-wrap gap-1.5 mb-3">
                        {data.issue.labels.map((label) => (
                          <span
                            key={label.name}
                            className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-medium border"
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
                    )}

                    {/* Meta */}
                    <div className="flex items-center gap-3 text-[11px] text-dark-text-muted">
                      <span className="flex items-center gap-1">
                        <Clock className="w-3 h-3" />
                        {formatDate(data.issue.created_at, locale)}
                      </span>
                      {data.issue.comments_count > 0 && (
                        <span className="flex items-center gap-1">
                          <MessageSquare className="w-3 h-3" />
                          {data.issue.comments_count} {t("issueDetail.replies")}
                        </span>
                      )}
                    </div>
                  </div>

                  {/* Issue body */}
                  <div className="rounded-lg border border-dark-border bg-dark-surface1 overflow-hidden mb-6">
                    {/* Author header */}
                    <div className="flex items-center gap-2 px-4 py-2.5 bg-dark-surface2/50 border-b border-dark-border">
                      <div className="w-5 h-5 rounded-full bg-dark-surface3 flex items-center justify-center">
                        <User className="w-3 h-3 text-dark-text-muted" />
                      </div>
                      <span className="text-xs font-medium text-dark-text">
                        {t("issueDetail.anonymous")}
                      </span>
                      <span className="text-[11px] text-dark-text-muted">
                        {formatTimeAgo(data.issue.created_at, locale)}
                      </span>
                    </div>

                    {/* Body content (parsed description or raw) */}
                    <div
                      className="px-4 py-3 changelog-body"
                      dangerouslySetInnerHTML={{
                        __html: renderMarkdown(bodyContent),
                      }}
                    />

                    {/* Structured metadata (feedback format only) */}
                    {data.issue.is_feedback_format && data.issue.metadata && (
                      <MetadataBar metadata={data.issue.metadata} locale={locale} t={t} />
                    )}

                    <ReactionsBar reactions={data.issue.reactions} />
                  </div>

                  {/* Comments */}
                  {data.comments.length > 0 && (
                    <div className="space-y-4">
                      <h3 className="text-sm font-semibold text-dark-text flex items-center gap-1.5">
                        <MessageSquare className="w-4 h-4" />
                        {t("issueDetail.commentsTitle").replace("{count}", String(data.comments.length))}
                      </h3>

                      {data.comments.map((comment) => {
                        const isDeveloper = comment.user.login !== data.issue.user.login;
                        return (
                        <motion.div
                          key={comment.id}
                          initial={{ opacity: 0, y: 8 }}
                          animate={{ opacity: 1, y: 0 }}
                          className="rounded-lg border border-dark-border bg-dark-surface1 overflow-hidden"
                        >
                          {/* Comment header */}
                          <div className="flex items-center gap-2 px-4 py-2.5 bg-dark-surface2/50 border-b border-dark-border">
                            <div className={`w-5 h-5 rounded-full flex items-center justify-center ${isDeveloper ? "bg-brand-blue/20" : "bg-dark-surface3"}`}>
                              <User className={`w-3 h-3 ${isDeveloper ? "text-brand-blue" : "text-dark-text-muted"}`} />
                            </div>
                            <span className="text-xs font-medium text-dark-text">
                              {isDeveloper ? t("issueDetail.developer") : t("issueDetail.anonymous")}
                            </span>
                            <span className="text-[11px] text-dark-text-muted">
                              {formatTimeAgo(comment.created_at, locale)}
                            </span>
                          </div>

                          {/* Comment body */}
                          <div
                            className="px-4 py-3 changelog-body"
                            dangerouslySetInnerHTML={{
                              __html: renderMarkdown(comment.body),
                            }}
                          />

                          <ReactionsBar reactions={comment.reactions} />
                        </motion.div>
                        );
                      })}
                    </div>
                  )}

                  {/* No comments */}
                  {data.comments.length === 0 && (
                    <div className="text-center py-6 text-dark-text-muted text-xs">
                      {t("issueDetail.noComments")}
                    </div>
                  )}
                </>
              )}
            </div>

            {/* Reply box */}
            {data && !loading && (
              <div className="shrink-0 border-t border-dark-border bg-dark-surface1 px-5 py-4">
                <div className="flex gap-3">
                  <textarea
                    rows={2}
                    maxLength={2000}
                    value={replyBody}
                    onChange={(e) => setReplyBody(e.target.value)}
                    placeholder={t("issueDetail.replyPlaceholder")}
                    className="flex-1 rounded-lg border border-dark-border bg-dark-surface2 px-3 py-2 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-blue/50 focus:ring-1 focus:ring-brand-blue/30 transition-colors resize-none"
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                        e.preventDefault();
                        handleReplySubmit();
                      }
                    }}
                  />
                  <button
                    type="button"
                    onClick={handleReplySubmit}
                    disabled={!replyBody.trim() || replyStatus === "sending"}
                    className={`self-end shrink-0 inline-flex items-center gap-1.5 rounded-lg px-4 py-2 text-sm font-medium transition-all duration-200 ${
                      replyBody.trim() && replyStatus !== "sending"
                        ? "bg-brand-blue text-white hover:bg-brand-blue/90 shadow-md shadow-brand-blue/20 cursor-pointer"
                        : "bg-dark-surface3 text-dark-text-muted cursor-not-allowed"
                    }`}
                  >
                    {replyStatus === "sending" ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : (
                      <Send className="w-4 h-4" />
                    )}
                    {replyStatus === "sending"
                      ? t("issueDetail.replySending")
                      : t("issueDetail.replySend")}
                  </button>
                </div>

                <div className="flex items-center justify-between mt-2">
                  <span className="text-[10px] text-dark-text-muted">
                    {t("issueDetail.replyCharCount", { count: String(replyBody.length) })}
                  </span>

                  <AnimatePresence mode="wait">
                    {replyStatus === "success" && (
                      <motion.span
                        key="reply-ok"
                        initial={{ opacity: 0, x: 10 }}
                        animate={{ opacity: 1, x: 0 }}
                        exit={{ opacity: 0 }}
                        className="flex items-center gap-1 text-xs text-success"
                      >
                        <CheckCircle2 className="w-3 h-3" />
                        {t("issueDetail.replySuccess")}
                      </motion.span>
                    )}
                    {replyStatus === "error" && (
                      <motion.span
                        key="reply-err"
                        initial={{ opacity: 0, x: 10 }}
                        animate={{ opacity: 1, x: 0 }}
                        exit={{ opacity: 0 }}
                        className="flex items-center gap-1 text-xs text-danger"
                      >
                        <AlertCircle className="w-3 h-3" />
                        {replyError}
                      </motion.span>
                    )}
                  </AnimatePresence>
                </div>
              </div>
            )}
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
