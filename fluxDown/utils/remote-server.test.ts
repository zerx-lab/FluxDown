import { expect, spyOn, test } from "bun:test";
import { REMOTE_MANAGEMENT_UNAVAILABLE, remoteListTasks, remoteRemoveTask, remoteVerify } from "./remote-server";

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

test("connection test reports management unavailable without blocking download takeover", async () => {
  const fetch = spyOn(globalThis, "fetch")
    .mockResolvedValueOnce(Response.json({ app: "FluxDown" }))
    .mockResolvedValueOnce(new Response(null, { status: 404 }));
  try {
    expect(await remoteVerify(config)).toMatchObject({
      success: true,
      message: REMOTE_MANAGEMENT_UNAVAILABLE,
    });
  } finally {
    fetch.mockRestore();
  }
});

test("task list distinguishes an unavailable management API from other failures", async () => {
  const fetch = spyOn(globalThis, "fetch")
    .mockResolvedValueOnce(new Response(null, { status: 404 }))
    .mockResolvedValueOnce(new Response(null, { status: 500 }));
  try {
    await expect(remoteListTasks(config)).rejects.toThrow("remote_management_unavailable");
    await expect(remoteListTasks(config)).rejects.toThrow("HTTP 500");
  } finally {
    fetch.mockRestore();
  }
});

test("connection test keeps takeover available on management API 5xx or non-JSON 200", async () => {
  const fetch = spyOn(globalThis, "fetch")
    .mockResolvedValueOnce(Response.json({ app: "FluxDown" }))
    .mockResolvedValueOnce(new Response(null, { status: 502 }))
    .mockResolvedValueOnce(Response.json({ app: "FluxDown" }))
    .mockResolvedValueOnce(new Response("<html></html>", { status: 200 }))
    .mockResolvedValueOnce(Response.json({ app: "FluxDown" }))
    .mockResolvedValueOnce(new Response(null, { status: 401 }));
  try {
    expect(await remoteVerify(config)).toMatchObject({ success: true, message: REMOTE_MANAGEMENT_UNAVAILABLE });
    expect(await remoteVerify(config)).toMatchObject({ success: true, message: REMOTE_MANAGEMENT_UNAVAILABLE });
    expect(await remoteVerify(config)).toMatchObject({ success: false, message: "remote_auth_failed" });
  } finally {
    fetch.mockRestore();
  }
});
