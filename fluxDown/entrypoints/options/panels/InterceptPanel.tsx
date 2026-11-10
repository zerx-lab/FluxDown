import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { Button } from "@/ui/components/Button";
import { Select } from "@/ui/components/Select";
import { TagInput } from "@/ui/components/TagInput";
import { TextInput } from "@/ui/components/TextInput";
import { showToast } from "@/ui/components/Toast";
import { usePlatform } from "@/ui/hooks/usePlatform";
import { useT } from "@/ui/i18n";
import {
  BUILTIN_EXTENSIONS,
  DEFAULT_SETTINGS,
  normalizeExtension,
  type BypassKey,
  type InterceptMode,
} from "@/utils/settings";
import { settings, updateSettings } from "../lib/store";
import { PanelShell, SettingGroup, SettingRow, ToggleRow } from "../parts";

const MB = 1024 * 1024;

/** 最小大小的固定预设（字节）；不在其中时视为自定义，显示 MB 输入框。 */
const MIN_SIZE_PRESETS: Record<string, string> = {
  "0": "",
  "102400": "100 KB",
  "524288": "512 KB",
  "1048576": "1 MB",
  "5242880": "5 MB",
  "10485760": "10 MB",
  "52428800": "50 MB",
  "104857600": "100 MB",
  "209715200": "200 MB",
  "524288000": "500 MB",
};

/** MIME：小写；`type/` 匹配整族，`type/subtype` 精确匹配。 */
const MIME_RE = /^[a-z0-9][a-z0-9.+-]*\/([a-z0-9][a-z0-9.+-]*)?$/;

export function InterceptPanel(): JSX.Element | null {
  const t = useT();
  const s = settings.value;
  const [customMode, setCustomMode] = useState(false);
  const [customDraft, setCustomDraft] = useState<string | null>(null);
  const platform = usePlatform();
  if (!s) return null;

  const isPreset = String(s.minFileSize) in MIN_SIZE_PRESETS;
  const sizeChoice = customMode || !isPreset ? "custom" : String(s.minFileSize);
  const draft = customDraft ?? String(Math.max(1, Math.ceil(s.minFileSize / MB)));

  const changeSizeChoice = (value: string) => {
    if (value === "custom") {
      // 选「自定义」只展开输入框；数值在输入框提交且合法时才保存，中途放弃不会把阈值改成 0。
      setCustomMode(true);
      return;
    }
    setCustomMode(false);
    setCustomDraft(null);
    void updateSettings({ minFileSize: Number(value) });
  };

  const commitCustomSize = (raw: string) => {
    const text = raw.trim();
    if (text === "" || !Number.isFinite(Number(text))) return;
    const mb = Math.max(0, Math.round(Number(text)));
    setCustomDraft(mb > 0 ? String(mb) : "");
    void updateSettings({ minFileSize: mb * MB });
  };

  const normalizeInclude = (raw: string): string => {
    const ext = normalizeExtension(raw);
    if (!ext) {
      showToast(t("options.rules.extInvalid"), "error");
      return "";
    }
    if (BUILTIN_EXTENSIONS.includes(ext) || s.customExtensions.includes(ext)) {
      showToast(t("options.rules.extExists", { ext }), "error");
      return "";
    }
    return ext;
  };

  const normalizeExclude = (raw: string): string => {
    const ext = normalizeExtension(raw);
    if (!ext) {
      showToast(t("options.rules.extInvalid"), "error");
      return "";
    }
    if (s.excludeExtensions.includes(ext)) {
      showToast(t("options.rules.extExists", { ext }), "error");
      return "";
    }
    return ext;
  };

  const normalizeMime = (raw: string): string => {
    const mime = raw.trim().toLowerCase();
    if (!MIME_RE.test(mime)) {
      showToast(t("options.rules.mimeInvalid"), "error");
      return "";
    }
    if (s.interceptMimeTypes.includes(mime)) {
      showToast(t("options.rules.extExists", { ext: mime }), "error");
      return "";
    }
    return mime;
  };

  const modeHint = s.interceptMode === "all" ? t("options.intercept.hintAll") : t("options.intercept.hintSmart");

  return (
    <PanelShell title={t("options.nav.intercept")}>
      <SettingGroup title={t("options.intercept.groupBasic")}>
        <ToggleRow
          title={t("options.intercept.enabled")}
          desc={t("options.intercept.enabledDesc")}
          checked={s.enabled}
          onChange={(v) => void updateSettings({ enabled: v })}
        />

        <SettingRow
          title={t("options.intercept.mode")}
          desc={modeHint}
          note={t("options.intercept.localDownloadHint")}
        >
          <Select<InterceptMode>
            value={s.interceptMode}
            onChange={(v) => void updateSettings({ interceptMode: v })}
            options={[
              { value: "smart", label: t("options.intercept.modeSmart") },
              { value: "all", label: t("options.intercept.modeAll") },
            ]}
          />
        </SettingRow>

        <ToggleRow
          title={t("options.intercept.magnet")}
          desc={t("options.intercept.magnetHint")}
          checked={s.interceptMagnet}
          onChange={(v) => void updateSettings({ interceptMagnet: v })}
        />

        {platform.desktopApp ? (
          <SettingRow title={t("options.intercept.bypassKey")} desc={t("options.intercept.bypassKeyDesc")}>
            <Select<BypassKey>
              value={s.bypassKey}
              onChange={(v) => void updateSettings({ bypassKey: v })}
              options={[
                { value: "alt", label: t("options.intercept.bypassKeyAlt") },
                { value: "shift", label: "Shift" },
                { value: "off", label: t("options.intercept.bypassKeyOff") },
              ]}
            />
          </SettingRow>
        ) : null}

        {platform.contextMenus ? (
          <ToggleRow
            title={t("options.intercept.contextMenu")}
            desc={t("options.intercept.contextMenuDesc")}
            checked={s.contextMenuEnabled}
            onChange={(v) => void updateSettings({ contextMenuEnabled: v })}
          />
        ) : null}
      </SettingGroup>

      <SettingGroup title={t("options.intercept.groupInclude")} desc={t("options.intercept.groupIncludeDesc")}>
        <SettingRow title={t("options.rules.extTitle")} desc={t("options.rules.extDesc")} stack>
          <TagInput
            tags={s.customExtensions}
            normalize={normalizeInclude}
            placeholder={t("options.rules.extPlaceholder")}
            onChange={(next) => void updateSettings({ customExtensions: next })}
          />
          <details class="opt-builtin">
            <summary>{t("options.rules.builtinExts")}</summary>
            <div class="opt-tag-list">
              {BUILTIN_EXTENSIONS.map((ext) => (
                <span class="fd-tag opt-tag-readonly" key={ext}>
                  {ext}
                </span>
              ))}
            </div>
          </details>
        </SettingRow>

        <SettingRow title={t("options.rules.mimeTitle")} desc={t("options.rules.mimeDesc")} stack>
          <TagInput
            tags={s.interceptMimeTypes}
            normalize={normalizeMime}
            placeholder={t("options.rules.mimePlaceholder")}
            onChange={(next) => void updateSettings({ interceptMimeTypes: next })}
          />
          <div class="opt-actions-end">
            <Button
              size="sm"
              onClick={() => {
                void updateSettings({ interceptMimeTypes: [...DEFAULT_SETTINGS.interceptMimeTypes] });
                showToast(t("options.rules.mimeResetDone"), "success");
              }}
            >
              {t("options.rules.resetMime")}
            </Button>
          </div>
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={t("options.intercept.groupExclude")} desc={t("options.intercept.groupExcludeDesc")}>
        <SettingRow title={t("options.intercept.minSize")} desc={t("options.rules.minFileSizeDesc")}>
          <div class="opt-stack-gap">
            <Select<string>
              value={sizeChoice}
              onChange={changeSizeChoice}
              options={[
                ...Object.entries(MIN_SIZE_PRESETS).map(([value, label]) => ({
                  value,
                  label: label || t("options.intercept.sizeNoLimit"),
                })),
                { value: "custom", label: t("options.rules.minFileSizeCustom") },
              ]}
            />
            {sizeChoice === "custom" ? (
              <TextInput
                type="number"
                min={0}
                value={draft}
                placeholder={t("options.rules.minFileSizeCustomPlaceholder")}
                onInput={setCustomDraft}
                onCommit={commitCustomSize}
              />
            ) : null}
          </div>
        </SettingRow>

        <SettingRow title={t("options.rules.excludeExtTitle")} desc={t("options.rules.excludeExtDesc")} stack>
          <TagInput
            tags={s.excludeExtensions}
            normalize={normalizeExclude}
            placeholder={t("options.rules.excludeExtPlaceholder")}
            onChange={(next) => void updateSettings({ excludeExtensions: next })}
          />
        </SettingRow>
      </SettingGroup>

      <SettingGroup title={t("options.intercept.groupFallback")}>
        <ToggleRow
          title={t("options.intercept.fallbackReject")}
          desc={t("options.intercept.fallbackRejectDesc")}
          checked={s.fallbackOnReject}
          onChange={(v) => void updateSettings({ fallbackOnReject: v })}
        />
        {platform.desktopApp ? (
          <>
            <ToggleRow
              title={t("options.intercept.fallbackCancel")}
              desc={t("options.intercept.fallbackCancelDesc")}
              checked={s.fallbackOnCancel}
              onChange={(v) => void updateSettings({ fallbackOnCancel: v })}
            />
            <ToggleRow
              title={t("options.intercept.askExclude")}
              desc={t("options.intercept.askExcludeDesc")}
              checked={s.askExcludeAfterCancels}
              onChange={(v) => void updateSettings({ askExcludeAfterCancels: v })}
            />
          </>
        ) : null}
      </SettingGroup>
    </PanelShell>
  );
}
