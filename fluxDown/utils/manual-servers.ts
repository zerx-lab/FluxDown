import { browser } from "wxt/browser";
import type { RemoteServerConfig } from "./remote-server";

export const MANUAL_SERVERS_KEY = "manualDownloadServers";

export interface ManualServer extends RemoteServerConfig {
  id: string;
  name: string;
}

export interface ManualServers {
  enabled: boolean;
  servers: ManualServer[];
}

export async function loadManualServers(): Promise<ManualServers> {
  const stored = (await browser.storage.local.get(MANUAL_SERVERS_KEY))[MANUAL_SERVERS_KEY] as ManualServers | undefined;
  return stored ?? { enabled: false, servers: [] };
}

export async function saveManualServers(settings: ManualServers): Promise<void> {
  await browser.storage.local.set({ [MANUAL_SERVERS_KEY]: settings });
}

export function manualServerUrl(input: string): string | undefined {
  try {
    const url = new URL(input.trim());
    if (!/^https?:$/.test(url.protocol) || url.username || url.password || url.search || url.hash) return undefined;
    return url.href.replace(/\/+$/, "");
  } catch {
    return undefined;
  }
}
