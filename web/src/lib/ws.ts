// WebSocket 实时通道：可重连、按 type 分派到轻量外部 store + Query 缓存。
//
// live 数据（speed/进度/分段）不进 React Query —— 高频更新走
// useSyncExternalStore 的细粒度订阅；任务/队列列表本体在 Query 缓存
// （['tasks'] / ['queues']），由 tasksSnapshot / queuesChanged 直接 setQueryData。

import { useSyncExternalStore } from 'react'
import type { QueryClient } from '@tanstack/react-query'
import { getBase, getToken, isAuthenticated } from './auth'
import { t } from './i18n'
import type {
  BtFileEntry,
  HlsQualityOption,
  GroupDto,
  QueueDto,
  ResolveVariantOption,
  SegmentProgressMsg,
  SegmentSplitMsg,
  TaskCdnEventMsg,
  TaskDto,
  TaskProgressMsg,
  WsClientMsg,
  WsServerMsg,
} from './types'

// ---------------- 轻量外部 store ----------------

export class Store<T> {
  private listeners = new Set<() => void>()
  private state: T
  constructor(initial: T) {
    this.state = initial
  }
  get = (): T => this.state
  set = (next: T | ((prev: T) => T)) => {
    this.state = typeof next === 'function' ? (next as (prev: T) => T)(this.state) : next
    for (const l of this.listeners) l()
  }
  subscribe = (cb: () => void) => {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }
}

export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.get, store.get)
}

// ---------------- store 实例 ----------------

export type TaskLive = Omit<TaskProgressMsg, 'taskId'>

export const liveStore = new Store<Record<string, TaskLive>>({})
export const segmentStore = new Store<Record<string, SegmentProgressMsg>>({})
/** 最近一次拆分事件（详情面板播放拆分动画用），带到达时间戳。 */
export const splitStore = new Store<(SegmentSplitMsg & { at: number }) | null>(null)
export const connStore = new Store<{
  status: 'connecting' | 'connected' | 'disconnected'
  rttMs: number | null
}>({ status: 'disconnected', rttMs: null })
/** 最近一次多 CDN 节点级活动事件（任务详情日志用），带到达时间戳（同 splitStore 范式）。 */
export const cdnEventStore = new Store<(TaskCdnEventMsg & { at: number }) | null>(null)
export const priorityStore = new Store<{ priorityTaskId: string; autoPausedCount: number }>({
  priorityTaskId: '',
  autoPausedCount: 0,
})
/** 待处理的 HLS/BT 选择请求（对话框消费后置 null）。 */
export const hlsRequestStore = new Store<{ taskId: string; options: HlsQualityOption[] } | null>(null)
/** 待处理的插件 resolve 变体（画质/格式）选择请求。 */
export const resolveVariantRequestStore = new Store<{
  taskId: string
  defaultIndex: number
  options: ResolveVariantOption[]
} | null>(null)
export const btRequestStore = new Store<{ taskId: string; files: BtFileEntry[] } | null>(null)
/** 组件（ffmpeg）安装/下载进度，按 component 名索引。 */
export const componentProgressStore = new Store<
  Record<string, { downloadedBytes: number; totalBytes: number }>
>({})
/** 最近一次组件操作结果（安装/卸载完成后设置一次，供设置页展示提示）。 */
export const componentResultStore = new Store<
  { component: string; ok: boolean; message: string; at: number } | null
>(null)
/**
 * 插件 onDone 钩子活动状态：taskId → 正在处理该任务的 pluginId 集合（非空即
 * 展示“插件处理中…”指示器）。事件为 fire-and-forget，可能丢失/乱序，故仅
 * 作旁路展示，绝不驱动任务状态机。
 */
export const pluginActivityStore = new Store<Record<string, Set<string>>>({})
/** 每个 taskId 的看门狗计时器：`running=true` 到达时设/重置，到期兜底清除
 * 该任务的活动条目，防止 `running=false` 丢失导致指示器永久悬挂。 */
const pluginActivityWatchdogs = new Map<string, ReturnType<typeof setTimeout>>()
/** 看门狗超时：钩子墙钟硬顶 1830s + 30s 余量。 */
const PLUGIN_ACTIVITY_WATCHDOG_MS = 1_860_000
/** 任务完成瞬间跃迁（status→3）的旁路监听：CDN 遥测上报等后台服务用
 *  （lib/cloud/cdn.ts 注册；ws 不反向 import，避免与 session.ts 的 Store 依赖成环）。 */
export const taskCompletionListeners = new Set<() => void>()

function clearPluginActivityWatchdog(taskId: string) {
  const timer = pluginActivityWatchdogs.get(taskId)
  if (timer) clearTimeout(timer)
  pluginActivityWatchdogs.delete(taskId)
}

/** 清空全部插件活动条目 + 看门狗（WS 重连时调用：断线期间的活动状态不可信）。 */
function clearAllPluginActivity() {
  for (const timer of pluginActivityWatchdogs.values()) clearTimeout(timer)
  pluginActivityWatchdogs.clear()
  if (Object.keys(pluginActivityStore.get()).length > 0) pluginActivityStore.set({})
}

// ---------------- 连接管理 ----------------

let socket: WebSocket | null = null
let reconnectTimer: ReturnType<typeof setTimeout> | null = null
let pingTimer: ReturnType<typeof setInterval> | null = null
let pingSentAt = 0
let attempts = 0
let queryClientRef: QueryClient | null = null

export function sendWs(msg: WsClientMsg) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg))
}

export function disconnectWs() {
  if (reconnectTimer) clearTimeout(reconnectTimer)
  if (pingTimer) clearInterval(pingTimer)
  reconnectTimer = null
  pingTimer = null
  socket?.close()
  socket = null
  connStore.set({ status: 'disconnected', rttMs: null })
}

export function connectWs(queryClient: QueryClient) {
  queryClientRef = queryClient
  if (socket && socket.readyState <= WebSocket.OPEN) return
  if (!isAuthenticated()) return
  openSocket()
}

function wsUrl(): string {
  const base = getBase()
  const origin = base || location.origin
  const url = new URL(origin)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  url.pathname = '/api/v1/ws'
  url.search = `?token=${encodeURIComponent(getToken())}`
  return url.toString()
}

function openSocket() {
  // 断线期间插件活动状态不可信（事件可能已丢失），每次（重）连接前清空。
  clearAllPluginActivity()
  connStore.set((s) => ({ ...s, status: 'connecting' }))
  const ws = new WebSocket(wsUrl())
  socket = ws

  ws.onopen = () => {
    attempts = 0
    connStore.set({ status: 'connected', rttMs: null })
    if (pingTimer) clearInterval(pingTimer)
    pingTimer = setInterval(() => {
      pingSentAt = performance.now()
      sendWs({ type: 'ping' })
    }, 15_000)
    // 立即测一次 RTT
    pingSentAt = performance.now()
    sendWs({ type: 'ping' })
  }

  ws.onmessage = (e) => {
    let msg: WsServerMsg
    try {
      msg = JSON.parse(e.data as string) as WsServerMsg
    } catch {
      return
    }
    dispatch(msg)
  }

  ws.onclose = () => {
    if (socket !== ws) return
    socket = null
    if (pingTimer) clearInterval(pingTimer)
    connStore.set({ status: 'disconnected', rttMs: null })
    // 指数退避重连（1s → 2s → 4s … 上限 15s）；登出后不再重连。
    if (!isAuthenticated()) return
    const delay = Math.min(1000 * 2 ** attempts, 15_000)
    attempts += 1
    reconnectTimer = setTimeout(openSocket, delay)
  }

  ws.onerror = () => {
    ws.close()
  }
}

function dispatch(msg: WsServerMsg) {
  const qc = queryClientRef
  switch (msg.type) {
    case 'taskProgress': {
      const { taskId, ...live } = msg
      liveStore.set((prev) => ({ ...prev, [taskId]: live }))
      if (qc) {
        const tasks = qc.getQueryData<TaskDto[]>(['tasks'])
        if (!tasks || !tasks.some((t) => t.taskId === taskId)) {
          // 新任务（其他客户端/aria2 创建）→ 拉全量。
          void qc.invalidateQueries({ queryKey: ['tasks'] })
        } else {
          const prev = tasks.find((t) => t.taskId === taskId)
          qc.setQueryData<TaskDto[]>(['tasks'], (old) =>
            old?.map((t) =>
              t.taskId === taskId
                ? {
                    ...t,
                    status: live.status,
                    downloadedBytes: live.downloadedBytes,
                    totalBytes: live.totalBytes || t.totalBytes,
                    fileName: live.fileName || t.fileName,
                    errorMessage: live.errorMessage,
                  }
                : t,
            ),
          )
          // 完成瞬间跃迁：REST 缓存无 completedAt（WS 进度不含该字段），拉一次全量补齐完成时间/耗时。
          if (live.status === 3 && prev && prev.status !== 3) {
            void qc.invalidateQueries({ queryKey: ['tasks'] })
            // CDN 遥测事件驱动上报：任务完成 → 10s 去抖后上传本轮样本（对齐桌面 home_page）。
            for (const fn of taskCompletionListeners) fn()
          }
        }
      }
      break
    }
    case 'tasksSnapshot':
      queryClientRef?.setQueryData<TaskDto[]>(['tasks'], msg.tasks)
      break
    case 'segmentProgress':
      segmentStore.set((prev) => ({ ...prev, [msg.taskId]: msg }))
      break
    case 'segmentSplit':
      splitStore.set({ ...msg, at: Date.now() })
      break
    case 'taskCdnEvent':
      cdnEventStore.set({ ...msg, at: Date.now() })
      break
    case 'taskMetaProbed':
      queryClientRef?.setQueryData<TaskDto[]>(['tasks'], (old) =>
        old?.map((t) =>
          t.taskId === msg.taskId
            ? { ...t, fileName: msg.fileName || t.fileName, totalBytes: msg.totalBytes || t.totalBytes }
            : t,
        ),
      )
      break
    case 'queuesChanged':
      queryClientRef?.setQueryData<QueueDto[]>(['queues'], msg.queues)
      break
    case 'groupsChanged':
      queryClientRef?.setQueryData<GroupDto[]>(['groups'], msg.groups)
      break
    case 'taskQueueChanged':
      queryClientRef?.setQueryData<TaskDto[]>(['tasks'], (old) =>
        old?.map((t) => (t.taskId === msg.taskId ? { ...t, queueId: msg.queueId } : t)),
      )
      break
    case 'queuePositionsChanged': {
      // 回写 queueOrder，驱动队列管理对话框「任务」Tab 的顺序实时重排。
      const pos = new Map(msg.positions.map((p) => [p.taskId, p.position]))
      queryClientRef?.setQueryData<TaskDto[]>(['tasks'], (old) =>
        old?.map((t) => (pos.has(t.taskId) ? { ...t, queueOrder: pos.get(t.taskId) } : t)),
      )
      break
    }
    case 'priorityTaskChanged':
      priorityStore.set({ priorityTaskId: msg.priorityTaskId, autoPausedCount: msg.autoPausedCount })
      break
    case 'hlsSelectionRequest':
      hlsRequestStore.set({ taskId: msg.taskId, options: msg.options })
      break
    case 'resolveVariantRequest':
      resolveVariantRequestStore.set({ taskId: msg.taskId, defaultIndex: msg.defaultIndex, options: msg.options })
      break
    case 'btSelectionRequest':
      btRequestStore.set({ taskId: msg.taskId, files: msg.files })
      break
    case 'pluginsChanged':
      void queryClientRef?.invalidateQueries({ queryKey: ['plugins'] })
      break
    case 'pluginAutoDisabled': {
      const identity = msg.identity
      // 动态 import 打破与 confirm.ts 的静态循环依赖（confirm.ts 反向依赖本模块的 Store）。
      void import('./confirm').then(({ alertDialog }) =>
        alertDialog({
          title: t('plugins.autoDisabledTitle'),
          message: t('plugins.autoDisabledMsg', { name: identity }),
        }),
      )
      break
    }
    case 'pluginHookActivity': {
      const { taskId, pluginId, running } = msg
      if (running) {
        pluginActivityStore.set((prev) => {
          const next = new Set(prev[taskId])
          next.add(pluginId)
          return { ...prev, [taskId]: next }
        })
        // 每次 running=true 都重置该 taskId 的看门狗（并发多插件钩子共用一个）。
        clearPluginActivityWatchdog(taskId)
        pluginActivityWatchdogs.set(
          taskId,
          setTimeout(() => {
            pluginActivityWatchdogs.delete(taskId)
            pluginActivityStore.set((prev) => {
              if (!(taskId in prev)) return prev
              const next = { ...prev }
              delete next[taskId]
              return next
            })
          }, PLUGIN_ACTIVITY_WATCHDOG_MS),
        )
      } else {
        pluginActivityStore.set((prev) => {
          const set = prev[taskId]
          if (!set?.has(pluginId)) return prev
          const next = new Set(set)
          next.delete(pluginId)
          if (next.size === 0) {
            const rest = { ...prev }
            delete rest[taskId]
            return rest
          }
          return { ...prev, [taskId]: next }
        })
        if (!(taskId in pluginActivityStore.get())) clearPluginActivityWatchdog(taskId)
      }
      break
    }
    case 'componentProgress':
      componentProgressStore.set((prev) => ({
        ...prev,
        [msg.component]: { downloadedBytes: msg.downloadedBytes, totalBytes: msg.totalBytes },
      }))
      break
    case 'componentResult':
      componentResultStore.set({ component: msg.component, ok: msg.ok, message: msg.message, at: Date.now() })
      componentProgressStore.set((prev) => {
        const next = { ...prev }
        delete next[msg.component]
        return next
      })
      void queryClientRef?.invalidateQueries({
        queryKey: msg.component === 'ytdlp' ? ['ytdlpStatus'] : ['ffmpegStatus'],
      })
      break
    case 'pong':
      connStore.set({ status: 'connected', rttMs: Math.round(performance.now() - pingSentAt) })
      break
  }
}

// ---------------- 派生 hooks ----------------

/** 全局下载速度（所有 downloading 任务 live speed 之和）。 */
export function useGlobalSpeed(): number {
  const live = useStore(liveStore)
  let sum = 0
  for (const v of Object.values(live)) if (v.status === 1) sum += v.speed
  return sum
}

/** 该任务是否有插件 onDone 钩子正在处理（(taskId, pluginId) 集合非空）。 */
export function useTaskPluginActivity(taskId: string): boolean {
  const activity = useStore(pluginActivityStore)
  return (activity[taskId]?.size ?? 0) > 0
}
