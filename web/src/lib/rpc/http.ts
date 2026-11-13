// `/api/web/*` 文件面：blob 上传、已完成文件下载、日志导出。
// 鉴权 = `Authorization: Bearer <访问密钥>`（fetch/XHR）或 `?token=`（浏览器原生下载）。

import { getToken } from '../access/credentials'

export type BlobKind = 'torrents' | 'plugins'

/** 上传失败类别：网络错误 / 调用方中止 / 服务端 HTTP 应答；`message` 仅作诊断，展示走 `describeUploadError`。 */
export type UploadErrorKind = 'network' | 'aborted' | 'http'

export class UploadError extends Error {
  readonly status: number
  readonly kind: UploadErrorKind
  constructor(status: number, message: string, kind: UploadErrorKind) {
    super(message)
    this.name = 'UploadError'
    this.status = status
    this.kind = kind
  }
}

export interface UploadOptions {
  /** 上传进度 0..1。 */
  onProgress?: (fraction: number) => void
  signal?: AbortSignal
}

/**
 * 上传原始字节到 agent（转发 daemon blob 存储），返回 `blobId`；
 * 再用 `daemon.task.create { torrentBlobId }` / `daemon.plugin.install { blobId }` 引用。
 * 使用 XHR 以获得上传进度。
 */
export function uploadBlob(kind: BlobKind, data: Blob, options: UploadOptions = {}): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', `/api/web/blobs/${kind}`)
    xhr.setRequestHeader('Authorization', `Bearer ${getToken()}`)
    xhr.setRequestHeader('Content-Type', 'application/octet-stream')
    xhr.responseType = 'json'
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) options.onProgress?.(event.loaded / event.total)
    }
    xhr.onerror = () => reject(new UploadError(0, 'network error', 'network'))
    xhr.onabort = () => reject(new UploadError(0, 'aborted', 'aborted'))
    xhr.onload = () => {
      const body = xhr.response as { blobId?: unknown; error?: unknown } | null
      if (xhr.status >= 200 && xhr.status < 300 && typeof body?.blobId === 'string') {
        resolve(body.blobId)
        return
      }
      const message = typeof body?.error === 'string' ? body.error : `HTTP ${xhr.status}`
      reject(new UploadError(xhr.status, message, 'http'))
    }
    options.signal?.addEventListener('abort', () => xhr.abort(), { once: true })
    xhr.send(data)
  })
}

/** 已完成任务文件的下载地址（`?token=` 鉴权，可直接作 `<a href>`）。 */
export function taskFileUrl(taskId: string): string {
  return `/api/web/files/tasks/${encodeURIComponent(taskId)}?token=${encodeURIComponent(getToken())}`
}

/** 日志导出 zip 的下载地址（`exportId` 来自 `daemon.diagnostics.prepareLogExport`）。 */
export function logExportUrl(exportId: string): string {
  return `/api/web/exports/${encodeURIComponent(exportId)}?token=${encodeURIComponent(getToken())}`
}

/** 触发浏览器原生下载（附件响应；不离开当前页）。 */
export function triggerDownload(url: string): void {
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.rel = 'noopener'
  anchor.download = ''
  // 用户脚本据此放行，避免从局域网 IP 访问时下载被劫持回 FluxDown 自身。
  anchor.setAttribute('data-fluxdown-skip', '1')
  document.body.append(anchor)
  anchor.click()
  anchor.remove()
}

export function downloadTaskFile(taskId: string): void {
  triggerDownload(taskFileUrl(taskId))
}

export function downloadLogExport(exportId: string): void {
  triggerDownload(logExportUrl(exportId))
}

/** 日志 zip（agent + daemon 日志）的下载地址。 */
export function logsExportUrl(): string {
  return `/api/web/logs/export?token=${encodeURIComponent(getToken())}`
}

/** 触发浏览器下载 agent + daemon 日志 zip（`/api/web/logs/export`）。 */
export async function exportLogs(): Promise<void> {
  triggerDownload(logsExportUrl())
}

