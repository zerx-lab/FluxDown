// AI 助手面板状态（窗口右侧全高面板，移动端为全屏浮层）：是否打开、宽度、当前视图（对话 / 历史会话）。
// 仅存会话内存，不持久化（与 GPUI 一致）；写法同 chromeStore：模块级快照 + useSyncExternalStore。

import { useSyncExternalStore } from 'react'

export const PANEL_DEFAULT_WIDTH = 400
export const PANEL_MIN_WIDTH = 320
export const PANEL_MAX_WIDTH = 720
/** 面板打开时主区（标题栏 + 活动栏 + 内容 + 状态栏）至少保留的宽度。 */
export const PANEL_MAIN_MIN_WIDTH = 360

export type AssistantPanelView = 'chat' | 'history'

export interface AssistantPanelState {
  open: boolean
  width: number
  view: AssistantPanelView
}

/**
 * 把期望宽度夹到 [320, 720]，并保证主区至少保留 360px（窗口过窄时上限收到 `windowWidth - 360`，
 * 但不低于最小宽度 320）。`windowWidth` 缺省 = 不考虑主区约束。
 */
export function clampPanelWidth(width: number, windowWidth?: number): number {
  const max = windowWidth === undefined ? PANEL_MAX_WIDTH : Math.max(PANEL_MIN_WIDTH, Math.min(PANEL_MAX_WIDTH, windowWidth - PANEL_MAIN_MIN_WIDTH))
  return Math.round(Math.min(max, Math.max(PANEL_MIN_WIDTH, width)))
}

/** ⌘K（macOS / iOS）或 Ctrl+K（其它平台）；带 Alt / Shift 的组合不算。 */
export function isAssistantShortcut(event: Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'>, mac: boolean): boolean {
  if (event.altKey || event.shiftKey || event.key.toLowerCase() !== 'k') return false
  return mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey
}

type Listener = () => void

const listeners = new Set<Listener>()
const INITIAL: AssistantPanelState = { open: false, width: PANEL_DEFAULT_WIDTH, view: 'chat' }
let state: AssistantPanelState = INITIAL

function set(next: AssistantPanelState): void {
  if (next.open === state.open && next.width === state.width && next.view === state.view) return
  state = next
  for (const listener of listeners) listener()
}

function subscribe(listener: Listener) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export const getAssistantPanel = (): AssistantPanelState => state

export function useAssistantPanel(): AssistantPanelState {
  return useSyncExternalStore(subscribe, getAssistantPanel, getAssistantPanel)
}

/** 打开面板；保留上次的 chat / history 视图。 */
export function openAssistantPanel(): void {
  set(state.open ? state : { ...state, open: true })
}

export function closeAssistantPanel(): void {
  set({ ...state, open: false })
}

export function toggleAssistantPanel(): void {
  if (state.open) closeAssistantPanel()
  else openAssistantPanel()
}

/** 设置宽度（夹到 [320, 720]；传入窗口宽度时再保证主区至少 360px）。渲染层另用 `max-width` 兜底窗口后续缩小。 */
export function setAssistantPanelWidth(width: number, windowWidth?: number): void {
  set({ ...state, width: clampPanelWidth(width, windowWidth) })
}

export function showAssistantHistory(): void {
  set({ ...state, view: 'history' })
}

export function showAssistantChat(): void {
  set({ ...state, view: 'chat' })
}

/** 测试用：回到初始状态。 */
export function resetAssistantPanel(): void {
  set(INITIAL)
}
