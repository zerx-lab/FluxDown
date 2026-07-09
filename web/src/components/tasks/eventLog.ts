// 任务事件时间线（模块级环形缓冲，仅本次会话内有效）。
// 订阅 lib/ws.ts 已公开的 liveStore / splitStore 变更并做本地 diff——不修改 lib/ws.ts。
// 只记录离散事件（状态迁移、分段拆分），不记录高频字节级 tick，避免刷屏也不造假数据。

import { t } from '../../lib/i18n'
import { liveStore, splitStore, Store } from '../../lib/ws'
import type { TaskStatus } from '../../lib/types'

export interface LogEntry {
  id: number
  at: number
  taskId: string
  message: string
  isError: boolean
}

const MAX_ENTRIES = 300

/** 状态文案：函数而非模块级常量，避免固化在某一语言（随当前语言实时取值）。 */
function statusText(s: TaskStatus): string {
  const KEYS = {
    0: 'status.pending',
    1: 'status.downloading',
    2: 'status.paused',
    3: 'status.completed',
    4: 'status.error',
    5: 'status.preparing',
  } as const
  return t(KEYS[s])
}

let seq = 0
let entries: LogEntry[] = []
export const eventLogStore = new Store<LogEntry[]>([])
const lastStatus = new Map<string, TaskStatus>()

function push(taskId: string, message: string, isError = false) {
  seq += 1
  const next = [...entries, { id: seq, at: Date.now(), taskId, message, isError }]
  entries = next.length > MAX_ENTRIES ? next.slice(next.length - MAX_ENTRIES) : next
  eventLogStore.set(entries)
}

liveStore.subscribe(() => {
  const live = liveStore.get()
  for (const taskId in live) {
    const status = live[taskId].status
    const prev = lastStatus.get(taskId)
    lastStatus.set(taskId, status)
    // 首次观测到该任务（如刚连接/刷新页面）不记录，避免把历史状态当作"刚发生的变更"刷屏。
    if (prev === undefined || prev === status) continue
    if (status === 4) push(taskId, t('event.errored', { message: live[taskId].errorMessage || t('event.unknownError') }), true)
    else push(taskId, t('event.statusChanged', { from: statusText(prev), to: statusText(status) }))
  }
})

splitStore.subscribe(() => {
  const s = splitStore.get()
  if (!s) return
  push(
    s.taskId,
    t('event.segmentSplit', {
      parent: s.parentIndex + 1,
      kind: s.isProactive ? t('event.proactive') : t('event.reactive'),
      child: s.childIndex + 1,
      total: s.totalSegments,
    }),
  )
})
