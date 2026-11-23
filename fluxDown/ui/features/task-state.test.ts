import { describe, expect, test } from "bun:test";
import { hideClearedCompleted, isCompletedFileMissing, nextClearedCompletedIds } from "./task-state";

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

describe("一键清理已完成记录", () => {
  const tasks = [
    { taskId: "a", status: 3 },
    { taskId: "b", status: 1 },
    { taskId: "c", status: 3 },
  ];

  test("清理名单收全部已完成任务，不含未完成任务", () => {
    expect(nextClearedCompletedIds(tasks, []).sort()).toEqual(["a", "c"]);
  });

  test("旧名单里已不在列表的 id 被丢弃，仍在的保留", () => {
    expect(nextClearedCompletedIds([{ taskId: "d", status: 3 }, { taskId: "a", status: 3 }], ["a", "gone"]).sort()).toEqual(["a", "d"]);
  });

  test("只隐藏名单内的已完成任务，同 id 的未完成任务（如重新下载）照常显示", () => {
    expect(hideClearedCompleted(tasks, ["a"]).map((x) => x.taskId)).toEqual(["b", "c"]);
    expect(hideClearedCompleted(tasks, ["b"]).map((x) => x.taskId)).toEqual(["a", "b", "c"]);
  });
});
