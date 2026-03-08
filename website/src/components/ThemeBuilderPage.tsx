import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ChangeEvent, MouseEvent as ReactMouseEvent } from "react";
import type { CSSProperties } from "react";
import {
  Copy,
  Download,
  Minus,
  Moon,
  Pause,
  Play,
  Plus,
  RefreshCw,
  Search,
  Settings,
  Square,
  Sun,
  Upload,
  X,
} from "lucide-react";
import { useLocale } from "@/lib/i18n";
import type { Messages } from "@/lib/locales";
import {
  type FluxThemeJson,
  type TokenDescriptor,
  TOKEN_GROUPS,
  argbToCssRgba,
  argbToRgbHex,
  defaultDarkTheme,
  defaultLightTheme,
  exportFluxThemeJson,
  getPathValue,
  getTokenDescriptors,
  normalizeHex8,
  parseFluxThemeJson,
  setPathValue,
  withAlpha,
  withRgb,
} from "@/lib/theme-builder";

interface PreviewSegment {
  index: number;
  startByte: number;
  endByte: number;
  downloadedBytes: number;
}

type PreviewTaskStatus = "downloading" | "paused" | "completed" | "error";
type PreviewFileCategory = "all" | "video" | "audio" | "document" | "image" | "archive" | "other";

interface PreviewTask {
  id: string;
  ext: string;
  name: string;
  subtitle: string;
  size: string;
  totalBytes: number;
  downloadedBytes: number;
  progress: number;
  speed: string;
  status: PreviewTaskStatus;
  fileCategory: PreviewFileCategory;
  segments: PreviewSegment[];
  url: string;
  saveDir: string;
  eta: string;
}

interface InspectorEntry {
  path: string;
  value: string;
}

interface InspectorMenuState {
  x: number;
  y: number;
  entries: InspectorEntry[];
}

const PREVIEW_TOTAL_BYTES = 2_254_857_830;

const PREVIEW_SEGMENTS: PreviewSegment[] = [
  { index: 0, startByte: 0, endByte: 281_857_228, downloadedBytes: 281_857_228 },
  { index: 1, startByte: 281_857_229, endByte: 563_714_457, downloadedBytes: 281_857_228 },
  { index: 2, startByte: 563_714_458, endByte: 845_571_686, downloadedBytes: 281_857_228 },
  { index: 3, startByte: 845_571_687, endByte: 1_127_428_915, downloadedBytes: 245_000_000 },
  { index: 4, startByte: 1_127_428_916, endByte: 1_409_286_144, downloadedBytes: 200_000_000 },
  { index: 5, startByte: 1_409_286_145, endByte: 1_691_143_373, downloadedBytes: 180_000_000 },
  { index: 6, startByte: 1_691_143_374, endByte: 1_973_000_602, downloadedBytes: 120_000_000 },
  { index: 7, startByte: 1_973_000_603, endByte: 2_254_857_830, downloadedBytes: 66_748_822 },
];

const PREVIEW_TASKS: PreviewTask[] = [
  {
    id: "t1",
    ext: "zip",
    name: "4K-wallpaper-collection.zip",
    subtitle: "HTTP · 847.2 MB · Paused",
    size: "847.2 MB",
    totalBytes: 888_300_000,
    downloadedBytes: 597_823_900,
    progress: 67.3,
    speed: "---",
    status: "paused",
    fileCategory: "archive",
    segments: [
      { index: 0, startByte: 0, endByte: 222_074_999, downloadedBytes: 222_074_999 },
      { index: 1, startByte: 222_075_000, endByte: 444_149_999, downloadedBytes: 195_000_000 },
      { index: 2, startByte: 444_150_000, endByte: 666_224_999, downloadedBytes: 120_748_900 },
      { index: 3, startByte: 666_225_000, endByte: 888_299_999, downloadedBytes: 60_000_000 },
    ],
    url: "https://cdn.example.com/4K-wallpaper-collection.zip",
    saveDir: "D:\\Downloads",
    eta: "---",
  },
  {
    id: "t2",
    ext: "mp4",
    name: "React-Advanced-Tutorial.mp4",
    subtitle: "HTTP · 2.1 GB · 45.2 MB/s",
    size: "2.1 GB",
    totalBytes: PREVIEW_TOTAL_BYTES,
    downloadedBytes: 1_657_320_506,
    progress: 73.5,
    speed: "45.2 MB/s",
    status: "downloading",
    fileCategory: "video",
    segments: PREVIEW_SEGMENTS,
    url: "https://media.example.com/React-Advanced-Tutorial.mp4",
    saveDir: "D:\\Downloads",
    eta: "13s",
  },
  {
    id: "t3",
    ext: "pdf",
    name: "annual-report-2025.pdf",
    subtitle: "HTTP · 24.6 MB",
    size: "24.6 MB",
    totalBytes: 25_795_276,
    downloadedBytes: 25_795_276,
    progress: 100,
    speed: "---",
    status: "completed",
    fileCategory: "document",
    segments: [
      { index: 0, startByte: 0, endByte: 12_897_637, downloadedBytes: 12_897_637 },
      { index: 1, startByte: 12_897_638, endByte: 25_795_275, downloadedBytes: 12_897_638 },
    ],
    url: "https://reports.example.com/annual-report-2025.pdf",
    saveDir: "D:\\Downloads",
    eta: "---",
  },
  {
    id: "t4",
    ext: "gz",
    name: "project-v2.0-src.tar.gz",
    subtitle: "HTTP · 312.4 MB · 28.7 MB/s",
    size: "312.4 MB",
    totalBytes: 327_580_000,
    downloadedBytes: 147_738_580,
    progress: 45.1,
    speed: "28.7 MB/s",
    status: "downloading",
    fileCategory: "archive",
    segments: [
      { index: 0, startByte: 0, endByte: 81_894_999, downloadedBytes: 81_895_000 },
      { index: 1, startByte: 81_895_000, endByte: 163_789_999, downloadedBytes: 45_000_000 },
      { index: 2, startByte: 163_790_000, endByte: 245_684_999, downloadedBytes: 15_843_580 },
      { index: 3, startByte: 245_685_000, endByte: 327_579_999, downloadedBytes: 5_000_000 },
    ],
    url: "https://releases.example.com/project-v2.0-src.tar.gz",
    saveDir: "D:\\Downloads",
    eta: "6s",
  },
  {
    id: "t5",
    ext: "exe",
    name: "system-driver-update.exe",
    subtitle: "HTTP · 89.3 MB · Timeout",
    size: "89.3 MB",
    totalBytes: 93_633_536,
    downloadedBytes: 11_236_024,
    progress: 12,
    speed: "---",
    status: "error",
    fileCategory: "other",
    segments: [
      { index: 0, startByte: 0, endByte: 46_816_767, downloadedBytes: 11_236_024 },
      { index: 1, startByte: 46_816_768, endByte: 93_633_535, downloadedBytes: 0 },
    ],
    url: "https://drivers.example.com/system-driver-update.exe",
    saveDir: "D:\\Downloads",
    eta: "---",
  },
];

const PREVIEW_SIDEBAR: Array<{ key: PreviewFileCategory; labelKey: keyof Messages }> = [
  { key: "all", labelKey: "mockup.allFiles" },
  { key: "video", labelKey: "mockup.video" },
  { key: "audio", labelKey: "mockup.audio" },
  { key: "document", labelKey: "mockup.document" },
  { key: "image", labelKey: "mockup.image" },
  { key: "archive", labelKey: "mockup.archive" },
  { key: "other", labelKey: "mockup.other" },
];

const PREVIEW_TABS: Array<{ key: "all" | PreviewTaskStatus; labelKey: keyof Messages }> = [
  { key: "all", labelKey: "mockup.tabAll" },
  { key: "downloading", labelKey: "mockup.tabDownloading" },
  { key: "completed", labelKey: "mockup.tabCompleted" },
  { key: "paused", labelKey: "mockup.tabPaused" },
  { key: "error", labelKey: "mockup.tabError" },
];

function cloneTheme(theme: FluxThemeJson): FluxThemeJson {
  return JSON.parse(JSON.stringify(theme)) as FluxThemeJson;
}

function tokenAttrs(...paths: string[]) {
  return { "data-token-paths": paths.join("|") };
}

function isHex8(value: string): boolean {
  return /^[0-9a-f]{8}$/i.test(value);
}

function formatInspectorValue(raw: unknown): string {
  if (typeof raw === "string") return raw;
  if (Array.isArray(raw)) return raw.join(", ");
  if (typeof raw === "number" || typeof raw === "boolean") return String(raw);
  if (raw === undefined || raw === null) return "undefined";
  return JSON.stringify(raw);
}

function rgbaFromTheme(theme: FluxThemeJson, path: string): string {
  const value = getPathValue(theme, path);
  if (typeof value !== "string") return "rgba(0, 0, 0, 1)";
  return argbToCssRgba(value);
}

function hexFromTheme(theme: FluxThemeJson, path: string): string {
  const value = getPathValue(theme, path);
  if (typeof value !== "string") return "ff000000";
  return normalizeHex8(value);
}

function colorForSegment(theme: FluxThemeJson, segmentIndex: number): string {
  const palette = theme.colors.segmentPalette.length > 0
    ? theme.colors.segmentPalette
    : ["ff22c55e"];
  if (segmentIndex % (palette.length + 1) === 0) {
    return argbToCssRgba(theme.colors.accent.color);
  }
  return argbToCssRgba(palette[(segmentIndex - 1) % palette.length]!);
}

function buildGridCells(
  theme: FluxThemeJson,
  segments: PreviewSegment[],
  totalBytes: number,
): boolean[] {
  const cols = 44;
  const rows = 9;
  const totalCells = cols * rows;
  const filled = new Array<boolean>(totalCells).fill(false);

  for (let i = 0; i < totalCells; i += 1) {
    const start = Math.floor((totalBytes * i) / totalCells);
    const end = Math.floor((totalBytes * (i + 1)) / totalCells);
    const seg = segments.find((item) => start < item.endByte && end > item.startByte);
    if (!seg) continue;

    const segSize = seg.endByte - seg.startByte + 1;
    const segProgress = Math.min(1, seg.downloadedBytes / segSize);
    const cellMid = start + (end - start) / 2;
    const localMid = cellMid - seg.startByte;
    filled[i] = localMid / segSize <= segProgress;
  }

  if (theme.appearance === "light") {
    return filled;
  }

  // 暗色模式下略微强化可见性
  return filled.map((v, idx) => (idx % 11 === 0 ? true : v));
}

function safeFileName(name: string): string {
  const cleaned = name.trim().replace(/[^\w\-]+/g, "_").toLowerCase();
  return cleaned || "flux_theme";
}

function getTaskStatusText(task: PreviewTask, t: (key: keyof Messages, params?: Record<string, string>) => string): string {
  if (task.status === "downloading") return t("mockup.statusDownloading");
  if (task.status === "paused") return t("mockup.statusPaused");
  if (task.status === "completed") return t("mockup.statusCompleted");
  return t("mockup.statusError");
}

function getTaskStatusColor(theme: FluxThemeJson, status: PreviewTaskStatus): string {
  if (status === "downloading") return argbToCssRgba(theme.colors.accent.color);
  if (status === "completed") return argbToCssRgba(theme.colors.status.success);
  if (status === "paused") return argbToCssRgba(theme.colors.status.warning);
  return argbToCssRgba(theme.colors.status.error);
}

function themedScrollbarVars(theme: FluxThemeJson): CSSProperties {
  return {
    "--tb-scroll-track": argbToCssRgba(theme.colors.surface.surface2),
    "--tb-scroll-thumb": argbToCssRgba(theme.colors.surface.surface3),
    "--tb-scroll-thumb-hover": argbToCssRgba(theme.colors.text.muted),
  } as CSSProperties;
}

function SidebarIcon({
  category,
  color,
}: {
  category: PreviewFileCategory;
  color: string;
}) {
  if (category === "all") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="3" y="3" width="7" height="7" />
        <rect x="14" y="3" width="7" height="7" />
        <rect x="14" y="14" width="7" height="7" />
        <rect x="3" y="14" width="7" height="7" />
      </svg>
    );
  }
  if (category === "video") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="2" y="2" width="20" height="20" rx="2" />
        <line x1="7" y1="2" x2="7" y2="22" />
        <line x1="17" y1="2" x2="17" y2="22" />
        <line x1="2" y1="12" x2="22" y2="12" />
      </svg>
    );
  }
  if (category === "audio") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M9 18V5l12-2v13" />
        <circle cx="6" cy="18" r="3" />
        <circle cx="18" cy="16" r="3" />
      </svg>
    );
  }
  if (category === "document") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
        <polyline points="14 2 14 8 20 8" />
        <line x1="16" y1="13" x2="8" y2="13" />
        <line x1="16" y1="17" x2="8" y2="17" />
      </svg>
    );
  }
  if (category === "image") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <circle cx="8.5" cy="8.5" r="1.5" />
        <polyline points="21 15 16 10 5 21" />
      </svg>
    );
  }
  if (category === "archive") {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <polyline points="21 8 21 21 3 21 3 8" />
        <rect x="1" y="3" width="22" height="5" />
        <line x1="10" y1="12" x2="14" y2="12" />
      </svg>
    );
  }
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
    </svg>
  );
}

export default function ThemeBuilderPage() {
  const { t } = useLocale();
  const [theme, setTheme] = useState<FluxThemeJson>(() => cloneTheme(defaultDarkTheme));
  const [search, setSearch] = useState("");
  const [status, setStatus] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [menu, setMenu] = useState<InspectorMenuState | null>(null);
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const tokenRowRefs = useRef<Record<string, HTMLDivElement | null>>({});

  const descriptors = useMemo(() => getTokenDescriptors(theme), [theme]);

  const descriptorsByGroup = useMemo(() => {
    const query = search.trim().toLowerCase();
    return TOKEN_GROUPS.map((group) => {
      const tokens = descriptors.filter((token) => {
        if (token.groupKey !== group.key) return false;
        if (!query) return true;
        return (
          token.path.toLowerCase().includes(query)
          || token.label.toLowerCase().includes(query)
          || group.key.toLowerCase().includes(query)
        );
      });
      return { group, tokens };
    }).filter((entry) => entry.tokens.length > 0);
  }, [descriptors, search]);

  const openInspector = useCallback((event: ReactMouseEvent<HTMLElement>) => {
    const target = event.target as HTMLElement | null;
    if (!target) return;
    const tokenNode = target.closest<HTMLElement>("[data-token-paths]");
    if (!tokenNode) return;

    const rawPaths = tokenNode.dataset.tokenPaths ?? "";
    const paths = rawPaths.split("|").map((path) => path.trim()).filter(Boolean);
    if (paths.length === 0) return;

    const entries = paths.map((path) => ({
      path,
      value: formatInspectorValue(getPathValue(theme, path)),
    }));

    event.preventDefault();
    setMenu({
      x: Math.min(event.clientX, window.innerWidth - 340),
      y: Math.min(event.clientY, window.innerHeight - 220),
      entries,
    });
  }, [theme]);

  useEffect(() => {
    if (!menu) return undefined;
    const close = () => setMenu(null);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("click", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [menu]);

  const updateHex = useCallback((path: string, value: string) => {
    setTheme((prev) => setPathValue(prev, path, normalizeHex8(value)));
  }, []);

  const updateRgb = useCallback((path: string, value: string) => {
    setTheme((prev) => {
      const current = hexFromTheme(prev, path);
      return setPathValue(prev, path, withRgb(current, value));
    });
  }, []);

  const updateAlpha = useCallback((path: string, alpha: number) => {
    setTheme((prev) => {
      const current = hexFromTheme(prev, path);
      return setPathValue(prev, path, withAlpha(current, alpha));
    });
  }, []);

  const applyAppearance = useCallback((appearance: "dark" | "light") => {
    setTheme((prev) => {
      if (prev.appearance === appearance) return prev;
      const base = cloneTheme(appearance === "dark" ? defaultDarkTheme : defaultLightTheme);
      return {
        ...base,
        name: prev.name,
        author: prev.author,
      };
    });
  }, []);

  const toggleAppearance = useCallback(() => {
    applyAppearance(theme.appearance === "dark" ? "light" : "dark");
  }, [applyAppearance, theme.appearance]);

  const handleImport = useCallback(async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;
    try {
      const text = await file.text();
      const imported = parseFluxThemeJson(text);
      setTheme(imported);
      setStatus({ type: "success", text: t("tb.importSuccess", { name: imported.name }) });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus({ type: "error", text: t("tb.importError", { reason: message }) });
    } finally {
      event.target.value = "";
    }
  }, [t]);

  const handleExport = useCallback(() => {
    const json = exportFluxThemeJson(theme);
    const blob = new Blob([json], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${safeFileName(theme.name)}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
    setStatus({ type: "success", text: t("tb.exportSuccess", { name: theme.name }) });
  }, [t, theme]);

  const handleCopyJson = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(exportFluxThemeJson(theme));
      setStatus({ type: "success", text: t("tb.copySuccess") });
    } catch {
      setStatus({ type: "error", text: t("tb.copyError") });
    }
  }, [t, theme]);

  const handleReset = useCallback(() => {
    setTheme(cloneTheme(defaultDarkTheme));
    setStatus({ type: "success", text: t("tb.resetSuccess") });
  }, [t]);

  const handleCopyInspector = useCallback(async (value: string) => {
    try {
      await navigator.clipboard.writeText(value);
      setStatus({ type: "success", text: t("tb.copySuccess") });
    } catch {
      setStatus({ type: "error", text: t("tb.copyError") });
    }
  }, [t]);

  const focusTokenPath = useCallback((path: string) => {
    setSearch(path);
    setMenu(null);

    let attempts = 0;
    const run = () => {
      const row = tokenRowRefs.current[path];
      if (row) {
        row.scrollIntoView({ behavior: "smooth", block: "center" });
        const hexInput = row.querySelector<HTMLInputElement>('input[type="text"]');
        hexInput?.focus();
        hexInput?.select();
        setFocusedPath(path);
        return;
      }

      if (attempts < 8) {
        attempts += 1;
        window.setTimeout(run, 50);
      }
    };

    window.setTimeout(run, 0);
  }, []);

  useEffect(() => {
    if (!focusedPath) return undefined;
    const timer = window.setTimeout(() => {
      setFocusedPath((current) => (current === focusedPath ? null : current));
    }, 1500);
    return () => window.clearTimeout(timer);
  }, [focusedPath]);

  return (
    <section className="relative min-h-screen overflow-hidden px-4 pb-12 pt-20 sm:px-6">
      <div className="pointer-events-none absolute inset-0 -z-10 overflow-hidden">
        <div className="absolute -top-24 left-1/2 h-[380px] w-[680px] -translate-x-1/2 rounded-full bg-brand-sky/[0.08] blur-[120px]" />
        <div className="absolute bottom-0 right-0 h-[360px] w-[480px] rounded-full bg-brand-cyan/[0.06] blur-[110px]" />
      </div>

      <div className="mx-auto max-w-[1520px]">
        <div className="mb-4 flex items-center justify-between gap-4">
          <div className="flex items-center gap-3">
            <span className="inline-flex rounded-full border border-brand-blue/30 bg-brand-blue/10 px-3 py-1 text-xs font-semibold text-brand-blue">
              {t("tb.badge")}
            </span>
            <h1 className="text-2xl font-semibold text-dark-text sm:text-3xl">{t("tb.title")}</h1>
          </div>
          <div className="hidden lg:flex items-center gap-2 rounded-lg border border-dark-border/60 bg-dark-surface1/50 px-3 py-1.5 text-[11px] text-dark-text-muted backdrop-blur-sm">
            <span>{t("tb.rightClickHint")}</span>
          </div>
        </div>

        {status && (
          <div
            className={`mb-3 flex items-center justify-between rounded-lg border px-3 py-1.5 text-sm backdrop-blur-sm ${
              status.type === "success"
                ? "border-success/40 bg-success/10 text-success"
                : "border-danger/40 bg-danger/10 text-danger"
            }`}
          >
            <span>{status.text}</span>
            <button
              type="button"
              onClick={() => setStatus(null)}
              className="rounded-md p-1 text-current/80 hover:bg-black/10"
              aria-label="close"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        )}

        <div
          className="grid gap-4 lg:grid-cols-[380px_minmax(0,1fr)]"
          onContextMenu={openInspector}
          {...tokenAttrs("colors.surface.background")}
        >
          <div
            className="flex flex-col rounded-2xl border border-dark-border/80 bg-dark-surface1/90 p-3 shadow-2xl shadow-black/20 backdrop-blur-sm lg:max-h-[calc(100vh-7rem)]"
            {...tokenAttrs("colors.surface.surface1", "colors.border.default")}
          >
            <div className="space-y-2 pb-2" {...tokenAttrs("name", "author", "appearance")}>
              <div className="grid grid-cols-2 gap-2">
                <label className="block">
                  <span className="mb-0.5 block text-[11px] font-medium text-dark-text-secondary">{t("tb.meta.name")}</span>
                  <input
                    value={theme.name}
                    onChange={(event) => setTheme((prev) => ({ ...prev, name: event.target.value }))}
                    className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-2.5 py-1.5 text-xs text-dark-text outline-none transition-colors focus:border-brand-blue/60"
                  />
                </label>
                <label className="block">
                  <span className="mb-0.5 block text-[11px] font-medium text-dark-text-secondary">{t("tb.meta.author")}</span>
                  <input
                    value={theme.author ?? ""}
                    onChange={(event) => {
                      const next = event.target.value;
                      setTheme((prev) => (next.trim() ? { ...prev, author: next } : { ...prev, author: undefined }));
                    }}
                    className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-2.5 py-1.5 text-xs text-dark-text outline-none transition-colors focus:border-brand-blue/60"
                  />
                </label>
              </div>

              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => applyAppearance("dark")}
                  className={`inline-flex flex-1 items-center justify-center gap-1.5 rounded-lg border px-2 py-1.5 text-xs font-medium transition-all ${
                    theme.appearance === "dark"
                      ? "border-brand-blue/50 bg-brand-blue/15 text-brand-blue shadow-sm shadow-brand-blue/10"
                      : "border-dark-border bg-dark-surface2 text-dark-text-secondary hover:text-dark-text"
                  }`}
                  {...tokenAttrs("appearance")}
                >
                  <Moon className="h-3.5 w-3.5" />
                  {t("tb.appearance.dark")}
                </button>
                <button
                  type="button"
                  onClick={() => applyAppearance("light")}
                  className={`inline-flex flex-1 items-center justify-center gap-1.5 rounded-lg border px-2 py-1.5 text-xs font-medium transition-all ${
                    theme.appearance === "light"
                      ? "border-brand-blue/50 bg-brand-blue/15 text-brand-blue shadow-sm shadow-brand-blue/10"
                      : "border-dark-border bg-dark-surface2 text-dark-text-secondary hover:text-dark-text"
                  }`}
                  {...tokenAttrs("appearance")}
                >
                  <Sun className="h-3.5 w-3.5" />
                  {t("tb.appearance.light")}
                </button>
                <div className="h-5 w-px bg-dark-border/60" />
                <button
                  type="button"
                  onClick={() => fileInputRef.current?.click()}
                  className="inline-flex items-center justify-center gap-1 rounded-lg border border-dark-border bg-dark-surface2/70 px-2 py-1.5 text-[11px] font-semibold text-dark-text-secondary transition-colors hover:bg-dark-surface2 hover:text-dark-text"
                >
                  <Upload className="h-3 w-3" />
                  {t("tb.actions.import")}
                </button>
                <button
                  type="button"
                  onClick={handleExport}
                  className="inline-flex items-center justify-center gap-1 rounded-lg border border-dark-border bg-dark-surface2/70 px-2 py-1.5 text-[11px] font-semibold text-dark-text-secondary transition-colors hover:bg-dark-surface2 hover:text-dark-text"
                >
                  <Download className="h-3 w-3" />
                  {t("tb.actions.export")}
                </button>
              </div>

              <div className="flex items-center gap-1">
                <div className="relative flex-1">
                  <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dark-text-muted" />
                  <input
                    value={search}
                    onChange={(event) => setSearch(event.target.value)}
                    placeholder={t("tb.searchPlaceholder")}
                    className="w-full rounded-lg border border-dark-border bg-dark-surface2 py-1.5 pl-8 pr-7 text-xs text-dark-text outline-none transition-colors focus:border-brand-blue/60"
                  />
                  {search && (
                    <button
                      type="button"
                      onClick={() => setSearch("")}
                      className="absolute right-2 top-1/2 -translate-y-1/2 rounded-full p-0.5 text-dark-text-muted transition-colors hover:bg-dark-surface3 hover:text-dark-text-secondary"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  )}
                </div>
                <button
                  type="button"
                  onClick={handleCopyJson}
                  className="inline-flex items-center justify-center rounded-md p-1.5 text-dark-text-muted transition-colors hover:bg-dark-surface2 hover:text-dark-text-secondary"
                  title={t("tb.actions.copyJson")}
                >
                  <Copy className="h-3.5 w-3.5" />
                </button>
                <button
                  type="button"
                  onClick={handleReset}
                  className="inline-flex items-center justify-center rounded-md p-1.5 text-dark-text-muted transition-colors hover:bg-dark-surface2 hover:text-dark-text-secondary"
                  title={t("tb.actions.reset")}
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                </button>
              </div>
            </div>

            <input ref={fileInputRef} type="file" accept=".json,application/json" className="hidden" onChange={handleImport} />

            <div className="tb-scrollbar min-h-0 flex-1 space-y-1.5 overflow-y-auto pr-1">
              {descriptorsByGroup.map(({ group, tokens }) => (
                <details key={group.key} open className="rounded-lg border border-dark-border/70 bg-dark-surface2/40">
                  <summary className="cursor-pointer list-none px-2.5 py-1.5 text-xs font-semibold text-dark-text">
                    <div className="flex items-center justify-between">
                      <span>{t(group.labelKey as keyof Messages)}</span>
                      <span className="rounded-full bg-dark-surface3/50 px-1.5 py-0.5 text-[10px] font-medium text-dark-text-muted">{tokens.length}</span>
                    </div>
                  </summary>
                  <div className="space-y-1 px-1.5 pb-1.5">
                    {tokens.map((token) => (
                      <TokenRow
                        key={token.path}
                        token={token}
                        value={hexFromTheme(theme, token.path)}
                        onHexChange={updateHex}
                        onRgbChange={updateRgb}
                        onAlphaChange={updateAlpha}
                        isFocused={focusedPath === token.path}
                        rowRef={(node) => {
                          tokenRowRefs.current[token.path] = node;
                        }}
                      />
                    ))}
                  </div>
                </details>
              ))}
            </div>
          </div>

          <PreviewPanel theme={theme} t={t} onToggleAppearance={toggleAppearance} />
        </div>
      </div>

      {menu && (
        <div
          className="fixed z-[100] w-[320px] rounded-xl border border-dark-border bg-dark-surface1 p-2 shadow-2xl shadow-black/40"
          style={{ left: menu.x, top: menu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <div className="mb-1 px-2 py-1 text-xs font-semibold text-dark-text-secondary">{t("tb.context.title")}</div>
          <div className="space-y-1">
            {menu.entries.map((entry) => {
              const color = isHex8(entry.value) ? argbToCssRgba(entry.value) : null;
              return (
                <div key={`${entry.path}-${entry.value}`} className="rounded-lg border border-dark-border bg-dark-surface2 p-2">
                  <button
                    type="button"
                    onClick={() => focusTokenPath(entry.path)}
                    className="mb-1 flex w-full items-center gap-2 rounded-md px-1 py-0.5 text-left hover:bg-dark-surface1"
                  >
                    {color && <span className="h-3 w-3 rounded-full border border-dark-border" style={{ backgroundColor: color }} />}
                    <code className="truncate text-[11px] text-dark-text-secondary">{entry.path}</code>
                  </button>
                  <div className="flex items-center justify-between gap-2">
                    <code className="truncate text-xs text-dark-text">{entry.value}</code>
                    <div className="flex items-center gap-1">
                      <button
                        type="button"
                        onClick={() => handleCopyInspector(entry.path)}
                        className="rounded-md border border-dark-border bg-dark-surface1 px-2 py-1 text-[10px] font-semibold text-dark-text-secondary hover:text-dark-text"
                      >
                        {t("tb.context.copyPath")}
                      </button>
                      <button
                        type="button"
                        onClick={() => handleCopyInspector(entry.value)}
                        className="rounded-md border border-dark-border bg-dark-surface1 px-2 py-1 text-[10px] font-semibold text-dark-text-secondary hover:text-dark-text"
                      >
                        {t("tb.context.copyValue")}
                      </button>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </section>
  );
}

function TokenRow({
  token,
  value,
  onHexChange,
  onRgbChange,
  onAlphaChange,
  isFocused,
  rowRef,
}: {
  token: TokenDescriptor;
  value: string;
  onHexChange: (path: string, value: string) => void;
  onRgbChange: (path: string, value: string) => void;
  onAlphaChange: (path: string, alpha: number) => void;
  isFocused?: boolean;
  rowRef?: (node: HTMLDivElement | null) => void;
}) {
  const alpha = Number.parseInt(value.slice(0, 2), 16);
  return (
    <div
      ref={rowRef}
      data-token-row-path={token.path}
      className={`grid grid-cols-[minmax(0,1fr)_32px_60px_80px] items-center gap-1.5 rounded-lg border px-2 py-1.5 transition-all ${
        isFocused
          ? "border-brand-blue/50 bg-brand-blue/10 ring-1 ring-brand-blue/40"
          : "border-dark-border/60 bg-dark-surface1 hover:border-dark-border"
      }`}
      {...tokenAttrs(token.path)}
    >
      <div className="min-w-0">
        <div className="truncate text-[11px] font-medium text-dark-text">{token.label}</div>
        <code className="block truncate text-[9px] leading-tight text-dark-text-muted">{token.path}</code>
      </div>

      <input
        type="color"
        value={argbToRgbHex(value)}
        onChange={(event) => onRgbChange(token.path, event.target.value)}
        className="h-6 w-full cursor-pointer rounded border border-dark-border/60 bg-dark-surface2 p-0.5"
      />

      <div className="flex items-center gap-1">
        <input
          type="range"
          min={0}
          max={255}
          value={alpha}
          onChange={(event) => onAlphaChange(token.path, Number.parseInt(event.target.value, 10))}
          className="h-1 w-full accent-brand-blue"
        />
        <span className="w-5 text-right text-[9px] tabular-nums text-dark-text-muted">{alpha}</span>
      </div>

      <input
        type="text"
        value={value}
        onChange={(event) => onHexChange(token.path, event.target.value)}
        className="w-full rounded-md border border-dark-border/60 bg-dark-surface2 px-1.5 py-1 font-mono text-[10px] text-dark-text outline-none transition-colors focus:border-brand-blue/60"
      />
    </div>
  );
}

function PreviewPanel({
  theme,
  t,
  onToggleAppearance,
}: {
  theme: FluxThemeJson;
  t: (key: keyof Messages, params?: Record<string, string>) => string;
  onToggleAppearance: () => void;
}) {
  const [activeFile, setActiveFile] = useState<PreviewFileCategory>("all");
  const [activeTab, setActiveTab] = useState<"all" | PreviewTaskStatus>("all");
  const [selectedTaskId, setSelectedTaskId] = useState<string>(
    PREVIEW_TASKS[1]?.id ?? PREVIEW_TASKS[0]!.id,
  );

  const filteredTasks = useMemo(
    () =>
      PREVIEW_TASKS.filter((task) => {
        if (activeFile !== "all" && task.fileCategory !== activeFile) return false;
        if (activeTab !== "all" && task.status !== activeTab) return false;
        return true;
      }),
    [activeFile, activeTab],
  );

  const selectedTask = useMemo(
    () =>
      PREVIEW_TASKS.find((task) => task.id === selectedTaskId)
      ?? filteredTasks[0]
      ?? PREVIEW_TASKS[0]!,
    [filteredTasks, selectedTaskId],
  );

  const gridCells = useMemo(
    () => buildGridCells(theme, selectedTask.segments, selectedTask.totalBytes),
    [selectedTask.segments, selectedTask.totalBytes, theme],
  );

  const countByFile = useCallback((file: PreviewFileCategory) => {
    if (file === "all") return PREVIEW_TASKS.length;
    return PREVIEW_TASKS.filter((task) => task.fileCategory === file).length;
  }, []);

  const countByTab = useCallback(
    (tab: "all" | PreviewTaskStatus) => {
      const tasks = activeFile === "all"
        ? PREVIEW_TASKS
        : PREVIEW_TASKS.filter((task) => task.fileCategory === activeFile);
      if (tab === "all") return tasks.length;
      return tasks.filter((task) => task.status === tab).length;
    },
    [activeFile],
  );

  return (
    <div className="flex flex-col rounded-2xl border border-dark-border/80 bg-dark-surface1/50 p-3 shadow-2xl shadow-black/20 backdrop-blur-sm lg:max-h-[calc(100vh-7rem)]" {...tokenAttrs("colors.surface.background", "colors.border.default")}>
      <div className="mb-2 flex items-center justify-end">
        <code className="rounded-md border border-dark-border/60 bg-dark-surface2/70 px-2 py-0.5 text-[10px] text-dark-text-muted">
          {theme.appearance}
        </code>
      </div>

      <div
        className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border shadow-2xl"
        style={{
          borderColor: rgbaFromTheme(theme, "colors.border.default"),
          backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
          boxShadow: `0 24px 64px ${argbToCssRgba(theme.colors.shadow)}`,
        }}
        {...tokenAttrs("colors.surface.surface1", "colors.border.default", "colors.shadow")}
      >
        <div
          className="flex h-9 items-center justify-between border-b px-3"
          style={{ borderColor: rgbaFromTheme(theme, "colors.border.default") }}
          {...tokenAttrs("colors.surface.surface1", "colors.border.default")}
        >
          <div className="flex items-center gap-2">
            <img src="/logo.svg" alt="" className="h-4 w-4 rounded" />
            <span className="text-[12px] font-semibold">
              <span style={{ color: rgbaFromTheme(theme, "colors.accent.color") }} {...tokenAttrs("colors.accent.color")}>Flux</span>
              <span style={{ color: rgbaFromTheme(theme, "colors.text.primary") }} {...tokenAttrs("colors.text.primary")}>Down</span>
            </span>
          </div>
          <div className="flex items-center" {...tokenAttrs("colors.text.secondary", "colors.element.hover", "colors.border.default", "colors.status.error")}>
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
            >
              <Pause className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
            >
              <Play className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
            >
              <Settings className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
              onClick={onToggleAppearance}
            >
              {theme.appearance === "dark" ? <Sun className="h-3.5 w-3.5" /> : <Moon className="h-3.5 w-3.5" />}
            </button>
            <div className="mx-0.5 h-4 w-px" style={{ backgroundColor: rgbaFromTheme(theme, "colors.border.default") }} />
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
            >
              <Minus className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
            >
              <Square className="h-3.5 w-3.5" />
            </button>
            <button
              type="button"
              className="inline-flex h-8 w-8 items-center justify-center rounded-sm"
              style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>

        <div className="flex min-h-0 flex-1">
          <aside
            className="hidden w-[170px] flex-col border-r md:flex"
            style={{
              borderColor: rgbaFromTheme(theme, "colors.border.default"),
              backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
            }}
            {...tokenAttrs("colors.surface.surface1", "colors.border.default")}
          >
            <div className="px-4 pt-3 text-[10px] font-medium uppercase tracking-wide" style={{ color: rgbaFromTheme(theme, "colors.text.muted") }}>
              {t("mockup.category")}
            </div>
            <div className="mt-1 space-y-0.5 px-2">
              {PREVIEW_SIDEBAR.map((item) => {
                const active = activeFile === item.key;
                return (
                  <button
                    key={item.key}
                    type="button"
                    onClick={() => {
                      setActiveFile(item.key);
                      setActiveTab("all");
                    }}
                    className="flex h-8 w-full items-center justify-between rounded-md px-2 text-left text-xs"
                    style={{
                      backgroundColor: active ? rgbaFromTheme(theme, "colors.element.selected") : "transparent",
                    }}
                    {...tokenAttrs("colors.element.selected", "colors.accent.color", "colors.text.secondary")}
                  >
                    <span className="inline-flex items-center gap-2" style={{ color: active ? rgbaFromTheme(theme, "colors.accent.color") : rgbaFromTheme(theme, "colors.text.secondary") }}>
                      <SidebarIcon category={item.key} color={active ? rgbaFromTheme(theme, "colors.accent.color") : rgbaFromTheme(theme, "colors.text.secondary")} />
                      {t(item.labelKey)}
                    </span>
                    <span style={{ color: active ? rgbaFromTheme(theme, "colors.accent.color") : rgbaFromTheme(theme, "colors.text.muted") }}>{countByFile(item.key)}</span>
                  </button>
                );
              })}
            </div>
          </aside>

          <div className="flex min-w-0 flex-1 flex-col" style={{ backgroundColor: rgbaFromTheme(theme, "colors.surface.background") }} {...tokenAttrs("colors.surface.background")}>
            <div
              className="flex h-[38px] items-center justify-between border-b px-2"
              style={{ borderColor: rgbaFromTheme(theme, "colors.border.default") }}
              {...tokenAttrs("colors.border.default", "colors.input.background", "colors.input.border", "colors.text.secondary", "colors.accent.color", "colors.accent.foreground")}
            >
              <div
                className="flex h-7 min-w-0 items-center gap-2 rounded-md border px-2 text-[11px]"
                style={{
                  borderColor: rgbaFromTheme(theme, "colors.input.border"),
                  backgroundColor: rgbaFromTheme(theme, "colors.input.background"),
                  color: rgbaFromTheme(theme, "colors.text.secondary"),
                }}
              >
                <Search className="h-3.5 w-3.5" />
                <span>Ctrl+F</span>
              </div>
              <div className="flex items-center gap-1">
                <button
                  type="button"
                  className="inline-flex h-7 items-center justify-center rounded-md border px-2 text-[11px] font-medium"
                  style={{
                    borderColor: rgbaFromTheme(theme, "colors.border.default"),
                    color: rgbaFromTheme(theme, "colors.text.secondary"),
                    backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
                  }}
                >
                  <Pause className="mr-1 h-3.5 w-3.5" />
                  {t("mockup.btnPause")}
                </button>
                <button
                  type="button"
                  className="inline-flex h-7 items-center justify-center rounded-md px-2 text-[11px] font-semibold"
                  style={{
                    backgroundColor: rgbaFromTheme(theme, "colors.accent.color"),
                    color: rgbaFromTheme(theme, "colors.accent.foreground"),
                  }}
                >
                  <Plus className="mr-1 h-3.5 w-3.5" />
                  {t("mockup.download")}
                </button>
                <button
                  type="button"
                  className="inline-flex h-7 w-7 items-center justify-center rounded-md border"
                  style={{
                    borderColor: rgbaFromTheme(theme, "colors.border.default"),
                    color: rgbaFromTheme(theme, "colors.text.secondary"),
                    backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
                  }}
                >
                  <Settings className="h-3.5 w-3.5" />
                </button>
              </div>
            </div>

            <div
              className="flex h-[34px] items-center gap-1 overflow-x-auto border-b px-2"
              style={{ borderColor: rgbaFromTheme(theme, "colors.border.default") }}
              {...tokenAttrs("colors.border.default", "colors.text.primary", "colors.text.muted", "colors.accent.color")}
            >
              {PREVIEW_TABS.map((tab) => {
                const active = activeTab === tab.key;
                return (
                  <button
                    key={tab.key}
                    type="button"
                    onClick={() => setActiveTab(tab.key)}
                    className="relative shrink-0 rounded px-2 py-1 text-[11px]"
                    style={{ color: active ? rgbaFromTheme(theme, "colors.text.primary") : rgbaFromTheme(theme, "colors.text.muted") }}
                  >
                    {t(tab.labelKey)} ({countByTab(tab.key)})
                    {active && <span className="absolute inset-x-0 -bottom-[7px] h-[2px]" style={{ backgroundColor: rgbaFromTheme(theme, "colors.accent.color") }} />}
                  </button>
                );
              })}
            </div>

            <div
              className="flex h-[30px] items-center border-b px-3 text-[10px] font-medium"
              style={{
                borderColor: rgbaFromTheme(theme, "colors.border.default"),
                backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
                color: rgbaFromTheme(theme, "colors.text.muted"),
              }}
              {...tokenAttrs("colors.surface.surface1", "colors.border.default", "colors.text.muted")}
            >
              <div className="min-w-0 flex-1">{t("mockup.colFilename")}</div>
              <div className="w-[120px] text-center">{t("mockup.colProgress")}</div>
              <div className="hidden w-[80px] text-center sm:block">{t("mockup.colSpeed")}</div>
              <div className="hidden w-[56px] text-right sm:block">{t("mockup.colStatus")}</div>
            </div>

            <div className="tb-scrollbar min-h-0 flex-1 overflow-y-auto" style={themedScrollbarVars(theme)}>
              {filteredTasks.map((task) => {
                const selected = selectedTask.id === task.id;
                const statusColor = getTaskStatusColor(theme, task.status);
                return (
                  <button
                    key={task.id}
                    type="button"
                    onClick={() => setSelectedTaskId(task.id)}
                    className="flex h-[46px] w-full items-center border-b px-3 text-left"
                    style={{
                      borderColor: rgbaFromTheme(theme, "colors.border.default"),
                      backgroundColor: selected ? rgbaFromTheme(theme, "colors.element.selected") : "transparent",
                    }}
                    {...tokenAttrs("colors.border.default", "colors.element.selected")}
                  >
                    <div className="flex min-w-0 flex-1 items-center gap-2">
                      <div
                        className="flex h-7 w-7 items-center justify-center rounded text-[9px] font-semibold uppercase"
                        style={{
                          backgroundColor: rgbaFromTheme(theme, "colors.surface.surface2"),
                          color: selected ? rgbaFromTheme(theme, "colors.accent.color") : rgbaFromTheme(theme, "colors.text.secondary"),
                        }}
                        {...tokenAttrs("colors.surface.surface2", "colors.accent.color", "colors.text.secondary")}
                      >
                        {task.ext}
                      </div>
                      <div className="min-w-0">
                        <div className="truncate text-[11px]" style={{ color: selected ? rgbaFromTheme(theme, "colors.accent.color") : rgbaFromTheme(theme, "colors.text.primary") }}>{task.name}</div>
                        <div className="truncate text-[10px]" style={{ color: rgbaFromTheme(theme, "colors.text.muted") }}>{task.subtitle}</div>
                      </div>
                    </div>
                    <div className="flex w-[120px] items-center gap-2 pr-2">
                      <div className="h-[3px] flex-1 overflow-hidden rounded" style={{ backgroundColor: rgbaFromTheme(theme, "colors.surface.surface3") }}>
                        <div className="h-full rounded" style={{ width: `${task.progress}%`, backgroundColor: statusColor }} />
                      </div>
                      <span className="text-[10px] tabular-nums" style={{ color: rgbaFromTheme(theme, "colors.text.secondary") }}>{task.progress.toFixed(1)}%</span>
                    </div>
                    <div className="hidden w-[80px] text-center text-[10px] sm:block" style={{ color: task.status === "downloading" ? rgbaFromTheme(theme, "colors.status.success") : rgbaFromTheme(theme, "colors.text.muted") }}>{task.speed}</div>
                    <div className="hidden w-[56px] text-right text-[10px] sm:block" style={{ color: statusColor }}>{getTaskStatusText(task, t)}</div>
                  </button>
                );
              })}
            </div>

            <div
              className="flex h-6 items-center gap-3 border-t px-3 text-[10px]"
              style={{
                borderColor: rgbaFromTheme(theme, "colors.border.default"),
                backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
                color: rgbaFromTheme(theme, "colors.text.muted"),
              }}
              {...tokenAttrs("colors.surface.surface1", "colors.border.default", "colors.text.muted", "colors.status.success")}
            >
              <span>{t("mockup.downloading")}</span>
              <span style={{ color: rgbaFromTheme(theme, "colors.status.success") }}>12.8 MB/s</span>
              <span>{t("mockup.statusActive", { n: "2", p: "1", t: "5" })}</span>
            </div>
          </div>

          <aside
            className="hidden w-[280px] shrink-0 border-l lg:flex lg:flex-col"
            style={{
              borderColor: rgbaFromTheme(theme, "colors.border.default"),
              backgroundColor: rgbaFromTheme(theme, "colors.surface.surface1"),
            }}
            {...tokenAttrs("colors.surface.surface1", "colors.border.default")}
          >
            <div className="flex h-[36px] items-center justify-between border-b px-3" style={{ borderColor: rgbaFromTheme(theme, "colors.border.default") }}>
              <span className="text-[12px] font-semibold" style={{ color: rgbaFromTheme(theme, "colors.text.primary") }}>{t("mockup.detail")}</span>
              <X className="h-3.5 w-3.5" style={{ color: rgbaFromTheme(theme, "colors.text.muted") }} />
            </div>

            <div className="tb-scrollbar min-h-0 flex-1 space-y-3 overflow-y-auto p-3" style={themedScrollbarVars(theme)}>
              <div className="flex items-center gap-2.5">
                <div className="flex h-9 w-9 items-center justify-center rounded-md text-[10px] font-semibold uppercase" style={{ backgroundColor: rgbaFromTheme(theme, "colors.surface.surface2"), color: rgbaFromTheme(theme, "colors.text.secondary") }}>
                  {selectedTask.ext}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-medium" style={{ color: rgbaFromTheme(theme, "colors.text.primary") }}>{selectedTask.name}</div>
                  <div className="truncate text-[10px]" style={{ color: rgbaFromTheme(theme, "colors.text.muted") }}>{selectedTask.subtitle}</div>
                </div>
              </div>

              <div className="text-2xl font-semibold tabular-nums" style={{ color: rgbaFromTheme(theme, "colors.text.primary") }}>
                {selectedTask.progress.toFixed(1)}%
              </div>

              <div className="flex h-1.5 overflow-hidden rounded" style={{ backgroundColor: rgbaFromTheme(theme, "colors.surface.surface3") }}>
                {selectedTask.segments.map((segment) => {
                  const size = segment.endByte - segment.startByte + 1;
                  const width = (size / selectedTask.totalBytes) * 100;
                  const progress = Math.min(1, segment.downloadedBytes / size) * 100;
                  return (
                    <div key={segment.index} className="h-full" style={{ width: `${width}%` }}>
                      <div className="h-full" style={{ width: `${progress}%`, backgroundColor: colorForSegment(theme, segment.index) }} />
                    </div>
                  );
                })}
              </div>

              <div className="space-y-1.5">
                <div className="text-[11px] font-medium" style={{ color: rgbaFromTheme(theme, "colors.text.muted") }}>{t("mockup.distLabel")}</div>
                <div
                  className="rounded-md border p-1.5"
                  style={{
                    borderColor: rgbaFromTheme(theme, "colors.border.default"),
                    backgroundColor: rgbaFromTheme(theme, "colors.surface.surface2"),
                  }}
                >
                  <div className="grid gap-1" style={{ gridTemplateColumns: "repeat(44, minmax(0, 1fr))" }}>
                    {gridCells.map((filled, index) => (
                      <div
                        // eslint-disable-next-line react/no-array-index-key
                        key={`grid-cell-${index}`}
                        className="h-1 w-1 rounded-[1px]"
                        style={{
                          backgroundColor: filled ? colorForSegment(theme, index % selectedTask.segments.length) : rgbaFromTheme(theme, "colors.surface.surface3"),
                        }}
                      />
                    ))}
                  </div>
                </div>
              </div>

              <div className="space-y-1">
                {[
                  { label: t("mockup.labelSize"), value: selectedTask.size, path: "colors.text.secondary" },
                  { label: t("mockup.labelDownloaded"), value: formatBytes(selectedTask.downloadedBytes), path: "colors.text.secondary" },
                  { label: t("mockup.labelSpeed"), value: selectedTask.speed, path: "colors.text.secondary" },
                  { label: t("mockup.labelRemaining"), value: selectedTask.eta, path: "colors.text.secondary" },
                  { label: t("mockup.labelStatus"), value: getTaskStatusText(selectedTask, t), path: "colors.accent.color" },
                  { label: t("mockup.labelThreads"), value: t("mockup.threadsValue", { n: String(selectedTask.segments.length) }), path: "colors.text.secondary" },
                  { label: t("mockup.labelPath"), value: selectedTask.saveDir, path: "colors.text.secondary" },
                  { label: t("mockup.labelUrl"), value: selectedTask.url, path: "colors.text.secondary" },
                ].map((row) => (
                  <div key={row.label} className="grid grid-cols-[72px_minmax(0,1fr)] gap-1.5">
                    <span className="text-[11px]" style={{ color: rgbaFromTheme(theme, "colors.text.muted") }}>{row.label}</span>
                    <span className="truncate text-[11px]" style={{ color: rgbaFromTheme(theme, row.path) }}>{row.value}</span>
                  </div>
                ))}
              </div>
            </div>

            <div className="flex items-center gap-2 border-t px-3 py-2" style={{ borderColor: rgbaFromTheme(theme, "colors.border.default") }}>
              <button
                type="button"
                className="inline-flex items-center justify-center rounded-md px-2.5 py-1 text-[11px] font-semibold"
                style={{
                  backgroundColor: rgbaFromTheme(theme, "colors.accent.color"),
                  color: rgbaFromTheme(theme, "colors.accent.foreground"),
                }}
              >
                {selectedTask.status === "downloading" ? t("mockup.btnPause") : t("mockup.btnResume")}
              </button>
              <button
                type="button"
                className="inline-flex items-center justify-center rounded-md border px-2.5 py-1 text-[11px] font-semibold"
                style={{
                  borderColor: rgbaFromTheme(theme, "colors.status.error"),
                  color: rgbaFromTheme(theme, "colors.status.error"),
                  backgroundColor: rgbaFromTheme(theme, "colors.element.active"),
                }}
              >
                {t("mockup.btnDelete")}
              </button>
            </div>
          </aside>
        </div>
      </div>

      <div className="mt-2 flex gap-2 text-[10px] text-dark-text-muted">
        <div className="flex-1 rounded-lg border border-dark-border/60 bg-dark-surface2/30 px-3 py-1.5 backdrop-blur-sm">
          <span className="font-semibold text-dark-text-secondary">{t("tb.compat.title")}</span>
          <span className="ml-1.5">{t("tb.compat.desc")}</span>
        </div>
        <div className="flex-1 rounded-lg border border-dark-border/60 bg-dark-surface2/30 px-3 py-1.5 backdrop-blur-sm">
          <span className="font-semibold text-dark-text-secondary">{t("tb.rightClickHint")}</span>
          <span className="ml-1.5">{t("tb.rightClickDesc")}</span>
        </div>
      </div>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${bytes} B`;
}
