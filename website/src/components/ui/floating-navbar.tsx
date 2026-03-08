import { useState, useEffect, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { cn } from "@/lib/utils";
import { useLocale } from "@/lib/i18n";

declare global {
  interface Window {
    __toggleTheme: () => void;
    __isLightTheme: () => boolean;
  }
}

export function FloatingNavbar({
  className,
}: {
  className?: string;
}) {
  const [visible, setVisible] = useState(true);
  const [scrolled, setScrolled] = useState(false);
  const [lastScrollY, setLastScrollY] = useState(0);
  const [isLight, setIsLight] = useState(false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const { locale, setLocale, t } = useLocale();

  const navItems = [
    { name: t("nav.features"), link: "/#features" },
    { name: t("nav.extension"), link: "/#extension" },
    { name: t("nav.download"), link: "/#download" },
    { name: t("nav.announcements"), link: "/announcements" },
    { name: t("nav.feedback"), link: "/feedback" },
    { name: t("nav.changelog"), link: "/changelog" },
    { name: t("nav.themeBuilder"), link: "/theme-builder" },
  ];

  useEffect(() => {
    // Sync initial theme state
    setIsLight(window.__isLightTheme?.() ?? false);
    const onThemeChange = (e: CustomEvent<{ light: boolean }>) => {
      setIsLight(e.detail.light);
    };
    window.addEventListener("theme-change", onThemeChange as EventListener);
    return () => window.removeEventListener("theme-change", onThemeChange as EventListener);
  }, []);

  const handleToggleTheme = useCallback(() => {
    window.__toggleTheme?.();
  }, []);

  const handleToggleLocale = useCallback(() => {
    setLocale(locale === "zh" ? "en" : "zh");
  }, [locale, setLocale]);

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
            {/* Mobile hamburger — visible only on small screens */}
            <button
              onClick={() => setMobileOpen((v) => !v)}
              className="flex sm:hidden items-center justify-center w-7 h-7 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200 cursor-pointer"
              aria-label="Toggle menu"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-dark-text-secondary">
                {mobileOpen
                  ? <><line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" /></>
                  : <><line x1="4" y1="7" x2="20" y2="7" /><line x1="4" y1="12" x2="20" y2="12" /><line x1="4" y1="17" x2="20" y2="17" /></>
                }
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

            {/* Separator */}
            <div className="hidden sm:block h-4 w-px bg-dark-border mx-1" />

            {/* Nav links — hidden on very small screens */}
            {navItems.map((item) => (
              <a
                key={item.link}
                href={item.link}
                className="hidden sm:inline-block text-xs font-medium text-dark-text-secondary hover:text-dark-text px-3 py-1.5 rounded-full hover:bg-dark-surface3/50 transition-all duration-200"
              >
                {item.name}
              </a>
            ))}

            {/* Spacer — pushes lang/theme to right on mobile */}
            <div className="flex-1 sm:hidden" />

            {/* Language toggle */}
            <button
              onClick={handleToggleLocale}
              className="flex items-center justify-center w-7 h-7 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200 cursor-pointer"
              aria-label="Toggle language"
            >
              <span className="text-[10px] font-semibold text-dark-text-secondary">
                {locale === "zh" ? "EN" : "中"}
              </span>
            </button>

            {/* Theme toggle */}
            <button
              onClick={handleToggleTheme}
              className="flex items-center justify-center w-7 h-7 rounded-full hover:bg-dark-surface3/50 transition-colors duration-200 cursor-pointer"
              aria-label="Toggle theme"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-dark-text-secondary">
                {isLight
                  ? <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
                  : <><circle cx="12" cy="12" r="5" /><line x1="12" y1="1" x2="12" y2="3" /><line x1="12" y1="21" x2="12" y2="23" /><line x1="4.22" y1="4.22" x2="5.64" y2="5.64" /><line x1="18.36" y1="18.36" x2="19.78" y2="19.78" /><line x1="1" y1="12" x2="3" y2="12" /><line x1="21" y1="12" x2="23" y2="12" /><line x1="4.22" y1="19.78" x2="5.64" y2="18.36" /><line x1="18.36" y1="5.64" x2="19.78" y2="4.22" /></>
                }
              </svg>
            </button>

            {/* Separator — hidden on very small screens */}
            <div className="hidden sm:block h-4 w-px bg-dark-border mx-1" />

            {/* CTA */}
            <a
              href="/#download"
              className="hidden sm:inline-flex items-center gap-1.5 rounded-full bg-brand-blue px-4 py-1.5 text-xs font-semibold text-white hover:bg-brand-blue/90 transition-colors"
            >
              <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
              </svg>
              {t("nav.download")}
            </a>
          </div>

          {/* Mobile dropdown menu */}
          <AnimatePresence>
            {mobileOpen && (
              <motion.div
                initial={{ opacity: 0, y: -8, height: 0 }}
                animate={{ opacity: 1, y: 0, height: "auto" }}
                exit={{ opacity: 0, y: -8, height: 0 }}
                transition={{ duration: 0.2, ease: "easeOut" }}
                className="sm:hidden mt-1 overflow-hidden rounded-2xl border border-dark-border bg-dark-bg/90 backdrop-blur-xl shadow-lg shadow-black/20"
              >
                <div className="flex flex-col p-2 gap-0.5">
                  {navItems.map((item) => (
                    <a
                      key={item.link}
                      href={item.link}
                      onClick={() => setMobileOpen(false)}
                      className="text-sm font-medium text-dark-text-secondary hover:text-dark-text px-4 py-2.5 rounded-xl hover:bg-dark-surface2 transition-colors"
                    >
                      {item.name}
                    </a>
                  ))}
                  <div className="h-px bg-dark-border mx-2 my-1" />
                  <a
                    href="/#download"
                    onClick={() => setMobileOpen(false)}
                    className="flex items-center justify-center gap-2 rounded-xl bg-brand-blue px-4 py-2.5 text-sm font-semibold text-white hover:bg-brand-blue/90 transition-colors"
                  >
                    <svg className="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
