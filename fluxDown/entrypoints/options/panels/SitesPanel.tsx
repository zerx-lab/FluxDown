import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Badge } from "@/ui/components/Badge";
import { Button } from "@/ui/components/Button";
import { EmptyState } from "@/ui/components/EmptyState";
import { IconButton } from "@/ui/components/IconButton";
import { Modal } from "@/ui/components/Modal";
import { Select } from "@/ui/components/Select";
import { showToast } from "@/ui/components/Toast";
import { TextInput } from "@/ui/components/TextInput";
import { Toggle } from "@/ui/components/Toggle";
import { useT } from "@/ui/i18n";
import {
  DEFAULT_SITE_PROFILE,
  SITE_PROFILES_KEY,
  normalizeSiteKey,
  parseSiteProfiles,
  saveSiteProfile,
  type SiteProfileFields,
} from "@/utils/site-profiles";
import { useLocalValue } from "../lib/store";
import { ConfirmModal, PanelShell } from "../parts";

type InterceptChoice = SiteProfileFields["intercept"];

interface EditState {
  /** 编辑已有站点时锁定 host；新增时可改。 */
  isNew: boolean;
  host: string;
  fields: SiteProfileFields;
}

interface SitesPanelProps {
  /** `#sites?host=<h>` 带入的待新增 host；消费后调用 onPrefillConsumed 清掉地址栏参数。 */
  prefillHost: string | null;
  onPrefillConsumed: () => void;
}

export function SitesPanel(props: SitesPanelProps): JSX.Element {
  const t = useT();
  const [profiles] = useLocalValue(SITE_PROFILES_KEY, parseSiteProfiles);
  const [editing, setEditing] = useState<EditState | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  useEffect(() => {
    if (!props.prefillHost) return;
    const host = normalizeSiteKey(props.prefillHost) ?? props.prefillHost;
    setEditing({ isNew: true, host, fields: { ...DEFAULT_SITE_PROFILE } });
    props.onPrefillConsumed();
  }, [props.prefillHost]);

  const hosts = Object.keys(profiles).sort();

  const save = async () => {
    if (!editing) return;
    const key = normalizeSiteKey(editing.host);
    if (!key) {
      showToast(t("domain.invalid", { domain: editing.host.trim() }), "error");
      return;
    }
    if (editing.isNew && key in profiles) {
      showToast(t("options.sites.exists", { host: key }), "error");
      return;
    }
    await saveSiteProfile(key, { ...editing.fields, saveDir: editing.fields.saveDir.trim(), updatedAt: Date.now() });
    setEditing(null);
    showToast(t("options.sites.saved", { host: key }), "success");
  };

  const remove = async (host: string) => {
    await saveSiteProfile(host, null);
    setRemoving(null);
    showToast(t("domain.removed", { domain: host }), "success");
  };

  const patchFields = (patch: Partial<SiteProfileFields>) => {
    if (editing) setEditing({ ...editing, fields: { ...editing.fields, ...patch } });
  };

  const flag = (on: boolean) => <Badge tone={on ? "success" : "neutral"}>{on ? t("options.on") : t("options.off")}</Badge>;

  const interceptLabel: Record<InterceptChoice, string> = {
    default: t("options.sites.interceptDefault"),
    never: t("options.sites.interceptNever"),
    always: t("options.sites.interceptAlways"),
  };

  return (
    <PanelShell title={t("options.nav.sites")} desc={t("options.sites.desc")}>
      <div class="opt-toolbar">
        <Button
          variant="primary"
          onClick={() => setEditing({ isNew: true, host: "", fields: { ...DEFAULT_SITE_PROFILE } })}
        >
          {t("options.sites.add")}
        </Button>
      </div>

      {hosts.length === 0 ? (
        <EmptyState icon="globe" title={t("options.sites.empty")} hint={t("options.sites.emptyHint")} />
      ) : (
        <div class="opt-table-wrap">
          <table class="opt-table">
            <thead>
              <tr>
                <th>{t("options.sites.colHost")}</th>
                <th>{t("options.sites.colIntercept")}</th>
                <th>{t("options.sites.colSniff")}</th>
                <th>{t("options.sites.colDot")}</th>
                <th>{t("options.sites.colFloat")}</th>
                <th>{t("options.sites.colSaveDir")}</th>
                <th class="opt-col-actions">{t("options.sites.colActions")}</th>
              </tr>
            </thead>
            <tbody>
              {hosts.map((host) => {
                const p = profiles[host];
                return (
                  <tr key={host}>
                    <td class="opt-cell-mono">{host}</td>
                    <td>
                      <Badge tone={p.intercept === "never" ? "danger" : p.intercept === "always" ? "accent" : "neutral"}>
                        {interceptLabel[p.intercept]}
                      </Badge>
                    </td>
                    <td>{flag(p.sniff)}</td>
                    <td>{flag(p.pageDot)}</td>
                    <td>{flag(p.floatButton)}</td>
                    <td class="opt-cell-mono opt-cell-wrap">{p.saveDir || "—"}</td>
                    <td class="opt-col-actions">
                      <IconButton
                        icon="edit"
                        label={t("options.sites.edit")}
                        onClick={() => setEditing({ isNew: false, host, fields: { ...p } })}
                      />
                      <IconButton icon="trash" label={t("options.sites.remove")} onClick={() => setRemoving(host)} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {editing ? (
        <Modal
          title={editing.isNew ? t("options.sites.add") : t("options.sites.editTitle", { host: editing.host })}
          onClose={() => setEditing(null)}
          width={480}
          footer={
            <>
              <Button onClick={() => setEditing(null)}>{t("options.cancel")}</Button>
              <Button variant="primary" onClick={() => void save()}>
                {t("options.save")}
              </Button>
            </>
          }
        >
          <div class="opt-form">
            <label class="opt-field">
              <span class="opt-field-label">{t("options.sites.colHost")}</span>
              <TextInput
                value={editing.host}
                disabled={!editing.isNew}
                placeholder={t("domain.placeholder")}
                onInput={(host) => setEditing({ ...editing, host })}
              />
            </label>
            <div class="opt-field">
              <span class="opt-field-label">{t("options.sites.colIntercept")}</span>
              <Select<InterceptChoice>
                value={editing.fields.intercept}
                onChange={(intercept) => patchFields({ intercept })}
                options={[
                  { value: "default", label: interceptLabel.default },
                  { value: "never", label: interceptLabel.never },
                  { value: "always", label: interceptLabel.always },
                ]}
              />
            </div>
            <div class="opt-field opt-field-inline">
              <span class="opt-field-label">{t("options.sites.colSniff")}</span>
              <Toggle
                checked={editing.fields.sniff}
                label={t("options.sites.colSniff")}
                onChange={(sniff) => patchFields({ sniff })}
              />
            </div>
            <div class="opt-field opt-field-inline">
              <span class="opt-field-label">{t("options.sites.colDot")}</span>
              <Toggle
                checked={editing.fields.pageDot}
                label={t("options.sites.colDot")}
                onChange={(pageDot) => patchFields({ pageDot })}
              />
            </div>
            <div class="opt-field opt-field-inline">
              <span class="opt-field-label">{t("options.sites.colFloat")}</span>
              <Toggle
                checked={editing.fields.floatButton}
                label={t("options.sites.colFloat")}
                onChange={(floatButton) => patchFields({ floatButton })}
              />
            </div>
            <label class="opt-field">
              <span class="opt-field-label">{t("options.sites.colSaveDir")}</span>
              <TextInput
                value={editing.fields.saveDir}
                placeholder={t("options.sites.saveDirPlaceholder")}
                onInput={(saveDir) => patchFields({ saveDir })}
              />
              <span class="opt-field-hint">{t("options.sites.saveDirHint")}</span>
            </label>
          </div>
        </Modal>
      ) : null}

      {removing ? (
        <ConfirmModal
          title={t("options.sites.remove")}
          message={t("options.sites.removeConfirm", { host: removing })}
          confirmLabel={t("options.sites.remove")}
          danger
          onCancel={() => setRemoving(null)}
          onConfirm={() => void remove(removing)}
        />
      ) : null}
    </PanelShell>
  );
}
