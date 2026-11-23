import type { FileSizeUnits } from '../../../lib/fileSizeUnits'
// 表格单元格语义（plugin_view_schema.json `table.columns[].kind`）：纯函数，组件只负责展示。

import { formatBytes, formatDateTime } from '../../downloads/model/task'
import { isTone } from './tones'
import type { Tone } from './tones'

export type CellKind = 'text' | 'bytes' | 'speed' | 'percent' | 'progress' | 'time' | 'badge'

export interface CellView {
  text: string
  /** `progress` 列：0..1。 */
  fraction?: number
  /** `badge` 列：色调。 */
  tone?: Tone
}

const EMPTY = '—'

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}

function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value))
}

export function formatCell(kind: string, value: unknown, units?: FileSizeUnits): CellView {
  switch (kind) {
    case 'bytes': {
      const n = finite(value)
      return { text: n === null ? EMPTY : formatBytes(Math.max(0, n), units) }
    }
    case 'speed': {
      const n = finite(value)
      return { text: n === null ? EMPTY : `${formatBytes(Math.max(0, n), units)}/s` }
    }
    case 'percent': {
      const n = finite(value)
      return { text: n === null ? EMPTY : `${Math.round(clamp01(n) * 100)}%` }
    }
    case 'progress': {
      const n = finite(value)
      return n === null ? { text: EMPTY } : { text: `${Math.round(clamp01(n) * 100)}%`, fraction: clamp01(n) }
    }
    case 'time': {
      const n = finite(value)
      return { text: n === null || n <= 0 ? EMPTY : formatDateTime(n / 1000) }
    }
    case 'badge': {
      if (typeof value === 'string') return { text: value, tone: 'neutral' }
      if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
        const record = value as Record<string, unknown>
        const text = typeof record.text === 'string' ? record.text : ''
        return { text, tone: isTone(record.tone) ? record.tone : 'neutral' }
      }
      return { text: '' }
    }
    default: {
      if (value === null || value === undefined) return { text: '' }
      return { text: typeof value === 'string' ? value : String(value) }
    }
  }
}
