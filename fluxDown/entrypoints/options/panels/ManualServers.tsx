import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { browser } from "wxt/browser";
import { Button } from "@/ui/components/Button";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import {
  loadManualServers,
  MANUAL_SERVERS_KEY,
  manualServerUrl,
  readManualServers,
  type ManualServer,
  type ManualServerChange,
  type ManualServers as Settings,
} from "@/utils/manual-servers";
import { sendAction } from "@/utils/messaging";
import { REMOTE_MANAGEMENT_UNAVAILABLE, remoteVerify } from "@/utils/remote-server";
import { SettingGroup, SettingRow, ToggleRow } from "../parts";

type ServerDraft = ManualServer & { kind: "add" | "edit" };

export function ManualServers(): JSX.Element | null {
  const t = useT();
  const [settings, setSettings] = useState<Settings>();
  const [draft, setDraft] = useState<ServerDraft>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    let initialRead = true;
    const onChanged = (changes: Record<string, { newValue?: unknown }>, area: string) => {
      if (area !== "local" || !(MANUAL_SERVERS_KEY in changes)) return;
      initialRead = false;
      setSettings(readManualServers(changes[MANUAL_SERVERS_KEY].newValue));
    };
    browser.storage.onChanged.addListener(onChanged);
    void loadManualServers().then(
      (value) => { if (initialRead) setSettings(value); },
      () => { if (initialRead) showToast(t("remote.manualStorageError"), "error"); },
    );
    return () => {
      initialRead = false;
      browser.storage.onChanged.removeListener(onChanged);
    };
  }, [t]);
  if (!settings) return null;

  async function write(change: ManualServerChange): Promise<void> {
    const result = await sendAction<"updateManualServers">({ action: "updateManualServers", change });
    if (!result?.ok) throw new Error("Manual server change rejected");
  }

  async function change(update: () => Promise<void>): Promise<void> {
    setBusy(true);
    setError("");
    try {
      await update();
    } catch {
      setError(t("remote.manualStorageError"));
    } finally {
      setBusy(false);
    }
  }

  function submit(server: ServerDraft): void {
    const name = server.name.trim();
    const remoteUrl = manualServerUrl(server.remoteUrl);
    const remoteToken = server.remoteToken.trim();
    if (!name || !remoteUrl || !remoteToken) {
      setError(t("remote.manualInvalid"));
      return;
    }
    void change(async () => {
      const result = await remoteVerify({ remoteUrl, remoteToken, rejectRedirects: true });
      if (!result.success || result.message === REMOTE_MANAGEMENT_UNAVAILABLE) {
        setError(result.message === "remote_auth_failed" ? t("remote.testAuthFailed") :
          result.message === REMOTE_MANAGEMENT_UNAVAILABLE ? t("remote.testManagementUnavailable") : t("remote.testUnreachable"));
        return;
      }
      await write({ kind: server.kind, server: { id: server.id, name, remoteUrl, remoteToken } });
      setDraft(undefined);
    });
  }

  return (
    <SettingGroup title={t("remote.manualChoose")}>
      <ToggleRow title={t("remote.manualEnable")} desc={t("remote.manualHint")} checked={settings.enabled}
        disabled={busy} onChange={(enabled) => void change(() => write({ kind: "toggle", enabled }))} />
      {settings.servers.map((server) => (
        <SettingRow key={server.id} title={server.name} desc={server.remoteUrl} inline>
          <Button disabled={busy} onClick={() => { setError(""); setDraft({ ...server, kind: "edit" }); }}>{t("remote.manualEdit")}</Button>
          <Button disabled={busy} onClick={() => {
            void change(async () => {
              await write({ kind: "remove", id: server.id });
              if (draft?.id === server.id) setDraft(undefined);
            });
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
        setDraft({ kind: "add", id: crypto.randomUUID(), name: "", remoteUrl: "", remoteToken: "" });
      }}>{t("remote.manualAdd")}</Button>}
      {error ? <p role="alert">{error}</p> : null}
    </SettingGroup>
  );
}
