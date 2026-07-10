import { useState, useEffect, useRef, useCallback, type ComponentType } from "react";
import { motion, AnimatePresence } from "framer-motion";
import {
  Download,
  Check,
  Loader2,
  Puzzle,
  TrendingUp,
  Bell,
  CheckCircle2,
  AlertCircle,
  AlertTriangle,
  Globe,
  Copy,
  Terminal,
} from "lucide-react";
import {
  SiApple,
  SiLinux,
  SiDocker,
  SiAndroid,
} from "@icons-pack/react-simple-icons";
import { LampEffect } from "@/components/ui/lamp-effect";
import { useLocale } from "@/lib/i18n";

const techStack = [
  { name: "Flutter", color: "text-brand-sky" },
  { name: "Rust", color: "text-[#dea584]" },
  { name: "Tokio", color: "text-brand-cyan" },
  { name: "SQLite", color: "text-success" },
];

const DOCKER_IMAGE = "ghcr.io/zerx-lab/fluxdown-server:latest";

const DOCKER_RUN_CMD = `docker run -d --name fluxdown-server \\
  -p 17800:17800 \\
  -v fluxdown-data:/data \\
  -v ./downloads:/root/Downloads \\
  --restart unless-stopped \\
  ${DOCKER_IMAGE}`;

const DOCKER_COMPOSE_YML = `services:
  fluxdown-server:
    image: ${DOCKER_IMAGE}
    container_name: fluxdown-server
    restart: unless-stopped
    ports:
      - "17800:17800"
    volumes:
      - fluxdown-data:/data
      - ./downloads:/root/Downloads

volumes:
  fluxdown-data:`;

// Scoop 自托管源安装命令（本仓库 bucket）。官方 extras 源待项目达标后再加。

const SCOOP_SELFHOSTED_CMD = `scoop bucket add fluxdown https://github.com/zerx-lab/FluxDown
scoop install fluxdown/fluxdown`;

/* Windows logo — not available in Simple Icons (trademark), use inline SVG */
function WindowsLogo({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="currentColor">
      <path d="M0 3.449L9.75 2.1v9.451H0m10.949-9.602L24 0v11.4H10.949M0 12.6h9.75v9.451L0 20.699M10.949 12.6H24V24l-13.051-1.849" />
    </svg>
  );
}

interface ReleaseAsset {
  name: string;
  size: number;
  download_url: string;
}

interface ReleaseInfo {
  version: string;
  tag: string;
  published_at: string;
  total_downloads: number;
  assets: {
    setup: ReleaseAsset | null;
    portable: ReleaseAsset | null;
    setup_arm64: ReleaseAsset | null;
    portable_arm64: ReleaseAsset | null;
    extension: ReleaseAsset | null;
    firefox_extension: ReleaseAsset | null;
    macos_dmg_arm64: ReleaseAsset | null;
    macos_dmg_x64: ReleaseAsset | null;
    macos_tarball_arm64: ReleaseAsset | null;
    macos_tarball_x64: ReleaseAsset | null;
    linux_appimage: ReleaseAsset | null;
    linux_deb: ReleaseAsset | null;
    linux_arch: ReleaseAsset | null;
    linux_tarball: ReleaseAsset | null;
  };
  /** FluxDown Server（headless Web 版）独立 release，无发布时为 null */
  server: {
    version: string;
    tag: string;
    assets: {
      windows_x64: ReleaseAsset | null;
      windows_arm64: ReleaseAsset | null;
      linux_x64: ReleaseAsset | null;
      linux_arm64: ReleaseAsset | null;
      macos_x64: ReleaseAsset | null;
      macos_arm64: ReleaseAsset | null;
      openwrt_x64: ReleaseAsset | null;
      openwrt_arm64: ReleaseAsset | null;
      openwrt_luci: ReleaseAsset | null;
      qnap_x64: ReleaseAsset | null;
      qnap_arm64: ReleaseAsset | null;
    };
  } | null;
  /** FluxDown CLI（命令行客户端）独立 release，无发布时为 null */
  cli: {
    version: string;
    tag: string;
    assets: {
      windows_x64: ReleaseAsset | null;
      windows_arm64: ReleaseAsset | null;
      linux_x64: ReleaseAsset | null;
      linux_arm64: ReleaseAsset | null;
      macos_x64: ReleaseAsset | null;
      macos_arm64: ReleaseAsset | null;
    };
  } | null;
  /** FluxDown 移动端（Android）独立 release，无发布时为 null */
  mobile: {
    version: string;
    tag: string;
    assets: {
      android_arm64: ReleaseAsset | null;
      android_armv7: ReleaseAsset | null;
      android_x64: ReleaseAsset | null;
      android_universal: ReleaseAsset | null;
    };
  } | null;
}

function formatSize(bytes: number): string {
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function DownloadSection() {
  const { t, locale } = useLocale();
  const [release, setRelease] = useState<ReleaseInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [selectedArch, setSelectedArch] = useState<Record<string, string>>({});
  const [activePlatform, setActivePlatform] = useState("windows");
  const platformNavRef = useRef<HTMLElement>(null);
  const navDidMount = useRef(false);

  // 移动端平台栏为横向滚动条：切换平台（点击或 Hero 下拉跳转）后把选中 pill 滚入视野。
  // 首次挂载跳过——默认 Windows 已在最左，避免无谓的居中滚动。
  useEffect(() => {
    if (!navDidMount.current) {
      navDidMount.current = true;
      return;
    }
    const btn = platformNavRef.current?.querySelector<HTMLButtonElement>(
      `button[data-platform="${activePlatform}"]`,
    );
    btn?.scrollIntoView({ block: "nearest", inline: "center", behavior: "smooth" });
  }, [activePlatform]);
  const [dockerTab, setDockerTab] = useState<"run" | "compose">("run");
  const [dockerCopied, setDockerCopied] = useState(false);

  const handleDockerCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(
        dockerTab === "run" ? DOCKER_RUN_CMD : DOCKER_COMPOSE_YML,
      );
      setDockerCopied(true);
      setTimeout(() => setDockerCopied(false), 2000);
    } catch {
      /* clipboard unavailable — ignore */
    }
  }, [dockerTab]);

  const [scoopCopied, setScoopCopied] = useState(false);

  const handleScoopCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(SCOOP_SELFHOSTED_CMD);
      setScoopCopied(true);
      setTimeout(() => setScoopCopied(false), 2000);
    } catch {
      /* clipboard unavailable — ignore */
    }
  }, []);

  useEffect(() => {
    fetch("/api/release")
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.json();
      })
      .then((data: ReleaseInfo) => setRelease(data))
      .catch((err) => console.error("Failed to fetch release info:", err))
      .finally(() => setLoading(false));
  }, []);

  // Hero「更多版本」下拉选中平台后跳转到本区并切换面板。
  // 本组件 client:visible 懒水合，事件可能先于监听器发出——挂载时消费挂起值兜底。
  useEffect(() => {
    const KEYS = ["windows", "macos", "linux", "docker", "web", "mobile", "cli"];
    const apply = (key: unknown) => {
      if (typeof key === "string" && KEYS.includes(key))
        setActivePlatform(key);
    };
    const w = window as { __fluxdownPendingPlatform?: string };
    apply(w.__fluxdownPendingPlatform);
    delete w.__fluxdownPendingPlatform;
    const handler = (e: Event) => {
      apply((e as CustomEvent<string>).detail);
      (window as { __fluxdownPendingPlatform?: string }).__fluxdownPendingPlatform =
        undefined;
    };
    window.addEventListener("fluxdown:select-platform", handler);
    return () =>
      window.removeEventListener("fluxdown:select-platform", handler);
  }, []);

  const [subscribeTarget, setSubscribeTarget] = useState<string | null>(null);
  const [subscribeEmail, setSubscribeEmail] = useState("");
  const [subscribeStatus, setSubscribeStatus] = useState<
    "idle" | "loading" | "success" | "duplicate" | "error"
  >("idle");

  const handleSubscribe = useCallback(
    async (platform: string) => {
      if (!subscribeEmail.trim()) return;
      setSubscribeStatus("loading");
      try {
        const res = await fetch("/api/subscribe", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ email: subscribeEmail.trim(), platform }),
        });
        if (res.status === 429) {
          setSubscribeStatus("error");
          return;
        }
        if (!res.ok) {
          setSubscribeStatus("error");
          return;
        }
        const data = await res.json();
        setSubscribeStatus(
          data.message === "already_subscribed" ? "duplicate" : "success",
        );
        if (data.message !== "already_subscribed") setSubscribeEmail("");
        setTimeout(() => {
          setSubscribeStatus("idle");
          setSubscribeTarget(null);
        }, 4000);
      } catch {
        setSubscribeStatus("error");
      }
    },
    [subscribeEmail],
  );

  const hasArm64Assets = !!(
    release?.assets.setup_arm64 || release?.assets.portable_arm64
  );
  const hasLinuxAssets = !!(
    release?.assets.linux_appimage ||
    release?.assets.linux_deb ||
    release?.assets.linux_arch ||
    release?.assets.linux_tarball
  );
  const serverAssets = release?.server?.assets;
  const hasServerAssets = !!(
    serverAssets &&
    (serverAssets.windows_x64 ||
      serverAssets.windows_arm64 ||
      serverAssets.linux_x64 ||
      serverAssets.linux_arm64 ||
      serverAssets.macos_x64 ||
      serverAssets.macos_arm64)
  );
  const cliAssets = release?.cli?.assets;
  const hasCliAssets = !!(
    cliAssets &&
    (cliAssets.windows_x64 ||
      cliAssets.windows_arm64 ||
      cliAssets.linux_x64 ||
      cliAssets.linux_arm64 ||
      cliAssets.macos_x64 ||
      cliAssets.macos_arm64)
  );
  const mobileAssets = release?.mobile?.assets;
  const hasMobileAssets = !!(
    mobileAssets &&
    (mobileAssets.android_arm64 ||
      mobileAssets.android_armv7 ||
      mobileAssets.android_x64 ||
      mobileAssets.android_universal)
  );

  const platforms: {
    key: string;
    name: string;
    icon: ComponentType<{ className?: string; size?: number; color?: string }>;
    arch: string;
    available: boolean;
    primary: boolean;
    badge: string;
    /** 独立图标背景样式（覆盖 primary/非 primary 默认背景），如 Docker/Web 版品牌色 */
    iconBg?: string;
    /** 平台独立版本号（如 FluxDown Server），缺省时用桌面客户端版本 */
    version?: string;
    setup: ReleaseAsset | null;
    portable: ReleaseAsset | null;
    setupLabel?: string;
    portableLabel?: string;
    /** Linux 等平台的多格式下载列表，存在时替代单一 portable 按钮 */
    packages?: Array<{ label: string; asset: ReleaseAsset | null }>;
    /** 多架构变体，存在时在卡片内显示架构切换 tabs */
    archVariants?: Array<{
      label: string;
      setup: ReleaseAsset | null;
      portable: ReleaseAsset | null;
    }>;
  }[] = [
    {
      key: "windows",
      name: t("dl.windows"),
      icon: WindowsLogo,
      arch: hasArm64Assets ? "x64 / ARM64" : "x64",
      available: true,
      primary: true,
      badge: t("dl.availableNow"),
      setup: release?.assets.setup ?? null,
      portable: release?.assets.portable ?? null,
      archVariants: hasArm64Assets
        ? [
            {
              label: "x64",
              setup: release?.assets.setup ?? null,
              portable: release?.assets.portable ?? null,
            },
            {
              label: "ARM64",
              setup: release?.assets.setup_arm64 ?? null,
              portable: release?.assets.portable_arm64 ?? null,
            },
          ]
        : undefined,
    },
    {
      key: "macos",
      name: t("dl.macos"),
      icon: SiApple,
      arch: "Apple Silicon / Intel",
      available: !!(
        release?.assets.macos_dmg_arm64 ||
        release?.assets.macos_dmg_x64 ||
        release?.assets.macos_tarball_arm64 ||
        release?.assets.macos_tarball_x64
      ),
      primary: !!(
        release?.assets.macos_dmg_arm64 || release?.assets.macos_dmg_x64
      ),
      badge:
        release?.assets.macos_dmg_arm64 ||
        release?.assets.macos_dmg_x64 ||
        release?.assets.macos_tarball_arm64 ||
        release?.assets.macos_tarball_x64
          ? t("dl.availableNow")
          : t("dl.comingSoon"),
      setup:
        release?.assets.macos_dmg_arm64 ??
        release?.assets.macos_dmg_x64 ??
        null,
      setupLabel: t("dl.dmg"),
      portable: null,
      portableLabel: "tar.gz",
      archVariants:
        release?.assets.macos_dmg_arm64 && release?.assets.macos_dmg_x64
          ? [
              {
                label: "Apple Silicon",
                setup: release?.assets.macos_dmg_arm64 ?? null,
                portable: release?.assets.macos_tarball_arm64 ?? null,
              },
              {
                label: "Intel (x64)",
                setup: release?.assets.macos_dmg_x64 ?? null,
                portable: release?.assets.macos_tarball_x64 ?? null,
              },
            ]
          : undefined,
    },
    {
      key: "linux",
      name: t("dl.linux"),
      icon: SiLinux,
      arch: "x64",
      available: hasLinuxAssets,
      primary: hasLinuxAssets,
      badge: hasLinuxAssets ? t("dl.availableNow") : t("dl.comingSoon"),
      setup: release?.assets.linux_appimage ?? null,
      setupLabel: t("dl.appimage"),
      portable: null,
      packages: [
        {
          label: "deb (Debian / Ubuntu)",
          asset: release?.assets.linux_deb ?? null,
        },
        {
          label: "pkg.tar.zst (Arch Linux)",
          asset: release?.assets.linux_arch ?? null,
        },
        {
          label: `tar.gz ${t("dl.linuxPortable")}`,
          asset: release?.assets.linux_tarball ?? null,
        },
      ],
    },
    {
      key: "docker",
      name: t("dl.docker"),
      icon: SiDocker,
      arch: t("dl.dockerArch"),
      available: true,
      primary: false,
      iconBg: "bg-gradient-to-br from-[#2496ED] to-[#0db7ed]",
      badge: t("dl.availableNow"),
      setup: null,
      portable: null,
    },
    {
      key: "web",
      name: t("dl.web"),
      icon: Globe,
      arch: t("dl.webArch"),
      available: hasServerAssets,
      primary: false,
      badge: hasServerAssets ? t("dl.availableNow") : t("dl.comingSoon"),
      iconBg: "bg-gradient-to-br from-brand-sky to-brand-cyan",
      version: release?.server?.version,
      setup: serverAssets?.windows_x64 ?? null,
      setupLabel: "Windows x64",
      portable: null,
      packages: [
        {
          label: "Windows ARM64 (zip)",
          asset: serverAssets?.windows_arm64 ?? null,
        },
        {
          label: "Linux x64 (tar.gz)",
          asset: serverAssets?.linux_x64 ?? null,
        },
        {
          label: "Linux ARM64 (tar.gz)",
          asset: serverAssets?.linux_arm64 ?? null,
        },
        {
          label: "macOS Apple Silicon (tar.gz)",
          asset: serverAssets?.macos_arm64 ?? null,
        },
        {
          label: "macOS Intel (tar.gz)",
          asset: serverAssets?.macos_x64 ?? null,
        },
        {
          label: "OpenWrt x86_64 (ipk)",
          asset: serverAssets?.openwrt_x64 ?? null,
        },
        {
          label: "OpenWrt aarch64 (ipk)",
          asset: serverAssets?.openwrt_arm64 ?? null,
        },
        {
          label: "OpenWrt LuCI (ipk)",
          asset: serverAssets?.openwrt_luci ?? null,
        },
        {
          label: "QNAP x64 (qpkg)",
          asset: serverAssets?.qnap_x64 ?? null,
        },
        {
          label: "QNAP ARM64 (qpkg)",
          asset: serverAssets?.qnap_arm64 ?? null,
        },
      ],
    },
    {
      key: "mobile",
      name: t("dl.mobile"),
      icon: SiAndroid,
      arch: "Android",
      available: hasMobileAssets,
      primary: false,
      badge: hasMobileAssets ? t("dl.availableNow") : t("dl.comingSoon"),
      iconBg: "bg-gradient-to-br from-[#3DDC84] to-[#2bb673]",
      version: release?.mobile?.version,
      setup: mobileAssets?.android_arm64 ?? null,
      setupLabel: "arm64-v8a (APK)",
      portable: null,
      packages: [
        {
          label: "armeabi-v7a (APK)",
          asset: mobileAssets?.android_armv7 ?? null,
        },
        {
          label: "x86_64 (APK)",
          asset: mobileAssets?.android_x64 ?? null,
        },
        {
          label: `universal (APK)`,
          asset: mobileAssets?.android_universal ?? null,
        },
      ],
    },
    {
      key: "cli",
      name: t("dl.cli"),
      icon: Terminal,
      arch: t("dl.cliArch"),
      available: hasCliAssets,
      primary: false,
      badge: hasCliAssets ? t("dl.availableNow") : t("dl.comingSoon"),
      iconBg: "bg-gradient-to-br from-brand-cyan to-brand-sky",
      version: release?.cli?.version,
      setup: cliAssets?.windows_x64 ?? null,
      setupLabel: "Windows x64",
      portable: null,
      packages: [
        {
          label: "Windows ARM64 (zip)",
          asset: cliAssets?.windows_arm64 ?? null,
        },
        {
          label: "Linux x64 (tar.gz)",
          asset: cliAssets?.linux_x64 ?? null,
        },
        {
          label: "Linux ARM64 (tar.gz)",
          asset: cliAssets?.linux_arm64 ?? null,
        },
        {
          label: "macOS Apple Silicon (tar.gz)",
          asset: cliAssets?.macos_arm64 ?? null,
        },
        {
          label: "macOS Intel (tar.gz)",
          asset: cliAssets?.macos_x64 ?? null,
        },
      ],
    },
  ];

  return (
    <section
      id="download"
      className="relative pt-16 sm:pt-20 pb-20 sm:pb-32 overflow-hidden bg-dark-bg"
    >
      <LampEffect>
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8 relative z-10">
          <motion.div
            className="text-center max-w-2xl mx-auto mb-16"
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5 }}
          >
            <span className="inline-flex items-center px-3 py-1 rounded-full text-xs font-semibold bg-brand-blue/10 text-brand-blue border border-brand-blue/20 uppercase tracking-widest">
              {t("dl.badge")}
            </span>
            <h2 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
              {t("dl.title")}
              <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
                {t("dl.titleHighlight")}
              </span>
              ?
            </h2>
            <p className="mt-4 text-dark-text-secondary text-lg">
              {t("dl.subtitle")}
            </p>
          </motion.div>

          {/* Platform selector panel（左侧平台列表 + 右侧详情面板） */}
          <motion.div
            className="max-w-4xl mx-auto mb-16"
            initial={{ opacity: 0, y: 30 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.6, delay: 0.1 }}
          >
            <div className="flex flex-col md:flex-row rounded-2xl border border-dark-border/60 bg-dark-surface1 overflow-hidden md:min-h-[400px]">
              {/* 平台侧栏（移动端为顶部横向滚动条） */}
              <aside className="md:w-52 shrink-0 border-b md:border-b-0 md:border-r border-dark-border/60 bg-dark-surface2/40 p-2.5 md:p-3">
                <p className="hidden md:block px-3 pt-1 pb-2 text-[10px] font-semibold uppercase tracking-widest text-dark-text-muted">
                  {t("dl.platformLabel")}
                </p>
                <nav ref={platformNavRef} className="flex md:flex-col gap-1 overflow-x-auto md:overflow-visible">
                  {platforms.map((p) => {
                    const Icon = p.icon;
                    const isActive = activePlatform === p.key;
                    return (
                      <button
                        key={p.key}
                        data-platform={p.key}
                        type="button"
                        onClick={() => setActivePlatform(p.key)}
                        className={`relative flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-xs font-semibold whitespace-nowrap transition-colors duration-200 ${
                          isActive
                            ? "text-dark-text"
                            : "text-dark-text-muted hover:text-dark-text-secondary hover:bg-dark-surface2/80"
                        }`}
                      >
                        {isActive && (
                          <motion.span
                            layoutId="dlActivePlatform"
                            className="absolute inset-0 rounded-lg bg-dark-surface3 border border-dark-border/80 shadow-sm"
                            transition={{
                              type: "spring",
                              stiffness: 500,
                              damping: 38,
                            }}
                          />
                        )}
                        <span className="relative z-10 flex items-center gap-2.5">
                          <Icon
                            className={`w-4 h-4 transition-colors ${isActive ? "text-brand-blue" : ""}`}
                            color="currentColor"
                          />
                          {p.name}
                          {!p.available && (
                            <span
                              className="w-1.5 h-1.5 rounded-full bg-dark-text-muted/40"
                              aria-hidden
                            />
                          )}
                        </span>
                      </button>
                    );
                  })}
                </nav>
              </aside>

              {/* 详情面板 */}
              <div className="flex-1 min-w-0 p-5 sm:p-7">
                <AnimatePresence mode="wait">
                  {(() => {
                    const p =
                      platforms.find((x) => x.key === activePlatform) ??
                      platforms[0];
                    const Icon = p.icon;
                    const currentArchLabel =
                      selectedArch[p.key] ?? p.archVariants?.[0]?.label;
                    const activeVariant = p.archVariants?.find(
                      (v) => v.label === currentArchLabel,
                    );
                    const effectiveSetup = activeVariant?.setup ?? p.setup;
                    const effectivePortable =
                      activeVariant?.portable ?? p.portable;
                    const formats: Array<{
                      label: string;
                      asset: ReleaseAsset;
                    }> = [];
                    if (effectivePortable)
                      formats.push({
                        label: p.portableLabel ?? t("dl.portablePkg"),
                        asset: effectivePortable,
                      });
                    for (const pkg of p.packages ?? [])
                      if (pkg.asset)
                        formats.push({ label: pkg.label, asset: pkg.asset });
                    return (
                      <motion.div
                        key={p.key}
                        initial={{ opacity: 0, y: 10 }}
                        animate={{ opacity: 1, y: 0 }}
                        exit={{ opacity: 0, y: -8 }}
                        transition={{ duration: 0.18, ease: "easeOut" }}
                      >
                        {/* 头部：图标 + 名称 + 徽标 + 版本 */}
                        <div className="flex items-start gap-4">
                          <div
                            className={`w-12 h-12 rounded-xl flex items-center justify-center shrink-0 ${
                              p.iconBg ??
                              "bg-gradient-to-br from-brand-sky to-brand-cyan"
                            }`}
                          >
                            <Icon
                              className="w-6 h-6 text-white"
                              color="currentColor"
                            />
                          </div>
                          <div className="min-w-0">
                            <div className="flex items-center gap-2 flex-wrap">
                              <h3 className="text-base font-semibold text-dark-text">
                                {p.name}
                              </h3>
                              {p.available ? (
                                <span className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-brand-blue/15 text-[10px] font-semibold text-brand-blue">
                                  <Check className="w-3 h-3" />
                                  {p.badge}
                                </span>
                              ) : (
                                <span className="inline-flex items-center px-2 py-0.5 rounded-full border border-dashed border-dark-text-muted/30 text-[10px] font-medium text-dark-text-muted">
                                  {p.badge}
                                </span>
                              )}
                            </div>
                            <p className="text-xs text-dark-text-muted mt-0.5">
                              {p.arch}
                            </p>
                            {p.available && release && p.key !== "docker" && (
                              <p className="text-[10px] text-dark-text-muted mt-0.5">
                                {t("dl.version", {
                                  version: p.version ?? release.version,
                                })}
                                {effectiveSetup && (
                                  <span className="ml-1.5">
                                    ({formatSize(effectiveSetup.size)})
                                  </span>
                                )}
                              </p>
                            )}
                            {p.key === "windows" && (
                              <p className="text-[10px] text-dark-text-muted/60 mt-0.5">
                                {t("dl.sysReq.windows")}
                              </p>
                            )}
                          </div>
                        </div>

                        <div className="mt-5 h-px bg-dark-border/60" />

                        {p.key === "docker" ? (
                          <div className="mt-5">
                            <div className="flex items-center justify-between gap-2 mb-3 flex-wrap">
                              {/* Tab 切换 */}
                              <div className="flex items-center gap-1 rounded-lg bg-dark-surface2 p-1">
                                {(
                                  [
                                    { key: "run", label: "docker run" },
                                    {
                                      key: "compose",
                                      label: "docker-compose.yml",
                                    },
                                  ] as const
                                ).map((tab) => (
                                  <button
                                    key={tab.key}
                                    type="button"
                                    onClick={() => setDockerTab(tab.key)}
                                    className={`relative px-3 py-1 rounded-md text-[11px] font-semibold font-mono transition-colors ${
                                      dockerTab === tab.key
                                        ? "text-brand-blue"
                                        : "text-dark-text-muted hover:text-dark-text-secondary"
                                    }`}
                                  >
                                    {dockerTab === tab.key && (
                                      <motion.span
                                        layoutId="dlDockerTab"
                                        className="absolute inset-0 rounded-md bg-brand-blue/20"
                                        transition={{
                                          type: "spring",
                                          stiffness: 500,
                                          damping: 38,
                                        }}
                                      />
                                    )}
                                    <span className="relative z-10">
                                      {tab.label}
                                    </span>
                                  </button>
                                ))}
                              </div>
                              {/* 复制按钮 */}
                              <button
                                type="button"
                                onClick={handleDockerCopy}
                                className="inline-flex items-center gap-1.5 rounded-lg border border-dark-border px-3 py-1.5 text-[11px] font-medium text-dark-text-secondary hover:bg-dark-surface3 transition-colors"
                              >
                                {dockerCopied ? (
                                  <>
                                    <Check className="w-3 h-3 text-success" />
                                    {t("dl.dockerCopied")}
                                  </>
                                ) : (
                                  <>
                                    <Copy className="w-3 h-3" />
                                    {t("dl.dockerCopy")}
                                  </>
                                )}
                              </button>
                            </div>
                            <AnimatePresence mode="wait">
                              <motion.pre
                                key={dockerTab}
                                initial={{ opacity: 0, y: 4 }}
                                animate={{ opacity: 1, y: 0 }}
                                exit={{ opacity: 0, y: -4 }}
                                transition={{ duration: 0.15 }}
                                className="rounded-lg bg-dark-bg border border-dark-border/60 p-4 text-xs leading-relaxed text-dark-text-secondary overflow-x-auto font-mono"
                              >
                                <code>
                                  {dockerTab === "run"
                                    ? DOCKER_RUN_CMD
                                    : DOCKER_COMPOSE_YML}
                                </code>
                              </motion.pre>
                            </AnimatePresence>
                            {dockerTab === "compose" && (
                              <pre className="mt-2 rounded-lg bg-dark-bg border border-dark-border/60 p-4 text-xs leading-relaxed text-dark-text-secondary overflow-x-auto font-mono">
                                <code>docker compose up -d</code>
                              </pre>
                            )}
                            <p className="mt-3 text-[11px] text-dark-text-muted">
                              {t("dl.dockerHint")}
                            </p>
                          </div>
                        ) : p.available ? (
                          <div className="mt-5 flex flex-col gap-4">
                            {/* CPU 架构切换 */}
                            {p.archVariants && p.archVariants.length > 1 && (
                              <div>
                                <p className="text-[10px] font-semibold uppercase tracking-widest text-dark-text-muted mb-2">
                                  {t("dl.archLabel")}
                                </p>
                                <div className="inline-flex items-center gap-1 rounded-lg bg-dark-surface2 p-1">
                                  {p.archVariants.map((v) => (
                                    <button
                                      key={v.label}
                                      type="button"
                                      onClick={() =>
                                        setSelectedArch((prev) => ({
                                          ...prev,
                                          [p.key]: v.label,
                                        }))
                                      }
                                      className={`relative px-3 py-1.5 rounded-md text-[11px] font-semibold transition-colors ${
                                        currentArchLabel === v.label
                                          ? "text-brand-blue"
                                          : "text-dark-text-muted hover:text-dark-text-secondary"
                                      }`}
                                    >
                                      {currentArchLabel === v.label && (
                                        <motion.span
                                          layoutId={`dlArch-${p.key}`}
                                          className="absolute inset-0 rounded-md bg-brand-blue/20"
                                          transition={{
                                            type: "spring",
                                            stiffness: 500,
                                            damping: 38,
                                          }}
                                        />
                                      )}
                                      <span className="relative z-10">
                                        {v.label}
                                      </span>
                                    </button>
                                  ))}
                                </div>
                              </div>
                            )}

                            {/* 主下载按钮 */}
                            {loading ? (
                              <div className="inline-flex items-center justify-center gap-2 rounded-lg bg-brand-blue/50 px-6 py-3 text-sm font-semibold text-white/70 cursor-wait sm:self-start">
                                <Loader2 className="w-4 h-4 animate-spin" />
                                {t("dl.loading")}
                              </div>
                            ) : effectiveSetup ? (
                              <a
                                href={effectiveSetup.download_url}
                                className="inline-flex items-center justify-center gap-2 rounded-lg bg-brand-blue px-6 py-3 text-sm font-semibold text-white hover:bg-brand-blue/90 transition-colors shadow-lg shadow-brand-blue/20 sm:self-start"
                              >
                                <Download className="w-4 h-4" />
                                {t("dl.downloadNow")} —{" "}
                                {p.setupLabel ?? t("dl.installPkg")}
                                <span className="text-white/70 font-normal">
                                  ({formatSize(effectiveSetup.size)})
                                </span>
                              </a>
                            ) : null}

                            {/* 其他格式 */}
                            {formats.length > 0 && (
                              <div>
                                <p className="text-[10px] font-semibold uppercase tracking-widest text-dark-text-muted mb-2">
                                  {t("dl.moreFormats")}
                                </p>
                                <div className="grid sm:grid-cols-2 gap-2">
                                  {formats.map((f) => (
                                    <a
                                      key={f.label}
                                      href={f.asset.download_url}
                                      className="inline-flex items-center gap-2 rounded-lg border border-dark-border px-4 py-2.5 text-[11px] font-medium text-dark-text-secondary hover:bg-dark-surface3 hover:border-dark-text-muted/30 transition-colors"
                                    >
                                      <Download className="w-3 h-3 shrink-0" />
                                      <span className="truncate">
                                        {f.label}
                                      </span>
                                      <span className="ml-auto text-dark-text-muted shrink-0">
                                        {formatSize(f.asset.size)}
                                      </span>
                                    </a>
                                  ))}
                                </div>
                              </div>
                            )}

                            {/* macOS 「已损坏」提示 */}
                            {p.key === "macos" && (
                              <a
                                href="/macos-gatekeeper"
                                className="inline-flex items-center gap-1.5 rounded-lg border border-amber-500/50 bg-amber-500/15 px-3 py-2 text-[10px] text-dark-text hover:bg-amber-500/25 hover:border-amber-500/70 transition-colors self-start"
                              >
                                <AlertTriangle className="w-3 h-3 flex-shrink-0 text-amber-500 shrink-0" />
                                {t("dl.macosWarning")}
                                <span className="text-amber-600 underline underline-offset-2 font-semibold">
                                  {t("dl.macosWarningLink")}
                                </span>
                              </a>
                            )}

                            {/* Web 版（FluxDown Server）部署指南 */}
                            {p.key === "web" && (
                              <a
                                href={`/docs/${locale}/headless-server/setup/`}
                                className="inline-flex items-center gap-1 text-[10px] text-dark-text-muted hover:text-brand-blue underline underline-offset-2 transition-colors self-start"
                              >
                                {t("dl.webGuide")}
                              </a>
                            )}

                            {/* CLI 文档链接 */}
                            {p.key === "cli" && (
                              <a
                                href={`/docs/${locale}/api/cli/`}
                                className="inline-flex items-center gap-1 text-[10px] text-dark-text-muted hover:text-brand-blue underline underline-offset-2 transition-colors self-start"
                              >
                                {t("dl.cliGuide")}
                              </a>
                            )}
                          </div>
                        ) : (
                          <div className="mt-5 max-w-sm flex flex-col gap-2">
                            <AnimatePresence mode="wait">
                              {subscribeTarget === p.key ? (
                                <motion.div
                                  key="subscribe-form"
                                  initial={{ opacity: 0, height: 0 }}
                                  animate={{ opacity: 1, height: "auto" }}
                                  exit={{ opacity: 0, height: 0 }}
                                  transition={{ duration: 0.2 }}
                                  className="flex flex-col gap-2"
                                >
                                  {subscribeStatus === "success" ? (
                                    <div className="flex items-center justify-center gap-1.5 rounded-lg border border-success/30 bg-success/10 px-4 py-2.5 text-xs font-medium text-success">
                                      <CheckCircle2 className="w-3.5 h-3.5" />
                                      {t("dl.subscribed")}
                                    </div>
                                  ) : subscribeStatus === "duplicate" ? (
                                    <div className="flex items-center justify-center gap-1.5 rounded-lg border border-brand-blue/30 bg-brand-blue/10 px-4 py-2.5 text-xs font-medium text-brand-blue">
                                      <CheckCircle2 className="w-3.5 h-3.5" />
                                      {t("dl.alreadySubscribed")}
                                    </div>
                                  ) : (
                                    <>
                                      <div className="flex gap-1.5">
                                        <input
                                          type="email"
                                          value={subscribeEmail}
                                          onChange={(e) =>
                                            setSubscribeEmail(e.target.value)
                                          }
                                          onKeyDown={(e) =>
                                            e.key === "Enter" &&
                                            handleSubscribe(p.key)
                                          }
                                          placeholder={t("dl.emailPlaceholder")}
                                          disabled={
                                            subscribeStatus === "loading"
                                          }
                                          className="flex-1 min-w-0 rounded-lg border border-dark-border bg-dark-surface2 px-3 py-2 text-xs text-dark-text placeholder:text-dark-text-muted/50 focus:outline-none focus:border-brand-blue/50 disabled:opacity-50 transition-colors"
                                        />
                                        <button
                                          type="button"
                                          onClick={() => handleSubscribe(p.key)}
                                          disabled={
                                            subscribeStatus === "loading" ||
                                            !subscribeEmail.trim()
                                          }
                                          className="flex-shrink-0 rounded-lg bg-brand-blue px-3 py-2 text-xs font-semibold text-white hover:bg-brand-blue/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                                        >
                                          {subscribeStatus === "loading" ? (
                                            <Loader2 className="w-3.5 h-3.5 animate-spin" />
                                          ) : (
                                            <Bell className="w-3.5 h-3.5" />
                                          )}
                                        </button>
                                      </div>
                                      {subscribeStatus === "error" && (
                                        <div className="flex items-center justify-center gap-1 text-[10px] text-red-400">
                                          <AlertCircle className="w-3 h-3" />
                                          {t("dl.subscribeError")}
                                        </div>
                                      )}
                                      <button
                                        type="button"
                                        onClick={() => {
                                          setSubscribeTarget(null);
                                          setSubscribeStatus("idle");
                                        }}
                                        className="text-[10px] text-dark-text-muted hover:text-dark-text-secondary transition-colors"
                                      >
                                        {t("dl.comingSoon")}
                                      </button>
                                    </>
                                  )}
                                </motion.div>
                              ) : (
                                <motion.button
                                  key="notify-btn"
                                  type="button"
                                  initial={{ opacity: 0 }}
                                  animate={{ opacity: 1 }}
                                  exit={{ opacity: 0 }}
                                  onClick={() => {
                                    setSubscribeTarget(p.key);
                                    setSubscribeStatus("idle");
                                    setSubscribeEmail("");
                                  }}
                                  className="inline-flex items-center justify-center gap-2 w-full rounded-lg border border-dashed border-dark-text-muted/30 px-5 py-2.5 text-xs font-medium text-dark-text-muted hover:border-brand-blue/40 hover:text-brand-blue/80 transition-colors duration-200"
                                >
                                  <Bell className="w-3.5 h-3.5" />
                                  {t("dl.notifyMe")}
                                </motion.button>
                              )}
                            </AnimatePresence>
                          </div>
                        )}
                      </motion.div>
                    );
                  })()}
                </AnimatePresence>
              </div>
            </div>
          </motion.div>

          {/* Scoop 安装（Windows 包管理器）*/}
          <motion.div
            className="max-w-4xl mx-auto mb-16"
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5, delay: 0.15 }}
          >
            <div className="rounded-xl border border-dark-border/60 bg-dark-surface1 p-5">
              <div className="flex items-center gap-3 mb-3">
                <div className="w-10 h-10 rounded-lg bg-dark-surface2 border border-dark-border/50 flex items-center justify-center flex-shrink-0">
                  <Terminal className="w-5 h-5 text-brand-blue" />
                </div>
                <div className="min-w-0">
                  <h3 className="text-sm font-semibold text-dark-text">
                    {t("dl.scoopTitle")}
                  </h3>
                  <p className="text-xs text-dark-text-muted mt-0.5">
                    {t("dl.scoopDesc")}
                  </p>
                </div>
              </div>
              <div className="flex items-center justify-end mb-3">
                <button
                  type="button"
                  onClick={handleScoopCopy}
                  className="inline-flex items-center gap-1.5 rounded-lg border border-dark-border px-3 py-1.5 text-[11px] font-medium text-dark-text-secondary hover:bg-dark-surface3 transition-colors"
                >
                  {scoopCopied ? (
                    <>
                      <Check className="w-3 h-3 text-success" />
                      {t("dl.dockerCopied")}
                    </>
                  ) : (
                    <>
                      <Copy className="w-3 h-3" />
                      {t("dl.dockerCopy")}
                    </>
                  )}
                </button>
              </div>
              <pre className="rounded-lg bg-dark-bg border border-dark-border/60 p-4 text-xs leading-relaxed text-dark-text-secondary overflow-x-auto font-mono">
                <code>{SCOOP_SELFHOSTED_CMD}</code>
              </pre>
              <p className="mt-3 text-[11px] text-dark-text-muted">
                {t("dl.scoopSelfHostedHint")}
              </p>
            </div>
          </motion.div>

          {/* Browser Extension */}
          <motion.div
            className="max-w-4xl mx-auto mb-16"
            initial={{ opacity: 0, y: 20 }}
            whileInView={{ opacity: 1, y: 0 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5, delay: 0.2 }}
          >
            <div className="relative rounded-xl border border-dark-border bg-dark-surface1 p-6 flex flex-col gap-4">
              {/* 标题行 */}
              <div className="flex items-center gap-4">
                <div className="w-12 h-12 rounded-xl bg-gradient-to-br from-brand-blue/20 to-brand-cyan/20 border border-brand-blue/20 flex items-center justify-center flex-shrink-0">
                  <Puzzle className="w-6 h-6 text-brand-blue" />
                </div>
                <div className="min-w-0">
                  <h3 className="text-sm font-semibold text-dark-text">
                    {t("dl.extensionTitle")}
                  </h3>
                  <p className="text-xs text-dark-text-muted mt-0.5">
                    {t("dl.extensionDesc")}
                  </p>
                  {(release?.assets.extension ||
                    release?.assets.firefox_extension) && (
                    <p className="text-[10px] text-dark-text-muted mt-1">
                      {t("dl.version", { version: release.version })}
                    </p>
                  )}
                  <p className="text-[10px] text-dark-text-muted/70 mt-0.5">
                    {t("dl.extensionOtherNote")}
                  </p>
                </div>
              </div>
              {/* 按钮行 — flex-wrap 自动换行 */}
              <div className="flex flex-wrap gap-2">
                {/* Chrome 官方商店按钮 */}
                <a
                  href="https://chromewebstore.google.com/detail/fluxdown/meleenglfggcmcajknpeeeiobnpfmahc"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center justify-center gap-2 rounded-lg border border-brand-blue/30 bg-brand-blue/10 px-4 py-2 text-xs font-semibold text-brand-blue hover:bg-brand-blue/20 transition-colors"
                >
                  <svg
                    className="w-3.5 h-3.5"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"
                  >
                    <path d="M12 0C8.21 0 4.831 1.757 2.632 4.501l3.953 6.848A5.454 5.454 0 0 1 12 6.545h10.691A12 12 0 0 0 12 0zM1.931 5.47A11.943 11.943 0 0 0 0 12c0 6.012 4.42 10.991 10.189 11.864l3.953-6.847a5.45 5.45 0 0 1-6.865-2.29zm13.342 2.166a5.446 5.446 0 0 1 1.45 7.09l.002.001h-.002l-5.344 9.257c.206.01.413.016.621.016 6.627 0 12-5.373 12-12 0-1.54-.29-3.011-.818-4.364zM12 16.364a4.364 4.364 0 1 1 0-8.728 4.364 4.364 0 0 1 0 8.728z" />
                  </svg>
                  {t("dl.extensionChromeStore")}
                </a>
                {/* Firefox 官方商店按钮 */}
                <a
                  href="https://addons.mozilla.org/zh-CN/firefox/addon/fluxdown"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center justify-center gap-2 rounded-lg border border-[#ff7139]/30 bg-[#ff7139]/10 px-4 py-2 text-xs font-semibold text-[#ff7139] hover:bg-[#ff7139]/20 transition-colors"
                >
                  <svg
                    className="w-3.5 h-3.5"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"
                  >
                    <path d="M12 0C5.373 0 0 5.373 0 12s5.373 12 12 12 12-5.373 12-12S18.627 0 12 0zm5.894 16.43c-.195.334-.413.65-.655.948-.494.61-1.07 1.084-1.7 1.41-.646.336-1.347.506-2.069.506-.723 0-1.424-.17-2.07-.506-.63-.326-1.205-.8-1.699-1.41-.242-.298-.46-.614-.655-.948C8.456 15.3 8 13.697 8 12c0-1.698.456-3.3 1.046-4.43.195-.334.413-.65.655-.948.494-.61 1.07-1.084 1.7-1.41C12.047 4.876 12.748 4.706 13.47 4.706c.722 0 1.423.17 2.069.506.63.326 1.206.8 1.7 1.41.242.298.46.614.655.948C18.484 8.7 19 10.303 19 12c0 1.697-.516 3.3-1.106 4.43z" />
                  </svg>
                  {t("dl.extensionFirefox")}
                </a>
                {/* Edge 官方商店按钮 */}
                <a
                  href="https://microsoftedge.microsoft.com/addons/detail/fluxdown/nglkkjbogjghekbhhcnccnpfedjbdhhd"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center justify-center gap-2 rounded-lg border border-[#0078d4]/30 bg-[#0078d4]/10 px-4 py-2 text-xs font-semibold text-[#3b9eff] hover:bg-[#0078d4]/20 transition-colors"
                >
                  <svg
                    className="w-3.5 h-3.5"
                    viewBox="0 0 24 24"
                    fill="currentColor"
                    aria-hidden="true"
                  >
                    <path d="M21.86 17.86q.14 0 .25.12.1.13.1.25t-.11.33l-.32.46-.43.53-.44.5q-.21.25-.38.42l-.22.22q-.78.74-1.7 1.36-.91.62-1.92 1.07-1 .44-2.07.69-1.06.25-2.13.25-1.41 0-2.74-.36-1.34-.36-2.51-1Q6.16 22 5.21 21.07q-.95-.94-1.6-2.13-.56-1.04-.84-2.18-.27-1.13-.27-2.31 0-1.4.4-2.7.4-1.31 1.16-2.43.78-1.12 1.86-2.01 1.1-.89 2.46-1.46.64-.27 1.26-.39.62-.13 1.25-.13.95 0 1.85.27.91.27 1.69.78.78.51 1.4 1.23.63.72 1.05 1.61.42.89.65 1.91.23 1 .23 2.1 0 1.2-.32 2.21-.31 1.02-.86 1.85-.54.83-1.27 1.45-.72.61-1.55 1-.82.4-1.69.59-.87.18-1.72.18-.61 0-1.23-.1-.61-.08-1.18-.27-.58-.18-1.1-.46-.53-.27-.99-.65l.49.06.51.02q.94 0 1.84-.25.89-.24 1.69-.69.8-.45 1.45-1.09.66-.65 1.13-1.45.84-1.43.84-3.16 0-1.62-.83-2.95-.81-1.32-2.16-2.04-.71-.37-1.49-.56-.78-.18-1.59-.18-1.42 0-2.71.55-1.27.55-2.31 1.49-1.04.94-1.72 2.21-.69 1.27-.85 2.7l-.04.5-.01.51v.51l.04.5q.05.45.13.91.09.45.23.88.13.42.31.83.18.4.41.78.54.82 1.21 1.5.66.69 1.43 1.21.78.53 1.65.89.87.36 1.78.55 1.04.16 2.09.16 1.06 0 2.09-.27 1.04-.27 1.99-.79.96-.51 1.81-1.27.84-.74 1.55-1.74.05-.07.16-.16.11-.08.22-.13.07-.04.13-.04zM7.66 15.41q-.05-.34-.06-.66-.02-.34-.02-.62 0-.78.16-1.43.17-.66.43-1.21.27-.55.61-1 .35-.45.67-.81-.92.43-1.69 1.04-.78.61-1.34 1.34-.55.74-.86 1.59-.31.84-.31 1.74 0 .26.04.55.04.29.1.59.07.29.16.59.1.3.21.58.38 1.01 1.06 1.84.69.84 1.6 1.45.91.61 2 .94 1.11.34 2.31.34 1.16 0 2.32-.32 1.18-.32 2.18-.94 1-.62 1.78-1.51.78-.89 1.21-1.99-.69.59-1.4 1.06-.71.46-1.49.79-.78.32-1.59.5-.83.18-1.7.18-1.39 0-2.55-.41-1.16-.4-2.05-1.13-.89-.74-1.49-1.74-.6-1-.84-2.21-.05-.27-.09-.55-.05-.27-.06-.55l-.01-.05-.51-.05z" />
                  </svg>
                  {t("dl.extensionEdgeStore")}
                </a>
                {/* Firefox 离线 XPI */}
                {!loading && release?.assets.firefox_extension && (
                  <a
                    href={release.assets.firefox_extension.download_url}
                    className="inline-flex items-center justify-center gap-2 rounded-lg border border-[#ff7139]/30 bg-[#ff7139]/10 px-4 py-2 text-xs font-semibold text-[#ff7139] hover:bg-[#ff7139]/20 transition-colors"
                  >
                    <Download className="w-3.5 h-3.5" />
                    Firefox XPI (
                    {formatSize(release.assets.firefox_extension.size)})
                  </a>
                )}
                {/* Chrome 离线包按钮 */}
                {loading ? (
                  <div className="inline-flex items-center justify-center gap-2 rounded-lg bg-brand-blue/50 px-4 py-2 text-xs font-semibold text-white/70 cursor-wait">
                    <Loader2 className="w-3.5 h-3.5 animate-spin" />
                    {t("dl.loading")}
                  </div>
                ) : release?.assets.extension ? (
                  <a
                    href={release.assets.extension.download_url}
                    title={t("dl.extensionOtherNote")}
                    className="inline-flex items-center justify-center gap-2 rounded-lg border border-brand-blue/30 bg-brand-blue/10 px-4 py-2 text-xs font-semibold text-brand-blue hover:bg-brand-blue/20 transition-colors"
                  >
                    <Download className="w-3.5 h-3.5" />
                    {t("dl.extensionOffline")} (
                    {formatSize(release.assets.extension.size)})
                  </a>
                ) : (
                  <div className="inline-flex items-center justify-center gap-2 rounded-lg border border-dark-border px-4 py-2 text-xs font-medium text-dark-text-muted">
                    {t("dl.extensionOffline")}
                  </div>
                )}
              </div>
            </div>
          </motion.div>

          {/* Tech stack + Downloads counter */}
          <motion.div
            className="flex flex-col items-center gap-4"
            initial={{ opacity: 0 }}
            whileInView={{ opacity: 1 }}
            viewport={{ once: true }}
            transition={{ duration: 0.5, delay: 0.3 }}
          >
            {release && release.total_downloads > 0 && (
              <div className="inline-flex items-center gap-2 text-sm text-dark-text-secondary">
                <TrendingUp className="w-4 h-4 text-success" />
                <span>
                  <span className="font-semibold text-dark-text">
                    {release.total_downloads.toLocaleString()}
                  </span>{" "}
                  {t("dl.totalDownloads")}
                </span>
              </div>
            )}
            <div className="inline-flex items-center gap-3 sm:gap-6 rounded-full border border-dark-border bg-dark-surface1/50 px-4 sm:px-6 py-2.5 sm:py-3 backdrop-blur-sm">
              {techStack.map((ts, i) => (
                <span key={ts.name}>
                  <span
                    className={`text-[10px] sm:text-xs font-semibold ${ts.color}`}
                  >
                    {ts.name}
                  </span>
                  {i < techStack.length - 1 && (
                    <span className="ml-3 sm:ml-6 inline-block h-3 sm:h-4 w-px bg-dark-border" />
                  )}
                </span>
              ))}
            </div>
          </motion.div>
        </div>
      </LampEffect>
    </section>
  );
}
