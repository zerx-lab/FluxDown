import { useState, useEffect, useCallback, useRef } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { useLocale } from "@/lib/i18n";
import type { Messages } from "@/lib/locales";

/** Voting has ended — disable vote buttons & hide upload section */
const VOTE_ENDED = true;

interface LogoItem {
  id: string;
  filename: string;
  submitterName: string;
  description: string;
  uploadedAt: string;
  votes: number;
  isBuiltin: boolean;
  imageUrl?: string;
}

const STORAGE_PREFIX = "fluxdown-logo-voted-";
const MAX_FILE_SIZE = 10 * 1024 * 1024; // 10MB
const ALLOWED_TYPES = [
  "image/png",
  "image/jpeg",
  "image/svg+xml",
  "image/webp",
];

function getVotedKey(id: string) {
  return `${STORAGE_PREFIX}${id}`;
}

function readVoted(id: string): boolean {
  try {
    return !!localStorage.getItem(getVotedKey(id));
  } catch {
    return false;
  }
}

function writeVoted(id: string, voted: boolean) {
  try {
    if (voted) {
      localStorage.setItem(getVotedKey(id), "1");
    } else {
      localStorage.removeItem(getVotedKey(id));
    }
  } catch {
    // localStorage unavailable
  }
}

// Checkerboard background for transparent images
const CHECKERBOARD_STYLE: React.CSSProperties = {
  backgroundImage: "repeating-conic-gradient(#1a1a1a 0% 25%, #252525 0% 50%)",
  backgroundSize: "20px 20px",
};

// SVG placeholder shown when an image fails to load
function LogoPlaceholder() {
  return (
    <svg
      width="64"
      height="64"
      viewBox="0 0 64 64"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className="opacity-30"
    >
      <rect width="64" height="64" rx="12" fill="#334155" />
      <path
        d="M20 44 L32 20 L44 44"
        stroke="#94a3b8"
        strokeWidth="3"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M24 36 L40 36"
        stroke="#94a3b8"
        strokeWidth="3"
        strokeLinecap="round"
      />
      <circle cx="32" cy="14" r="3" fill="#38bdf8" />
    </svg>
  );
}

// Rank badge: 🥇🥈🥉 for top 3, number for 4–10
function RankBadge({ rank }: { rank: number }) {
  if (rank === 1) {
    return (
      <div
        className="absolute top-2 left-2 z-10 text-xl leading-none select-none"
        title="Rank 1"
      >
        🥇
      </div>
    );
  }
  if (rank === 2) {
    return (
      <div
        className="absolute top-2 left-2 z-10 text-xl leading-none select-none"
        title="Rank 2"
      >
        🥈
      </div>
    );
  }
  if (rank === 3) {
    return (
      <div
        className="absolute top-2 left-2 z-10 text-xl leading-none select-none"
        title="Rank 3"
      >
        🥉
      </div>
    );
  }
  return (
    <div className="absolute top-2 left-2 z-10 flex items-center justify-center w-6 h-6 rounded-full bg-dark-surface2 border border-dark-border text-[11px] font-bold text-dark-text-secondary select-none">
      {rank}
    </div>
  );
}

// Spinner icon
function Spinner({ className = "w-4 h-4" }: { className?: string }) {
  return (
    <svg
      className={`${className} animate-spin`}
      viewBox="0 0 24 24"
      fill="none"
    >
      <circle
        className="opacity-25"
        cx="12"
        cy="12"
        r="10"
        stroke="currentColor"
        strokeWidth="4"
      />
      <path
        className="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
      />
    </svg>
  );
}

// Skeleton card for loading state
function SkeletonCard() {
  return (
    <div className="rounded-xl border border-dark-border bg-dark-surface1 overflow-hidden animate-pulse">
      <div className="h-40 bg-dark-surface2" />
      <div className="p-4 space-y-2">
        <div className="h-3 w-2/3 rounded bg-dark-surface2" />
        <div className="h-3 w-1/2 rounded bg-dark-surface2" />
        <div className="mt-3 h-8 rounded bg-dark-surface2" />
      </div>
    </div>
  );
}

interface LogoCardProps {
  logo: LogoItem;
  rank: number | null;
  isVoted: boolean;
  isVoting: boolean;
  voteEnded?: boolean;
  onVote: (id: string, currentlyVoted: boolean) => void;
  t: (key: keyof Messages, params?: Record<string, string>) => string;
}

function LogoCard({
  logo,
  rank,
  isVoted,
  isVoting,
  voteEnded,
  onVote,
  t,
}: LogoCardProps) {
  const [imgError, setImgError] = useState(false);

  const imageUrl = logo.isBuiltin
    ? `/logos/${logo.filename}`
    : (logo.imageUrl ?? null);

  const isTop1 = rank === 1;

  const displayName = logo.submitterName || t("logoVote.anonymous");
  const isBuiltin = logo.isBuiltin;

  return (
    <motion.div
      initial={{ opacity: 0, y: 16 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.35 }}
      className={`group relative rounded-xl border overflow-hidden transition-all duration-300 bg-dark-surface1
        hover:-translate-y-1 hover:shadow-xl hover:shadow-black/20
        ${
          isTop1
            ? "border-brand-sky/60 ring-1 ring-brand-sky/25 hover:border-brand-sky/80"
            : "border-dark-border hover:border-dark-text-muted"
        }
      `}
    >
      {/* Top-1 glow */}
      {isTop1 && (
        <div className="absolute inset-0 bg-linear-to-b from-brand-sky/8 to-transparent pointer-events-none" />
      )}

      {/* Rank badge */}
      {rank !== null && <RankBadge rank={rank} />}

      {/* Built-in / Community badge */}
      <div className="absolute top-2 right-2 z-10">
        <span
          className={`inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium border
            ${
              isBuiltin
                ? "bg-brand-sky/15 border-brand-sky/30 text-brand-sky"
                : "bg-dark-surface2 border-dark-border text-dark-text-muted"
            }
          `}
        >
          {isBuiltin ? t("logoVote.builtin") : t("logoVote.community")}
        </span>
      </div>

      {/* Logo image */}
      <div
        className="relative flex items-center justify-center h-40 overflow-hidden"
        style={CHECKERBOARD_STYLE}
      >
        {imageUrl && !imgError ? (
          <img
            src={imageUrl}
            alt={logo.filename}
            className="max-h-36 max-w-[90%] object-contain transition-transform duration-300 group-hover:scale-105"
            onError={() => setImgError(true)}
          />
        ) : (
          <LogoPlaceholder />
        )}
      </div>

      {/* Meta */}
      <div className="p-4">
        <div className="flex items-center gap-1.5 mb-1">
          <span className="text-xs text-dark-text-muted">
            {t("logoVote.uploadedBy", { name: displayName })}
          </span>
        </div>

        {logo.description && (
          <p className="text-xs text-dark-text-secondary leading-relaxed line-clamp-2 mb-3">
            {logo.description}
          </p>
        )}

        {/* Vote button */}
        <div className="flex items-center justify-between gap-2 mt-3">
          <span className="text-sm font-semibold tabular-nums text-dark-text-secondary">
            {t("logoVote.votes", { n: String(logo.votes) })}
          </span>
          {!voteEnded && (
            <button
              onClick={() => onVote(logo.id, isVoted)}
              disabled={isVoting}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium border transition-all duration-200 disabled:opacity-60 disabled:cursor-not-allowed
                ${
                  isVoted
                    ? "bg-brand-blue/20 border-brand-blue/50 text-brand-blue hover:bg-brand-blue/30"
                    : "bg-transparent border-dark-border text-dark-text-secondary hover:border-dark-text-muted hover:text-dark-text"
                }
              `}
            >
              {isVoting ? (
                <>
                  <Spinner className="w-3 h-3" />
                  <span>{t("logoVote.voting")}</span>
                </>
              ) : isVoted ? (
                <>
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="M22 11.08V12a10 10 0 1 1-5.93-9.14" />
                    <polyline points="22 4 12 14.01 9 11.01" />
                  </svg>
                  <span>{t("logoVote.unvote")}</span>
                </>
              ) : (
                <>
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="M14 9V5a3 3 0 0 0-3-3l-4 9v11h11.28a2 2 0 0 0 2-1.7l1.38-9a2 2 0 0 0-2-2.3H14z" />
                    <path d="M7 22H4a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2h3" />
                  </svg>
                  <span>{t("logoVote.vote")}</span>
                </>
              )}
            </button>
          )}
        </div>
      </div>
    </motion.div>
  );
}

export default function LogoVotePage() {
  const { t } = useLocale();

  // Logo list state
  const [logos, setLogos] = useState<LogoItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);

  // Per-logo voted state (id → voted)
  const [votedMap, setVotedMap] = useState<Record<string, boolean>>({});
  // Per-logo voting-in-progress state
  const [votingMap, setVotingMap] = useState<Record<string, boolean>>({});

  // Status message
  const [statusMsg, setStatusMsg] = useState<{
    text: string;
    type: "success" | "error";
  } | null>(null);

  // Upload form state
  const [dragActive, setDragActive] = useState(false);
  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [submitName, setSubmitName] = useState("");
  const [submitDesc, setSubmitDesc] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitStatus, setSubmitStatus] = useState<{
    text: string;
    type: "success" | "error";
  } | null>(null);

  const fileInputRef = useRef<HTMLInputElement>(null);

  // Load logos
  const fetchLogos = useCallback(async (showLoading = false) => {
    setLoadError(false);
    if (showLoading) setLoading(true);
    try {
      const res = await fetch("/api/logo-vote");
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      console.log("[LogoVotePage] raw response:", json);
      const data: LogoItem[] = Array.isArray(json)
        ? json
        : Array.isArray(json?.logos)
          ? json.logos
          : [];
      console.log("[LogoVotePage] parsed logos:", data.length);
      setLogos(data);

      // Init voted state from localStorage
      const map: Record<string, boolean> = {};
      for (const item of data) {
        map[item.id] = readVoted(item.id);
      }
      setVotedMap(map);
    } catch (err) {
      console.error("[LogoVotePage] fetchLogos error:", err);
      setLoadError(true);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchLogos(true);
  }, [fetchLogos]);

  // Show status message then auto-hide
  const showStatus = useCallback(
    (text: string, type: "success" | "error", target: "vote" | "submit") => {
      if (target === "vote") {
        setStatusMsg({ text, type });
        setTimeout(() => setStatusMsg(null), 3000);
      } else {
        setSubmitStatus({ text, type });
        setTimeout(() => setSubmitStatus(null), 4000);
      }
    },
    [],
  );

  // Handle vote / unvote
  const handleVote = useCallback(
    async (id: string, currentlyVoted: boolean) => {
      if (votingMap[id]) return;

      setVotingMap((prev) => ({ ...prev, [id]: true }));
      setStatusMsg(null);

      try {
        const res = await fetch("/api/logo-vote", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            logoId: id,
            action: currentlyVoted ? "unvote" : "vote",
          }),
        });

        if (res.status === 429) {
          showStatus(t("logoVote.rateLimited"), "error", "vote");
          return;
        }

        if (!res.ok) {
          showStatus(t("logoVote.voteError"), "error", "vote");
          return;
        }

        const newVoted = !currentlyVoted;
        setVotedMap((prev) => ({ ...prev, [id]: newVoted }));
        writeVoted(id, newVoted);

        setLogos((prev) =>
          prev.map((item) =>
            item.id === id
              ? { ...item, votes: item.votes + (newVoted ? 1 : -1) }
              : item,
          ),
        );

        showStatus(
          newVoted ? t("logoVote.voteSuccess") : t("logoVote.unvoteSuccess"),
          "success",
          "vote",
        );
      } catch {
        showStatus(t("logoVote.voteError"), "error", "vote");
      } finally {
        setVotingMap((prev) => ({ ...prev, [id]: false }));
      }
    },
    [votingMap, showStatus, t],
  );

  // File validation
  const validateFile = useCallback(
    (file: File): string | null => {
      if (!ALLOWED_TYPES.includes(file.type))
        return t("logoVote.fileInvalidType");
      if (file.size > MAX_FILE_SIZE) return t("logoVote.fileTooLarge");
      return null;
    },
    [t],
  );

  // Set selected file + generate preview
  const applyFile = useCallback(
    (file: File) => {
      const err = validateFile(file);
      if (err) {
        showStatus(err, "error", "submit");
        return;
      }
      // Revoke previous object URL
      if (previewUrl) URL.revokeObjectURL(previewUrl);
      setSelectedFile(file);
      setPreviewUrl(URL.createObjectURL(file));
      setSubmitStatus(null);
    },
    [validateFile, showStatus, previewUrl],
  );

  // Drag events
  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDragActive(true);
  };
  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDragActive(false);
  };
  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setDragActive(false);
    const file = e.dataTransfer.files?.[0];
    if (file) applyFile(file);
  };

  const handleFileInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (file) applyFile(file);
    // Reset input so same file can be re-selected
    e.target.value = "";
  };

  // Submit logo
  const handleSubmit = useCallback(async () => {
    if (!selectedFile || submitting) return;

    setSubmitting(true);
    setSubmitStatus(null);

    try {
      const formData = new FormData();
      formData.append("file", selectedFile);
      if (submitName.trim())
        formData.append("submitterName", submitName.trim());
      if (submitDesc.trim()) formData.append("description", submitDesc.trim());

      const res = await fetch("/api/logo-submit", {
        method: "POST",
        body: formData,
      });

      if (res.status === 429) {
        showStatus(t("logoVote.submitRateLimited"), "error", "submit");
        return;
      }

      if (!res.ok) {
        showStatus(t("logoVote.submitError"), "error", "submit");
        return;
      }

      showStatus(t("logoVote.submitSuccess"), "success", "submit");

      // Reset form
      if (previewUrl) URL.revokeObjectURL(previewUrl);
      setSelectedFile(null);
      setPreviewUrl(null);
      setSubmitName("");
      setSubmitDesc("");

      // Refresh list
      await fetchLogos(true);
    } catch {
      showStatus(t("logoVote.submitError"), "error", "submit");
    } finally {
      setSubmitting(false);
    }
  }, [
    selectedFile,
    submitting,
    submitName,
    submitDesc,
    previewUrl,
    showStatus,
    fetchLogos,
    t,
  ]);

  // Compute rank for each logo (1-based, sorted by votes desc already from server)
  const getRank = (index: number): number | null => {
    // First 10 items get a rank badge
    if (index < 10) return index + 1;
    return null;
  };

  return (
    <section className="pt-24 sm:pt-32 pb-16 sm:pb-24">
      <div className="mx-auto max-w-6xl px-4 sm:px-6 lg:px-8">
        {/* ── Header ── */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.5 }}
          className="text-center mb-12 sm:mb-16"
        >
          <span
            className={`inline-flex items-center gap-2 rounded-full border px-4 py-1.5 text-xs font-medium backdrop-blur-sm mb-6 ${
              VOTE_ENDED
                ? "border-dark-text-muted/30 bg-dark-surface2/60 text-dark-text-muted"
                : "border-dark-border bg-dark-surface1/50 text-dark-text-secondary"
            }`}
          >
            {VOTE_ENDED ? (
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="text-dark-text-muted"
              >
                <path d="M22 11.08V12a10 10 0 1 1-5.93-9.14" />
                <polyline points="22 4 12 14.01 9 11.01" />
              </svg>
            ) : (
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="text-brand-sky"
              >
                <path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2z" />
              </svg>
            )}
            {VOTE_ENDED ? t("logoVote.endedBadge") : t("logoVote.badge")}
          </span>

          <h1 className="text-4xl sm:text-5xl font-bold tracking-tight leading-tight">
            <span className="text-dark-text">{t("logoVote.title")}</span>
            <span className="bg-linear-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("logoVote.titleHighlight")}
            </span>
          </h1>

          <p className="mt-4 text-base sm:text-lg text-dark-text-secondary max-w-2xl mx-auto leading-relaxed">
            {VOTE_ENDED ? t("logoVote.endedSubtitle") : t("logoVote.subtitle")}
          </p>
        </motion.div>

        {/* ── Loading ── */}
        {loading && (
          <div className="mb-12">
            <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-4 sm:gap-5">
              {Array.from({ length: 8 }).map((_, i) => (
                <SkeletonCard key={i} />
              ))}
            </div>
          </div>
        )}

        {/* ── Load error ── */}
        {loadError && (
          <div className="flex items-center justify-center py-20 mb-12">
            <span className="text-sm text-danger">
              {t("logoVote.loadError")}
            </span>
          </div>
        )}

        {/* ── Logo Grid ── */}
        {!loading && !loadError && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            transition={{ duration: 0.4, delay: 0.1 }}
            className="mb-12"
          >
            {logos.length === 0 ? (
              <div className="flex items-center justify-center py-20 text-sm text-dark-text-muted">
                {t("logoVote.noLogos")}
              </div>
            ) : (
              <div className="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-4 sm:gap-5">
                {logos.map((logo, index) => (
                  <LogoCard
                    key={logo.id}
                    logo={logo}
                    rank={getRank(index)}
                    isVoted={!!votedMap[logo.id]}
                    isVoting={!!votingMap[logo.id]}
                    voteEnded={VOTE_ENDED}
                    onVote={handleVote}
                    t={t}
                  />
                ))}
              </div>
            )}

            {/* Vote status message */}
            <AnimatePresence>
              {statusMsg && (
                <motion.div
                  key="vote-status"
                  initial={{ opacity: 0, y: 6 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: -6 }}
                  transition={{ duration: 0.2 }}
                  className="mt-6 text-center"
                >
                  <span
                    className={`text-sm font-medium ${
                      statusMsg.type === "success"
                        ? "text-success"
                        : "text-danger"
                    }`}
                  >
                    {statusMsg.text}
                  </span>
                </motion.div>
              )}
            </AnimatePresence>
          </motion.div>
        )}

        {/* ── Community Upload (hidden when vote ended) ── */}
        {!VOTE_ENDED && (
          <motion.div
            initial={{ opacity: 0, y: 24 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.5, delay: 0.2 }}
            className="rounded-2xl border border-dark-border bg-dark-surface1 overflow-hidden"
          >
            {/* Section header */}
            <div className="px-6 pt-6 pb-4 border-b border-dark-border">
              <h2 className="text-xl font-semibold text-dark-text mb-1">
                {t("logoVote.uploadTitle")}
              </h2>
              <p className="text-sm text-dark-text-secondary">
                {t("logoVote.uploadDesc")}
              </p>
            </div>

            <div className="p-6 space-y-5">
              {/* Drop zone */}
              <div
                onDragOver={handleDragOver}
                onDragLeave={handleDragLeave}
                onDrop={handleDrop}
                onClick={() => fileInputRef.current?.click()}
                className={`relative flex flex-col items-center justify-center gap-3 rounded-xl border-2 border-dashed transition-all duration-200 cursor-pointer min-h-40 select-none
                  ${
                    dragActive
                      ? "border-brand-sky bg-brand-sky/8 scale-[1.01]"
                      : selectedFile
                        ? "border-brand-blue/50 bg-brand-blue/5 hover:border-brand-blue"
                        : "border-dark-border bg-dark-surface2/50 hover:border-dark-text-muted hover:bg-dark-surface2"
                  }
                `}
              >
                <input
                  ref={fileInputRef}
                  type="file"
                  accept={ALLOWED_TYPES.join(",")}
                  className="hidden"
                  onChange={handleFileInputChange}
                />

                {selectedFile && previewUrl ? (
                  /* File preview */
                  <div className="flex flex-col sm:flex-row items-center gap-4 w-full px-4 py-2">
                    <div
                      className="flex items-center justify-center w-24 h-24 rounded-lg overflow-hidden shrink-0"
                      style={CHECKERBOARD_STYLE}
                    >
                      <img
                        src={previewUrl}
                        alt="preview"
                        className="max-w-full max-h-full object-contain"
                      />
                    </div>
                    <div className="text-center sm:text-left min-w-0">
                      <p className="text-sm font-medium text-dark-text truncate max-w-xs">
                        {selectedFile.name}
                      </p>
                      <p className="text-xs text-dark-text-muted mt-0.5">
                        {(selectedFile.size / 1024).toFixed(1)} KB ·{" "}
                        {selectedFile.type.replace("image/", "").toUpperCase()}
                      </p>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          if (previewUrl) URL.revokeObjectURL(previewUrl);
                          setSelectedFile(null);
                          setPreviewUrl(null);
                          setSubmitStatus(null);
                        }}
                        className="mt-2 text-xs text-danger hover:text-danger/80 transition-colors"
                      >
                        ✕ Remove
                      </button>
                    </div>
                  </div>
                ) : (
                  /* Empty state */
                  <>
                    <div
                      className={`flex items-center justify-center w-12 h-12 rounded-xl transition-colors ${dragActive ? "bg-brand-sky/20" : "bg-dark-surface2"}`}
                    >
                      <svg
                        width="24"
                        height="24"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.5"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        className={
                          dragActive ? "text-brand-sky" : "text-dark-text-muted"
                        }
                      >
                        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                        <polyline points="17 8 12 3 7 8" />
                        <line x1="12" y1="3" x2="12" y2="15" />
                      </svg>
                    </div>
                    <div className="text-center px-4">
                      <p
                        className={`text-sm font-medium transition-colors ${dragActive ? "text-brand-sky" : "text-dark-text-secondary"}`}
                      >
                        {dragActive
                          ? t("logoVote.dropHintActive")
                          : t("logoVote.dropHint")}
                      </p>
                      <p className="text-xs text-dark-text-muted mt-1">
                        {t("logoVote.fileLimit")}
                      </p>
                    </div>
                  </>
                )}
              </div>

              {/* Form fields */}
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                <div className="space-y-1.5">
                  <label className="block text-xs font-medium text-dark-text-secondary">
                    {t("logoVote.nameLabel")}
                  </label>
                  <input
                    type="text"
                    value={submitName}
                    onChange={(e) => setSubmitName(e.target.value)}
                    placeholder={t("logoVote.namePlaceholder")}
                    maxLength={50}
                    className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-3 py-2 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-sky/60 focus:ring-1 focus:ring-brand-sky/30 transition-colors"
                  />
                </div>
                <div className="space-y-1.5">
                  <label className="block text-xs font-medium text-dark-text-secondary">
                    {t("logoVote.descLabel")}
                  </label>
                  <input
                    type="text"
                    value={submitDesc}
                    onChange={(e) => setSubmitDesc(e.target.value)}
                    placeholder={t("logoVote.descPlaceholder")}
                    maxLength={120}
                    className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-3 py-2 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-sky/60 focus:ring-1 focus:ring-brand-sky/30 transition-colors"
                  />
                </div>
              </div>

              {/* Submit button + status */}
              <div className="flex flex-col sm:flex-row items-start sm:items-center gap-3">
                <button
                  onClick={handleSubmit}
                  disabled={!selectedFile || submitting}
                  className="flex items-center gap-2 px-5 py-2.5 rounded-lg bg-linear-to-r from-brand-sky to-brand-cyan text-white text-sm font-medium transition-all duration-200 hover:opacity-90 hover:shadow-lg hover:shadow-brand-sky/20 disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:opacity-40 disabled:hover:shadow-none"
                >
                  {submitting ? (
                    <>
                      <Spinner className="w-4 h-4" />
                      <span>{t("logoVote.submitting")}</span>
                    </>
                  ) : (
                    <>
                      <svg
                        width="16"
                        height="16"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="2"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      >
                        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                        <polyline points="17 8 12 3 7 8" />
                        <line x1="12" y1="3" x2="12" y2="15" />
                      </svg>
                      <span>{t("logoVote.submit")}</span>
                    </>
                  )}
                </button>

                <AnimatePresence>
                  {submitStatus && (
                    <motion.span
                      key="submit-status"
                      initial={{ opacity: 0, x: -4 }}
                      animate={{ opacity: 1, x: 0 }}
                      exit={{ opacity: 0, x: 4 }}
                      transition={{ duration: 0.2 }}
                      className={`text-sm font-medium ${
                        submitStatus.type === "success"
                          ? "text-success"
                          : "text-danger"
                      }`}
                    >
                      {submitStatus.text}
                    </motion.span>
                  )}
                </AnimatePresence>
              </div>
            </div>
          </motion.div>
        )}
      </div>
    </section>
  );
}
