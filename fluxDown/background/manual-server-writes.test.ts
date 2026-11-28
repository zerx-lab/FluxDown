import { expect, test } from "bun:test";
import type { ManualServer, ManualServers } from "@/utils/manual-servers";
import { createManualServerWriter } from "./manual-server-writes";

const alpha: ManualServer = { id: "a", name: "Alpha", remoteUrl: "https://a.example", remoteToken: "a-token" };
const beta: ManualServer = { id: "b", name: "Beta", remoteUrl: "https://b.example", remoteToken: "b-token" };

function holdFirstWrite(initial: ManualServers) {
  let stored = initial;
  let firstWrite = true;
  const started = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  const change = createManualServerWriter(async () => structuredClone(stored), async (next) => {
    if (firstWrite) {
      firstWrite = false;
      started.resolve();
      await release.promise;
    }
    stored = structuredClone(next);
  });
  return { change, started: started.promise, release: release.resolve, read: () => stored };
}

test("overlapping saves and a toggle preserve both servers and the new setting", async () => {
  const store = holdFirstWrite({ enabled: false, servers: [] });
  const first = store.change({ kind: "add", server: alpha });
  await store.started;
  const second = store.change({ kind: "add", server: beta });
  const toggle = store.change({ kind: "toggle", enabled: true });
  store.release();

  expect(await Promise.all([first, second, toggle])).toEqual([true, true, true]);
  expect(store.read()).toEqual({ enabled: true, servers: [alpha, beta] });
});

test("a stale edit cannot recreate a concurrently removed server", async () => {
  const store = holdFirstWrite({ enabled: true, servers: [alpha, beta] });
  const remove = store.change({ kind: "remove", id: alpha.id });
  await store.started;
  const edit = store.change({ kind: "edit", server: { ...alpha, name: "Stale edit" } });
  store.release();

  expect(await Promise.all([remove, edit])).toEqual([true, false]);
  expect(store.read()).toEqual({ enabled: true, servers: [beta] });
});

test("a failed save reports failure without losing queued changes", async () => {
  let stored: ManualServers = { enabled: true, servers: [] };
  const change = createManualServerWriter(async () => structuredClone(stored), async (next) => {
    if (next.servers.some(({ id }) => id === alpha.id)) throw new Error("storage write failed");
    stored = structuredClone(next);
  });
  const first = change({ kind: "add", server: alpha });
  const second = change({ kind: "add", server: beta });

  await expect(first).rejects.toThrow("storage write failed");
  expect(await second).toBe(true);
  expect(stored).toEqual({ enabled: true, servers: [beta] });
});
