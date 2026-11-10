import { describe, expect, test } from "bun:test";
import { createSendDeduper } from "./send-dedupe";

type Outcome = { success: boolean; id?: number };

function setup(windowMs = 3000) {
  let clock = 1_000;
  const deduper = createSendDeduper<Outcome>(windowMs, () => clock);
  let calls = 0;
  const send = (outcome: Outcome) => () => {
    calls += 1;
    return Promise.resolve(outcome);
  };
  return { deduper, send, calls: () => calls, advance: (ms: number) => (clock += ms) };
}

describe("createSendDeduper", () => {
  test("发送进行中的同 key 请求复用同一次发送", async () => {
    const { deduper } = setup();
    const slow = Promise.withResolvers<Outcome>();
    const first = deduper.run("u", () => slow.promise);
    const second = deduper.run("u", () => {
      throw new Error("不应再发送");
    });
    expect(first.reused).toBe(false);
    expect(second.reused).toBe(true);
    slow.resolve({ success: true, id: 1 });
    expect(await second.promise).toEqual({ success: true, id: 1 });
  });

  test("成功后窗口内复用结果，窗口过后重新发送", async () => {
    const { deduper, send, calls, advance } = setup(3000);
    await deduper.run("u", send({ success: true, id: 1 })).promise;
    advance(2999);
    const within = deduper.run("u", send({ success: true, id: 2 }));
    expect(within.reused).toBe(true);
    expect(await within.promise).toEqual({ success: true, id: 1 });

    advance(1);
    const after = deduper.run("u", send({ success: true, id: 3 }));
    expect(after.reused).toBe(false);
    expect(await after.promise).toEqual({ success: true, id: 3 });
    expect(calls()).toBe(2);
  });

  test("失败结果不缓存：落定后可重试", async () => {
    const { deduper, send, calls } = setup();
    expect((await deduper.run("u", send({ success: false })).promise).success).toBe(false);
    const retry = deduper.run("u", send({ success: true, id: 2 }));
    expect(retry.reused).toBe(false);
    expect((await retry.promise).success).toBe(true);
    expect(calls()).toBe(2);
  });

  test("发送抛错不缓存", async () => {
    const { deduper, send } = setup();
    await expect(deduper.run("u", () => Promise.reject(new Error("boom"))).promise).rejects.toThrow("boom");
    const retry = deduper.run("u", send({ success: true }));
    expect(retry.reused).toBe(false);
    expect((await retry.promise).success).toBe(true);
  });

  test("不同 key 互不影响", async () => {
    const { deduper, send, calls } = setup();
    await deduper.run("a", send({ success: true })).promise;
    const other = deduper.run("b", send({ success: true }));
    expect(other.reused).toBe(false);
    await other.promise;
    expect(calls()).toBe(2);
  });
});
