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

export type ManualServerChange =
  | { kind: "toggle"; enabled: boolean }
  | { kind: "add"; server: ManualServer }
  | { kind: "edit"; server: ManualServer }
  | { kind: "remove"; id: string };

export function readManualServers(value: unknown): ManualServers {
  return (value as ManualServers | undefined) ?? { enabled: false, servers: [] };
}

export async function loadManualServers(): Promise<ManualServers> {
  return readManualServers((await browser.storage.local.get(MANUAL_SERVERS_KEY))[MANUAL_SERVERS_KEY]);
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
