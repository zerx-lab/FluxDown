import { useState, useCallback } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { HelpCircle, ChevronDown } from "lucide-react";
import { useLocale } from "@/lib/i18n";
import type { Messages } from "@/lib/locales";

/** FAQ keys stored in locale files as faq.items.0.q / faq.items.0.a etc. */
const FAQ_COUNT = 9;

function FaqItem({
  index,
  t,
}: {
  index: number;
  t: (key: keyof Messages) => string;
}) {
  const [open, setOpen] = useState(false);
  const toggle = useCallback(() => setOpen((o) => !o), []);

  const qKey = `faq.items.${index}.q` as keyof Messages;
  const aKey = `faq.items.${index}.a` as keyof Messages;

  return (
    <div className="border-b border-dark-border last:border-b-0">
      <button
        type="button"
        onClick={toggle}
        className="flex w-full items-center justify-between gap-4 py-5 text-left cursor-pointer group"
      >
        <span className="text-sm font-medium text-dark-text group-hover:text-brand-sky transition-colors">
          {t(qKey)}
        </span>
        <ChevronDown
          className={`w-4 h-4 flex-shrink-0 text-dark-text-muted transition-transform duration-200 ${open ? "rotate-180" : ""}`}
        />
      </button>
      <AnimatePresence initial={false}>
        {open && (
          <motion.div
            key="answer"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.2, ease: "easeInOut" }}
            className="overflow-hidden"
          >
            <p className="pb-5 text-sm text-dark-text-secondary leading-relaxed">
              {t(aKey)}
            </p>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

export default function FaqSection() {
  const { t } = useLocale();

  return (
    <section className="relative py-20 sm:py-28 bg-dark-bg">
      <div className="mx-auto max-w-3xl px-4 sm:px-6 lg:px-8">
        {/* Header */}
        <motion.div
          className="text-center mb-12"
          initial={{ opacity: 0, y: 20 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5 }}
        >
          <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-semibold bg-brand-sky/10 text-brand-sky border border-brand-sky/20 uppercase tracking-widest">
            <HelpCircle className="w-3 h-3" />
            {t("faq.badge")}
          </span>
          <h1 className="mt-6 text-3xl sm:text-4xl lg:text-5xl font-bold tracking-tight text-dark-text">
            {t("faq.title")}
            <span className="bg-gradient-to-r from-brand-sky to-brand-cyan bg-clip-text text-transparent">
              {t("faq.titleHighlight")}
            </span>
          </h1>
          <p className="mt-4 text-dark-text-secondary text-lg">
            {t("faq.subtitle")}
          </p>
        </motion.div>

        {/* FAQ List */}
        <motion.div
          className="rounded-xl border border-dark-border bg-dark-surface1 px-6"
          initial={{ opacity: 0, y: 30 }}
          whileInView={{ opacity: 1, y: 0 }}
          viewport={{ once: true }}
          transition={{ duration: 0.6, delay: 0.1 }}
        >
          {Array.from({ length: FAQ_COUNT }, (_, i) => (
            <FaqItem key={i} index={i} t={t} />
          ))}
        </motion.div>

        {/* CTA */}
        <motion.div
          className="mt-10 text-center"
          initial={{ opacity: 0 }}
          whileInView={{ opacity: 1 }}
          viewport={{ once: true }}
          transition={{ duration: 0.5, delay: 0.2 }}
        >
          <p className="text-sm text-dark-text-secondary">
            {t("faq.moreQuestions")}{" "}
            <a href="/feedback" className="text-brand-blue hover:underline">
              {t("faq.contactUs")}
            </a>
          </p>
        </motion.div>
      </div>
    </section>
  );
}
