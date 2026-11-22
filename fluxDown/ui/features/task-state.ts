/** 任务列表的纯展示判定（无 DOM / 扩展 API 依赖，便于单测）。 */

/** NMH `tasks` 应答里的任务状态码：3 = 已完成。 */
const STATUS_COMPLETED = 3;

/**
 * 已完成任务的落盘文件是否已被删除 / 移走。
 * 仅「已完成 + 主程序标记 fileMissing」才算：其他状态下该标志无意义；旧版主程序不发该字段，视为未缺失。
 */
export function isCompletedFileMissing(task: { status: number; fileMissing?: boolean }): boolean {
  return task.status === STATUS_COMPLETED && task.fileMissing === true;
}

/**
 * 「一键清理」后的隐藏名单：本次列表里全部已完成任务 ∪ 旧名单中仍在列表里的 id。
 * 只影响扩展自身的展示，不删除主程序任务；已滚出列表的旧 id 顺带丢弃，名单有界。
 */
export function nextClearedCompletedIds(tasks: readonly { taskId: string; status: number }[], prev: readonly string[]): string[] {
  const present = new Set(tasks.map((x) => x.taskId));
  const out = new Set(prev.filter((id) => present.has(id)));
  for (const task of tasks) if (task.status === STATUS_COMPLETED) out.add(task.taskId);
  return [...out];
}

/** 过滤掉已被扩展「清理」隐藏的已完成任务；未完成任务始终保留。 */
export function hideClearedCompleted<T extends { taskId: string; status: number }>(tasks: readonly T[], cleared: readonly string[]): T[] {
  if (cleared.length === 0) return [...tasks];
  const hidden = new Set(cleared);
  return tasks.filter((x) => x.status !== STATUS_COMPLETED || !hidden.has(x.taskId));
}
