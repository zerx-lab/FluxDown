import type { ManualServerChange, ManualServers } from "@/utils/manual-servers";

/** One writer in background for all options pages; queue the read as well as the write. */
export function createManualServerWriter(
  read: () => Promise<ManualServers>,
  write: (settings: ManualServers) => Promise<void>,
): (change: ManualServerChange) => Promise<boolean> {
  let queue: Promise<unknown> = Promise.resolve();
  return (change) => {
    const result = queue.then(async () => {
      const current = await read();
      switch (change.kind) {
        case "toggle":
          await write({ ...current, enabled: change.enabled });
          break;
        case "add":
          if (current.servers.some(({ id }) => id === change.server.id)) return false;
          await write({ ...current, servers: [...current.servers, change.server] });
          break;
        case "edit":
          if (!current.servers.some(({ id }) => id === change.server.id)) return false;
          await write({ ...current, servers: current.servers.map((server) =>
            server.id === change.server.id ? change.server : server) });
          break;
        case "remove":
          await write({ ...current, servers: current.servers.filter(({ id }) => id !== change.id) });
          break;
      }
      return true;
    });
    queue = result.catch(() => {});
    return result;
  };
}
