// FluxCloud 云账户 REST 客户端 —— 独立于本地下载器的 lib/api.ts，走 FluxCloud
// server 契约 v1（见 FluxCloud/server）。401（需鉴权接口）自动用 refreshToken 刷新
// 一次并重放原请求，刷新也失败则清空会话并把原 401 抛出去；并发请求触发的刷新
// 去重为单个 in-flight promise，避免刷新风暴。

import { applyCloudSession, clearCloudSession, cloudDefaultDeviceName, cloudDeviceId, CLOUD_DEVICE_PLATFORM, getCloudAccessToken, getCloudRefreshToken } from './session'
import type { AuthResponse, CdnConfig, CdnConfigResult, CloudDevice, CloudProfile, DevicesResponse, LoginResult, RemoteTask, RemoteTasksResponse, TtlResponse } from './types'
import { CloudApiError } from './types'

/** 默认服务地址：Actions 打包时经 VITE_FLUXCLOUD_BASE_URL 构建期注入官方地址，
 *  未注入（本地开发）回退本地联调端口，与桌面端 FLUXCLOUD_BASE_URL dart-define 对称。 */
const DEFAULT_BASE_URL: string = import.meta.env.VITE_FLUXCLOUD_BASE_URL?.trim() || 'http://127.0.0.1:8720'
const BASE_KEY = 'fluxdown.cloud.base'
const API_PREFIX = '/api/v1'

/** 当前生效的云服务地址：仅开发构建允许 localStorage 自定义覆盖（对应设置项也只在
 *  开发构建显示），生产构建锁定构建期注入的默认常量——与桌面端 CloudApiConfig
 *  的 kDebugMode 门控对称，避免残留覆盖值指向失效地址。 */
export function getCloudBaseUrl(): string {
  if (!import.meta.env.DEV) return DEFAULT_BASE_URL
  const custom = localStorage.getItem(BASE_KEY)
  return custom?.trim() ? custom.trim() : DEFAULT_BASE_URL
}

/** 是否为用户自定义地址（非默认值），供设置页展示"恢复默认"按钮状态。 */
export function isCloudBaseUrlCustom(): boolean {
  if (!import.meta.env.DEV) return false
  const custom = localStorage.getItem(BASE_KEY)
  return !!custom?.trim() && custom.trim() !== DEFAULT_BASE_URL
}

/** 云服务地址是否允许编辑（仅开发构建），供设置页决定是否渲染地址编辑行。 */
export const CLOUD_BASE_URL_EDITABLE: boolean = import.meta.env.DEV

export function setCloudBaseUrl(url: string) {
  localStorage.setItem(BASE_KEY, url.trim())
}

export function resetCloudBaseUrl() {
  localStorage.removeItem(BASE_KEY)
}

/** 请求中的设备三元组（所有发令牌的接口都带，见契约）。不上报 appVersion（v1.1
 *  新增可选字段）：面板是独立 web 服务，package.json 里的 "0.0.0" 只是占位符，
 *  没有真实语义化版本可报，误报一个假版本号不如干脆留空，服务端按可空字段兜底。 */
function deviceTriple() {
  const deviceName = cloudDefaultDeviceName()
  return {
    deviceId: cloudDeviceId(),
    ...(deviceName ? { deviceName } : {}),
    devicePlatform: CLOUD_DEVICE_PLATFORM,
  }
}

async function rawRequest<T>(method: string, path: string, body?: unknown, authed = false): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' }
  if (body !== undefined) headers['Content-Type'] = 'application/json'
  if (authed) headers.Authorization = `Bearer ${getCloudAccessToken()}`
  const res = await fetch(`${getCloudBaseUrl()}${API_PREFIX}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })
  const text = await res.text()
  let json: unknown = {}
  if (text.trim()) {
    try {
      json = JSON.parse(text)
    } catch {
      /* 非 JSON 响应体，按空对象处理 */
    }
  }
  if (!res.ok) {
    const err = json as { code?: string; message?: string }
    throw new CloudApiError(err.code ?? 'unknown_error', err.message ?? res.statusText, res.status)
  }
  return json as T
}

let refreshing: Promise<void> | null = null

/** 401 时用 refreshToken 刷新一次；并发请求共享同一个 in-flight promise。刷新失败清空会话。 */
function refreshSession(): Promise<void> {
  if (!refreshing) {
    refreshing = (async () => {
      const rt = getCloudRefreshToken()
      if (!rt) {
        clearCloudSession()
        throw new CloudApiError('unauthorized', 'no refresh token', 401)
      }
      try {
        const auth = await rawRequest<AuthResponse>('POST', '/auth/refresh', { refreshToken: rt })
        applyCloudSession(auth)
      } catch (e) {
        clearCloudSession()
        throw e
      }
    })().finally(() => {
      refreshing = null
    })
  }
  return refreshing
}

/** 需要 Bearer 认证的调用：命中 401 时刷新一次并重放，仍失败则把原错误抛出去。 */
async function authedRequest<T>(method: string, path: string, body?: unknown): Promise<T> {
  try {
    return await rawRequest<T>(method, path, body, true)
  } catch (e) {
    if (!(e instanceof CloudApiError) || e.status !== 401) throw e
    await refreshSession()
    return await rawRequest<T>(method, path, body, true)
  }
}
/** GET /cdn/config 专用请求：需要发送 If-None-Match、读取响应 ETag，且 304 是正常
 *  「未变更」结果而非错误，与 rawRequest 的错误语义不同（对齐桌面端 fetchCdnConfig）。
 *  命中 401 时刷新一次并重放。 */
async function fetchCdnConfigOnce(ifNoneMatch?: string | null): Promise<CdnConfigResult> {
  const headers: Record<string, string> = {
    Accept: 'application/json',
    Authorization: `Bearer ${getCloudAccessToken()}`,
  }
  if (ifNoneMatch) headers['If-None-Match'] = ifNoneMatch
  const res = await fetch(`${getCloudBaseUrl()}${API_PREFIX}/cdn/config`, { headers })
  if (res.status === 304) return { notModified: true, etag: null, config: null }
  const text = await res.text()
  if (res.ok) {
    let config: CdnConfig | null = null
    try {
      config = text.trim() ? (JSON.parse(text) as CdnConfig) : null
    } catch {
      /* 非 JSON 响应体按无配置处理 */
    }
    return { notModified: false, etag: res.headers.get('ETag'), config }
  }
  let code = 'unknown_error'
  let message = res.statusText
  try {
    const err = JSON.parse(text) as { code?: string; message?: string }
    code = err.code ?? code
    message = err.message ?? message
  } catch {
    /* 错误体不是合法 JSON：保留默认 code/message */
  }
  throw new CloudApiError(code, message, res.status)
}

export const cloudApi = {
  /** POST /auth/register：发码建 pending 用户（或为未完成注册的邮箱重发验证码）。 */
  register: (email: string, password: string, nickname?: string) =>
    rawRequest<TtlResponse>('POST', '/auth/register', {
      email,
      password,
      ...(nickname?.trim() ? { nickname: nickname.trim() } : {}),
    }),

  /** POST /auth/register/verify：验证码激活 pending 用户 + 信任当前设备 + 签发令牌。 */
  registerVerify: (email: string, code: string) =>
    rawRequest<AuthResponse>('POST', '/auth/register/verify', { email, code, ...deviceTriple() }),

  /** POST /auth/login：tagged 响应，设备已受信任直接下发令牌，新设备返回 deviceVerificationRequired。
   *  account（v1.2）：邮箱或纯数字 Origin ID，服务端按格式分流查询。 */
  login: (account: string, password: string) =>
    rawRequest<LoginResult>('POST', '/auth/login', { account, password, ...deviceTriple() }),

  /** POST /auth/login/verify：新设备验证码登录，重新校验密码 + 消费验证码 + 信任设备（account 语义同 login）。 */
  loginVerify: (account: string, password: string, code: string) =>
    rawRequest<AuthResponse>('POST', '/auth/login/verify', { account, password, code, ...deviceTriple() }),

  /** POST /auth/code/send：验证码登录用的验证码。 */
  codeSend: (email: string) => rawRequest<TtlResponse>('POST', '/auth/code/send', { email }),

  /** POST /auth/code/verify：验证码登录（邮箱不存在则自动注册，此时采用 nickname；已存在
   *  用户忽略该字段），信任当前设备。 */
  codeVerify: (email: string, code: string, nickname?: string) =>
    rawRequest<AuthResponse>('POST', '/auth/code/verify', {
      email,
      code,
      ...deviceTriple(),
      ...(nickname?.trim() ? { nickname: nickname.trim() } : {}),
    }),

  /** POST /auth/refresh：刷新令牌轮换。 */
  refresh: (refreshToken: string) => rawRequest<AuthResponse>('POST', '/auth/refresh', { refreshToken }),

  /** POST /auth/logout。 */
  logout: (refreshToken: string) => rawRequest<unknown>('POST', '/auth/logout', { refreshToken }),

  /** GET /me：当前用户信息 + 套餐能力快照。 */
  me: () => authedRequest<CloudProfile>('GET', '/me'),

  /** GET /devices：当前用户名下已信任设备，按 lastSeenAt 降序。 */
  devices: () => authedRequest<DevicesResponse>('GET', '/devices'),

  /** PATCH /devices/{id} {name}：设备改名。 */
  renameDevice: (id: string, name: string) => authedRequest<CloudDevice>('PATCH', `/devices/${id}`, { name }),

  /** DELETE /devices/{id}：删除设备 + 吊销其名下全部未撤销 refresh token。 */
  deleteDevice: (id: string) => authedRequest<unknown>('DELETE', `/devices/${id}`),

  /** POST /tasks/dispatch：向指定设备下发跨设备下载任务（云中转，见 mdc §1.4）；
   *  deviceId 为发起端自身标识，供服务端记录 fromDevice。 */
  dispatchTask: (req: { toDevice: string; url: string; saveDir?: string; fileName?: string }) =>
    authedRequest<RemoteTask>('POST', '/tasks/dispatch', { ...req, deviceId: cloudDeviceId() }),

  /** GET /tasks/remote：拉取当前账号下全部跨设备任务全量（持久态 join 内存进度快照，
   *  首次加载/SSE 断线重连用，见 mdc §1.4）。 */
  remoteTasks: () => authedRequest<RemoteTasksResponse>('GET', '/tasks/remote'),

  /** GET /cdn/config：ETag 条件请求（P1 §四契约），304 返回 notModified。
   *  命中 401 时刷新一次并重放（fetchCdnConfigOnce 不走 authedRequest 因返回形态不同）。 */
  cdnConfig: async (ifNoneMatch?: string | null): Promise<CdnConfigResult> => {
    try {
      return await fetchCdnConfigOnce(ifNoneMatch)
    } catch (e) {
      if (!(e instanceof CloudApiError) || e.status !== 401) throw e
      await refreshSession()
      return await fetchCdnConfigOnce(ifNoneMatch)
    }
  },

  /** POST /cdn/report：上报一批 CDN 众包遥测样本（≤64 条/次，超量由调用方分批；
   *  device_hash 由服务端从鉴权设备派生，本端不发送）。成功 204。 */
  cdnReport: (samples: unknown[]) => authedRequest<unknown>('POST', '/cdn/report', { samples }),
}
