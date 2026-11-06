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
