import { useState, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { MessageSquarePlus, Lightbulb, Bug, MessageCircle, Send, Loader2, CheckCircle2, AlertCircle } from "lucide-react";
import { useLocale } from "@/lib/i18n";

type FeedbackType = "feature" | "bug" | "other";

interface FormState {
  type: FeedbackType;
  title: string;
  description: string;
  appVersion: string;
  contact: string;
}

const INITIAL_FORM: FormState = {
  type: "feature",
  title: "",
  description: "",
  appVersion: "",
  contact: "",
};

const TYPE_CONFIG: { type: FeedbackType; icon: typeof Lightbulb; colorClass: string }[] = [
  { type: "feature", icon: Lightbulb, colorClass: "text-warning border-warning/30 bg-warning/10" },
  { type: "bug", icon: Bug, colorClass: "text-danger border-danger/30 bg-danger/10" },
  { type: "other", icon: MessageCircle, colorClass: "text-brand-cyan border-brand-cyan/30 bg-brand-cyan/10" },
];

interface FeedbackSectionProps {
  onSuccess?: () => void;
}

export default function FeedbackSection({ onSuccess }: FeedbackSectionProps) {
  const { t } = useLocale();
  const [form, setForm] = useState<FormState>(INITIAL_FORM);
  const [status, setStatus] = useState<"idle" | "submitting" | "success" | "error">("idle");
  const [errorMsg, setErrorMsg] = useState("");

  const handleSubmit = useCallback(async () => {
    if (!form.title.trim() || !form.description.trim() || !form.appVersion.trim()) return;

    setStatus("submitting");
    setErrorMsg("");

    try {
      const res = await fetch("/api/feedback", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          type: form.type,
          title: form.title.trim(),
          description: form.description.trim(),
          appVersion: form.appVersion.trim(),
          contact: form.contact.trim() || undefined,
        }),
      });

      if (res.status === 429) {
        setStatus("error");
        setErrorMsg(t("fb.rateLimited"));
        return;
      }

      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        setStatus("error");
        setErrorMsg(data.error || t("fb.submitError"));
        return;
      }

      setStatus("success");
      setForm(INITIAL_FORM);
      onSuccess?.();

      // 5 秒后重置状态
      setTimeout(() => setStatus("idle"), 5000);
    } catch {
      setStatus("error");
      setErrorMsg(t("fb.submitError"));
    }
  }, [form, t, onSuccess]);

  const canSubmit =
    form.title.trim().length > 0 &&
    form.description.trim().length > 0 &&
    form.appVersion.trim().length > 0 &&
    status !== "submitting";

  return (
    <section id="feedback" className="relative py-20 sm:py-28 overflow-hidden bg-dark-bg">
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8 relative z-10">
        {/* Header */}
        <motion.div
          className="text-center max-w-2xl mx-auto mb-12"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5 }}
        >
          <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-brand-cyan/10 text-brand-cyan border border-brand-cyan/20 uppercase tracking-widest">
            <MessageSquarePlus className="w-3 h-3" />
            {t("fb.badge")}
          </span>
          <h2 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
            {t("fb.title")}
            <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("fb.titleHighlight")}
            </span>
          </h2>
          <p className="mt-4 text-dark-text-secondary text-lg">
            {t("fb.subtitle")}
          </p>
        </motion.div>

        {/* Form Card */}
        <motion.div
          className="max-w-2xl mx-auto"
          initial={{ opacity: 0, y: 30 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.6, delay: 0.1 }}
        >
          <div className="rounded-xl border border-dark-border bg-dark-surface1 p-6 sm:p-8">
            {/* Type Selector */}
            <div className="mb-6">
              <label className="block text-sm font-medium text-dark-text mb-3">
                {t("fb.typeLabel")}
              </label>
              <div className="grid grid-cols-3 gap-3">
                {TYPE_CONFIG.map(({ type, icon: Icon, colorClass }) => (
                  <button
                    key={type}
                    type="button"
                    onClick={() => setForm((f) => ({ ...f, type }))}
                    className={`relative flex flex-col items-center gap-2 rounded-lg border p-3 sm:p-4 transition-all duration-200 cursor-pointer ${
                      form.type === type
                        ? colorClass
                        : "border-dark-border bg-dark-surface2 text-dark-text-secondary hover:border-dark-text-muted hover:bg-dark-surface3"
                    }`}
                  >
                    <Icon className="w-5 h-5" />
                    <span className="text-xs font-medium">{t(`fb.type.${type}`)}</span>
                    {form.type === type && (
                      <motion.div
                        layoutId="feedback-type-indicator"
                        className="absolute inset-0 rounded-lg border-2 border-current pointer-events-none"
                        transition={{ type: "spring", bounce: 0.2, duration: 0.4 }}
                      />
                    )}
                  </button>
                ))}
              </div>
            </div>

            {/* Title */}
            <div className="mb-5">
              <label htmlFor="fb-title" className="block text-sm font-medium text-dark-text mb-2">
                {t("fb.titleLabel")} <span className="text-danger">*</span>
              </label>
              <input
                id="fb-title"
                type="text"
                maxLength={200}
                value={form.title}
                onChange={(e) => setForm((f) => ({ ...f, title: e.target.value }))}
                placeholder={t("fb.titlePlaceholder")}
                className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-4 py-2.5 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-blue/50 focus:ring-1 focus:ring-brand-blue/30 transition-colors"
              />
              <p className="mt-1 text-[10px] text-dark-text-muted text-right">{form.title.length}/200</p>
            </div>

            {/* Description */}
            <div className="mb-5">
              <label htmlFor="fb-desc" className="block text-sm font-medium text-dark-text mb-2">
                {t("fb.descLabel")} <span className="text-danger">*</span>
              </label>
              <textarea
                id="fb-desc"
                rows={5}
                maxLength={5000}
                value={form.description}
                onChange={(e) => setForm((f) => ({ ...f, description: e.target.value }))}
                placeholder={t("fb.descPlaceholder")}
                className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-4 py-2.5 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-blue/50 focus:ring-1 focus:ring-brand-blue/30 transition-colors resize-none"
              />
              <p className="mt-1 text-[10px] text-dark-text-muted text-right">{form.description.length}/5000</p>
            </div>

            {/* App Version (required) */}
            <div className="mb-5">
              <label htmlFor="fb-version" className="block text-sm font-medium text-dark-text mb-2">
                {t("fb.versionLabel")} <span className="text-danger">*</span>
              </label>
              <input
                id="fb-version"
                type="text"
                maxLength={50}
                value={form.appVersion}
                onChange={(e) => setForm((f) => ({ ...f, appVersion: e.target.value }))}
                placeholder={t("fb.versionPlaceholder")}
                className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-4 py-2.5 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-blue/50 focus:ring-1 focus:ring-brand-blue/30 transition-colors"
              />
              <p className="mt-1.5 text-xs text-dark-text-muted">{t("fb.versionHint")}</p>
            </div>

            {/* Contact (optional) */}
            <div className="mb-6">
              <label htmlFor="fb-contact" className="block text-sm font-medium text-dark-text mb-2">
                {t("fb.contactLabel")}
                <span className="text-dark-text-muted font-normal ml-1.5">({t("fb.optional")})</span>
              </label>
              <input
                id="fb-contact"
                type="text"
                value={form.contact}
                onChange={(e) => setForm((f) => ({ ...f, contact: e.target.value }))}
                placeholder={t("fb.contactPlaceholder")}
                className="w-full rounded-lg border border-dark-border bg-dark-surface2 px-4 py-2.5 text-sm text-dark-text placeholder:text-dark-text-muted focus:outline-none focus:border-brand-blue/50 focus:ring-1 focus:ring-brand-blue/30 transition-colors"
              />
              <p className="mt-1.5 text-xs text-dark-text-muted">{t("fb.contactHint")}</p>
            </div>

            {/* Submit Button + Status */}
            <div className="flex items-center gap-4">
              <button
                type="button"
                onClick={handleSubmit}
                disabled={!canSubmit}
                className={`inline-flex items-center gap-2 rounded-lg px-6 py-2.5 text-sm font-semibold transition-all duration-200 ${
                  canSubmit
                    ? "bg-brand-blue text-white hover:bg-brand-blue/90 shadow-lg shadow-brand-blue/20 cursor-pointer"
                    : "bg-dark-surface3 text-dark-text-muted cursor-not-allowed"
                }`}
              >
                {status === "submitting" ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    {t("fb.submitting")}
                  </>
                ) : (
                  <>
                    <Send className="w-4 h-4" />
                    {t("fb.submit")}
                  </>
                )}
              </button>

              {/* Status message */}
              <AnimatePresence mode="wait">
                {status === "success" && (
                  <motion.div
                    key="success"
                    initial={{ opacity: 0, x: -10 }}
                    animate={{ opacity: 1, x: 0 }}
                    exit={{ opacity: 0 }}
                    className="flex items-center gap-1.5 text-sm text-success"
                  >
                    <CheckCircle2 className="w-4 h-4" />
                    {t("fb.success")}
                  </motion.div>
                )}
                {status === "error" && (
                  <motion.div
                    key="error"
                    initial={{ opacity: 0, x: -10 }}
                    animate={{ opacity: 1, x: 0 }}
                    exit={{ opacity: 0 }}
                    className="flex items-center gap-1.5 text-sm text-danger"
                  >
                    <AlertCircle className="w-4 h-4" />
                    {errorMsg}
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
          </div>
        </motion.div>
      </div>
    </section>
  );
}
