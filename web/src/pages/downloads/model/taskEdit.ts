// 「重命名 / 更换链接 / 修改线程数」的可用性判定与线程数输入解析（镜像 GPUI `TaskMenuFacts::can_rename` /
// `can_change_url` / `can_change_segments` 与 `pages/task_edit.rs::parse_task_segments`，改一处须同步另一处）。

import { MAX_TASK_SEGMENTS } from '../../../lib/threadsRisk'
import type { DownloadTaskView } from './task'

type EditFacts = Pick<DownloadTaskView, 'source' | 'state' | 'url'>

function hasPrefix(url: string, prefixes: readonly string[]): boolean {
  const lower = url.toLowerCase()
  return prefixes.some((prefix) => lower.startsWith(prefix))
}

/** BT 任务源：`magnet:` 链接或 `torrent-file://` 本地种子哨兵。 */
export function isBtUrl(url: string): boolean {
  return hasPrefix(url, ['magnet:', 'torrent-file://'])
}

/** 引擎 `change_task_url` 接受的源：http(s) / ftp。 */
export function isReplaceableUrl(url: string): boolean {
  return hasPrefix(url, ['http://', 'https://', 'ftp://'])
}

/**
 * 「重命名」：本地非 BT、不在下载 / 准备中的任务（对齐引擎 `rename_task`：活跃任务 `task-active`，
 * BT 落盘名由完成布局决定、做种期间持有句柄，`bt-unsupported`）。eD2K 与 HTTP 同以 `<名>.fdownloading` 续传，可改名。
 */
export function canRename(view: EditFacts & Pick<DownloadTaskView, 'preparing'>): boolean {
  return view.source === 'local' && view.state !== 'downloading' && !view.preparing && !isBtUrl(view.url)
}

/** 「更换链接」：本地任务，暂停 / 失败 / 等待（引擎拒绝下载中与已完成），源为 http(s) / ftp。 */
export function canChangeUrl(view: EditFacts): boolean {
  return (
    view.source === 'local' &&
    (view.state === 'paused' || view.state === 'failed' || view.state === 'pending') &&
    isReplaceableUrl(view.url)
  )
}

/** 「修改线程数」：本地非 BT、未完成的任务（下载中也可，引擎静默暂停后恢复）。 */
export function canChangeSegments(view: EditFacts): boolean {
  return view.source === 'local' && view.state !== 'completed' && !isBtUrl(view.url)
}

/** 「完整性校验」：本地已完成的 BT 任务且目标文件未丢失（镜像 GPUI `can_verify_integrity`；调用方保证单选）。 */
export function canVerifyIntegrity(view: EditFacts & Pick<DownloadTaskView, 'fileMissing'>): boolean {
  return view.source === 'local' && view.state === 'completed' && isBtUrl(view.url) && !view.fileMissing
}

/** 下载顺序：全部非顺序 = 最稀有（默认）；全部顺序 = 按顺序；混选或空选 = 两项都不点亮。 */
export type PieceOrder = 'rarest' | 'sequential' | 'mixed'

/** 「选片方式」：全部为本地、未完成的 BT 任务（多选取交集）。 */
export function canSetPiecePicking(views: readonly EditFacts[]): boolean {
  return views.length > 0 && views.every((view) => view.source === 'local' && view.state !== 'completed' && isBtUrl(view.url))
}

/** 所选任务的下载顺序。空选与混选都不点亮。 */
export function pieceOrder(views: readonly Pick<DownloadTaskView, 'btSequential'>[]): PieceOrder {
  if (views.length === 0) return 'mixed'
  const sequential = views[0]!.btSequential
  return views.every((view) => view.btSequential === sequential) ? (sequential ? 'sequential' : 'rarest') : 'mixed'
}

/** 需要改成目标顺序的任务；已是该状态的跳过。 */
export function pieceOrderTargets<T extends Pick<DownloadTaskView, 'btSequential'>>(views: readonly T[], sequential: boolean): T[] {
  return views.filter((view) => view.btSequential !== sequential)
}

/** 顺序只带 `sequential`。`false` = 最稀有优先。 */
export function pieceSequentialParams(taskId: string, sequential: boolean): { taskId: string; sequential: boolean } {
  return { taskId, sequential }
}

/** 「先下载首尾」是否全部打开（空选为 false）。 */
export function pieceOptionAllOn(views: readonly Pick<DownloadTaskView, 'btFirstLastPiecesFirst'>[]): boolean {
  return views.length > 0 && views.every((view) => view.btFirstLastPiecesFirst)
}

/** 首尾切换只带该字段，目标为「并非全部已开」。 */
export function pieceEndsParams(taskId: string, allOn: boolean): { taskId: string; firstLastPiecesFirst: boolean } {
  return { taskId, firstLastPiecesFirst: !allOn }
}

/** 线程数输入 → 线程数：`0..=MAX_TASK_SEGMENTS` 的整数（0 = 自动）；空串 / 非数字 / 超限 → null。 */
export function parseTaskSegments(input: string): number | null {
  const text = input.trim()
  if (!/^\d+$/.test(text)) return null
  const value = Number(text)
  return Number.isSafeInteger(value) && value <= MAX_TASK_SEGMENTS ? value : null
}
