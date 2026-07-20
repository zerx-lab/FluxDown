import { useState, useEffect, useCallback, useRef } from "react";
import type { Messages } from "@/lib/locales";
import { motion, AnimatePresence } from "framer-motion";
import {
  History,
  Tag,
  Calendar,
  Loader2,
  ChevronDown,
  FileCode,
  FileText,
  Check,
  Download,
  Package,
  MonitorDown,
  Cpu,
  Terminal,
  Smartphone,
} from "lucide-react";
import { useLocale } from "@/lib/i18n";

interface ReleaseAsset {
  name: string;
  size: number;
  download_url: string;
}

interface Release {
  tag: string;
  version: string;
  published_at: string;
  body: string;
  /** 预览预发布版（vX.Y.Z-rc.N）标记 */
  prerelease?: boolean;
  assets: ReleaseAsset[];
}

type Channel = "stable" | "frontier";

const PER_PAGE = 10;

/** 格式化文件大小 */
function formatSize(bytes: number): string {
  if (bytes === 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** 根据文件名推断平台/类型标签与图标 */
function inferAssetMeta(name: string): {
  label: string;
  sub: string;
  icon: React.ReactNode;
} {
  const lower = name.toLowerCase();

  // CLI（独立 cli-v* release，命名 FluxDown-CLI-<ver>-<os>-<arch>.<ext>）
  if (lower.startsWith("fluxdown-cli-")) {
    const platMatch = lower.match(
      /-(windows|linux|macos)-(x64|arm64)\.(zip|tar\.gz)$/,
    );
    const sub = platMatch ? `${platMatch[1]} ${platMatch[2]}` : "命令行工具";
    return {
      label: "CLI",
      sub,
      icon: <Terminal className="w-3.5 h-3.5" />,
    };
  }
  // Android（独立 mobile-v* release，命名 FluxDown-<ver>-android-<abi>.apk）
  if (lower.includes("-android-") && lower.endsWith(".apk")) {
    const abiMatch = lower.match(/-android-([a-z0-9_-]+)\.apk$/);
    return {
      label: "Android",
      sub: abiMatch ? `${abiMatch[1]} APK` : "APK",
      icon: <Smartphone className="w-3.5 h-3.5" />,
    };
  }

  // Windows
  if (
    lower.endsWith("-windows-x64-setup.exe") ||
    lower.endsWith("-windows-setup.exe")
  ) {
    return {
      label: "Windows",
      sub: "x64 安装包",
      icon: <MonitorDown className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-windows-arm64-setup.exe")) {
    return {
      label: "Windows",
      sub: "ARM64 安装包",
      icon: <MonitorDown className="w-3.5 h-3.5" />,
    };
  }
  if (
    lower.endsWith("-windows-x64-portable.zip") ||
    lower.endsWith("-windows-portable.zip")
  ) {
    return {
      label: "Windows",
      sub: "x64 便携版",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-windows-arm64-portable.zip")) {
    return {
      label: "Windows",
      sub: "ARM64 便携版",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  // macOS
  if (lower.endsWith("-macos-arm64.dmg")) {
    return {
      label: "macOS",
      sub: "Apple Silicon 安装镜像",
      icon: <MonitorDown className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-macos-x64.dmg")) {
    return {
      label: "macOS",
      sub: "Intel (x64) 安装镜像",
      icon: <MonitorDown className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith(".dmg")) {
    return {
      label: "macOS",
      sub: "安装镜像",
      icon: <MonitorDown className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-macos-arm64.pkg")) {
    return {
      label: "macOS",
      sub: "Apple Silicon 安装包",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-macos-x64.pkg")) {
    return {
      label: "macOS",
      sub: "Intel (x64) 安装包",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith(".pkg")) {
    return {
      label: "macOS",
      sub: "安装包",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-macos-arm64.tar.gz")) {
    return {
      label: "macOS",
      sub: "Apple Silicon Tarball",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-macos-x64.tar.gz")) {
    return {
      label: "macOS",
      sub: "Intel (x64) Tarball",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  // Linux
  if (lower.endsWith("-linux-x64.appimage")) {
    return {
      label: "Linux",
      sub: "AppImage x64",
      icon: <Cpu className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-linux-x64.deb")) {
    return {
      label: "Linux",
      sub: "Debian/Ubuntu",
      icon: <Cpu className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-linux-x64.pkg.tar.zst")) {
    return {
      label: "Linux",
      sub: "Arch Linux",
      icon: <Cpu className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-linux-x64.tar.gz")) {
    return {
      label: "Linux",
      sub: "x64 Tarball",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith(".rpm")) {
    return {
      label: "Linux",
      sub: "RPM 包",
      icon: <Cpu className="w-3.5 h-3.5" />,
    };
  }
  // Extension
  if (lower.endsWith("-chrome.zip") || lower.endsWith("-extension.zip")) {
    return {
      label: "扩展",
      sub: "Chrome / Edge",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  if (lower.endsWith("-firefox.xpi")) {
    return {
      label: "扩展",
      sub: "Firefox",
      icon: <Package className="w-3.5 h-3.5" />,
    };
  }
  // fallback
  return {
    label: "其他",
    sub: name,
    icon: <Download className="w-3.5 h-3.5" />,
  };
}

/** 平台组排序权重 */
const PLATFORM_ORDER: Record<string, number> = {
  Windows: 0,
  macOS: 1,
  Linux: 2,
  扩展: 3,
  Android: 4,
  CLI: 5,
  其他: 99,
};

/** 将 assets 按平台分组 */
function groupAssets(assets: ReleaseAsset[]): Array<{
  platform: string;
  items: Array<{
    asset: ReleaseAsset;
    meta: ReturnType<typeof inferAssetMeta>;
  }>;
}> {
  const map = new Map<
    string,
    Array<{ asset: ReleaseAsset; meta: ReturnType<typeof inferAssetMeta> }>
  >();

  for (const asset of assets) {
    const meta = inferAssetMeta(asset.name);
    if (!map.has(meta.label)) map.set(meta.label, []);
    map.get(meta.label)!.push({ asset, meta });
  }

  return Array.from(map.entries())
    .sort(([a], [b]) => (PLATFORM_ORDER[a] ?? 50) - (PLATFORM_ORDER[b] ?? 50))
    .map(([platform, items]) => ({ platform, items }));
}

/** 对非代码块的 Markdown 段落做 HTML 转义 + 简单 Markdown 处理 */
function renderInlineMarkdown(segment: string): string {
  return segment
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
      '<h4 class="text-sm font-semibold text-dark-text mt-5 mb-2">$1</h4>',
    )
    .replace(
      /^## (.+)$/gm,
      '<h3 class="text-base font-semibold text-dark-text mt-6 mb-2">$1</h3>',
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

/** 简易 Markdown → HTML */
function renderMarkdown(md: string): string {
  const FENCE_RE = /^([ \t]*)```([^\n]*)\n([\s\S]*?)^\1```[ \t]*$/gm;
  const parts: Array<{ type: "code" | "text"; content: string }> = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  while ((match = FENCE_RE.exec(md)) !== null) {
    if (match.index > lastIndex) {
      parts.push({ type: "text", content: md.slice(lastIndex, match.index) });
    }
    const _indent = match[1];
    const lang = match[2];
    const code = match[3];
    const indentLen = _indent.length;
    const dedented = indentLen
      ? code
          .split("\n")
          .map((line) =>
            line.startsWith(_indent) ? line.slice(indentLen) : line,
          )
          .join("\n")
      : code;
    const escaped = dedented
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    const langAttr = lang.trim()
      ? ` data-lang="${lang.trim().replace(/"/g, "&quot;")}"`
      : "";
    const html = `<pre class="changelog-pre my-3 rounded-lg bg-dark-surface3 border border-dark-border overflow-x-auto p-4"${langAttr}><code class="text-xs font-mono text-dark-text-secondary leading-relaxed whitespace-pre">${escaped.replace(/\n$/, "")}</code></pre>`;
    parts.push({ type: "code", content: html });
    lastIndex = match.index + match[0].length;
  }

  if (lastIndex < md.length) {
    parts.push({ type: "text", content: md.slice(lastIndex) });
  }

  return parts
    .map((part) =>
      part.type === "code" ? part.content : renderInlineMarkdown(part.content),
    )
    .join("");
}

function formatDate(dateStr: string, locale: string): string {
  const date = new Date(dateStr);
  return date.toLocaleDateString(locale === "zh" ? "zh-CN" : "en-US", {
    year: "numeric",
    month: "long",
    day: "numeric",
    timeZone: "Asia/Shanghai",
  });
}

/** 去除内联 Markdown 语法，返回纯文本 */
function cleanInline(text: string): string {
  return text
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/\*(.+?)\*/g, "$1")
    .replace(/`([^`]+)`/g, "[$1]");
}

/** 双语 release body 的语言标记（由 release 工作流翻译步骤写入） */
const LANG_MARKER_RE = /<!--\s*fluxdown:lang:(zh|en)\s*-->/g;

/**
 * 从双语 release body 中取出当前语言区块。
 * 无标记（历史版本 / 翻译失败回退）时原样返回全文。
 */
function pickLocaleBody(body: string, locale: string): string {
  const matches = [...body.matchAll(LANG_MARKER_RE)];
  if (matches.length === 0) return body;

  const sections = new Map<string, string>();
  for (let i = 0; i < matches.length; i++) {
    const start = (matches[i].index ?? 0) + matches[i][0].length;
    const end =
      i + 1 < matches.length ? (matches[i + 1].index ?? body.length) : body.length;
    sections.set(matches[i][1], body.slice(start, end).trim());
  }
  return sections.get(locale) ?? sections.get("zh") ?? body;
}

/** 将 release body 转为适合 QQ群公告粘贴的纯文本 */
function toPlainText(release: Release, locale: string): string {
  const date = formatDate(release.published_at, locale);
  const result: string[] = [
    `【FluxDown ${release.tag} 更新日志】`,
    `📅 ${date}`,
    "",
  ];

  let counter = 0;
  let lastWasBlank = true;

  for (const raw of pickLocaleBody(release.body, locale).split("\n")) {
    const line = raw.trimEnd();
    const trimmed = line.trim();

    if (!trimmed) {
      if (!lastWasBlank) {
        result.push("");
        lastWasBlank = true;
      }
      continue;
    }
    lastWasBlank = false;

    if (trimmed.startsWith("## ")) {
      counter = 0;
      result.push(`▌ ${trimmed.slice(3).trim()}`);
    } else if (trimmed.startsWith("### ")) {
      counter = 0;
      result.push(`  ◆ ${trimmed.slice(4).trim()}`);
    } else if (trimmed.startsWith("- ")) {
      counter += 1;
      result.push(`${counter}. ${cleanInline(trimmed.slice(2).trim())}`);
    } else {
      result.push(cleanInline(trimmed));
    }
  }

  while (result.length > 0 && result[result.length - 1] === "") {
    result.pop();
  }

  return result.join("\n");
}

function timeAgo(dateStr: string, locale: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const days = Math.floor((now - then) / (1000 * 60 * 60 * 24));
  if (locale === "zh") {
    if (days === 0) return "今天";
    if (days === 1) return "昨天";
    if (days < 30) return `${days} 天前`;
    if (days < 365) return `${Math.floor(days / 30)} 个月前`;
    return `${Math.floor(days / 365)} 年前`;
  }
  if (days === 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days} days ago`;
  if (days < 365) return `${Math.floor(days / 30)} months ago`;
  return `${Math.floor(days / 365)} years ago`;
}

function CopyButtons({
  release,
  locale,
  t,
}: {
  release: Release;
  locale: string;
  t: (key: keyof Messages) => string;
}) {
  const [mdState, setMdState] = useState<"idle" | "copied">("idle");
  const [textState, setTextState] = useState<"idle" | "copied">("idle");

  const copy = async (content: string, which: "md" | "text") => {
    try {
      await navigator.clipboard.writeText(content);
    } catch {
      const el = document.createElement("textarea");
      el.value = content;
      el.style.position = "fixed";
      el.style.opacity = "0";
      document.body.appendChild(el);
      el.select();
      document.execCommand("copy");
      document.body.removeChild(el);
    }
    if (which === "md") {
      setMdState("copied");
      setTimeout(() => setMdState("idle"), 2000);
    } else {
      setTextState("copied");
      setTimeout(() => setTextState("idle"), 2000);
    }
  };

  return (
    <div className="ml-auto flex items-center gap-0.5 shrink-0">
      {/* Copy Markdown */}
      <button
        onClick={() => copy(pickLocaleBody(release.body, locale), "md")}
        title={t("changelog.copyMd") + " (Markdown)"}
        className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs text-dark-text-muted hover:text-dark-text hover:bg-dark-surface2 transition-all duration-150 cursor-pointer select-none"
      >
        {mdState === "copied" ? (
          <Check className="w-3 h-3 text-brand-sky shrink-0" />
        ) : (
          <FileCode className="w-3 h-3 shrink-0" />
        )}
        <span className={mdState === "copied" ? "text-brand-sky" : ""}>
          {mdState === "copied" ? t("changelog.copied") : t("changelog.copyMd")}
        </span>
      </button>

      {/* Copy plain text */}
      <button
        onClick={() => copy(toPlainText(release, locale), "text")}
        title={t("changelog.copyPlain") + " (QQ群公告)"}
        className="inline-flex items-center gap-1 px-2 py-1 rounded text-xs text-dark-text-muted hover:text-dark-text hover:bg-dark-surface2 transition-all duration-150 cursor-pointer select-none"
      >
        {textState === "copied" ? (
          <Check className="w-3 h-3 text-brand-sky shrink-0" />
        ) : (
          <FileText className="w-3 h-3 shrink-0" />
        )}
        <span className={textState === "copied" ? "text-brand-sky" : ""}>
          {textState === "copied"
            ? t("changelog.copied")
            : t("changelog.copyPlain")}
        </span>
      </button>
    </div>
  );
}

/** 单个 asset 下载按钮 */
function AssetButton({ asset }: { asset: ReleaseAsset }) {
  const meta = inferAssetMeta(asset.name);
  return (
    <a
      href={asset.download_url}
      download
      title={asset.name}
      className="group flex items-center gap-2 px-3 py-2 rounded-lg border border-dark-border bg-dark-surface2 hover:border-brand-sky/40 hover:bg-brand-sky/5 transition-all duration-150"
    >
      <span className="text-dark-text-muted group-hover:text-brand-sky transition-colors shrink-0">
        {meta.icon}
      </span>
      <div className="min-w-0 flex-1">
        <div className="text-xs font-medium text-dark-text group-hover:text-brand-sky transition-colors leading-tight truncate">
          {meta.sub}
        </div>
        <div className="text-[10px] text-dark-text-muted leading-tight mt-0.5">
          {formatSize(asset.size)}
        </div>
      </div>
      <Download className="w-3 h-3 text-dark-text-muted group-hover:text-brand-sky transition-colors shrink-0" />
    </a>
  );
}

/** 版本下载面板（可折叠） */
function AssetsPanel({
  release,
  t,
}: {
  release: Release;
  t: (key: keyof Messages) => string;
}) {
  const [open, setOpen] = useState(false);
  const groups = groupAssets(release.assets);

  if (release.assets.length === 0) return null;

  return (
    <div className="border-t border-dark-border">
      {/* 折叠触发按钮 */}
      <button
        onClick={() => setOpen((v) => !v)}
        className="w-full flex items-center gap-2 px-5 py-3 text-xs text-dark-text-muted hover:text-dark-text hover:bg-dark-surface2/50 transition-all duration-150 cursor-pointer select-none"
      >
        <Download className="w-3.5 h-3.5 shrink-0" />
        <span className="font-medium">
          {t("changelog.downloadAssets")}
          <span className="ml-1.5 px-1.5 py-0.5 rounded-full bg-dark-surface3 text-dark-text-muted text-[10px] font-normal">
            {release.assets.length}
          </span>
        </span>
        <ChevronDown
          className={`w-3.5 h-3.5 ml-auto shrink-0 transition-transform duration-200 ${open ? "rotate-180" : ""}`}
        />
      </button>

      {/* 下载列表 */}
      <AnimatePresence initial={false}>
        {open && (
          <motion.div
            key="assets"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: "easeInOut" }}
            className="overflow-hidden"
          >
            <div className="px-5 pb-4 pt-1 space-y-4">
              {groups.map(({ platform, items }) => (
                <div key={platform}>
                  {/* 平台标题 */}
                  <div className="flex items-center gap-2 mb-2">
                    <span className="text-[10px] font-semibold text-dark-text-muted uppercase tracking-wider">
                      {platform}
                    </span>
                    <div className="flex-1 h-px bg-dark-border" />
                  </div>
                  {/* 文件网格 */}
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
                    {items.map(({ asset }) => (
                      <AssetButton key={asset.name} asset={asset} />
                    ))}
                  </div>
                </div>
              ))}
              {/* 免责说明 */}
              <p className="text-[10px] text-dark-text-muted leading-relaxed pt-1">
                {t("changelog.assetsNote")}
              </p>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

export default function ChangelogSection() {
  const { locale, t } = useLocale();
  const [channel, setChannel] = useState<Channel>("stable");
  const [releases, setReleases] = useState<Release[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState("");
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  const initialFetched = useRef(false);

  const fetchPage = useCallback(
    async (p: number, append: boolean, ch: Channel) => {
      if (append) {
        setLoadingMore(true);
      } else {
        setLoading(true);
      }
      setError("");

      try {
        const res = await fetch(
          `/api/changelog?page=${p}&per_page=${PER_PAGE}&channel=${ch}`,
        );
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = await res.json();

        const incoming: Release[] = data.releases || [];
        setReleases((prev) => (append ? [...prev, ...incoming] : incoming));
        setHasMore(data.has_more ?? false);
        setPage(p);
      } catch (err) {
        setError(String(err));
      } finally {
        setLoading(false);
        setLoadingMore(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (initialFetched.current) return;
    initialFetched.current = true;
    fetchPage(1, false, "stable");
  }, [fetchPage]);

  const handleSwitchChannel = (ch: Channel) => {
    if (ch === channel || loading) return;
    setChannel(ch);
    setReleases([]);
    fetchPage(1, false, ch);
  };

  const handleLoadMore = () => {
    if (loadingMore || !hasMore) return;
    fetchPage(page + 1, true, channel);
  };

  return (
    <section className="relative py-20 sm:py-28 bg-dark-bg">
      <div className="mx-auto max-w-3xl px-4 sm:px-6 lg:px-8">
        {/* Header */}
        <motion.div
          className="text-center mb-14"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5 }}
        >
          <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-brand-sky/10 text-brand-sky border border-brand-sky/20 uppercase tracking-widest">
            <History className="w-3 h-3" />
            {t("changelog.badge")}
          </span>
          <h1 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
            {t("changelog.title")}
            <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("changelog.titleHighlight")}
            </span>
          </h1>
          <p className="mt-4 text-dark-text-secondary text-base sm:text-lg max-w-xl mx-auto">
            {t("changelog.subtitle")}
          </p>
        </motion.div>

        {/* Channel tabs: 稳定版 / 预览版 */}
        <div className="mb-10 flex flex-col items-center gap-3">
          <div className="inline-flex items-center rounded-lg border border-dark-border bg-dark-surface1 p-1">
            {(["stable", "frontier"] as const).map((ch) => (
              <button
                key={ch}
                onClick={() => handleSwitchChannel(ch)}
                className={`px-4 py-1.5 rounded-md text-sm font-medium transition-colors cursor-pointer ${
                  channel === ch
                    ? ch === "frontier"
                      ? "bg-amber-500/15 text-amber-400"
                      : "bg-brand-sky/15 text-brand-sky"
                    : "text-dark-text-muted hover:text-dark-text-secondary"
                }`}
              >
                {ch === "stable"
                  ? t("changelog.tabStable")
                  : t("changelog.tabFrontier")}
              </button>
            ))}
          </div>
          {channel === "frontier" && (
            <p className="text-xs text-dark-text-muted max-w-md text-center">
              {t("changelog.frontierHint")}
            </p>
          )}
        </div>

        {/* Initial loading */}
        {loading && (
          <div className="flex items-center justify-center py-20">
            <Loader2 className="w-6 h-6 text-brand-sky animate-spin" />
          </div>
        )}

        {/* Error */}
        {error && !loading && (
          <div className="text-center py-12">
            <p className="text-sm text-danger">{t("changelog.error")}</p>
          </div>
        )}

        {/* Empty */}
        {!loading && !error && releases.length === 0 && (
          <div className="text-center py-12">
            <p className="text-sm text-dark-text-muted">
              {t("changelog.empty")}
            </p>
          </div>
        )}

        {/* Release timeline */}
        {!loading && releases.length > 0 && (
          <div className="relative">
            {/* Timeline line */}
            <div className="absolute left-[19px] top-2 bottom-2 w-px bg-dark-border hidden sm:block" />

            <div className="space-y-8">
              {releases.map((release, index) => (
                <motion.article
                  key={release.tag}
                  initial={{ opacity: 0, y: 20 }}
                  whileInView={{ opacity: 1, y: 0 }}
                  viewport={{ once: true, margin: "-50px" }}
                  transition={{
                    duration: 0.4,
                    delay: Math.min(index * 0.05, 0.3),
                  }}
                  className="relative sm:pl-12"
                >
                  {/* Timeline dot */}
                  <div
                    className={`absolute left-2.5 top-1.5 w-3 h-3 rounded-full border-2 bg-dark-bg hidden sm:block ${release.prerelease ? "border-amber-400" : "border-brand-sky"}`}
                  />

                  {/* Card */}
                  <div className="rounded-xl border border-dark-border bg-dark-surface1 overflow-hidden">
                    {/* Card header */}
                    <div className="flex flex-wrap items-center gap-3 px-5 py-4 border-b border-dark-border bg-dark-surface1">
                      <span
                        className={`inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-semibold border ${release.prerelease ? "bg-amber-500/10 text-amber-400 border-amber-500/20" : "bg-brand-sky/10 text-brand-sky border-brand-sky/20"}`}
                      >
                        <Tag className="w-3 h-3" />
                        {release.tag}
                      </span>
                      <span className="inline-flex items-center gap-1.5 text-xs text-dark-text-muted">
                        <Calendar className="w-3 h-3" />
                        {formatDate(release.published_at, locale)}
                      </span>
                      <span className="text-xs text-dark-text-muted">
                        {timeAgo(release.published_at, locale)}
                      </span>
                      <CopyButtons release={release} locale={locale} t={t} />
                    </div>

                    {/* Card body */}
                    <div
                      className="px-5 py-4 changelog-body"
                      dangerouslySetInnerHTML={{
                        __html: renderMarkdown(pickLocaleBody(release.body, locale)),
                      }}
                    />

                    {/* Assets download panel */}
                    <AssetsPanel release={release} t={t} />
                  </div>
                </motion.article>
              ))}
            </div>

            {/* Load more */}
            {hasMore && (
              <div className="flex justify-center mt-10">
                <button
                  onClick={handleLoadMore}
                  disabled={loadingMore}
                  className="inline-flex items-center gap-2 px-5 py-2.5 rounded-lg border border-dark-border bg-dark-surface1 text-sm text-dark-text-secondary hover:text-dark-text hover:bg-dark-surface2 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {loadingMore ? (
                    <Loader2 className="w-4 h-4 animate-spin" />
                  ) : (
                    <ChevronDown className="w-4 h-4" />
                  )}
                  {loadingMore
                    ? t("changelog.loading")
                    : t("changelog.loadMore")}
                </button>
              </div>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
