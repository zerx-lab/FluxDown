import { useState, useEffect, useCallback, useRef } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { cn, DEMO_URL, GITHUB_REPO_URL } from "@/lib/utils";
import { useLocale, saveLocale, type Locale } from "@/lib/i18n";
import { LOCALES } from "@/lib/locales";

declare global {
  interface Window {
    __toggleTheme: () => void;
    __isLightTheme: () => boolean;
  }
}

interface DropdownItem {
  name: string;
  link: string;
  icon?: React.ReactNode;
}

interface NavDropdown {
  label: string;
  items: DropdownItem[];
}

/** 下拉菜单组件 */
function NavDropdownMenu({
  dropdown,
  scrolled,
}: {
  dropdown: NavDropdown;
  scrolled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const timeout = useRef<ReturnType<typeof setTimeout>>(undefined);
  const containerRef = useRef<HTMLDivElement>(null);

  const handleEnter = () => {
    clearTimeout(timeout.current);
    setOpen(true);
  };

  const handleLeave = () => {
    timeout.current = setTimeout(() => setOpen(false), 150);
  };

  // 点击外部关闭
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    };
    if (open) document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, [open]);

  return (
    <div
      ref={containerRef}
      className="relative hidden sm:block"
      onMouseEnter={handleEnter}
      onMouseLeave={handleLeave}
    >
      <button
        onClick={() => setOpen((v) => !v)}
        className={cn(
          "inline-flex items-center gap-1 text-xs font-medium text-dark-text-secondary hover:text-dark-text px-3 py-1.5 rounded-full hover:bg-dark-surface3/50 transition-all duration-200 cursor-pointer",
        )}
      >
        {dropdown.label}
        <svg
          width="10"
          height="10"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={cn(
            "transition-transform duration-200",
            open && "rotate-180",
          )}
        >
          <polyline points="6 9 12 15 18 9" />
        </svg>
      </button>

      <AnimatePresence>
        {open && (
          <motion.div
            initial={{ opacity: 0, y: -4, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -4, scale: 0.97 }}
            transition={{ duration: 0.15, ease: "easeOut" }}
            className="absolute top-full left-1/2 -translate-x-1/2 mt-2 min-w-[160px] rounded-xl border border-dark-border bg-dark-bg/95 backdrop-blur-xl shadow-xl shadow-black/25 overflow-hidden z-50"
          >
            <div className="py-1.5">
              {dropdown.items.map((item) => (
                <a
                  key={item.link}
                  href={item.link}
                  onClick={() => setOpen(false)}
                  className="flex items-center gap-2.5 px-4 py-2 text-xs font-medium text-dark-text-secondary hover:text-dark-text hover:bg-dark-surface2/80 transition-colors duration-150"
                >
                  {item.icon && (
                    <span className="flex-shrink-0 text-dark-text-muted">
                      {item.icon}
                    </span>
                  )}
                  {item.name}
                </a>
              ))}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

/** 下拉菜单图标集 */
const icons = {
  extension: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
    </svg>
  ),
  theme: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <circle cx="13.5" cy="6.5" r=".5" />
      <circle cx="17.5" cy="10.5" r=".5" />
      <circle cx="8.5" cy="7.5" r=".5" />
      <circle cx="6.5" cy="12.5" r=".5" />
      <path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.926 0 1.648-.746 1.648-1.688 0-.437-.18-.835-.437-1.125-.29-.289-.438-.652-.438-1.125a1.64 1.64 0 0 1 1.668-1.668h1.996c3.051 0 5.555-2.503 5.555-5.554C21.965 6.012 17.461 2 12 2z" />
    </svg>
  ),
  plugins: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M4 7a2 2 0 0 1 2-2h3V4a2 2 0 1 1 4 0v1h3a2 2 0 0 1 2 2v3h1a2 2 0 1 1 0 4h-1v3a2 2 0 0 1-2 2h-3v-1a2 2 0 1 0-4 0v1H6a2 2 0 0 1-2-2v-3H3a2 2 0 1 1 0-4h1V7z" />
    </svg>
  ),
  changelog: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M12 8v4l3 3" />
      <circle cx="12" cy="12" r="10" />
    </svg>
  ),
  faq: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <circle cx="12" cy="12" r="10" />
      <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" />
      <line x1="12" y1="17" x2="12.01" y2="17" />
    </svg>
  ),
  announcements: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="m3 11 18-5v12L3 13v-2z" />
      <path d="M11.6 16.8a3 3 0 1 1-5.8-1.6" />
    </svg>
  ),
  logoVote: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <circle cx="12" cy="12" r="10" />
      <path d="M8 12h8" />
      <path d="M12 8v8" />
    </svg>
  ),
  feedback: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
    </svg>
  ),
  featureVote: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M7 10v12" />
      <path d="M15 5.88 14 10h5.83a2 2 0 0 1 1.92 2.56l-2.33 8A2 2 0 0 1 17.5 22H4a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2h2.76a2 2 0 0 0 1.79-1.11L12 2a3.13 3.13 0 0 1 3 3.88Z" />
    </svg>
  ),
  api: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <polyline points="16 18 22 12 16 6" />
      <polyline points="8 6 2 12 8 18" />
    </svg>
  ),
  docs: (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" />
      <path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" />
    </svg>
  ),
};

export function FloatingNavbar({ className }: { className?: string }) {
  const [visible, setVisible] = useState(true);
  const [scrolled, setScrolled] = useState(false);
  const [lastScrollY, setLastScrollY] = useState(0);
  const [isLight, setIsLight] = useState(false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const { locale, setLocale, t } = useLocale();

  // --- 导航结构定义 ---

  // 一级直接链接
  const directLinks: { name: string; link: string; external?: boolean }[] = [
    { name: t("nav.features"), link: "/#features" },
    { name: t("nav.download"), link: "/#download" },
    { name: t("nav.demo"), link: DEMO_URL, external: true },
  ];

  // 「资源」下拉菜单
  const resourcesDropdown: NavDropdown = {
    label: t("nav.resources"),
    items: [
      { name: t("nav.extension"), link: "/#extension", icon: icons.extension },
      {
        name: t("nav.themeBuilder"),
        link: "/theme-builder",
        icon: icons.theme,
      },
      {
        name: t("nav.themeMarket"),
        link: "/themes",
        icon: icons.theme,
      },
      {
        name: t("nav.pluginMarket"),
        link: "/plugins",
        icon: icons.plugins,
      },
      { name: t("nav.changelog"), link: "/changelog", icon: icons.changelog },
      { name: t("nav.docs"), link: `/docs/${locale}/`, icon: icons.docs },
      { name: t("nav.faq"), link: "/faq", icon: icons.faq },
      { name: t("nav.apiDocs"), link: "/api-docs", icon: icons.api },
    ],
  };

  // 「社区」下拉菜单
  const communityDropdown: NavDropdown = {
    label: t("nav.community"),
    items: [
      {
        name: t("nav.announcements"),
        link: "/announcements",
        icon: icons.announcements,
      },
      {
        name: t("nav.featureVote"),
        link: "/feature-vote",
        icon: icons.featureVote,
      },
      { name: t("nav.feedback"), link: "/feedback", icon: icons.feedback },
    ],
  };

  // 移动端：全部展开的列表
  const mobileNavGroups = [
    {
      items: directLinks,
    },
    {
      label: t("nav.resources"),
      items: resourcesDropdown.items,
    },
    {
      label: t("nav.community"),
      items: communityDropdown.items,
    },
    {
      items: [{ name: t("nav.sponsor"), link: "/sponsor" }],
    },
  ];

  useEffect(() => {
    setIsLight(window.__isLightTheme?.() ?? false);
    const onThemeChange = (e: CustomEvent<{ light: boolean }>) => {
      setIsLight(e.detail.light);
    };
    window.addEventListener("theme-change", onThemeChange as EventListener);
    return () =>
      window.removeEventListener(
        "theme-change",
        onThemeChange as EventListener,
      );
  }, []);

  const handleToggleTheme = useCallback(() => {
    window.__toggleTheme?.();
  }, []);

  const [localeMenuOpen, setLocaleMenuOpen] = useState(false);
  const localeMenuRef = useRef<HTMLDivElement>(null);

  // 点击外部关闭语言菜单
  useEffect(() => {
    if (!localeMenuOpen) return;
    const onClick = (e: MouseEvent) => {
      if (!localeMenuRef.current?.contains(e.target as Node)) {
        setLocaleMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [localeMenuOpen]);

  const handleSelectLocale = useCallback(
    (loc: Locale) => {
      setLocaleMenuOpen(false);
      // 页面存在该语言的 hreflang 变体且 URL 不同 → 真实导航(SEO 与标签页标题
      // 均由目标 URL 的 SSR 承担;固定语言页只能靠导航切换)。先持久化偏好。
      const alt = document.querySelector<HTMLLinkElement>(
        `link[rel="alternate"][hreflang="${loc}"]`,
      );
      if (alt?.href) {
        const target = new URL(alt.href);
        if (target.pathname !== window.location.pathname) {
          saveLocale(loc);
          // hreflang 是生产域绝对 URL;导航只取 pathname,保持当前 origin
          // (本地 preview / 预发环境不跳生产域)。
          window.location.assign(target.pathname + target.search + target.hash);
          return;
        }
      }
      setLocale(loc);
    },
    [setLocale],
  );

  useEffect(() => {
    const handleScroll = () => {
      const currentScrollY = window.scrollY;
      setScrolled(currentScrollY > 80);
      if (currentScrollY > lastScrollY && currentScrollY > 200) {
        setVisible(false);
      } else {
        setVisible(true);
      }
      setLastScrollY(currentScrollY);
    };

    window.addEventListener("scroll", handleScroll, { passive: true });
    return () => window.removeEventListener("scroll", handleScroll);
  }, [lastScrollY]);

  // 移动端展开时锁定 body 滚动
  useEffect(() => {
    if (mobileOpen) {
      document.body.style.overflow = "hidden";
    } else {
      document.body.style.overflow = "";
    }
    return () => {
      document.body.style.overflow = "";
    };
  }, [mobileOpen]);

  return (
    <AnimatePresence>
      {visible && (
        <motion.header
          initial={{ y: -100, opacity: 0 }}
          animate={{ y: 0, opacity: 1 }}
          exit={{ y: -100, opacity: 0 }}
          transition={{ duration: 0.3 }}
          className={cn(
            "fixed top-4 inset-x-4 sm:inset-x-0 z-50 sm:mx-auto sm:max-w-fit",
            className,
          )}
        >
          <div
            className={cn(
              "flex items-center gap-1 rounded-full border px-2 py-2 transition-all duration-300",
              scrolled
                ? "border-dark-border bg-dark-bg/80 backdrop-blur-xl shadow-lg shadow-black/20"
                : "border-dark-border/50 bg-dark-surface1/60 backdrop-blur-md",
            )}
          >
            {/* 移动端汉堡菜单按钮 */}
            <button
              onClick={() => setMobileOpen((v) => !v)}
              className="flex sm:hidden items-center justify-center w-7 h-7 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200 cursor-pointer"
              aria-label="Toggle menu"
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="text-dark-text-secondary"
              >
                {mobileOpen ? (
                  <>
                    <line x1="18" y1="6" x2="6" y2="18" />
                    <line x1="6" y1="6" x2="18" y2="18" />
                  </>
                ) : (
                  <>
                    <line x1="4" y1="7" x2="20" y2="7" />
                    <line x1="4" y1="12" x2="20" y2="12" />
                    <line x1="4" y1="17" x2="20" y2="17" />
                  </>
                )}
              </svg>
            </button>

            {/* Logo */}
            <a href="/" className="flex items-center gap-2 px-3 py-1">
              <img src="/logo.svg" alt="FluxDown" className="h-6 w-6" />
              <span className="text-sm font-semibold tracking-tight hidden sm:inline">
                <span className="text-brand-sky">Flux</span>
                <span className="text-dark-text">Down</span>
              </span>
            </a>

            {/* 分隔线 */}
            <div className="hidden sm:block h-4 w-px bg-dark-border mx-1" />

            {/* 一级直接链接 */}
            {directLinks.map((item) => (
              <a
                key={item.link}
                href={item.link}
                target={item.external ? "_blank" : undefined}
                rel={item.external ? "noopener noreferrer" : undefined}
                className="hidden sm:inline-block text-xs font-medium text-dark-text-secondary hover:text-dark-text px-3 py-1.5 rounded-full hover:bg-dark-surface3/50 transition-all duration-200"
              >
                {item.name}
              </a>
            ))}

            {/* 「资源」下拉菜单 */}
            <NavDropdownMenu dropdown={resourcesDropdown} scrolled={scrolled} />

            {/* 「社区」下拉菜单 */}
            <NavDropdownMenu dropdown={communityDropdown} scrolled={scrolled} />

            {/* 赞助链接（带爱心图标） */}
            <a
              href="/sponsor"
              className="hidden sm:inline-flex items-center gap-1 text-xs font-medium text-dark-text-secondary hover:text-pink-400 px-3 py-1.5 rounded-full hover:bg-pink-500/10 transition-all duration-200"
            >
              <svg
                width="12"
                height="12"
                viewBox="0 0 24 24"
                fill="currentColor"
                stroke="none"
                className="text-pink-400/80"
              >
                <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
              </svg>
              {t("nav.sponsor")}
            </a>

            {/* 占位——移动端将语言/主题推到右侧 */}
            <div className="flex-1 sm:hidden" />

            {/* 语言选择下拉框（语言列表由 locales/*.json 自动发现） */}
            <div ref={localeMenuRef} className="relative">
              <button
                onClick={() => setLocaleMenuOpen((v) => !v)}
                className="flex items-center justify-center gap-0.5 h-7 px-1.5 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200 cursor-pointer"
                aria-label="Select language"
                aria-expanded={localeMenuOpen}
              >
                <svg
                  width="13"
                  height="13"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  className="text-dark-text-secondary"
                >
                  <circle cx="12" cy="12" r="10" />
                  <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
                  <path d="M2 12h20" />
                </svg>
                <span className="text-[10px] font-semibold text-dark-text-secondary uppercase">
                  {locale}
                </span>
              </button>
              <AnimatePresence>
                {localeMenuOpen && (
                  <motion.div
                    initial={{ opacity: 0, y: -4, scale: 0.97 }}
                    animate={{ opacity: 1, y: 0, scale: 1 }}
                    exit={{ opacity: 0, y: -4, scale: 0.97 }}
                    transition={{ duration: 0.15, ease: "easeOut" }}
                    className="absolute top-full right-0 mt-2 min-w-[140px] rounded-xl border border-dark-border bg-dark-bg/95 backdrop-blur-xl shadow-xl shadow-black/25 overflow-hidden z-50"
                  >
                    <div className="py-1.5">
                      {LOCALES.map(({ code, name }) => (
                        <button
                          key={code}
                          onClick={() => handleSelectLocale(code)}
                          className={cn(
                            "flex w-full items-center justify-between px-4 py-2 text-xs font-medium transition-colors duration-150 cursor-pointer",
                            code === locale
                              ? "text-dark-text bg-dark-surface2/60"
                              : "text-dark-text-secondary hover:text-dark-text hover:bg-dark-surface2/80",
                          )}
                        >
                          {name}
                          {code === locale && (
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
                              <polyline points="20 6 9 17 4 12" />
                            </svg>
                          )}
                        </button>
                      ))}
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>

            {/* 主题切换 */}
            <button
              onClick={handleToggleTheme}
              className="flex items-center justify-center w-7 h-7 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200 cursor-pointer"
              aria-label="Toggle theme"
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="text-dark-text-secondary"
              >
                {isLight ? (
                  <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
                ) : (
                  <>
                    <circle cx="12" cy="12" r="5" />
                    <line x1="12" y1="1" x2="12" y2="3" />
                    <line x1="12" y1="21" x2="12" y2="23" />
                    <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
                    <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
                    <line x1="1" y1="12" x2="3" y2="12" />
                    <line x1="21" y1="12" x2="23" y2="12" />
                    <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
                    <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
                  </>
                )}
              </svg>
            </button>

            {/* GitHub 仓库 */}
            <a
              href={GITHUB_REPO_URL}
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center justify-center w-7 h-7 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200"
              aria-label={t("nav.github")}
              title={t("nav.github")}
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="currentColor"
                className="text-dark-text-secondary"
              >
                <path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.55 0-.27-.01-1.17-.02-2.12-3.2.7-3.87-1.36-3.87-1.36-.52-1.33-1.28-1.68-1.28-1.68-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.19 1.76 1.19 1.03 1.76 2.69 1.25 3.35.96.1-.75.4-1.25.72-1.54-2.55-.29-5.24-1.28-5.24-5.68 0-1.26.45-2.28 1.19-3.09-.12-.29-.51-1.46.11-3.05 0 0 .97-.31 3.17 1.18a11.04 11.04 0 0 1 5.78 0c2.2-1.49 3.17-1.18 3.17-1.18.62 1.59.23 2.76.11 3.05.74.81 1.19 1.83 1.19 3.09 0 4.41-2.69 5.38-5.26 5.67.41.35.77 1.05.77 2.12 0 1.53-.01 2.76-.01 3.14 0 .3.21.67.8.55A11.51 11.51 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5z" />
              </svg>
            </a>

            {/* 分隔线 */}
            <div className="hidden sm:block h-4 w-px bg-dark-border mx-1" />

            {/* CTA 下载按钮 */}
            <a
              href="/#download"
              className="hidden sm:inline-flex items-center gap-1.5 rounded-full bg-brand-blue px-4 py-1.5 text-xs font-semibold text-white hover:bg-brand-blue/90 transition-colors"
            >
              <svg
                className="h-3.5 w-3.5"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
              </svg>
              {t("nav.download")}
            </a>
          </div>

          {/* 移动端下拉菜单 */}
          <AnimatePresence>
            {mobileOpen && (
              <motion.div
                initial={{ opacity: 0, y: -8, height: 0 }}
                animate={{ opacity: 1, y: 0, height: "auto" }}
                exit={{ opacity: 0, y: -8, height: 0 }}
                transition={{ duration: 0.2, ease: "easeOut" }}
                className="sm:hidden mt-1 overflow-hidden rounded-2xl border border-dark-border bg-dark-bg/90 backdrop-blur-xl shadow-lg shadow-black/20"
              >
                <div className="flex flex-col p-2 gap-0.5 max-h-[70vh] overflow-y-auto">
                  {mobileNavGroups.map((group, gi) => (
                    <div key={gi}>
                      {/* 分组标题 */}
                      {group.label && (
                        <div className="px-4 pt-2 pb-1">
                          <span className="text-[10px] font-semibold uppercase tracking-widest text-dark-text-muted">
                            {group.label}
                          </span>
                        </div>
                      )}
                      {group.items.map((item) => (
                        <a
                          key={item.link}
                          href={item.link}
                          target={
                            "external" in item && item.external
                              ? "_blank"
                              : undefined
                          }
                          rel={
                            "external" in item && item.external
                              ? "noopener noreferrer"
                              : undefined
                          }
                          onClick={() => setMobileOpen(false)}
                          className="flex items-center gap-2.5 text-sm font-medium text-dark-text-secondary hover:text-dark-text px-4 py-2.5 rounded-xl hover:bg-dark-surface2 transition-colors"
                        >
                          {"icon" in item && item.icon && (
                            <span className="flex-shrink-0 text-dark-text-muted">
                              {item.icon}
                            </span>
                          )}
                          {item.name}
                        </a>
                      ))}
                      {/* 分组分隔线（最后一组除外） */}
                      {gi < mobileNavGroups.length - 1 && (
                        <div className="h-px bg-dark-border/50 mx-2 my-1" />
                      )}
                    </div>
                  ))}

                  <div className="h-px bg-dark-border mx-2 my-1" />

                  {/* 移动端 CTA 下载按钮 */}
                  <a
                    href="/#download"
                    onClick={() => setMobileOpen(false)}
                    className="flex items-center justify-center gap-2 rounded-xl bg-brand-blue px-4 py-2.5 text-sm font-semibold text-white hover:bg-brand-blue/90 transition-colors"
                  >
                    <svg
                      className="h-4 w-4"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                      <polyline points="7 10 12 15 17 10" />
                      <line x1="12" y1="15" x2="12" y2="3" />
                    </svg>
                    {t("nav.download")}
                  </a>
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </motion.header>
      )}
    </AnimatePresence>
  );
}
