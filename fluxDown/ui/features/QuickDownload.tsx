/**
 * 快速下载输入框：粘贴一条 URL，经 background 的 downloadUrls 发送。TaskList（桌面，连接后显示）与
 * Android popup（无任务列表）共用同一实现。
 */
import type { JSX } from "preact";
import { useState } from "preact/hooks";
import { sendAction } from "@/utils/messaging";
import { Button } from "@/ui/components/Button";
import { TextInput } from "@/ui/components/TextInput";
import { showToast } from "@/ui/components/Toast";
import { useT } from "@/ui/i18n";
import "./features.css";

export function QuickDownload(props: { onSent?: () => void; channel?: "remote" }): JSX.Element {
  const t = useT();
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    const value = url.trim();
    if (!value) return;
    try {
      new URL(value);
    } catch {
      showToast(t("popup.quickDownload.invalidUrl"), "error");
      return;
    }
    setBusy(true);
    try {
      const res = await sendAction<"downloadUrls">({ action: "downloadUrls", urls: [value], channel: props.channel });
      if (res?.cancelled) {
        showToast(t("remote.manualCancelled"));
      } else if (res?.success) {
        showToast(t("popup.quickDownload.sent"), "success");
        setUrl("");
        props.onSent?.();
      } else {
        showToast(res?.message || t("popup.quickDownload.failed"), "error");
      }
    } catch {
      showToast(t("popup.quickDownload.failed"), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="fd-quick">
      <TextInput
        value={url}
        placeholder={t("popup.quickDownload.placeholder")}
        onInput={setUrl}
        onCommit={() => void submit()}
      />
      <Button size="sm" variant="primary" disabled={busy} onClick={() => void submit()}>
        {t("popup.quickDownload.button")}
      </Button>
    </div>
  );
}
