// 对话框内部共用的小工具（格式化、队列显示名、错误码映射、时间戳 hook）。

import { useEffect, useState } from 'react'
import { LATER_QUEUE_ID, MAIN_QUEUE_ID, RpcError, errorMessage } from '../../../lib/rpc'
import type { QueueDto } from '../../../lib/rpc'
import type { TFunction } from '../../../i18n'

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] as const

/** 字节数 → `1.5 MB`；0 或负值显示 `0 B`。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${unit === 0 ? value : value.toFixed(value >= 100 ? 0 : 1)} ${UNITS[unit]}`
}

/** 队列显示名：内置队列本地化，自定义队列用用户命名。 */
export function queueLabel(t: TFunction, queue: { queueId: string; name: string }): string {
  if (queue.queueId === MAIN_QUEUE_ID) return t('mainQueue')
  if (queue.queueId === LATER_QUEUE_ID) return t('laterQueue')
  return queue.name
}

/** 按 queueId 找显示名；找不到时退回 id。 */
export function queueLabelById(t: TFunction, queues: readonly QueueDto[], queueId: string): string {
  const queue = queues.find((item) => item.queueId === queueId)
  return queue ? queueLabel(t, queue) : queueLabel(t, { queueId, name: queueId })
}

/** 引擎稳定错误码（`invalid-name` 等）→ i18n 键；错误消息里包含该码即命中。 */
export function mapEngineError(error: unknown, table: Readonly<Record<string, string>>): string | null {
  if (error instanceof RpcError && error.is('notFound') && 'not-found' in table) return table['not-found']
  const message = errorMessage(error)
  for (const [code, key] of Object.entries(table)) {
    if (message.includes(code)) return key
  }
  return null
}

/** 每 `intervalMs` 刷新一次的当前时间戳（ms）；`null` = 不需要走字（不启动定时器）。 */
export function useNow(intervalMs: number | null = 1000): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (intervalMs === null) return
    const timer = setInterval(() => setNow(Date.now()), intervalMs)
    return () => clearInterval(timer)
  }, [intervalMs])
  return now
}

/** 文件名（路径末段）。 */
export function baseName(path: string): string {
  const parts = path.split('/')
  return parts[parts.length - 1] || path
}
