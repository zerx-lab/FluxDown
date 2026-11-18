import { expect, spyOn, test } from "bun:test";
import { remoteListTasks, remoteRemoveTask } from "./remote-server";

const config = { remoteUrl: "https://fluxdown.invalid/proxy", remoteToken: "example-key" };

test("remote task polling exposes current progress but not unrelated server fields", async () => {
  const task = {
    taskId: "task-1", fileName: "example.bin", status: 1, createdAt: "1",
    totalBytes: 100, downloadedBytes: 20,
  };
  const fetch = spyOn(globalThis, "fetch")
    .mockResolvedValueOnce(Response.json([{ ...task, saveDir: "/srv/downloads" }]))
    .mockResolvedValueOnce(Response.json([{ ...task, downloadedBytes: 70 }]));
  try {
    expect((await remoteListTasks(config))[0]).toMatchObject(task);
    const [updated] = await remoteListTasks(config);
    expect(updated?.downloadedBytes).toBe(70);
    expect(updated).not.toHaveProperty("saveDir");
    expect(fetch.mock.calls[0]?.[0]).toBe(`${config.remoteUrl}/api/v1/tasks`);
    const options = fetch.mock.calls[0]?.[1];
    expect(new Headers(options?.headers).get("X-FluxDown-Token")).toBe(config.remoteToken);
    expect(options?.redirect).toBe("error");
  } finally {
    fetch.mockRestore();
  }
});

test("remote removal addresses the exact task and preserves downloaded files", async () => {
  const fetch = spyOn(globalThis, "fetch")
    .mockResolvedValueOnce(Response.json({ success: true }))
    .mockResolvedValueOnce(Response.json({ success: false }, { status: 404 }));
  try {
    expect(await remoteRemoveTask(config, "task-1")).toBe(true);
    expect(fetch.mock.calls[0]?.[0]).toBe(`${config.remoteUrl}/api/v1/tasks/task-1?deleteFiles=false`);
    expect(fetch.mock.calls[0]?.[1]?.method).toBe("DELETE");
    await expect(remoteRemoveTask(config, "task-1")).rejects.toThrow("HTTP 404");
  } finally {
    fetch.mockRestore();
  }
});
