import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { Select } from "@/ui/components/Select";
import { TextInput } from "@/ui/components/TextInput";
import { usePlatform } from "@/ui/hooks/usePlatform";
import { useT } from "@/ui/i18n";
import { SNIFF_MAX_RESOURCES_LIMIT, normalizeSniffMaxResources } from "@/utils/sniff-limit";
import { settings, updateSettings } from "../lib/store";
import { PanelShell, SettingRow, ToggleRow } from "../parts";

export function SniffPanel(): JSX.Element | null {
  const t = useT();
  const platform = usePlatform();
  const [limitDraft, setLimitDraft] = useState<string | null>(null);
  const s = settings.value;
  if (!s) return null;

  const commitLimit = (raw: string) => {
    const text = raw.trim();
    // 清空 / 非数字：放弃本次编辑，输入框回到已保存的值
    if (text === "" || !Number.isFinite(Number(text))) {
      setLimitDraft(null);
      return;
    }
    const next = normalizeSniffMaxResources(Number(text));
    setLimitDraft(null);
    if (next !== s.sniffMaxResources) void updateSettings({ sniffMaxResources: next });
  };

  return (
    <PanelShell title={t("options.nav.sniff")}>
      <ToggleRow
        title={t("options.sniff.enabled")}
        desc={t("options.sniff.enabledDesc")}
        checked={s.resourceSniffing}
        onChange={(v) => void updateSettings({ resourceSniffing: v })}
      />
      <ToggleRow
        title={t("options.sniff.pageDot")}
        desc={t("options.sniff.pageDotDesc")}
        checked={s.showPageDot}
        onChange={(v) => void updateSettings({ showPageDot: v })}
      />
      <ToggleRow
        title={t("options.sniff.floatButton")}
        desc={t("options.sniff.floatButtonDesc")}
        checked={s.showFloatingButton}
        onChange={(v) => void updateSettings({ showFloatingButton: v })}
      />
      {platform.android ? null : (
        <SettingRow title={t("options.sniff.floatTrigger")} desc={t("options.sniff.floatTriggerDesc")}>
          <Select<"hover" | "always">
            value={s.floatButtonTrigger}
            disabled={!s.showFloatingButton}
            onChange={(v) => void updateSettings({ floatButtonTrigger: v })}
            options={[
              { value: "hover", label: t("options.sniff.triggerHover") },
              { value: "always", label: t("options.sniff.triggerAlways") },
            ]}
          />
        </SettingRow>
      )}
      <SettingRow title={t("options.sniff.floatPosition")} desc={t("options.sniff.floatPositionDesc")}>
        <Select<"top-right" | "top-left" | "bottom-right" | "bottom-left">
          value={s.floatButtonPosition}
          disabled={!s.showFloatingButton}
          onChange={(v) => void updateSettings({ floatButtonPosition: v })}
          options={[
            { value: "top-right", label: t("options.sniff.positionTopRight") },
            { value: "top-left", label: t("options.sniff.positionTopLeft") },
            { value: "bottom-right", label: t("options.sniff.positionBottomRight") },
            { value: "bottom-left", label: t("options.sniff.positionBottomLeft") },
          ]}
        />
      </SettingRow>
      <ToggleRow
        title={t("options.sniff.images")}
        desc={t("options.sniff.imagesDesc")}
        checked={s.sniffImages}
        onChange={(v) => void updateSettings({ sniffImages: v })}
      />
      <SettingRow
        title={t("options.sniff.maxResources")}
        desc={t("options.sniff.maxResourcesDesc", { max: String(SNIFF_MAX_RESOURCES_LIMIT) })}
      >
        <TextInput
          type="number"
          min={1}
          max={SNIFF_MAX_RESOURCES_LIMIT}
          value={limitDraft ?? String(s.sniffMaxResources)}
          label={t("options.sniff.maxResources")}
          disabled={!s.resourceSniffing}
          onInput={setLimitDraft}
          onCommit={commitLimit}
        />
      </SettingRow>
      <ToggleRow
        title={t("options.sniff.lowConfidence")}
        desc={t("options.sniff.lowConfidenceDesc")}
        checked={s.showLowConfidence}
        onChange={(v) => void updateSettings({ showLowConfidence: v })}
      />
    </PanelShell>
  );
}
