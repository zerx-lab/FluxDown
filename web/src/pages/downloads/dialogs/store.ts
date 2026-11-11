// 对话框宿主的内部状态：模块级 store（任何位置都可 `openXxx()`），`DownloadDialogsHost` 订阅渲染。

import { useSyncExternalStore } from 'react'
import { submitTorrentFiles } from './torrents'

/** 已并入过「新建下载」表单的外部捕获事务 id：避免关闭后（忽略失败时）反复弹出。 */
export const capturesShown = new Set<string>()

export interface NewDownloadOptions {
  urls?: string[]
  torrentFiles?: File[]
  queueId?: string
  /** 预选「下载到」目标（`cloud:<deviceId>` / `link:<fingerprint>`）；缺省 = 本服务器。 */
  target?: string
}

/** 打开中的「新建下载」会话；已打开时再次 `openNewDownload` 只追加注入项。 */
export interface NewDownloadSession {
  /** 每次从关闭到打开递增，用作组件 key（表单整体重置）。 */
  session: number
  queueId?: string
  target?: string
  /** 打开时预填的链接。 */
  initialUrls: string[]
  /** 会话期间追加的链接 / 种子（id 递增，组件按 id 去重处理）。 */
  injections: { id: number; urls: string[]; torrentFiles: File[] }[]
}

export interface DialogsState {
  newDownload: NewDownloadSession | null
  queueManager: { queueId?: string; nonce: number } | null
  groupDetail: string | null
  rename: string | null
  changeUrl: string | null
  changeSegments: string | null
  /** 「移动到…」的 BT 任务 id。 */
  moveStorage: string | null
  /** 「任务限速…」的 BT 任务 id。 */
  taskLimits: string | null
  seedLocal: boolean
}

let state: DialogsState = {
  newDownload: null,
  queueManager: null,
  groupDetail: null,
  rename: null,
  changeUrl: null,
  changeSegments: null,
  moveStorage: null,
  taskLimits: null,
  seedLocal: false,
}
let counter = 0
const listeners = new Set<() => void>()

function publish(patch: Partial<DialogsState>): void {
  state = { ...state, ...patch }
  for (const listener of listeners) listener()
}

const subscribe = (listener: () => void): (() => void) => {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useDialogsState(): DialogsState {
  return useSyncExternalStore(subscribe, () => state)
}

export function openNewDownload(options: NewDownloadOptions = {}): void {
  const urls = (options.urls ?? []).map((url) => url.trim()).filter((url) => url !== '')
  const torrentFiles = options.torrentFiles ?? []
  const current = state.newDownload
  if (!current && urls.length === 0 && torrentFiles.length > 0) {
    // 只有种子文件：不需要表单，直接上传建任务（BT 文件选择由 daemon 选择请求驱动）。
    void submitTorrentFiles(torrentFiles, { queueId: options.queueId })
    return
  }
  if (current) {
    if (urls.length > 0 || torrentFiles.length > 0) {
      counter += 1
      publish({ newDownload: { ...current, injections: [...current.injections, { id: counter, urls, torrentFiles }] } })
    }
    return
  }
  counter += 1
  publish({
    newDownload: {
      session: counter,
      queueId: options.queueId,
      target: options.target,
      initialUrls: urls,
      injections: torrentFiles.length > 0 ? [{ id: counter, urls: [], torrentFiles }] : [],
    },
  })
}

export function closeNewDownload(): void {
  publish({ newDownload: null })
}

export function openQueueManager(queueId?: string): void {
  counter += 1
  publish({ queueManager: { queueId, nonce: counter } })
}

export function closeQueueManager(): void {
  publish({ queueManager: null })
}

export function openGroupDetail(groupId: string): void {
  publish({ groupDetail: groupId })
}

export function closeGroupDetail(): void {
  publish({ groupDetail: null })
}

export function openRename(taskId: string): void {
  publish({ rename: taskId })
}

export function closeRename(): void {
  publish({ rename: null })
}

export function openChangeUrl(taskId: string): void {
  publish({ changeUrl: taskId })
}

export function closeChangeUrl(): void {
  publish({ changeUrl: null })
}

export function openChangeSegments(taskId: string): void {
  publish({ changeSegments: taskId })
}

export function closeChangeSegments(): void {
  publish({ changeSegments: null })
}

export function openMoveStorage(taskId: string): void {
  publish({ moveStorage: taskId })
}

export function closeMoveStorage(): void {
  publish({ moveStorage: null })
}

export function openTaskLimits(taskId: string): void {
  publish({ taskLimits: taskId })
}

export function closeTaskLimits(): void {
  publish({ taskLimits: null })
}

export function openSeedLocal(): void {
  publish({ seedLocal: true })
}

export function closeSeedLocal(): void {
  publish({ seedLocal: false })
}
