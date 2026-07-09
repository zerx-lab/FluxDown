// 类型化 REST 客户端。401 → 清凭证跳登录。

import { clearCredentials, getBase, getToken } from './auth'
import { t, translateBackendMessage } from './i18n'
import type {
  ApiInfo,
  ConfigMap,
  CreateQueueRequest,
  CreateTaskRequest,
  CreatedTask,
  FsListResponse,
  ProxyTestRequest,
  ProxyTestResponse,
  QueueDto,
  StatsResponse,
  TaskDto,
  TokenResponse,
} from './types'

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${getBase()}${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${getToken()}`,
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
      ...init?.headers,
    },
  })
  if (res.status === 401) {
    clearCredentials()
    if (!location.pathname.startsWith('/login')) location.href = '/login'
    throw new ApiError(401, 'unauthorized')
  }
  if (!res.ok) {
    let message = res.statusText
    try {
      const body = (await res.json()) as { message?: string }
      if (body.message) message = body.message
    } catch {
      /* 非 JSON 错误体，用 statusText */
    }
    throw new ApiError(res.status, translateBackendMessage(message))
  }
  return (await res.json()) as T
}

export const api = {
  // 登录校验（带指定凭证探测，不写存储）
  probe: async (base: string, token: string): Promise<ApiInfo> => {
    const res = await fetch(`${base}/api/v1/info`, {
      headers: { Authorization: `Bearer ${token}` },
    })
    if (!res.ok) throw new ApiError(res.status, res.status === 401 ? t('login.invalidToken') : res.statusText)
    return (await res.json()) as ApiInfo
  },

  info: () => apiFetch<ApiInfo>('/api/v1/info'),
  listTasks: () => apiFetch<TaskDto[]>('/api/v1/tasks'),
  getTask: (id: string) => apiFetch<TaskDto>(`/api/v1/tasks/${id}`),
  createTask: (req: CreateTaskRequest) =>
    apiFetch<CreatedTask>('/api/v1/tasks', { method: 'POST', body: JSON.stringify(req) }),
  deleteTask: (id: string, deleteFiles: boolean) =>
    apiFetch<unknown>(`/api/v1/tasks/${id}?deleteFiles=${deleteFiles}`, { method: 'DELETE' }),
  pauseTask: (id: string) => apiFetch<unknown>(`/api/v1/tasks/${id}/pause`, { method: 'PUT' }),
  continueTask: (id: string) =>
    apiFetch<unknown>(`/api/v1/tasks/${id}/continue`, { method: 'PUT' }),
  pauseAll: () => apiFetch<unknown>('/api/v1/tasks/pause', { method: 'PUT' }),
  continueAll: () => apiFetch<unknown>('/api/v1/tasks/continue', { method: 'PUT' }),
  boostTask: (id: string) => apiFetch<unknown>(`/api/v1/tasks/${id}/boost`, { method: 'PUT' }),
  moveTaskToQueue: (id: string, queueId: string) =>
    apiFetch<unknown>(`/api/v1/tasks/${id}/queue`, {
      method: 'PUT',
      body: JSON.stringify({ queueId }),
    }),

  listQueues: () => apiFetch<QueueDto[]>('/api/v1/queues'),
  createQueue: (req: CreateQueueRequest) =>
    apiFetch<unknown>('/api/v1/queues', { method: 'POST', body: JSON.stringify(req) }),
  updateQueue: (id: string, req: CreateQueueRequest) =>
    apiFetch<unknown>(`/api/v1/queues/${id}`, { method: 'PUT', body: JSON.stringify(req) }),
  deleteQueue: (id: string) => apiFetch<unknown>(`/api/v1/queues/${id}`, { method: 'DELETE' }),

  getConfig: () => apiFetch<ConfigMap>('/api/v1/config'),
  putConfig: (entries: ConfigMap) =>
    apiFetch<unknown>('/api/v1/config', { method: 'PUT', body: JSON.stringify(entries) }),

  fsList: (path: string) =>
    apiFetch<FsListResponse>(`/api/v1/fs/list?path=${encodeURIComponent(path)}`),
  proxyTest: (req: ProxyTestRequest) =>
    apiFetch<ProxyTestResponse>('/api/v1/proxy/test', { method: 'POST', body: JSON.stringify(req) }),
  regenerateToken: () =>
    apiFetch<TokenResponse>('/api/v1/token/regenerate', { method: 'POST' }),
  stats: () => apiFetch<StatsResponse>('/api/v1/stats'),
}

/** 「保存到本地」下载地址（浏览器导航下载，token 走查询参数）。 */
export function taskFileUrl(taskId: string): string {
  return `${getBase()}/api/v1/tasks/${taskId}/file?token=${encodeURIComponent(getToken())}`
}
