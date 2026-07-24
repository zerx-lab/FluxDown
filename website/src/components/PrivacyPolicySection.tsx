import { useLocale } from "@/lib/i18n";

export default function PrivacyPolicySection() {
  const { t } = useLocale();

  return (
    <section className="relative py-20 sm:py-28 overflow-hidden bg-dark-bg">
      <div className="mx-auto max-w-3xl px-4 sm:px-6 lg:px-8 relative z-10">
        <div className="text-center mb-12">
          <h1 className="text-3xl sm:text-4xl font-bold tracking-tight text-dark-text">
            {t("privacy.title")}
          </h1>
          <p className="mt-3 text-dark-text-muted text-sm">
            {t("privacy.lastUpdated")}
          </p>
        </div>

        <div className="prose-custom space-y-8">
          {/* Introduction */}
          <div>
            <p className="text-dark-text-secondary leading-relaxed text-sm">
              {t("privacy.intro")}
            </p>
          </div>

          {/* Section 1: Information We Collect */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s1.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm mb-3">
              {t("privacy.s1.desc")}
            </p>
            <ul className="space-y-2 text-sm text-dark-text-secondary list-disc list-inside">
              <li>{t("privacy.s1.item1")}</li>
              <li>{t("privacy.s1.item2")}</li>
              <li>{t("privacy.s1.item3")}</li>
              <li>{t("privacy.s1.item4")}</li>
            </ul>
          </div>

          {/* Section 2: Information We Do NOT Collect */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s2.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm mb-3">
              {t("privacy.s2.desc")}
            </p>
            <ul className="space-y-2 text-sm text-dark-text-secondary list-disc list-inside">
              <li>{t("privacy.s2.item1")}</li>
              <li>{t("privacy.s2.item2")}</li>
              <li>{t("privacy.s2.item3")}</li>
              <li>{t("privacy.s2.item4")}</li>
            </ul>
          </div>

          {/* Section 3: Browser Extension */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s3.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm mb-3">
              {t("privacy.s3.desc")}
            </p>
            <ul className="space-y-2 text-sm text-dark-text-secondary list-disc list-inside">
              <li>{t("privacy.s3.item1")}</li>
              <li>{t("privacy.s3.item2")}</li>
              <li>{t("privacy.s3.item3")}</li>
            </ul>
          </div>

          {/* Section 4: Website Analytics */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s4.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm">
              {t("privacy.s4.desc")}
            </p>
          </div>

          {/* Section 5: Data Storage */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s5.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm">
              {t("privacy.s5.desc")}
            </p>
          </div>

          {/* Section 6: Third-Party Services */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s6.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm mb-3">
              {t("privacy.s6.desc")}
            </p>
            <ul className="space-y-2 text-sm text-dark-text-secondary list-disc list-inside">
              <li>{t("privacy.s6.item1")}</li>
              <li>{t("privacy.s6.item2")}</li>
            </ul>
          </div>

          {/* Section 7: Children's Privacy */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s7.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm">
              {t("privacy.s7.desc")}
            </p>
          </div>

          {/* Section 8: Changes to This Policy */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s8.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm">
              {t("privacy.s8.desc")}
            </p>
          </div>

          {/* Section 9: Contact */}
          <div>
            <h2 className="text-lg font-semibold text-dark-text mb-3">
              {t("privacy.s9.title")}
            </h2>
            <p className="text-dark-text-secondary leading-relaxed text-sm">
              {t("privacy.s9.desc")}
            </p>
          </div>
        </div>
      </div>
    </section>
  );
}
