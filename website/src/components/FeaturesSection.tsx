import { motion } from "framer-motion";
import { BentoGrid, BentoGridItem } from "@/components/ui/bento-grid";
import { Cpu, Layers, Globe, Gauge, RefreshCw, Chrome, Palette, ShieldCheck, Puzzle, Package, Zap } from "lucide-react";
import { useLocale } from "@/lib/i18n";

/** Mini terminal output — Rust engine card */
const RustTerminal = () => (
  <div className="rounded-lg border border-dark-border bg-dark-bg p-2.5 font-mono text-[10px] leading-relaxed overflow-hidden">
    <div className="text-dark-text-muted">$ cargo build --release</div>
    <div className="text-success">   Compiling hub v0.1.0</div>
    <div className="text-success">   Compiling tokio v1.42</div>
    <div className="text-brand-sky">    Finished release [optimized]</div>
  </div>
);

/** Protocol badges — Multi-Protocol card */
const ProtocolBadges = () => (
  <div className="flex flex-wrap gap-1.5">
    {[
      { name: "HTTP/2", color: "text-brand-sky bg-brand-sky/10 border-brand-sky/20" },
      { name: "HTTPS", color: "text-success bg-success/10 border-success/20" },
      { name: "FTP", color: "text-warning bg-warning/10 border-warning/20" },
      { name: "BitTorrent", color: "text-[#A855F7] bg-[#A855F7]/10 border-[#A855F7]/20" },
      { name: "ED2K", color: "text-[#EC4899] bg-[#EC4899]/10 border-[#EC4899]/20" },
    ].map((p) => (
      <span key={p.name} className={`inline-flex items-center px-2 py-0.5 rounded text-[10px] font-medium border ${p.color}`}>
        {p.name}
      </span>
    ))}
  </div>
);

/** Speed gauge bar — Speed Control card */
const SpeedGauge = () => (
  <div className="rounded-lg border border-dark-border bg-dark-bg p-2.5 space-y-1.5">
    <div className="flex items-center justify-between text-[10px]">
      <span className="text-dark-text-muted">Bandwidth</span>
      <span className="text-dark-text font-medium">32.4 MB/s</span>
    </div>
    <div className="h-1.5 rounded-full bg-dark-surface3 overflow-hidden">
      <div className="h-full rounded-full bg-gradient-to-r from-success via-warning to-danger" style={{ width: "65%" }} />
    </div>
    <div className="flex items-center justify-between text-[9px] text-dark-text-muted">
      <span>Limit: 50 MB/s</span>
      <span>65%</span>
    </div>
  </div>
);

/** Resume progress segments — Resume Anywhere card */
const ResumeProgress = () => (
  <div className="rounded-lg border border-dark-border bg-dark-bg p-2.5 space-y-1.5">
    <div className="flex items-center justify-between text-[10px]">
      <span className="text-dark-text-muted">video-4k.mkv</span>
      <span className="text-brand-sky font-medium">72%</span>
    </div>
    <div className="flex h-2 rounded-full overflow-hidden gap-px">
      <div className="bg-success" style={{ width: "45%" }} />
      <div className="bg-success/40" style={{ width: "27%" }} />
      <div className="bg-dark-surface3 flex-1" />
    </div>
    <div className="flex gap-3 text-[9px] text-dark-text-muted">
      <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-sm bg-success inline-block" /> Done</span>
      <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-sm bg-success/40 inline-block" /> Resuming</span>
      <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-sm bg-dark-surface3 inline-block" /> Pending</span>
    </div>
  </div>
);

/** Color scheme swatches — Beautiful Interface card */
const ColorSchemes = () => {
  const schemes = [
    { name: "Blue", color: "#3B82F6" },
    { name: "Green", color: "#22C55E" },
    { name: "Violet", color: "#8B5CF6" },
    { name: "Rose", color: "#F43F5E" },
    { name: "Orange", color: "#F97316" },
    { name: "Cyan", color: "#06B6D4" },
  ];
  return (
    <div className="rounded-lg border border-dark-border bg-dark-bg p-2.5 space-y-2">
      <div className="flex items-center justify-between text-[10px]">
        <span className="text-dark-text-muted">Color Scheme</span>
        <span className="text-dark-text font-medium">12 themes</span>
      </div>
      <div className="flex gap-1.5">
        {schemes.map((s) => (
          <motion.div
            key={s.name}
            className="flex-1 h-5 rounded-sm cursor-pointer"
            style={{ backgroundColor: s.color }}
            initial={{ opacity: 0, scale: 0.8 }}
            whileInView={{ opacity: 1, scale: 1 }}
            viewport={{ once: true }}
            transition={{ delay: schemes.indexOf(s) * 0.06, duration: 0.25 }}
            whileHover={{ scale: 1.15 }}
          />
        ))}
      </div>
      <div className="flex gap-3 text-[9px] text-dark-text-muted">
        <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-full bg-dark-text inline-block" /> Dark</span>
        <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-full bg-dark-surface3 border border-dark-border inline-block" /> Light</span>
      </div>
    </div>
  );
};

/** Privacy badges — Clean & Private card */
const PrivacyBadges = () => (
  <div className="flex flex-wrap gap-1.5">
    {[
      { label: "Zero Ads", color: "text-success bg-success/10 border-success/20" },
      { label: "Zero Tracking", color: "text-brand-sky bg-brand-sky/10 border-brand-sky/20" },
      { label: "No Account", color: "text-[#A855F7] bg-[#A855F7]/10 border-[#A855F7]/20" },
      { label: "Local-First", color: "text-warning bg-warning/10 border-warning/20" },
    ].map((b) => (
      <span key={b.label} className={`inline-flex items-center px-2 py-0.5 rounded text-[10px] font-medium border ${b.color}`}>
        {b.label}
      </span>
    ))}
  </div>
);

/** Plugin snippet — Plugin System card */
const PluginSnippet = () => (
  <div className="rounded-lg border border-dark-border bg-dark-bg p-2.5 font-mono text-[10px] leading-relaxed overflow-hidden">
    <div className="text-dark-text-muted">// resolver plugin</div>
    <div className="text-brand-sky">globalThis.<span className="text-[#22C55E]">resolve</span>(ctx) {'{'}</div>
    <div className="text-dark-text-secondary pl-3">return {'{'} url: direct {'}'};</div>
    <div className="text-brand-sky">{'}'}</div>
  </div>
);

/** Component badges — Managed Components card */
const ComponentBadges = () => (
  <div className="flex flex-wrap gap-1.5">
    {[
      { name: "ffmpeg", color: "text-[#22C55E] bg-[#22C55E]/10 border-[#22C55E]/20" },
      { name: "yt-dlp", color: "text-[#EC4899] bg-[#EC4899]/10 border-[#EC4899]/20" },
      { name: "sandboxed", color: "text-brand-sky bg-brand-sky/10 border-brand-sky/20" },
    ].map((c) => (
      <span key={c.name} className={`inline-flex items-center px-2 py-0.5 rounded text-[10px] font-medium border ${c.color}`}>
        {c.name}
      </span>
    ))}
  </div>
);

/** Mirror route comparison — Community Speed Network card */
const CdnRouteViz = () => {
  const routes = [
    { name: "origin", latency: "1.2 MB/s", width: "22%", color: "#64748B", best: false },
    { name: "mirror-hk", latency: "6.8 MB/s", width: "62%", color: "#38bdf8", best: false },
    { name: "mirror-sg", latency: "11.4 MB/s", width: "96%", color: "#22C55E", best: true },
  ];
  return (
    <div className="rounded-lg border border-dark-border bg-dark-bg p-2.5 space-y-1.5 font-mono text-[10px]">
      {routes.map((r) => (
        <div key={r.name} className="flex items-center gap-2">
          <span className="w-16 shrink-0 text-dark-text-muted">{r.name}</span>
          <div className="flex-1 h-1.5 rounded-full bg-dark-surface2 overflow-hidden">
            <motion.div
              className="h-full rounded-full"
              style={{ backgroundColor: r.color }}
              initial={{ width: 0 }}
              whileInView={{ width: r.width }}
              viewport={{ once: true }}
              transition={{ duration: 0.8, ease: "easeOut" }}
            />
          </div>
          <span className="w-16 shrink-0 text-right" style={{ color: r.color }}>
            {r.latency}{r.best ? " ✓" : ""}
          </span>
        </div>
      ))}
    </div>
  );
};

const IDMGridVisualization = () => {
  const colors = [
    "#3B82F6", "#22C55E", "#F59E0B", "#A855F7",
    "#06B6D4", "#EC4899", "#14B8A6", "#EF4444",
    "#8B5CF6", "#F97316", "#10B981", "#E11D48",
    "#0EA5E9", "#D946EF", "#84CC16", "#64748B",
  ];
  const cells = Array.from({ length: 64 }, (_, i) => {
    const segIdx = Math.floor(i / 4) % colors.length;
    // Deterministic "random" pattern to avoid SSR hydration mismatch
    const downloaded = !((i * 7 + 3) % 5 === 0);
    return { color: colors[segIdx], downloaded };
  });

  return (
    <div className="rounded-lg border border-dark-border bg-dark-surface2 p-2">
      <div className="grid grid-cols-16 gap-[1.5px]" style={{ gridTemplateColumns: "repeat(16, 1fr)" }}>
        {cells.map((cell, i) => (
          <motion.div
            key={i}
            className="aspect-square rounded-[1px]"
            style={{
              width: "5px",
              height: "5px",
              backgroundColor: cell.downloaded ? cell.color : `${cell.color}1F`,
            }}
            initial={{ opacity: 0, scale: 0 }}
            whileInView={{ opacity: 1, scale: 1 }}
            transition={{ delay: i * 0.008, duration: 0.2 }}
            viewport={{ once: true }}
          />
        ))}
      </div>
    </div>
  );
};

export default function FeaturesSection() {
  const { t } = useLocale();

  const features = [
    // Row 1: 4 single-col cards
    {
      title: t("features.rustTitle"),
      description: t("features.rustDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#38bdf8]/10"><Cpu className="w-5 h-5 text-[#38bdf8]" /></div>,
      className: "",
      header: <RustTerminal />,
    },
    {
      title: t("features.protoTitle"),
      description: t("features.protoDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#22C55E]/10"><Globe className="w-5 h-5 text-[#22C55E]" /></div>,
      className: "",
      header: <ProtocolBadges />,
    },
    {
      title: t("features.speedTitle"),
      description: t("features.speedDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#F59E0B]/10"><Gauge className="w-5 h-5 text-[#F59E0B]" /></div>,
      className: "",
      header: <SpeedGauge />,
    },
    {
      title: t("features.resumeTitle"),
      description: t("features.resumeDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#A855F7]/10"><RefreshCw className="w-5 h-5 text-[#A855F7]" /></div>,
      className: "",
      header: <ResumeProgress />,
    },
    // Row 2: 2 double-col cards side by side
    {
      title: t("features.segTitle"),
      description: t("features.segDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#06b6d4]/10"><Layers className="w-5 h-5 text-[#06b6d4]" /></div>,
      className: "md:col-span-2 lg:col-span-2",
      header: <IDMGridVisualization />,
    },
    {
      title: t("features.browserTitle"),
      description: t("features.browserDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#EC4899]/10"><Chrome className="w-5 h-5 text-[#EC4899]" /></div>,
      className: "md:col-span-2 lg:col-span-2",
    },
    // Row 3: 2 double-col cards — UI & Privacy
    {
      title: t("features.uiTitle"),
      description: t("features.uiDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#8B5CF6]/10"><Palette className="w-5 h-5 text-[#8B5CF6]" /></div>,
      className: "md:col-span-2 lg:col-span-2",
      header: <ColorSchemes />,
    },
    {
      title: t("features.cleanTitle"),
      description: t("features.cleanDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#22C55E]/10"><ShieldCheck className="w-5 h-5 text-[#22C55E]" /></div>,
      className: "md:col-span-2 lg:col-span-2",
      header: <PrivacyBadges />,
    },
    // Row 4: 2 double-col cards — Plugins & Components
    {
      title: t("features.pluginTitle"),
      description: t("features.pluginDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#06b6d4]/10"><Puzzle className="w-5 h-5 text-[#06b6d4]" /></div>,
      className: "md:col-span-2 lg:col-span-2",
      header: <PluginSnippet />,
    },
    {
      title: t("features.componentTitle"),
      description: t("features.componentDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#EC4899]/10"><Package className="w-5 h-5 text-[#EC4899]" /></div>,
      className: "md:col-span-2 lg:col-span-2",
      header: <ComponentBadges />,
    },
    // Row 5: full-width highlight — Community Speed Network
    {
      title: t("features.cdnTitle"),
      description: t("features.cdnDesc"),
      icon: <div className="inline-flex items-center justify-center w-10 h-10 rounded-lg bg-[#F59E0B]/10"><Zap className="w-5 h-5 text-[#F59E0B]" /></div>,
      className: "md:col-span-2 lg:col-span-4",
      header: <CdnRouteViz />,
    },
  ];

  return (
    <section id="features" className="relative py-20 sm:py-32 overflow-hidden">
      <div className="absolute top-0 left-1/2 -translate-x-1/2 w-[800px] h-[400px] bg-brand-blue/[0.02] blur-[160px] rounded-full -z-10" />

        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
        <motion.div
          className="text-center max-w-2xl mx-auto mb-16"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, margin: "-100px" }}
          transition={{ duration: 0.5 }}
        >
          <span className="inline-flex items-center px-3 py-1 rounded-full text-xs font-semibold bg-[#38bdf8]/10 text-[#38bdf8] border border-[#38bdf8]/20 uppercase tracking-widest">
            {t("features.badge")}
          </span>
          <h2 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
            {t("features.title")}
            <span className="bg-gradient-to-r from-[#38bdf8] to-[#06b6d4] bg-clip-text text-transparent">{t("features.titleHighlight")}</span>
          </h2>
          <p className="mt-4 text-dark-text-secondary text-lg leading-relaxed">
            {t("features.subtitle")}
          </p>
        </motion.div>

        <motion.div
          initial={{ opacity: 0, y: 40 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true, margin: "-100px" }}
          transition={{ duration: 0.8, delay: 0.1 }}
        >
          <BentoGrid className="max-w-7xl">
            {features.map((f, i) => (
              <BentoGridItem key={i} title={f.title} description={f.description} icon={f.icon} header={f.header} className={f.className} />
            ))}
          </BentoGrid>
        </motion.div>
      </div>
    </section>
  );
}
