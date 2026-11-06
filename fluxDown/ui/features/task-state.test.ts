import { describe, expect, test } from "bun:test";
import { isCompletedFileMissing } from "./task-state";

describe("isCompletedFileMissing", () => {
  test("已完成且 fileMissing 为真 → 缺失", () => {
    expect(isCompletedFileMissing({ status: 3, fileMissing: true })).toBe(true);
  });

  test("已完成但标志缺省 / false（含旧版主程序不发该字段）→ 不缺失", () => {
    expect(isCompletedFileMissing({ status: 3 })).toBe(false);
    expect(isCompletedFileMissing({ status: 3, fileMissing: false })).toBe(false);
  });

  test("未完成状态即使带标志也不算缺失", () => {
    for (const status of [0, 1, 2, 4, 5]) {
      expect(isCompletedFileMissing({ status, fileMissing: true })).toBe(false);
    }
  });
});
