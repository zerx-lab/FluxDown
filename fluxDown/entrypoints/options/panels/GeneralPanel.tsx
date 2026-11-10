import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { browser } from "wxt/browser";
import { Button } from "@/ui/components/Button";
import { Select } from "@/ui/components/Select";
import { showToast } from "@/ui/components/Toast";
import { usePlatform } from "@/ui/hooks/usePlatform";
import { bumpLocale, useT } from "@/ui/i18n";
import { saveTheme } from "@/ui/theme";
import { exportConnectionDiagnostics } from "@/utils/connection-diagnostics";
import { clearLocale, saveLocale } from "@/utils/i18n";
import { settings, updateSettings, useLocalValue } from "../lib/store";
import { PanelShell, SettingGroup, SettingRow, ToggleRow } from "../parts";

type LocaleChoice = "auto" | "zh-CN" | "zh-TW" | "en";
type ThemeChoice = "system" | "light" | "dark";

export function GeneralPanel(): JSX.Element | null {
  const t = useT();
  const s = settings.value;
  const [locale] = useLocalValue<LocaleChoice>("fluxdown_locale", (raw) =>
    raw === "zh-CN" || raw === "zh-TW" || raw === "en" ? raw : "auto",
  );
  const [theme] = useLocalValue<ThemeChoice>("theme", (raw) => (raw === "light" || raw === "dark" ? raw : "system"));
  const [exporting, setExporting] = useState(false);
  const platform = usePlatform();
  if (!s) return null;

  const changeLocale = async (next: LocaleChoice) => {
    if (next === "auto") await clearLocale();
    else await saveLocale(next);
    bumpLocale();
  };

  const resetStats = async () => {
    await browser.storage.local.set({ stats: { sent: 0, failed: 0, date: new Date().toDateString() } });
    showToast(t("stats.resetDone"), "success");
  };

  const exportDiagnostics = async () => {
    setExporting(true);
    try {
      await exportConnectionDiagnostics();
      showToast(t("diagnostics.exportStarted"), "success");
    } catch {
      showToast(t("diagnostics.exportFailed"), "error");
    } finally {
      setExporting(false);
    }
  };

  return (
    <PanelShell title={t("options.nav.general")}>
      <SettingGroup title={t("options.general.groupInterface")}>
        <SettingRow title={t("options.general.languageTitle")} desc={t("options.general.languageDesc")}>
          <Select<LocaleChoice>
            value={locale}
            onChange={changeLocale}
            options={[
              { value: "auto", label: t("options.general.languageAuto") },
              { value: "zh-CN", label: "简体中文" },
              { value: "zh-TW", label: "繁體中文（台灣）" },
              { value: "en", label: "English" },
            ]}
          />
        </SettingRow>

        <SettingRow title={t("options.general.themeTitle")} desc={t("options.general.themeDesc")}>
          <Select<ThemeChoice>
            value={theme}
            onChange={(v) => void saveTheme(v)}
            options={[
              { value: "system", label: t("options.theme.system") },
              { value: "light", label: t("options.theme.light") },
              { value: "dark", label: t("options.theme.dark") },
            ]}
          />
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={t("options.general.groupNotify")}>
        {platform.android ? null : (
          <ToggleRow
            title={t("options.general.notifyLocalTitle")}
            desc={t("options.general.notifyLocalDesc")}
            checked={s.notifyLocalTask}
            onChange={(v) => void updateSettings({ notifyLocalTask: v })}
          />
        )}
        <ToggleRow
          title={t("options.general.notifyRemoteTitle")}
          desc={t("options.general.notifyRemoteDesc")}
          checked={s.notifyRemoteTask}
          onChange={(v) => void updateSettings({ notifyRemoteTask: v })}
        />
      </SettingGroup>

      <SettingGroup title={t("options.general.groupAdvanced")}>
        {platform.android ? null : (
          <ToggleRow
            title={t("options.protocol.label")}
            desc={t("options.protocol.desc")}
            checked={s.enableFluxdownProtocol}
            onChange={(v) => void updateSettings({ enableFluxdownProtocol: v })}
          />
        )}

        <SettingRow title={t("stats.title")} desc={t("options.general.statsDesc")} inline>
          <Button onClick={() => void resetStats()}>{t("stats.reset")}</Button>
        </SettingRow>

        {platform.android ? null : (
          <SettingRow title={t("diagnostics.title")} desc={t("diagnostics.description")} inline>
            <Button disabled={exporting} onClick={() => void exportDiagnostics()}>
              {exporting ? t("diagnostics.exporting") : t("diagnostics.export")}
            </Button>
          </SettingRow>
        )}
      </SettingGroup>
    </PanelShell>
  );
}
