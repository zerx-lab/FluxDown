import { useState, useEffect } from "react";
import { useLocale } from "@/lib/i18n";

export default function Footer() {
  const { t } = useLocale();
  // SSR 安全：初始值固定，useEffect 中更新为实际年份，避免 hydration mismatch
  const [year, setYear] = useState(2025);
  useEffect(() => {
    setYear(new Date().getFullYear());
  }, []);

  return (
    <footer className="relative overflow-hidden bg-dark-bg">
      {/* Top gradient divider */}
      <div className="relative h-px w-full">
        <div className="absolute inset-0 bg-gradient-to-r from-transparent via-brand-sky/30 to-transparent" />
      </div>

      {/* Main content */}
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8 pt-14 sm:pt-16 pb-8">
        <div className="grid grid-cols-2 gap-10 sm:grid-cols-6 lg:grid-cols-12">
          {/* Brand column */}
          <div className="col-span-2 sm:col-span-6 lg:col-span-5">
            <a href="/" className="inline-flex items-center gap-2.5 group">
              <img
                src="/logo.svg"
                alt="FluxDown"
                className="h-8 w-8 transition-transform duration-300 group-hover:scale-110"
              />
              <span className="text-lg font-bold tracking-tight">
                <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
                  Flux
                </span>
                <span className="text-dark-text">Down</span>
              </span>
            </a>

            <p className="mt-4 text-[13px] leading-relaxed text-dark-text-secondary max-w-sm">
              {t("footer.desc")}
            </p>

            {/* Tech badges */}
            <div className="mt-5 flex flex-wrap gap-2">
              {["Rust", "Flutter", "Tokio", "SQLite"].map((tech) => (
                <span
                  key={tech}
                  className="inline-flex items-center rounded-md border border-dark-border/60 bg-dark-surface1 px-2 py-0.5 text-[10px] font-medium text-dark-text-muted tracking-wide"
                >
                  {tech}
                </span>
              ))}
            </div>
          </div>

          {/* Product column */}
          <div className="col-span-1 sm:col-span-2 lg:col-span-2">
            <h3 className="text-xs font-semibold uppercase tracking-widest text-dark-text-muted mb-4">
              {t("footer.product")}
            </h3>
            <ul className="space-y-2.5">
              {[
                { href: "/#features", label: t("footer.features") },
                { href: "/#extension", label: t("footer.browserExtension") },
                { href: "/#download", label: t("footer.download") },
                { href: "/changelog", label: t("footer.changelog") },
                { href: "/theme-builder", label: t("footer.themeBuilder") },
              ].map(({ href, label }) => (
                <li key={href}>
                  <a
                    href={href}
                    className="text-[13px] text-dark-text-secondary hover:text-brand-sky transition-colors duration-200"
                  >
                    {label}
                  </a>
                </li>
              ))}
            </ul>
          </div>

          {/* Resources column */}
          <div className="col-span-1 sm:col-span-2 lg:col-span-2">
            <h3 className="text-xs font-semibold uppercase tracking-widest text-dark-text-muted mb-4">
              {t("footer.support")}
            </h3>
            <ul className="space-y-2.5">
              {[
                { href: "/faq", label: t("footer.faq") },
                { href: "/feedback", label: t("footer.feedback") },
                { href: "/feedback", label: t("footer.contact") },
              ].map(({ href, label }, i) => (
                <li key={`${href}-${i}`}>
                  <a
                    href={href}
                    className="text-[13px] text-dark-text-secondary hover:text-brand-sky transition-colors duration-200"
                  >
                    {label}
                  </a>
                </li>
              ))}
            </ul>
          </div>

          {/* Legal column */}
          <div className="col-span-1 sm:col-span-2 lg:col-span-2 lg:col-start-11">
            <h3 className="text-xs font-semibold uppercase tracking-widest text-dark-text-muted mb-4">
              {t("footer.legal")}
            </h3>
            <ul className="space-y-2.5">
              {[
                { href: "/privacy", label: t("footer.privacy") },
                { href: "/terms", label: t("footer.terms") },
              ].map(({ href, label }) => (
                <li key={href}>
                  <a
                    href={href}
                    className="text-[13px] text-dark-text-secondary hover:text-brand-sky transition-colors duration-200"
                  >
                    {label}
                  </a>
                </li>
              ))}
            </ul>
          </div>
        </div>

        {/* Bottom bar */}
        <div className="mt-14 pt-6 border-t border-dark-border/50">
          <div className="flex flex-col sm:flex-row items-center justify-between gap-4">
            <p className="text-xs text-dark-text-muted/80">
              {t("footer.copyright", { year: String(year) })}
            </p>

            <span className="text-[11px] text-dark-text-muted/50">
              {t("footer.builtWith")}
            </span>
          </div>
        </div>
      </div>

      {/* Background decorative glow */}
      <div className="absolute bottom-0 left-1/2 -translate-x-1/2 w-[600px] h-[200px] bg-brand-sky/[0.02] blur-[120px] rounded-full pointer-events-none" />
    </footer>
  );
}
