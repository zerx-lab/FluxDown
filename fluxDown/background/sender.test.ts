import { beforeEach, describe, expect, mock, test } from "bun:test";
import { createStorageArea } from "@/utils/storage-mock.test-util";
import type { DownloadRequest } from "@/utils/native-messaging";

const local = createStorageArea();
const sync = createStorageArea();
mock.module("wxt/browser", () => ({
  browser: {
    storage: { local, sync },
    runtime: { getPlatformInfo: async () => ({ os: "linux" }) },
    cookies: { getAll: async () => [] },
    alarms: { get: async () => undefined, create: async () => {}, clear: async () => {} },
  },
}));

// 显式列出 download-dispatch 的全部导出：bun 的 mock.module 进程内共享，其它测试文件
// （host-page-scripts.test.ts）也会替换它，所以不能依赖「真实模块」的导出。
const dispatched: DownloadRequest[] = [];
mock.module("@/utils/download-dispatch", () => ({
  checkFluxDownAvailable: async () => true,
  checkFluxDownAvailableWithRetry: async () => true,
  checkRemoteAvailable: async () => false,
  effectiveRemoteMode: async () => "off",
  isUnreachableFailure: () => false,
  sendBatchDownloadRequest: async () => ({ success: true, channel: "local" }),
  warmupNativeHost: () => {},
  sendDownloadRequest: async (request: DownloadRequest) => {
    dispatched.push(request);
    return { success: true, channel: "local" };
  },
}));
// breaker / tasks 只在发送成败后做熔断与轮询登记，与去重接线无关；mock 掉可避免把
// native-messaging 等被其它测试文件局部 mock 的模块拉进链接图。
mock.module("./breaker", () => ({
  allowBypass: () => {},
  claimAppDownNotice: () => true,
  markAppDown: () => {},
  markAppUp: () => {},
  probeAppReachable: async () => true,
}));
mock.module("./tasks", () => ({ maybeArmTaskPollForChannel: () => {} }));

// 必须先 mock 再加载被测模块（与 host-page-scripts.test.ts 同一约定），故此处动态导入。
const { sendToFluxDown } = await import("./sender");
const { requestRecordCache } = await import("./request-cache");
const { recordCapturedRequest } = await import("./request-record");

// 去重状态是模块级的（3s 窗口），每个用例用独立 URL 隔离。
let seq = 0;
let url = "";
const form = (id: string) => ({ kind: "formData" as const, fields: { id: [id] } });
const post = (id: string) => recordCapturedRequest(requestRecordCache, url, "POST", form(id), Date.now());

beforeEach(() => {
  url = `https://oa.example.com/api/export?case=${(seq += 1)}`;
  dispatched.length = 0;
  requestRecordCache.clear();
});

describe("sendToFluxDown 自动拦截去重接线", () => {
  test("同 URL 的两次 GET 自动拦截只送达一次", async () => {
    const [a, b] = await Promise.all([
      sendToFluxDown({ url, origin: "intercept" }),
      sendToFluxDown({ url, origin: "intercept" }),
    ]);
    expect(dispatched).toHaveLength(1);
    expect(a.success && b.success).toBe(true);
  });

  test("同 URL、不同 POST body 的两次自动拦截都要送达（不得静默丢弃第二个）", async () => {
    post("1");
    await sendToFluxDown({ url, origin: "intercept" });
    post("2");
    const second = await sendToFluxDown({ url, origin: "intercept" });
    expect(second.success).toBe(true);
    expect(dispatched.map((r) => r.method)).toEqual(["POST", "POST"]);
    expect(dispatched.map((r) => r.body)).toEqual([form("1"), form("2")]);
  });

  test("同 URL、同 POST body 的重复自动拦截仍只送达一次", async () => {
    post("1");
    await sendToFluxDown({ url, origin: "intercept" });
    post("1");
    await sendToFluxDown({ url, origin: "intercept" });
    expect(dispatched).toHaveLength(1);
  });

  test("同 URL 的 GET 与 POST 互不去重", async () => {
    await sendToFluxDown({ url, origin: "intercept" });
    post("1");
    await sendToFluxDown({ url, origin: "intercept" });
    expect(dispatched.map((r) => r.method)).toEqual([undefined, "POST"]);
  });

  test("手动发送与带 channel 的发送不去重", async () => {
    await sendToFluxDown({ url });
    await sendToFluxDown({ url });
    await sendToFluxDown({ url, origin: "intercept", channel: "local" });
    await sendToFluxDown({ url, origin: "intercept", channel: "local" });
    expect(dispatched).toHaveLength(4);
  });
});
