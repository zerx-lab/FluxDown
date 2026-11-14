import type { EditTaskParams, TaskEditSnapshot } from '../../../lib/rpc'

export interface TaskEditValues {
  saveDir: string; fileName: string; proxyUrl: string; segments: number; downloadLimitKib: number
  userAgent: string; headers: Record<string, string>; cookies: string; cookiesEdited: boolean
  authRef: string; category: string; tags: string[]; scheduledAt: number
}

export function taskEditValues(snapshot: TaskEditSnapshot): TaskEditValues {
  return {
    saveDir: snapshot.saveDir, fileName: snapshot.fileName, proxyUrl: snapshot.proxyUrl,
    segments: snapshot.segments, downloadLimitKib: snapshot.properties.downloadLimitBps > 0 ? Math.ceil(snapshot.properties.downloadLimitBps / 1024) : 0,
    userAgent: snapshot.properties.userAgent, headers: { ...snapshot.headers }, cookies: '', cookiesEdited: false,
    authRef: snapshot.properties.authRef, category: snapshot.properties.category,
    tags: [...snapshot.properties.tags], scheduledAt: snapshot.properties.scheduledAt,
  }
}

export function taskEditPatch(snapshot: TaskEditSnapshot, values: TaskEditValues): EditTaskParams {
  const patch: EditTaskParams = { taskId: snapshot.taskId, expectedRevision: snapshot.revision }
  if (snapshot.capabilities.storage && values.saveDir !== snapshot.saveDir) patch.saveDir = values.saveDir
  if (snapshot.capabilities.rename && values.fileName !== snapshot.fileName) patch.fileName = values.fileName
  if (snapshot.capabilities.proxy && values.proxyUrl !== snapshot.proxyUrl) patch.proxyUrl = values.proxyUrl
  if (snapshot.capabilities.segments && values.segments !== snapshot.segments) patch.segments = values.segments
  const bps = values.downloadLimitKib * 1024
  if (Number.isSafeInteger(bps) && values.downloadLimitKib !== Math.ceil(Math.max(0, snapshot.properties.downloadLimitBps) / 1024)) patch.downloadLimitBps = bps
  if (snapshot.capabilities.request && values.userAgent !== snapshot.properties.userAgent) patch.userAgent = values.userAgent
  const headerChanged = JSON.stringify(Object.entries(values.headers).sort()) !== JSON.stringify(Object.entries(snapshot.headers).sort())
  if (snapshot.capabilities.request && headerChanged) patch.headers = values.headers
  if (snapshot.capabilities.request && values.cookiesEdited) patch.cookies = values.cookies
  if (snapshot.capabilities.request && values.authRef !== snapshot.properties.authRef) patch.authRef = values.authRef
  if (values.category !== snapshot.properties.category) patch.category = values.category
  if (JSON.stringify(values.tags) !== JSON.stringify(snapshot.properties.tags)) patch.tags = values.tags
  if (snapshot.capabilities.schedule && values.scheduledAt !== snapshot.properties.scheduledAt) patch.scheduledAt = values.scheduledAt
  return patch
}

export function hasTaskEditChanges(patch: EditTaskParams): boolean {
  return Object.keys(patch).some((key) => key !== 'taskId' && key !== 'expectedRevision')
}
