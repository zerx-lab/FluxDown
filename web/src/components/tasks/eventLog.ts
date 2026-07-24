// 任务事件时间线（模块级环形缓冲，仅本次会话内有效）。
// 订阅 lib/ws.ts 已公开的 liveStore / splitStore / cdnEventStore 变更并做本地 diff。
// 只记录离散事件（状态迁移、分段拆分、多 CDN 节点活动），不记录高频字节级 tick，
// 避免刷屏也不造假数据。

import { t } from '../../lib/i18n'
import { fmtBytes } from '../../lib/format'
import { cdnEventStore, liveStore, splitStore, Store } from '../../lib/ws'
import type { TaskCdnEventMsg, TaskStatus } from '../../lib/types'

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

/** 多 CDN 候选来源标记 → 本地化标签（对齐桌面 _cdnOriginLabel）。 */
function cdnOriginLabel(origin: string): string {
  if (origin === 'sys') return t('detail.cdnOriginSys')
  if (origin.startsWith('doh:')) return t('detail.cdnOriginDoh', { ep: origin.slice(4) })
  if (origin.startsWith('ecs:')) return t('detail.cdnOriginEcs', { ep: origin.slice(4) })
  return origin // 未知标记原样展示（引擎新增来源时向前兼容）
}

/** 多 CDN 事件 → 日志文案（多行：首行摘要，节点细节缩进列出；对齐桌面
 *  _cdnEventText，未知 kind 返回空串跳过，向前兼容）。 */
function cdnEventText(e: TaskCdnEventMsg): string {
  switch (e.kind) {
    case 'pool': {
      const lines = [
        t('detail.cdnPool', { host: e.host, n: e.nodes.length }),
        `  ${t('detail.cdnPoolStats', { candidates: e.candidates, alive: e.alive, cap: e.cap })}${e.autoCap ? t('detail.cdnAutoSuffix') : ''}`,
      ]
      for (const n of e.nodes) {
        let line = `  ${n.ip} · ${cdnOriginLabel(n.origin)}`
        if (n.ewmaBps > 0) line += ` · ${t('detail.cdnPrior', { speed: fmtBytes(n.ewmaBps) })}`
        lines.push(line)
      }
      return lines.join('\n')
    }
    case 'kick': {
      const reason =
        e.reason === 'validator'
          ? t('detail.cdnKickValidator')
          : e.reason === 'build'
            ? t('detail.cdnKickBuild')
            : t('detail.cdnKickFail', { n: e.candidates })
      return t('detail.cdnKick', { ip: e.ip, reason })
    }
    case 'breaker':
      return t('detail.cdnBreaker', { host: e.host })
    case 'fallback':
      return e.reason === 'few'
        ? t('detail.cdnFallbackFew', { candidates: e.candidates, alive: e.alive })
        : t('detail.cdnFallbackError')
    case 'leases': {
      const lines = [t('detail.cdnLeases', { host: e.host })]
      for (const n of e.nodes) {
        const ip = n.ip === 'SYS' ? t('detail.cdnNodeSys') : n.ip
        let line = `  ${t('detail.cdnLeasesNode', { ip, count: n.active })}`
        if (n.bytes > 0) line += ` · ${fmtBytes(n.bytes)}`
        lines.push(line)
      }
      return lines.join('\n')
    }
    case 'summary': {
      const lines = [t('detail.cdnSummary', { host: e.host })]
      for (const n of e.nodes) {
        const ip = n.ip === 'SYS' ? t('detail.cdnNodeSys') : n.ip
        lines.push(`  ${t('detail.cdnSummaryNode', { ip, bytes: fmtBytes(n.bytes), speed: fmtBytes(n.ewmaBps) })}`)
      }
      return lines.join('\n')
    }
    default:
      return ''
  }
}

/** 每任务最近一条 leases 快照的条目 id：连续 leases 就地覆盖为滚动快照，
 *  避免节流后的并发快照刷屏（对齐桌面 download_controller 的合并规则）。 */
const lastLeasesEntry = new Map<string, number>()

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
cdnEventStore.subscribe(() => {
  const e = cdnEventStore.get()
  if (!e) return
  const message = cdnEventText(e)
  if (!message) return
  if (e.kind === 'leases') {
    // 连续 leases 就地覆盖（滚动快照）：仅当该任务最后一条日志正是上次 leases 时。
    const prevId = lastLeasesEntry.get(e.taskId)
    const lastForTask = [...entries].reverse().find((en) => en.taskId === e.taskId)
    if (prevId !== undefined && lastForTask && lastForTask.id === prevId) {
      entries = entries.map((en) => (en.id === prevId ? { ...en, at: Date.now(), message } : en))
      eventLogStore.set(entries)
      return
    }
    push(e.taskId, message)
    lastLeasesEntry.set(e.taskId, seq)
    return
  }
  lastLeasesEntry.delete(e.taskId)
  push(e.taskId, message)
})
