import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { NmhPortLike } from "@/utils/nmh-dedicated";

/** 共享端口上的请求（`nmhCall`）与专用端口上的连接（`connectNative`）分别记录。 */
const sharedCalls: Array<{ action: string; payload: unknown; timeoutMs: number | undefined }> = [];
const connections: Array<{ name: string; port: NmhPortLike & { sent: Array<Record<string, unknown>>; reply(data: object): void } }> = [];
let sharedReply: { success: boolean; message?: string; data?: unknown } = { success: true };

mock.module("@/utils/native-messaging", () => ({
  NMH_NAME: "com.fluxdown.nmh",
  nmhCall: async (action: string, payload: unknown, timeoutMs?: number) => {
    sharedCalls.push({ action, payload, timeoutMs });
    return sharedReply;
  },
}));
// 与 sender.test.ts 共享同一进程内的 mock.module：这里必须列全 download-dispatch 的导出，
// 否则 sender / breaker 等模块（已被先加载）重链接时会因缺失具名导出而报错。
mock.module("@/utils/download-dispatch", () => ({
  checkFluxDownAvailable: async () => true,
  checkFluxDownAvailableWithRetry: async () => true,
  checkRemoteAvailable: async () => false,
  effectiveRemoteMode: async () => "off",
  isUnreachableFailure: () => false,
  sendBatchDownloadRequest: async () => ({ success: true }),
  sendDownloadRequest: async () => ({ success: true }),
  warmupNativeHost: () => {},
}));
mock.module("wxt/browser", () => ({
  browser: {
    runtime: {
      connectNative(name: string) {
        const messageListeners: Array<(m: unknown) => void> = [];
        const port = {
          sent: [] as Array<Record<string, unknown>>,
          postMessage(message: unknown) {
            port.sent.push(message as Record<string, unknown>);
          },
          disconnect() {},
          onMessage: { addListener: (l: (m: unknown) => void) => void messageListeners.push(l) },
          onDisconnect: { addListener: () => {} },
          reply(data: object) {
            const msgId = port.sent.at(-1)?.msg_id;
            for (const l of messageListeners) l({ success: true, msg_id: msgId, data });
          },
        };
        connections.push({ name, port });
        return port;
      },
    },
  },
}));

// 必须在 mock.module 安装之后再加载 host（静态 import 会先于 mock 求值，host 会拿到真实的 wxt/browser 与 NMH）。
const host = await import("./host");

beforeEach(() => {
  sharedCalls.length = 0;
  connections.length = 0;
  sharedReply = { success: true };
});

describe("plugin invoke", () => {
  test("never uses the shared NMH port: it opens a dedicated connectNative port per plugin", async () => {
    const first = host.invokePlugin("a@x", "resolve", { id: 1 }, "https://p.test/");
    const other = host.invokePlugin("b@x", "resolve", undefined, "https://p.test/");
    expect(sharedCalls).toHaveLength(0);
    expect(connections.map((c) => c.name)).toEqual(["com.fluxdown.nmh", "com.fluxdown.nmh"]);
    expect(connections[0].port.sent[0]).toMatchObject({
      action: "plugin_invoke",
      identity: "a@x",
      name: "resolve",
      args: { id: 1 },
      pageUrl: "https://p.test/",
    });
    // undefined 的 args 以 null 发送（JSON 无 undefined）
    expect(connections[1].port.sent[0]).toMatchObject({ identity: "b@x", args: null });
    connections[0].port.reply({ value: { title: "t" } });
    connections[1].port.reply({ value: 2 });
    expect(await first).toEqual({ ok: true, value: { title: "t" } });
    expect(await other).toEqual({ ok: true, value: 2 });
    expect(sharedCalls).toHaveLength(0);
  });

  test("same plugin invocations queue on one dedicated port", async () => {
    // 池是模块级单例：用没被其它用例占用过的 identity，避免复用前一个用例留下的空闲端口。
    const a = host.invokePlugin("queue@x", "one", null, "https://p.test/");
    const b = host.invokePlugin("queue@x", "two", null, "https://p.test/");
    expect(connections).toHaveLength(1);
    expect(connections[0].port.sent).toHaveLength(1);
    connections[0].port.reply({ value: 1 });
    await a;
    expect(connections[0].port.sent).toHaveLength(2);
    connections[0].port.reply({ value: 2 });
    expect(await b).toEqual({ ok: true, value: 2 });
  });
});

describe("page script requests use the shared port", () => {
  test("fetchPageScripts / fetchPageScriptSource go through nmhCall", async () => {
    sharedReply = { success: true, data: { sequence: 2, epoch: "e", scripts: [] } };
    expect(await host.fetchPageScripts()).toEqual({ sequence: 2, epoch: "e", scripts: [] });
    sharedReply = { success: false, message: "page script too large" };
    expect(await host.fetchPageScriptSource("a@x", "one")).toEqual({ ok: false, message: "page script too large" });
    expect(sharedCalls.map((c) => c.action)).toEqual(["page_scripts", "page_script_source"]);
    expect(sharedCalls[1].payload).toEqual({ identity: "a@x", id: "one" });
    expect(connections).toHaveLength(0);
  });

  test("unreachable desktop / malformed index yields null", async () => {
    sharedReply = { success: false, message: "timeout" };
    expect(await host.fetchPageScripts()).toBeNull();
    sharedReply = { success: true, data: { nope: true } };
    expect(await host.fetchPageScripts()).toBeNull();
  });

  test("capability gate: actions must include page_scripts", () => {
    expect(host.supportsPageScripts({ protocol: 2, connected: true, actions: ["page_scripts"] })).toBe(true);
    expect(host.supportsPageScripts({ protocol: 2, connected: true, actions: [] })).toBe(false);
    expect(host.supportsPageScripts({ protocol: 1, connected: true, actions: ["page_scripts"] })).toBe(false);
    expect(host.supportsPageScripts({ protocol: 2, connected: false, actions: ["page_scripts"] })).toBe(false);
  });
});
