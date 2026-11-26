import { render } from "preact";
import { browser } from "wxt/browser";
import { Button } from "@/ui/components/Button";
import { initI18n, t } from "@/utils/i18n";
import "@/ui/theme/tokens.css";
import "@/ui/theme/base.css";

interface Choice { name: string; address?: string }

async function main(): Promise<void> {
  await initI18n();
  const root = document.querySelector("main");
  if (!root) return;
  root.style.cssText = "max-width: 32rem; margin: 3rem auto; padding: 1rem; overflow-wrap: anywhere";
  document.title = t("remote.manualChoose");
  const port = browser.runtime.connect({ name: "manual-download-target" });
  // A connected port alone does not keep a Chromium MV3 worker alive.
  const heartbeat = setInterval(() => port.postMessage("waiting"), 20_000);
  let sent = false;
  port.onDisconnect.addListener(() => {
    void browser.runtime.lastError;
    sent = true;
    clearInterval(heartbeat);
    render(<p role="status">{t("remote.manualCancelled")}</p>, root);
  });
  port.onMessage.addListener((choices: Choice[]) => {
    const submit = (choice: number | null) => {
      if (sent) return;
      sent = true;
      clearInterval(heartbeat);
      port.postMessage(choice);
    };
    window.addEventListener("keydown", (event) => {
      if (event.key === "Escape") submit(null);
    });
    render(<>
      <h1>{t("remote.manualChoose")}</h1>
      <ul style={{ display: "grid", gap: "0.75rem", padding: 0, listStyle: "none" }}>
        {choices.map(({ name, address }, index) => <li key={index}>
          <Button onClick={() => submit(index)}>{name}</Button>
          {address ? <div>{address}</div> : null}
        </li>)}
      </ul>
      <Button onClick={() => submit(null)}>{t("remote.manualCancel")}</Button>
    </>, root);
  });
}

void main();
