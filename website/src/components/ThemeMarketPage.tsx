import { useEffect, useMemo, useState, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { useLocale } from "@/lib/i18n";

// ── 数据类型（对应 fluxdown-themes 仓库 index.json）──

const THEMES_REPO = "zerx-lab/fluxdown-themes";
const RAW_BASE = `https://raw.githubusercontent.com/${THEMES_REPO}/main`;
const REPO_URL = `https://github.com/${THEMES_REPO}`;

interface VariantAsset {
  theme: string; // 仓库相对路径，如 themes/<id>/theme.dark.json
  screenshot: string;
}

interface MarketTheme {
  id: string;
  name: string;
  author: string;
  version: string;
  description?: string;
  tags?: string[];
  variants: Record<string, VariantAsset>;
}

interface MarketIndex {
  themes: MarketTheme[];
}

/** 仓库相对路径 → raw.githubusercontent.com 绝对 URL */
function rawUrl(path: string): string {
  return `${RAW_BASE}/${path}`;
}

/** 拉取 JSON 并触发浏览器下载（raw 无 Content-Disposition，走 blob） */
async function downloadTheme(path: string): Promise<void> {
  const res = await fetch(rawUrl(path));
  if (!res.ok) throw new Error(String(res.status));
  const blob = await res.blob();
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = path.split("/").slice(-2).join("-"); // <id>-theme.dark.json
  a.click();
  URL.revokeObjectURL(a.href);
}

const VARIANT_ORDER = ["dark", "light"] as const;

function orderedVariants(t: MarketTheme): [string, VariantAsset][] {
  return Object.entries(t.variants).sort(
    (a, b) =>
      VARIANT_ORDER.indexOf(a[0] as (typeof VARIANT_ORDER)[number]) -
      VARIANT_ORDER.indexOf(b[0] as (typeof VARIANT_ORDER)[number]),
  );
}

// ── Lightbox ──

interface LightboxState {
  theme: MarketTheme;
  index: number; // 当前变体索引
}

function Lightbox({
  state,
  onClose,
  onNavigate,
  t,
}: {
  state: LightboxState;
  onClose: () => void;
  onNavigate: (index: number) => void;
  t: (key: never, params?: Record<string, string>) => string;
}) {
  const variants = orderedVariants(state.theme);
  const [key, asset] = variants[state.index];

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowLeft" && state.index > 0) onNavigate(state.index - 1);
      if (e.key === "ArrowRight" && state.index < variants.length - 1)
        onNavigate(state.index + 1);
    };
    window.addEventListener("keydown", onKey);
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = "";
    };
  }, [state.index, variants.length, onClose, onNavigate]);

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      className="fixed inset-0 z-[100] flex flex-col items-center justify-center bg-black/85 backdrop-blur-sm p-4 sm:p-8"
      onClick={onClose}
    >
      {/* 顶栏 */}
      <div
        className="flex items-center justify-between w-full max-w-5xl mb-3"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="text-sm text-white/90 font-medium">
          {state.theme.name}
          <span className="ml-2 text-xs text-white/50">
            {t(`themes.variant.${key}` as never)}
          </span>
        </div>
        <button
          onClick={onClose}
          aria-label="Close"
          className="w-8 h-8 rounded-full bg-white/10 hover:bg-white/20 text-white flex items-center justify-center transition-colors"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </div>

      {/* 大图 */}
      <div
        className="relative w-full max-w-5xl"
        onClick={(e) => e.stopPropagation()}
      >
        <img
          src={rawUrl(asset.screenshot)}
          alt={`${state.theme.name} — ${key}`}
          className="w-full h-auto rounded-xl border border-white/10 shadow-2xl select-none"
          draggable={false}
        />
        {/* 左右切换 */}
        {state.index > 0 && (
          <button
            onClick={() => onNavigate(state.index - 1)}
            aria-label="Previous"
            className="absolute left-2 top-1/2 -translate-y-1/2 w-10 h-10 rounded-full bg-black/50 hover:bg-black/70 text-white flex items-center justify-center transition-colors"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        )}
        {state.index < variants.length - 1 && (
          <button
            onClick={() => onNavigate(state.index + 1)}
            aria-label="Next"
            className="absolute right-2 top-1/2 -translate-y-1/2 w-10 h-10 rounded-full bg-black/50 hover:bg-black/70 text-white flex items-center justify-center transition-colors"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="m9 18 6-6-6-6" />
            </svg>
          </button>
        )}
      </div>

      {/* 变体指示点 */}
      {variants.length > 1 && (
        <div
          className="flex items-center gap-2 mt-4"
          onClick={(e) => e.stopPropagation()}
        >
          {variants.map(([vk], i) => (
            <button
              key={vk}
              onClick={() => onNavigate(i)}
              aria-label={vk}
              className={`h-1.5 rounded-full transition-all ${
                i === state.index ? "w-6 bg-white" : "w-1.5 bg-white/40 hover:bg-white/60"
              }`}
            />
          ))}
        </div>
      )}
    </motion.div>
  );
}

// ── 主题卡片 ──

function ThemeCard({
  theme,
  onPreview,
  t,
  index,
}: {
  theme: MarketTheme;
  onPreview: (index: number) => void;
  t: (key: never, params?: Record<string, string>) => string;
  index: number;
}) {
  const variants = orderedVariants(theme);
  const [active, setActive] = useState(0);
  const [, cover] = variants[Math.min(active, variants.length - 1)];

  return (
    <motion.div
      initial={{ opacity: 0, y: 20 }}
      whileInView={{ opacity: 1, y: 0 }}
      viewport={{ once: true, margin: "-50px" }}
      transition={{ duration: 0.4, delay: (index % 6) * 0.05 }}
      className="group rounded-2xl border border-dark-border bg-dark-surface1/50 overflow-hidden backdrop-blur-sm hover:border-dark-text-muted/30 transition-colors"
    >
      {/* 截图区（16:10 视口，可点击放大） */}
      <button
        onClick={() => onPreview(active)}
        className="relative block w-full aspect-[16/10] overflow-hidden bg-dark-surface2 cursor-zoom-in"
        aria-label={`Preview ${theme.name}`}
      >
        <img
          src={rawUrl(cover.screenshot)}
          alt={`${theme.name} screenshot`}
          loading="lazy"
          className="absolute inset-0 w-full h-full object-cover transition-transform duration-300 group-hover:scale-[1.02]"
        />
        <div className="absolute inset-0 bg-gradient-to-t from-black/30 via-transparent to-transparent opacity-0 group-hover:opacity-100 transition-opacity" />
        <span className="absolute bottom-2 right-2 inline-flex items-center gap-1 rounded-full bg-black/60 px-2 py-1 text-[10px] text-white/90 opacity-0 group-hover:opacity-100 transition-opacity">
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="11" cy="11" r="8" />
            <path d="m21 21-4.3-4.3M11 8v6M8 11h6" />
          </svg>
          {t("themes.clickToZoom" as never)}
        </span>
      </button>

      <div className="p-4">
        {/* 名称 + 版本 */}
        <div className="flex items-center justify-between gap-2">
          <h3 className="text-sm font-semibold text-dark-text truncate">{theme.name}</h3>
          <span className="shrink-0 text-[10px] text-dark-text-muted font-mono">v{theme.version}</span>
        </div>
        {/* 作者 */}
        <a
          href={`https://github.com/${theme.author}`}
          target="_blank"
          rel="noopener noreferrer"
          className="mt-0.5 inline-block text-xs text-dark-text-muted hover:text-brand-sky transition-colors"
        >
          @{theme.author}
        </a>
        {/* 描述 */}
        {theme.description && (
          <p className="mt-1.5 text-xs text-dark-text-secondary line-clamp-2 leading-relaxed">
            {theme.description}
          </p>
        )}

        {/* 变体切换 + tags */}
        <div className="mt-3 flex items-center justify-between gap-2">
          <div className="flex items-center gap-1">
            {variants.map(([vk], i) => (
              <button
                key={vk}
                onClick={() => setActive(i)}
                className={`rounded-full px-2.5 py-1 text-[10px] font-medium transition-colors ${
                  i === active
                    ? "bg-dark-surface3 text-dark-text"
                    : "text-dark-text-muted hover:text-dark-text-secondary"
                }`}
              >
                {t(`themes.variant.${vk}` as never)}
              </button>
            ))}
          </div>
          {theme.tags && theme.tags.length > 0 && (
            <div className="flex items-center gap-1 overflow-hidden">
              {theme.tags.slice(0, 2).map((tag) => (
                <span
                  key={tag}
                  className="shrink-0 rounded-full border border-dark-border px-2 py-0.5 text-[10px] text-dark-text-muted"
                >
                  {tag}
                </span>
              ))}
            </div>
          )}
        </div>

        {/* 下载按钮（每个变体一个） */}
        <div className="mt-3 flex items-center gap-2">
          {variants.map(([vk, asset]) => (
            <button
              key={vk}
              onClick={() => {
                downloadTheme(asset.theme).catch(() => window.open(rawUrl(asset.theme), "_blank"));
              }}
              className="flex-1 inline-flex items-center justify-center gap-1.5 rounded-lg bg-brand-blue/90 hover:bg-brand-blue px-3 py-1.5 text-xs font-semibold text-white transition-colors cursor-pointer"
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3" />
              </svg>
              {variants.length > 1
                ? t(`themes.download.${vk}` as never)
                : t("themes.download" as never)}
            </button>
          ))}
        </div>
      </div>
    </motion.div>
  );
}

// ── 页面 ──

export default function ThemeMarketPage() {
  const { t } = useLocale();
  const [index, setIndex] = useState<MarketIndex | null>(null);
  const [error, setError] = useState(false);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [lightbox, setLightbox] = useState<LightboxState | null>(null);

  useEffect(() => {
    fetch(`${RAW_BASE}/index.json`)
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((data: MarketIndex) => setIndex(data))
      .catch(() => setError(true))
      .finally(() => setLoading(false));
  }, []);

  const filtered = useMemo(() => {
    const themes = index?.themes ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return themes;
    return themes.filter(
      (th) =>
        th.name.toLowerCase().includes(q) ||
        th.author.toLowerCase().includes(q) ||
        th.id.includes(q) ||
        (th.tags ?? []).some((tag) => tag.toLowerCase().includes(q)),
    );
  }, [index, query]);

  const openPreview = useCallback((theme: MarketTheme, i: number) => {
    setLightbox({ theme, index: i });
  }, []);


  return (
    <section className="pt-24 sm:pt-32 pb-16 sm:pb-20">
      <div className="mx-auto max-w-6xl px-4 sm:px-6 lg:px-8">
        {/* 页头 */}
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: 0.5 }}
          className="text-center mb-10 sm:mb-14"
        >
          <span className="inline-flex items-center gap-2 rounded-full border border-dark-border bg-dark-surface1/50 px-4 py-1.5 text-xs font-medium text-dark-text-secondary backdrop-blur-sm mb-6">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-brand-sky">
              <circle cx="13.5" cy="6.5" r=".5" fill="currentColor" />
              <circle cx="17.5" cy="10.5" r=".5" fill="currentColor" />
              <circle cx="8.5" cy="7.5" r=".5" fill="currentColor" />
              <circle cx="6.5" cy="12.5" r=".5" fill="currentColor" />
              <path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.926 0 1.648-.746 1.648-1.688 0-.437-.18-.835-.437-1.125-.29-.289-.438-.652-.438-1.125a1.64 1.64 0 0 1 1.668-1.668h1.996c3.051 0 5.555-2.503 5.555-5.554C21.965 6.012 17.461 2 12 2z" />
            </svg>
            {t("themes.badge")}
          </span>

          <h1 className="text-4xl sm:text-5xl font-bold tracking-tight leading-tight">
            {t("themes.title")}
          </h1>

          <p className="mt-4 text-base sm:text-lg text-dark-text-secondary max-w-2xl mx-auto leading-relaxed">
            {t("themes.subtitle")}
          </p>

          {/* 搜索 + 提交入口 */}
          <div className="mt-8 flex flex-col sm:flex-row items-center justify-center gap-3">
            <div className="relative w-full sm:w-80">
              <svg
                width="15"
                height="15"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="absolute left-3 top-1/2 -translate-y-1/2 text-dark-text-muted"
              >
                <circle cx="11" cy="11" r="8" />
                <path d="m21 21-4.3-4.3" />
              </svg>
              <input
                type="text"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("themes.searchPlaceholder")}
                className="w-full rounded-full border border-dark-border bg-dark-surface1/50 pl-9 pr-4 py-2 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-sky/50 transition-colors backdrop-blur-sm"
              />
            </div>
            <a
              href={`${REPO_URL}?tab=contributing-ov-file`}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1.5 rounded-full border border-dark-border bg-dark-surface1/50 px-4 py-2 text-xs font-medium text-dark-text-secondary hover:text-dark-text hover:border-dark-text-muted/40 transition-colors backdrop-blur-sm"
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor">
                <path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0 0 24 12c0-6.63-5.37-12-12-12z" />
              </svg>
              {t("themes.submitCta")}
            </a>
          </div>
        </motion.div>

        {/* 加载/错误/空态 */}
        {loading && (
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5">
            {Array.from({ length: 6 }, (_, i) => (
              <div
                key={i}
                className="rounded-2xl border border-dark-border bg-dark-surface1/30 overflow-hidden animate-pulse"
              >
                <div className="aspect-[16/10] bg-dark-surface2/60" />
                <div className="p-4 space-y-2">
                  <div className="h-4 w-2/5 rounded bg-dark-surface2/60" />
                  <div className="h-3 w-1/4 rounded bg-dark-surface2/40" />
                  <div className="h-8 rounded-lg bg-dark-surface2/40 mt-3" />
                </div>
              </div>
            ))}
          </div>
        )}

        {error && !loading && (
          <div className="text-center py-20">
            <p className="text-sm text-dark-text-muted">{t("themes.loadError")}</p>
            <a
              href="https://github.com/zerx-lab/fluxdown-themes"
              target="_blank"
              rel="noopener noreferrer"
              className="mt-3 inline-block text-xs text-brand-sky hover:underline"
            >
              github.com/zerx-lab/fluxdown-themes →
            </a>
          </div>
        )}

        {!loading && !error && filtered.length === 0 && (
          <div className="text-center py-20">
            <p className="text-sm text-dark-text-muted">{t("themes.empty")}</p>
          </div>
        )}

        {/* 主题网格 */}
        {!loading && !error && filtered.length > 0 && (
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5">
            {filtered.map((theme, i) => (
              <ThemeCard
                key={theme.id}
                theme={theme}
                index={i}
                onPreview={(vi) => openPreview(theme, vi)}
                t={t as never}
              />
            ))}
          </div>
        )}

        {/* 底部：如何使用 */}
        {!loading && !error && (
          <motion.div
            initial={{ opacity: 0 }}
            whileInView={{ opacity: 1 }}
            viewport={{ once: true }}
            className="mt-14 rounded-2xl border border-dark-border bg-dark-surface1/40 p-6 backdrop-blur-sm"
          >
            <h2 className="text-sm font-semibold text-dark-text mb-3">{t("themes.howTo.title")}</h2>
            <ol className="space-y-2 text-xs text-dark-text-secondary leading-relaxed list-decimal list-inside">
              <li>{t("themes.howTo.step1")}</li>
              <li>{t("themes.howTo.step2")}</li>
              <li>
                {t("themes.howTo.step3")}{" "}
                <a
                  href={`${REPO_URL}?tab=contributing-ov-file`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-brand-sky hover:underline"
                >
                  {t("themes.howTo.guideLink")}
                </a>
              </li>
            </ol>
          </motion.div>
        )}
      </div>

      {/* Lightbox */}
      <AnimatePresence>
        {lightbox && (
          <Lightbox
            state={lightbox}
            onClose={() => setLightbox(null)}
            onNavigate={(i) => setLightbox((s) => (s ? { ...s, index: i } : s))}
            t={t as never}
          />
        )}
      </AnimatePresence>
    </section>
  );
}
