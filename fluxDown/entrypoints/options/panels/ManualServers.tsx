import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Button } from "@/ui/components/Button";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import { loadManualServers, manualServerUrl, saveManualServers, type ManualServer, type ManualServers as Settings } from "@/utils/manual-servers";
import { REMOTE_MANAGEMENT_UNAVAILABLE, remoteVerify } from "@/utils/remote-server";
import { SettingGroup, SettingRow, ToggleRow } from "../parts";

export function ManualServers(): JSX.Element | null {
  const t = useT();
  const [settings, setSettings] = useState<Settings>();
  const [draft, setDraft] = useState<ManualServer>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    void loadManualServers().then(setSettings, () => showToast(t("remote.manualStorageError"), "error"));
  }, []);
  if (!settings) return null;

  async function change(update: (previous: Settings) => Promise<Settings | undefined>): Promise<void> {
    setBusy(true);
    setError("");
    try {
      const next = await update(await loadManualServers());
      if (next) {
        await saveManualServers(next);
        setSettings(next);
      }
    } catch {
      setError(t("remote.manualStorageError"));
    } finally {
      setBusy(false);
    }
  }

  function submit(server: ManualServer): void {
    const name = server.name.trim();
    const remoteUrl = manualServerUrl(server.remoteUrl);
    const remoteToken = server.remoteToken.trim();
    if (!name || !remoteUrl || !remoteToken) {
      setError(t("remote.manualInvalid"));
      return;
    }
    void change(async (previous) => {
      const result = await remoteVerify({ remoteUrl, remoteToken, rejectRedirects: true });
      if (!result.success || result.message === REMOTE_MANAGEMENT_UNAVAILABLE) {
        setError(result.message === "remote_auth_failed" ? t("remote.testAuthFailed") :
          result.message === REMOTE_MANAGEMENT_UNAVAILABLE ? t("remote.testManagementUnavailable") : t("remote.testUnreachable"));
        return undefined;
      }
      const saved = { id: server.id, name, remoteUrl, remoteToken };
      const current = await loadManualServers();
      setDraft(undefined);
      return { ...current, servers: previous.servers.some(({ id }) => id === saved.id)
        ? current.servers.map((item) => item.id === saved.id ? saved : item)
        : [...current.servers, saved] };
    });
  }

  return (
    <SettingGroup title={t("remote.manualChoose")}>
      <ToggleRow title={t("remote.manualEnable")} desc={t("remote.manualHint")} checked={settings.enabled}
        disabled={busy} onChange={(enabled) => void change(async (previous) => ({ ...previous, enabled }))} />
      {settings.servers.map((server) => (
        <SettingRow key={server.id} title={server.name} desc={server.remoteUrl} inline>
          <Button disabled={busy} onClick={() => { setError(""); setDraft(server); }}>{t("remote.manualEdit")}</Button>
          <Button disabled={busy} onClick={() => {
            if (draft?.id === server.id) setDraft(undefined);
            void change(async (previous) => ({ ...previous, servers: previous.servers.filter(({ id }) => id !== server.id) }));
          }}>{t("remote.manualRemove")}</Button>
        </SettingRow>
      ))}
      {draft ? (
        <form onSubmit={(event) => { event.preventDefault(); submit(draft); }}>
          <fieldset disabled={busy}>
            <SettingRow title={t("remote.manualName")}><input class="fd-input" required maxLength={80} aria-label={t("remote.manualName")}
              value={draft.name} onInput={(e) => setDraft({ ...draft, name: e.currentTarget.value })} /></SettingRow>
            <SettingRow title={t("remote.serverUrl")}><input class="fd-input" required type="url" aria-label={t("remote.serverUrl")}
              value={draft.remoteUrl} onInput={(e) => setDraft({ ...draft, remoteUrl: e.currentTarget.value })} /></SettingRow>
            <SettingRow title={t("remote.token")}><input class="fd-input" required type="password" autoComplete="off" aria-label={t("remote.token")}
              value={draft.remoteToken} onInput={(e) => setDraft({ ...draft, remoteToken: e.currentTarget.value })} /></SettingRow>
            <Button type="submit" variant="primary" disabled={busy}>{t(busy ? "remote.testing" : "remote.manualVerifySave")}</Button>
            <Button disabled={busy} onClick={() => { setError(""); setDraft(undefined); }}>{t("remote.manualCancel")}</Button>
          </fieldset>
        </form>
      ) : <Button disabled={busy} onClick={() => {
        setError("");
        setDraft({ id: crypto.randomUUID(), name: "", remoteUrl: "", remoteToken: "" });
      }}>{t("remote.manualAdd")}</Button>}
      {error ? <p role="alert">{error}</p> : null}
    </SettingGroup>
  );
}
