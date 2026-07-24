import { useState, useEffect, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { useLocale } from "@/lib/i18n";
import { ANNOUNCEMENTS } from "@/lib/announcements";
import type { Announcement } from "@/lib/announcements";

const STORAGE_KEY = "fluxdown-dismissed-announcements";

/** 官方域名列表；首项为主域名，用于"前往官网"按钮。 */
const OFFICIAL_SITES = [
  "https://www.fluxdown.com",
  "https://fluxdown.com",
  "https://fluxdown.zerx.dev",
] as const;

function getDismissed(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

function setDismissed(ids: string[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(ids));
  } catch {
    // localStorage unavailable
  }
}

export default function AnnouncementModal() {
  const { t } = useLocale();
  const [current, setCurrent] = useState<Announcement | null>(null);

  useEffect(() => {
    const dismissed = getDismissed();
    const active = ANNOUNCEMENTS.find(
      (a) => a.active && a.popup && !dismissed.includes(a.id),
    );
    if (active) setCurrent(active);
  }, []);

  const handleClose = useCallback(() => {
    if (!current) return;
    const dismissed = getDismissed();
    setDismissed([...dismissed, current.id]);
    setCurrent(null);
  }, [current]);

  const primarySite = OFFICIAL_SITES[0];

  return (
    <AnimatePresence>
      {current && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 }}
          className="fixed inset-0 z-[100] flex items-center justify-center p-4"
        >
          <div
            className="absolute inset-0 bg-black/70 backdrop-blur-sm"
            onClick={handleClose}
          />

          <motion.div
            initial={{ opacity: 0, scale: 0.94, y: 16 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.96, y: 8 }}
            transition={{ duration: 0.25, ease: [0.22, 1, 0.36, 1] }}
            role="alertdialog"
            aria-modal="true"
            className="relative w-full max-w-lg overflow-hidden rounded-2xl border border-destructive/30 bg-dark-surface1 shadow-2xl shadow-black/40"
          >
            <div className="absolute inset-x-0 top-0 h-1 bg-gradient-to-r from-destructive via-warning to-destructive" />

            <div className="p-6 sm:p-8">
              <div className="flex items-center gap-3">
                <div className="shrink-0 flex items-center justify-center w-11 h-11 rounded-full bg-destructive/15 border border-destructive/30">
                  <svg
                    width="22"
                    height="22"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    className="text-destructive"
                  >
                    <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
                    <line x1="12" y1="9" x2="12" y2="13" />
                    <line x1="12" y1="17" x2="12.01" y2="17" />
                  </svg>
                </div>
                <h2 className="text-xl sm:text-2xl font-bold tracking-tight text-dark-text">
                  {t("announcementModal.title")}
                </h2>
              </div>

              <p className="mt-5 text-sm sm:text-base text-dark-text-secondary leading-relaxed">
                {t("announcementModal.body")}
              </p>

              <div className="mt-6 space-y-3">
                <div className="rounded-xl border border-destructive/25 bg-destructive/[0.06] px-4 py-3">
                  <p className="text-[11px] font-medium uppercase tracking-wide text-destructive/90">
                    {t("announcementModal.fakeLabel")}
                  </p>
                  <p className="mt-1 font-mono text-sm text-dark-text line-through decoration-destructive/60">
                    {t("announcementModal.fakeSite")}
                  </p>
                </div>

                <div className="rounded-xl border border-success/25 bg-success/[0.06] px-4 py-3">
                  <p className="text-[11px] font-medium uppercase tracking-wide text-success/90">
                    {t("announcementModal.officialLabel")}
                  </p>
                  {OFFICIAL_SITES.map((site) => (
                    <a
                      key={site}
                      href={site}
                      className="mt-1 flex w-fit items-center gap-1.5 font-mono text-sm text-success hover:underline"
                    >
                      {site}
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
                        <path d="M7 7h10v10" />
                        <path d="M7 17 17 7" />
                      </svg>
                    </a>
                  ))}
                </div>
              </div>

              <div className="mt-7 flex flex-col-reverse sm:flex-row sm:justify-end gap-3">
                <button
                  onClick={handleClose}
                  className="inline-flex items-center justify-center rounded-lg border border-dark-border px-4 py-2.5 text-sm font-medium text-dark-text-secondary hover:bg-dark-surface2 hover:text-dark-text transition-colors cursor-pointer"
                >
                  {t("announcementModal.confirm")}
                </button>
                <a
                  href={primarySite}
                  onClick={handleClose}
                  className="inline-flex items-center justify-center rounded-lg bg-gradient-to-r from-brand-blue to-brand-cyan px-5 py-2.5 text-sm font-semibold text-white shadow-lg shadow-brand-blue/20 hover:opacity-90 transition-opacity"
                >
                  {t("announcementModal.goOfficial")}
                </a>
              </div>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
