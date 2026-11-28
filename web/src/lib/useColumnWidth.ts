// 左侧列表列宽：拖拽 / 键盘调节，范围 176–400，记忆到 localStorage（RSS 订阅源列、助手会话列共用）。

import { useRef, useState } from 'react'
import type { KeyboardEvent, PointerEvent } from 'react'

export const COLUMN_DEFAULT = 240
export const COLUMN_MIN = 176
export const COLUMN_MAX = 400
const KEYBOARD_STEP = 16

const clampWidth = (width: number) => Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, Math.round(width)))

function readStoredWidth(storageKey: string): number {
  try {
    const stored = Number(localStorage.getItem(storageKey))
    return Number.isFinite(stored) && stored > 0 ? clampWidth(stored) : COLUMN_DEFAULT
  } catch {
    return COLUMN_DEFAULT
  }
}

export interface ColumnWidthHandleProps {
  onPointerDown: (event: PointerEvent<HTMLDivElement>) => void
  onPointerMove: (event: PointerEvent<HTMLDivElement>) => void
  onPointerUp: () => void
  onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => void
}

/** `storageKey`：该列宽度在 localStorage 里的键（各页面各用各的）。 */
export function useColumnWidth(storageKey: string): { width: number; handleProps: ColumnWidthHandleProps } {
  const [width, setWidth] = useState(() => readStoredWidth(storageKey))
  const drag = useRef<{ startX: number; startWidth: number } | null>(null)
  const persist = (value: number) => {
    try {
      localStorage.setItem(storageKey, String(value))
    } catch {
      // 隐私模式等场景：宽度只在本次会话有效。
    }
  }
  const handleProps: ColumnWidthHandleProps = {
    onPointerDown: (event) => {
      drag.current = { startX: event.clientX, startWidth: width }
      event.currentTarget.setPointerCapture(event.pointerId)
    },
    onPointerMove: (event) => {
      const state = drag.current
      if (state) setWidth(clampWidth(state.startWidth + event.clientX - state.startX))
    },
    onPointerUp: () => {
      if (drag.current) persist(width)
      drag.current = null
    },
    onKeyDown: (event) => {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
      event.preventDefault()
      const next = clampWidth(width + (event.key === 'ArrowRight' ? KEYBOARD_STEP : -KEYBOARD_STEP))
      setWidth(next)
      persist(next)
    },
  }
  return { width, handleProps }
}
