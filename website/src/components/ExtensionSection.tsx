import { motion, AnimatePresence } from "framer-motion";
import { MousePointerClick, Shield, Filter } from "lucide-react";
import { DotBackground } from "@/components/ui/grid-background";
import { useState, useEffect } from "react";
import { useLocale } from "@/lib/i18n";

/* ============================================================
   ExtensionSection — Interactive Browser Extension Mockup
   Users can:
   - Click extension icon to show/hide popup
   - Toggle Auto Intercept on/off
   - Click file type filter badges to toggle them
   - See animated stats counting & new file catches appearing
   ============================================================ */

const FILE_TYPES = [".zip", ".exe", ".dmg", ".mp4", ".pdf", ".rar", ".iso"];

const INITIAL_CATCHES = [
  "video-hd.mp4",
  "report-2025.pdf",
  "project-archive.zip",
];

// New catches that appear periodically
const INCOMING_FILES = [
  "flutter-sdk-3.27.dmg",
  "design-assets.rar",
  "podcast-ep42.mp3",
  "database-backup.sql",
];

export default function ExtensionSection() {
  const [popupVisible, setPopupVisible] = useState(true);
  const [toggleOn, setToggleOn] = useState(true);
  const [activeFilters, setActiveFilters] = useState<Set<string>>(new Set(FILE_TYPES));
  const [catches, setCatches] = useState(INITIAL_CATCHES);
  const [stats, setStats] = useState({ today: 12, week: 47, total: 384 });
  const [incomingIdx, setIncomingIdx] = useState(0);
  const { t } = useLocale();

  // Periodically add new catches when toggle is ON
  useEffect(() => {
    if (!toggleOn) return;
    const interval = setInterval(() => {
      setIncomingIdx((prev) => {
        const nextIdx = (prev + 1) % INCOMING_FILES.length;
        const newFile = INCOMING_FILES[prev % INCOMING_FILES.length]!;
        setCatches((c) => [newFile, ...c.slice(0, 2)]);
        setStats((s) => ({
          today: s.today + 1,
          week: s.week + 1,
          total: s.total + 1,
        }));
        return nextIdx;
      });
    }, 4000);
    return () => clearInterval(interval);
  }, [toggleOn]);

  const toggleFilter = (ext: string) => {
    setActiveFilters((prev) => {
      const next = new Set(prev);
      if (next.has(ext)) next.delete(ext);
      else next.add(ext);
      return next;
    });
  };

  return (
    <section id="extension" className="relative py-20 sm:py-32 overflow-hidden">
      <DotBackground className="absolute inset-0 -z-10" />

      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8 relative z-10">
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-16 items-center">
          {/* Left: Content */}
          <motion.div
            initial={{ opacity: 0, x: -40 }}
            whileInView={{ opacity: 1, x: 0 }}
            viewport={{ once: true, margin: "-100px" }}
            transition={{ duration: 0.7 }}
            className="space-y-6"
          >
            <div>
              <span className="inline-flex items-center px-3 py-1 rounded-full text-xs font-semibold bg-[#06b6d4]/10 text-[#06b6d4] border border-[#06b6d4]/20 uppercase tracking-widest mb-4">
                {t("ext.badge")}
              </span>
              <h2 className="text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
                {t("ext.title")}
                <span className="bg-gradient-to-r from-[#06b6d4] to-[#38bdf8] bg-clip-text text-transparent">{t("ext.titleHighlight")}</span>
              </h2>
              <p className="mt-4 text-dark-text-secondary text-lg leading-relaxed">
                {t("ext.subtitle")}
              </p>
            </div>

            <div className="space-y-4">
              {[
                { Icon: MousePointerClick, iconBoxClass: "bg-sky-500/10 border-sky-500/20", iconClass: "text-sky-400", titleKey: "ext.feat1.title" as const, descKey: "ext.feat1.desc" as const },
                { Icon: Shield, iconBoxClass: "bg-emerald-500/10 border-emerald-500/20", iconClass: "text-emerald-400", titleKey: "ext.feat2.title" as const, descKey: "ext.feat2.desc" as const },
                { Icon: Filter, iconBoxClass: "bg-violet-500/10 border-violet-500/20", iconClass: "text-violet-400", titleKey: "ext.feat3.title" as const, descKey: "ext.feat3.desc" as const },
              ].map((item, i) => (
                <motion.div
                  key={item.titleKey}
                  initial={{ opacity: 0, y: 20 }}
                  whileInView={{ opacity: 1, y: 0 }}
                  viewport={{ once: true }}
                  transition={{ delay: 0.2 + i * 0.1, duration: 0.5 }}
                  className="flex gap-4"
                >
                  <div className={`shrink-0 w-10 h-10 rounded-lg border flex items-center justify-center ${item.iconBoxClass}`}>
                    <item.Icon className={`w-5 h-5 ${item.iconClass}`} />
                  </div>
                  <div>
                    <h4 className="text-sm font-semibold text-dark-text">{t(item.titleKey)}</h4>
                    <p className="text-xs text-dark-text-secondary mt-0.5">{t(item.descKey)}</p>
                  </div>
                </motion.div>
              ))}
            </div>

            <motion.div
              initial={{ opacity: 0, y: 20 }}
              whileInView={{ opacity: 1, y: 0 }}
              viewport={{ once: true }}
              transition={{ delay: 0.5, duration: 0.5 }}
              className="flex flex-wrap gap-3 pt-2"
            >
              <a
                href="https://chromewebstore.google.com/detail/fluxdown/meleenglfggcmcajknpeeeiobnpfmahc"
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-2 rounded-lg bg-[#06b6d4]/10 border border-[#06b6d4]/30 px-5 py-2.5 text-sm font-semibold text-[#06b6d4] hover:bg-[#06b6d4]/20 transition-colors"
              >
                <svg className="w-4 h-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                  <path d="M12 0C8.21 0 4.831 1.757 2.632 4.501l3.953 6.848A5.454 5.454 0 0 1 12 6.545h10.691A12 12 0 0 0 12 0zM1.931 5.47A11.943 11.943 0 0 0 0 12c0 6.012 4.42 10.991 10.189 11.864l3.953-6.847a5.45 5.45 0 0 1-6.865-2.29zm13.342 2.166a5.446 5.446 0 0 1 1.45 7.09l.002.001h-.002l-5.344 9.257c.206.01.413.016.621.016 6.627 0 12-5.373 12-12 0-1.54-.29-3.011-.818-4.364zM12 16.364a4.364 4.364 0 1 1 0-8.728 4.364 4.364 0 0 1 0 8.728z"/>
                </svg>
                {t("ext.addToChrome")}
              </a>
            </motion.div>

          </motion.div>

          {/* Right: Interactive Extension Popup Mockup */}
          <motion.div
            initial={{ opacity: 0, x: 40 }}
            whileInView={{ opacity: 1, x: 0 }}
            viewport={{ once: true, margin: "-100px" }}
            transition={{ duration: 0.7, delay: 0.1 }}
            className="relative"
          >
            <div className="absolute -inset-8 rounded-3xl bg-gradient-to-br from-[#06b6d4]/10 via-transparent to-brand-blue/10 blur-2xl opacity-50 pointer-events-none" />

            <div className="relative rounded-xl border border-dark-border bg-dark-surface2 shadow-2xl overflow-hidden max-w-sm mx-auto select-none">
              {/* Browser toolbar */}
              <div className="flex items-center gap-2 px-3 py-2.5 border-b border-dark-border bg-dark-surface1">
                <div className="flex gap-1.5">
                  <div className="w-2.5 h-2.5 rounded-full bg-danger/60 hover:bg-danger transition-colors cursor-pointer" />
                  <div className="w-2.5 h-2.5 rounded-full bg-warning/60 hover:bg-warning transition-colors cursor-pointer" />
                  <div className="w-2.5 h-2.5 rounded-full bg-success/60 hover:bg-success transition-colors cursor-pointer" />
                </div>
                <div className="flex-1 mx-2 bg-dark-bg rounded-md px-3 py-1">
                  <span className="text-[10px] text-dark-text-muted">example.com/downloads</span>
                </div>
                {/* Extension icon — clickable */}
                <motion.div
                  onClick={() => setPopupVisible((v) => !v)}
                  className="w-6 h-6 rounded flex items-center justify-center cursor-pointer"
                  whileTap={{ scale: 0.9 }}
                  animate={{
                    backgroundColor: popupVisible ? "rgba(59,130,246,0.3)" : "rgba(59,130,246,0.1)",
                  }}
                  transition={{ duration: 0.15 }}
                >
                  <img src="/logo.svg" alt="" className="w-4 h-4" />
                </motion.div>
              </div>

              {/* Popup content — toggled by clicking extension icon */}
              <AnimatePresence>
                {popupVisible && (
                  <motion.div
                    initial={{ height: 0, opacity: 0 }}
                    animate={{ height: "auto", opacity: 1 }}
                    exit={{ height: 0, opacity: 0 }}
                    transition={{ duration: 0.25, ease: "easeInOut" }}
                    className="overflow-hidden"
                  >
                    <div className="bg-dark-surface1 p-4 space-y-4">
                      {/* Header */}
                      <div className="flex items-center justify-between">
                        <div className="flex items-center gap-2">
                          <img src="/logo.svg" alt="" className="w-6 h-6" />
                          <span className="text-xs font-semibold">
                            <span className="text-brand-sky">Flux</span>
                            <span className="text-dark-text">Down</span>
                          </span>
                        </div>
                        <div className="flex items-center gap-1.5">
                          <motion.div
                            className="w-2 h-2 rounded-full"
                            animate={{
                              backgroundColor: toggleOn ? "#22C55E" : "#52525B",
                              scale: toggleOn ? [1, 1.3, 1] : 1,
                            }}
                            transition={{ scale: { repeat: Infinity, duration: 2 }, backgroundColor: { duration: 0.2 } }}
                          />
                          <span
                            className="text-[10px] font-medium transition-colors duration-200"
                            style={{ color: toggleOn ? "#22C55E" : "var(--color-dark-text-muted)" }}
                          >
                            {toggleOn ? t("ext.connected") : t("ext.paused")}
                          </span>
                        </div>
                      </div>

                      {/* Stats */}
                      <div className="grid grid-cols-3 gap-2">
                        {[
                          { v: stats.today, l: t("ext.today") },
                          { v: stats.week, l: t("ext.thisWeek") },
                          { v: stats.total, l: t("ext.total") },
                        ].map((s) => (
                          <div key={s.l} className="rounded-lg bg-dark-surface2 border border-dark-border p-2.5 text-center">
                            <motion.div
                              key={s.v}
                              className="text-base font-bold text-dark-text"
                              style={{ fontVariantNumeric: "tabular-nums" }}
                              initial={{ y: -4, opacity: 0.5 }}
                              animate={{ y: 0, opacity: 1 }}
                              transition={{ duration: 0.25 }}
                            >
                              {s.v}
                            </motion.div>
                            <div className="text-[9px] text-dark-text-muted mt-0.5">{s.l}</div>
                          </div>
                        ))}
                      </div>

                      {/* Toggle — clickable */}
                      <div
                        onClick={() => setToggleOn((v) => !v)}
                        className="flex items-center justify-between rounded-lg bg-dark-surface2 border border-dark-border p-3 cursor-pointer hover:border-dark-surface3 transition-colors"
                      >
                        <span className="text-xs font-medium text-dark-text">{t("ext.autoIntercept")}</span>
                        <motion.div
                          className="relative w-9 h-5 rounded-full"
                          animate={{ backgroundColor: toggleOn ? "#22C55E" : "var(--color-dark-text-muted)" }}
                          transition={{ duration: 0.2 }}
                        >
                          <motion.div
                            className="absolute top-0.5 w-4 h-4 rounded-full bg-white shadow-sm"
                            animate={{ x: toggleOn ? 18 : 2 }}
                            transition={{ type: "spring", stiffness: 500, damping: 30 }}
                          />
                        </motion.div>
                      </div>

                      {/* Recent catches — animated list */}
                      <div>
                        <div className="text-[10px] text-dark-text-muted mb-2 font-medium">{t("ext.recentCatches")}</div>
                        <div className="space-y-0">
                          <AnimatePresence mode="popLayout" initial={false}>
                            {catches.map((f, i) => (
                              <motion.div
                                key={f}
                                layout
                                initial={{ opacity: 0, x: -20, height: 0 }}
                                animate={{ opacity: 1, x: 0, height: 24 }}
                                exit={{ opacity: 0, x: 20, height: 0 }}
                                transition={{ duration: 0.3 }}
                                className="flex items-center gap-2 text-xs text-dark-text-secondary overflow-hidden cursor-default"
                                style={{ height: 24 }}
                              >
                                <div
                                  className="w-1.5 h-1.5 rounded-full shrink-0"
                                  style={{ backgroundColor: i === 0 && toggleOn ? "#3B82F6" : "#22C55E" }}
                                />
                                <span className="truncate">{f}</span>
                              </motion.div>
                            ))}
                          </AnimatePresence>
                        </div>
                      </div>

                      {/* File type filters — clickable badges */}
                      <div>
                        <div className="text-[10px] text-dark-text-muted mb-2 font-medium">{t("ext.fileTypeFilters")}</div>
                        <div className="flex flex-wrap gap-1.5">
                          {FILE_TYPES.map((ext) => {
                            const active = activeFilters.has(ext);
                            return (
                              <motion.span
                                key={ext}
                                onClick={() => toggleFilter(ext)}
                                className="px-2 py-0.5 text-[10px] rounded border cursor-pointer"
                                whileTap={{ scale: 0.93 }}
                                animate={{
                                  backgroundColor: active ? "rgba(59,130,246,0.12)" : "var(--color-dark-surface2)",
                                  borderColor: active ? "rgba(59,130,246,0.35)" : "var(--color-dark-border)",
                                  color: active ? "#3B82F6" : "var(--color-dark-text-muted)",
                                }}
                                transition={{ duration: 0.15 }}
                              >
                                {ext}
                              </motion.span>
                            );
                          })}
                        </div>
                      </div>

                      {/* Quick settings */}
                      <div className="space-y-2 pt-2 border-t border-dark-border">
                        <div className="flex items-center justify-between text-xs">
                          <span className="text-dark-text-muted">{t("ext.minFileSize")}</span>
                          <span className="text-dark-text">1 MB</span>
                        </div>
                      </div>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
          </motion.div>
        </div>
      </div>
    </section>
  );
}
