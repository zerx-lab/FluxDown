import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { loadManualServers, manualServerUrl } from "@/utils/manual-servers";
import type { DispatchTarget } from "@/utils/download-dispatch";
import { t } from "@/utils/i18n";
import { logWarn } from "@/utils/error-log";
import type { SendChannel, SendResult } from "./protocol";
import { getCachedSettings } from "./settings-cache";

type Choice = { name: string; address?: string; target: DispatchTarget };
const pending = new Map<string, { choices: Choice[]; finish: (target: DispatchTarget | null) => void }>();

export function cancelledSelection(): SendResult {
  return { success: false, message: t("remote.manualCancelled") };
}

/** undefined = keep default routing; null = no dispatch. Only explicit user actions call this. */
export async function chooseManualTarget(channel?: SendChannel): Promise<DispatchTarget | null | undefined> {
  const { enabled, servers } = await loadManualServers();
  if (!enabled) return channel === "local" || channel === "remote" ? channel : undefined;
  const settings = await getCachedSettings();
  const choices: Choice[] = [{ name: t("menu.sendLocal"), target: "local" }];
  const defaultUrl = settings.remoteVerified && manualServerUrl(settings.remoteUrl);
  if (defaultUrl) {
    choices.push({
      name: t("remote.manualDefault"),
      address: defaultUrl,
      target: { remoteUrl: defaultUrl, remoteToken: settings.remoteToken, rejectRedirects: true },
    });
  }
  for (const { name, remoteUrl, remoteToken } of servers) {
    choices.push({ name, address: remoteUrl, target: { remoteUrl, remoteToken, rejectRedirects: true } });
  }
  const url = browser.runtime.getURL("/manual-target.html") + "?request=" + crypto.randomUUID();
  let tabId: number | undefined;
  let finish!: (target: DispatchTarget | null) => void;
  const result = new Promise<DispatchTarget | null>((resolve) => { finish = resolve; });
  const onRemoved = (removed: number) => {
    if (tabId !== removed) return;
    tabId = undefined;
    finish(null);
  };
  pending.set(url, { choices, finish });
  browser.tabs.onRemoved.addListener(onRemoved);
  const expiry = setTimeout(() => finish(null), 120_000);
  try {
    tabId = (await browser.tabs.create({ url })).id;
    return await result;
  } finally {
    clearTimeout(expiry);
    pending.delete(url);
    browser.tabs.onRemoved.removeListener(onRemoved);
    if (tabId !== undefined) {
      void browser.tabs.remove(tabId).catch((error) => {
        logWarn("[FluxDown] Target chooser tab unavailable:", error);
      });
    }
  }
}

export function installManualTarget(): void {
  browser.runtime.onConnect.addListener((port: Browser.runtime.Port) => {
    if (port.name !== "manual-download-target") return;
    const url = port.sender?.url;
    const prompt = url && pending.get(url);
    if (!prompt) {
      port.disconnect();
      return;
    }
    pending.delete(url);
    port.onDisconnect.addListener(() => prompt.finish(null));
    port.onMessage.addListener((selection: unknown) => {
      if (selection === null) prompt.finish(null);
      else if (typeof selection === "number" && Number.isInteger(selection)) {
        const target = prompt.choices[selection]?.target;
        if (target) prompt.finish(target);
      }
    });
    // The chooser never receives access keys or request data.
    port.postMessage(prompt.choices.map(({ name, address }) => ({ name, address })));
  });
}
