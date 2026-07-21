// 跨设备任务的实时视图 —— 已登录且有订阅者时开 SSE 长连接 `/api/v1/tasks/events`，
// 增量应用 task.dispatch/task.status（RemoteTaskDto 平铺）/task.progress（批量 items[]）/
// presence 事件；断线指数退避重连，重连即 `GET /tasks/remote` 拉一次全量补齐（持久态 +
// 内存进度快照，见 mdc §1.5）。与 lib/ws.ts 的本地任务 WebSocket 完全独立（云端跨设备
// 视图 vs 本地下载引擎）；复用同一套「轻量外部 store」（Store/useStore）。
//
// 引用计数：多个组件可同时调用 useRemoteTasks()，只在第一个订阅者挂载时建连，最后一个
// 卸载时断开，避免未使用页面常驻长连接；登出（cloudSessionStore 转 unauthenticated）时
// 组件重渲染触发 effect 清理，连接与缓存一并清空。

import { useEffect } from 'react'
import { Store, useStore } from '../ws'
import { cloudApi, getCloudBaseUrl } from './client'
import { cloudDeviceId, getCloudAccessToken, useCloudSession } from './session'
import type { RemoteTask } from './types'

interface RemoteProgressItem {
  taskId: string
  downloadedBytes: number
  speed: number
  progress: number
}

interface RemoteTasksState {
  tasks: Map<string, RemoteTask>
  onlineDeviceIds: Set<string>
}

const remoteTasksStore = new Store<RemoteTasksState>({ tasks: new Map(), onlineDeviceIds: new Set() })

let source: EventSource | null = null
let reconnectTimer: ReturnType<typeof setTimeout> | null = null
let attempts = 0
let refCount = 0

/** task.dispatch / task.status 事件：payload 是平铺的 RemoteTaskDto，直接整条替换。 */
function applyTaskEvent(data: Record<string, unknown>) {
  const task = data as unknown as RemoteTask
  if (!task.id) return
  remoteTasksStore.set((prev) => {
    const tasks = new Map(prev.tasks)
    tasks.set(task.id, task)
    return { ...prev, tasks }
  })
}

/** task.progress 事件：`{items:[{taskId,downloadedBytes,speed,progress}]}` 批量，仅更新已知任务。 */
function applyProgressEvent(items: RemoteProgressItem[]) {
  if (!items?.length) return
  remoteTasksStore.set((prev) => {
    const tasks = new Map(prev.tasks)
    for (const item of items) {
      const cur = tasks.get(item.taskId)
      if (!cur) continue
      tasks.set(item.taskId, { ...cur, downloadedBytes: item.downloadedBytes, speed: item.speed, progress: item.progress })
    }
    return { ...prev, tasks }
  })
}

/** presence 事件：`{deviceId,online}`。 */
function applyPresenceEvent(deviceId: string, online: boolean) {
  if (!deviceId) return
  remoteTasksStore.set((prev) => {
    const onlineDeviceIds = new Set(prev.onlineDeviceIds)
    if (online) onlineDeviceIds.add(deviceId)
    else onlineDeviceIds.delete(deviceId)
    return { ...prev, onlineDeviceIds }
  })
}

/** 全量拉取（首次连接 + 每次重连）：与内存快照合并（覆盖同 id 条目，不清空未提及的）。 */
async function seed() {
  try {
    const { tasks } = await cloudApi.remoteTasks()
    remoteTasksStore.set((prev) => {
      const next = new Map(prev.tasks)
      for (const task of tasks) next.set(task.id, task)
      return { ...prev, tasks: next }
    })
  } catch (err) {
    console.warn('[remoteTasks] seed failed', err)
  }
}

function scheduleReconnect() {
  if (reconnectTimer || refCount <= 0) return
  attempts += 1
  const delay = Math.min(30_000, 1_000 * 2 ** Math.min(attempts, 5))
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null
    if (refCount > 0) connect()
  }, delay)
}

function connect() {
  if (source) return
  const token = getCloudAccessToken()
  if (!token) return
  void seed()
  const url = `${getCloudBaseUrl()}/api/v1/tasks/events?access_token=${encodeURIComponent(token)}&deviceId=${encodeURIComponent(cloudDeviceId())}`
  const es = new EventSource(url)
  source = es
  es.onopen = () => {
    attempts = 0
  }
  es.onmessage = (ev) => {
    let data: Record<string, unknown>
    try {
      data = JSON.parse(ev.data)
    } catch {
      return
    }
    switch (data.type) {
      case 'task.dispatch':
      case 'task.status':
        applyTaskEvent(data)
        break
      case 'task.progress':
        applyProgressEvent((data.items as RemoteProgressItem[] | undefined) ?? [])
        break
      case 'presence':
        applyPresenceEvent(data.deviceId as string, !!data.online)
        break
      default:
        break
    }
  }
  es.onerror = () => {
    es.close()
    if (source === es) source = null
    scheduleReconnect()
  }
}

function disconnect() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer)
    reconnectTimer = null
  }
  source?.close()
  source = null
  attempts = 0
  remoteTasksStore.set({ tasks: new Map(), onlineDeviceIds: new Set() })
}

/** 已登录时订阅跨设备任务实时视图；未登录返回空集合（不建连）。 */
export function useRemoteTasks(): { remoteTasks: RemoteTask[]; onlineDeviceIds: Set<string> } {
  const session = useCloudSession()
  const state = useStore(remoteTasksStore)

  useEffect(() => {
    if (session.status !== 'authenticated') return
    refCount += 1
    connect()
    return () => {
      refCount -= 1
      if (refCount <= 0) disconnect()
    }
  }, [session.status])

  return { remoteTasks: Array.from(state.tasks.values()), onlineDeviceIds: state.onlineDeviceIds }
}
