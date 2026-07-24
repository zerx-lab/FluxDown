// FluxDown 本地互联（link）管理面 REST 客户端 —— 局域网直连配对/发现/下发任务，与云中转
// （lib/cloud/*）完全独立、互不影响。契约见 native/api/src/routes.rs 新增 API_LINK_* 常量 +
// web/link_mgmt_contract.md；复用 lib/api.ts 的 apiFetch，走同一鉴权/错误处理管线（401 清
// 凭证跳登录、非 2xx 抛 ApiError，message 经 translateBackendMessage 本地化）。
//
// 后端未启用/不支持设备互联（ApiHost 默认实现，host 为 None 时）统一返回固定英文 message
// "device link not supported by this host"（见 native/api/src/service.rs link_unsupported()），
// 未收录进 i18n.ts 的 BACKEND_KEYS 映射表，因此 translateBackendMessage 会原样透传——
// isLinkUnsupportedError() 据此精确识别该场景，UI 展示「宿主不支持/未启用」的专用提示，
// 而不是把它当成普通错误文案。

import { apiFetch } from './api'
import type { I18nKey } from './i18n'

// ---------------------------------------------------------------------------
// DTO —— 与契约 camelCase 字段一一对应
// ---------------------------------------------------------------------------

/** 发现的对端设备快照（mDNS 广播命中或手动 probe 命中），对应引擎 DiscoveredPeer。 */
export interface LinkDiscoveredPeerDto {
  fingerprint?: string
  name: string
  platform?: string
  host: string
  port: number
  appVersion?: string
  source: 'mdns' | 'manual'
}

/** 已配对设备（PeerRecord 精简视图，严禁透出 link_secret/identity_pub）。 */
export interface LinkDeviceDto {
  fingerprint: string
  name: string
  platform?: string
  online: boolean
  pairedAt: number
  lastSeenAt: number
}

/** POST /api/v1/link/code 响应（本机出示一次性配对码）。 */
export interface LinkCodeResponse {
  code: string
  ttlSeconds: number
}

/** POST /api/v1/link/pair/begin 响应：token 用于 finish，sas 供人工核对。 */
export interface LinkPairBeginResponse {
  token: string
  sas: string
  peerName: string
  peerFingerprint: string
}

/** POST /api/v1/link/pair/finish 响应：accept=false 或对端拒绝时 paired=false，device 省略。 */
export interface LinkPairFinishResponse {
  paired: boolean
  device?: LinkDeviceDto
}

export interface LinkDispatchTaskRequest {
  url: string
  saveDir?: string
  fileName?: string
}

// ---------------------------------------------------------------------------
// 宿主不支持设备互联 —— 统一识别 + 友好文案
// ---------------------------------------------------------------------------

/** ApiHost 默认方法（host 未接入 LinkManager 时）统一返回的英文 message，稳定契约字符串。 */
const LINK_UNSUPPORTED_MESSAGE = 'device link not supported by this host'

export function isLinkUnsupportedError(err: unknown): boolean {
  return err instanceof Error && err.message === LINK_UNSUPPORTED_MESSAGE
}

/** 把任意 link.* 请求错误转成展示文案：宿主不支持时给专用提示，否则原样展示服务端 message
 *  （配对码错误/超时/不可达等场景服务端 message 已经是可读英文短句，本地化映射表未收录时
 *  原样透传，见本文件头注）。 */
export function friendlyLinkError(t: (key: I18nKey, params?: Record<string, string | number>) => string, err: unknown): string {
  if (isLinkUnsupportedError(err)) return t('link.unsupportedHost')
  return err instanceof Error ? err.message : String(err)
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

export const linkApi = {
  /** POST /api/v1/link/discovery：开始/停止 mDNS 发现。start 幂等，且会清空发现快照。 */
  discovery: (action: 'start' | 'stop') =>
    apiFetch<{ ok: boolean }>('/api/v1/link/discovery', { method: 'POST', body: JSON.stringify({ action }) }),

  /** GET /api/v1/link/discovered：当前发现快照，配合 2s 轮询使用。 */
  discovered: () => apiFetch<{ peers: LinkDiscoveredPeerDto[] }>('/api/v1/link/discovered'),

  /** POST /api/v1/link/probe：按 host:port 手动探测一台设备（结果不进入发现快照）。 */
  probe: (host: string, port: number) =>
    apiFetch<LinkDiscoveredPeerDto>('/api/v1/link/probe', { method: 'POST', body: JSON.stringify({ host, port }) }),

  /** POST /api/v1/link/pair/begin：出示对端配对码换取 token + SAS（供双方人工核对）。 */
  pairBegin: (host: string, port: number, code: string) =>
    apiFetch<LinkPairBeginResponse>('/api/v1/link/pair/begin', {
      method: 'POST',
      body: JSON.stringify({ host, port, code }),
    }),

  /** POST /api/v1/link/pair/finish：SAS 核对后确认（accept=true）或拒绝配对。 */
  pairFinish: (token: string, accept: boolean) =>
    apiFetch<LinkPairFinishResponse>('/api/v1/link/pair/finish', {
      method: 'POST',
      body: JSON.stringify({ token, accept }),
    }),

  /** GET /api/v1/link/devices：已配对设备（online 为服务端并发探测结果）。 */
  devices: () => apiFetch<{ devices: LinkDeviceDto[] }>('/api/v1/link/devices'),

  /** DELETE /api/v1/link/devices/{fingerprint}：解除配对。 */
  removeDevice: (fingerprint: string) =>
    apiFetch<{ ok: boolean }>(`/api/v1/link/devices/${encodeURIComponent(fingerprint)}`, { method: 'DELETE' }),

  /** POST /api/v1/link/devices/{fingerprint}/tasks：向已配对设备下发下载任务。 */
  dispatchTask: (fingerprint: string, req: LinkDispatchTaskRequest) =>
    apiFetch<{ taskId: string }>(`/api/v1/link/devices/${encodeURIComponent(fingerprint)}/tasks`, {
      method: 'POST',
      body: JSON.stringify(req),
    }),

  /** POST /api/v1/link/code：生成一次性配对码（本机作为"被配对"一方出示）。 */
  generateCode: () => apiFetch<LinkCodeResponse>('/api/v1/link/code', { method: 'POST' }),
}
