/** 远程服务器面板：逻辑自旧 options/main.ts 迁移（地址 / 令牌失焦保存、测试连接解锁远程模式）。 */
import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { Button } from "@/ui/components/Button";
import { TextInput } from "@/ui/components/TextInput";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import { verifyRemoteSettings, type RemoteMode } from "@/utils/settings";
import { refreshSettings, settings, updateSettings } from "../lib/store";
import { PanelShell, SettingRow } from "../parts";

export function RemotePanel(): JSX.Element | null {
  const t = useT();
  const s = settings.value;
  const [urlDraft, setUrlDraft] = useState<string | null>(null);
  const [tokenDraft, setTokenDraft] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState("");
  if (!s) return null;

  const verified = Boolean(s.remoteVerified);
  const url = urlDraft ?? s.remoteUrl;
  const token = tokenDraft ?? s.remoteToken;

  const modeHints: Record<RemoteMode, string> = {
    off: t("remote.modeHintOff"),
    fallback: t("remote.modeHintFallback"),
    always: t("remote.modeHintAlways"),
  };
  const modeDesc = [modeHints[s.remoteMode], verified ? "" : t("remote.verifyRequired")].filter(Boolean).join(" ");

  /** remote-server.ts 返回稳定的 message 前缀，这里映射为本地化文案。 */
  const testErrorMessage = (message?: string): string => {
    if (message === "remote_auth_failed") return t("remote.testAuthFailed");
    if (message === "remote_not_configured") return t("remote.testNotConfigured");
    if (message?.startsWith("remote_unreachable")) return t("remote.testUnreachable");
    return t("remote.testFailed", { message: message || "unknown" });
  };

  const runTest = async () => {
    const remoteUrl = url.trim().replace(/\/+$/, "");
    if (!remoteUrl) {
      setTestResult(t("remote.testNotConfigured"));
      showToast(t("remote.testNotConfigured"), "error");
      return;
    }
    setTesting(true);
    setTestResult(t("remote.testing"));
    try {
      // 先落盘输入值（change 事件可能尚未触发），再验证：只有本机成功验证的当前连接才写入指纹。
      await updateSettings({ remoteUrl, remoteToken: token });
      const result = await verifyRemoteSettings({ remoteUrl, remoteToken: token });
      const ok = result.success;
      const text = ok
        ? t("remote.testSuccess", { app: result.app || "FluxDown", version: result.version || "" })
        : testErrorMessage(result.message);
      setTestResult(text);
      showToast(text, ok ? "success" : "error");
    } catch (e) {
      const text = t("remote.testFailed", { message: String(e) });
      setTestResult(text);
      showToast(text, "error");
    } finally {
      setTesting(false);
      setUrlDraft(null);
      setTokenDraft(null);
      await refreshSettings();
    }
  };

  return (
    <PanelShell title={t("options.nav.remote")}>
      <SettingRow title={t("remote.serverUrl")} desc={t("options.remoteDesc")}>
        <TextInput
          value={url}
          placeholder={t("remote.serverUrlPlaceholder")}
          onInput={setUrlDraft}
          onCommit={(value) => {
            void updateSettings({ remoteUrl: value.trim() }).then(() => setUrlDraft(null));
          }}
        />
      </SettingRow>

      <SettingRow title={t("remote.token")} desc={t("options.tokenDesc")}>
        <TextInput
          type="password"
          value={token}
          placeholder={t("remote.tokenPlaceholder")}
          onInput={setTokenDraft}
          onCommit={(value) => {
            void updateSettings({ remoteToken: value }).then(() => setTokenDraft(null));
          }}
        />
      </SettingRow>

      <SettingRow
        title={t("remote.testConnection")}
        desc={verified ? t("options.verifiedState") : t("options.unverifiedState")}
        inline
      >
        <span class="opt-test-result">{testResult}</span>
        <Button variant="primary" disabled={testing} onClick={() => void runTest()}>
          {t("remote.testConnection")}
        </Button>
      </SettingRow>

      {/* 模式依赖上方连接验证，放在验证之后：自上而下 填地址 → 测试 → 选模式 */}
      <SettingRow title={t("remote.mode")} desc={modeDesc}>
        <select
          class="fd-select opt-fill"
          value={s.remoteMode}
          aria-label={t("remote.mode")}
          onChange={(e) => void updateSettings({ remoteMode: e.currentTarget.value as RemoteMode })}
        >
          <option value="off" selected={s.remoteMode === "off"}>
            {t("remote.modeOff")}
          </option>
          <option value="fallback" disabled={!verified} selected={s.remoteMode === "fallback"}>
            {t("remote.modeFallback")}
          </option>
          <option value="always" disabled={!verified} selected={s.remoteMode === "always"}>
            {t("remote.modeAlways")}
          </option>
        </select>
      </SettingRow>
    </PanelShell>
  );
}
