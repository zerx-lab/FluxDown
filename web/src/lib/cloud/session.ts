// 云账户会话 —— accessToken/refreshToken/用户快照 + 设备身份，localStorage 持久化。
// 与本地下载器登录态（lib/auth.ts）完全独立：云账户是面板的可选增值功能，面板本身
// 作为一台设备接入 FluxCloud（deviceId 常驻本地，devicePlatform 固定 "web"），
// 与宿主 App 的账户状态无关。
//
// 订阅：cloudSessionStore 是轻量外部 store（复用 lib/ws.ts 的 Store），UI 用
// useCloudSession() 订阅登录态变化；网络层（client.ts）在令牌过期/登出时直接
// 调用本文件的 clearCloudSession() 等写操作。

import { Store, useStore } from '../ws'
import type { AuthResponse, CloudUser } from './types'

const ACCESS_TOKEN_KEY = 'fluxdown.cloud.accessToken'
const REFRESH_TOKEN_KEY = 'fluxdown.cloud.refreshToken'
const USER_KEY = 'fluxdown.cloud.user'
const DEVICE_ID_KEY = 'fluxdown.cloud.deviceId'

export interface CloudSessionState {
  status: 'authenticated' | 'unauthenticated'
  user: CloudUser | null
}

function restore(): CloudSessionState {
  const at = localStorage.getItem(ACCESS_TOKEN_KEY)
  const rt = localStorage.getItem(REFRESH_TOKEN_KEY)
  const userRaw = localStorage.getItem(USER_KEY)
  if (!at || !rt || !userRaw) return { status: 'unauthenticated', user: null }
  try {
    return { status: 'authenticated', user: JSON.parse(userRaw) as CloudUser }
  } catch {
    return { status: 'unauthenticated', user: null }
  }
}

export const cloudSessionStore = new Store<CloudSessionState>(restore())

/** 订阅云账户登录态；已登录时 user 非空。 */
export function useCloudSession(): CloudSessionState {
  return useStore(cloudSessionStore)
}

export function getCloudAccessToken(): string {
  return localStorage.getItem(ACCESS_TOKEN_KEY) ?? ''
}

export function getCloudRefreshToken(): string {
  return localStorage.getItem(REFRESH_TOKEN_KEY) ?? ''
}

export function isCloudLoggedIn(): boolean {
  return getCloudAccessToken() !== ''
}

/** 登录/注册/验证码验证/刷新 成功后落盘会话（令牌 + 用户快照）并通知订阅者。 */
export function applyCloudSession(auth: AuthResponse) {
  localStorage.setItem(ACCESS_TOKEN_KEY, auth.accessToken)
  localStorage.setItem(REFRESH_TOKEN_KEY, auth.refreshToken)
  localStorage.setItem(USER_KEY, JSON.stringify(auth.user))
  cloudSessionStore.set({ status: 'authenticated', user: auth.user })
}

/** 清空云账户会话（退出登录 / 刷新失败 / 删除当前设备后本地同步登出）。 */
export function clearCloudSession() {
  localStorage.removeItem(ACCESS_TOKEN_KEY)
  localStorage.removeItem(REFRESH_TOKEN_KEY)
  localStorage.removeItem(USER_KEY)
  cloudSessionStore.set({ status: 'unauthenticated', user: null })
}

// ---------------------------------------------------------------------------
// 设备身份 —— 持久 deviceId + devicePlatform 常量 + UA 探测默认设备名。
// ---------------------------------------------------------------------------

/** 面板本身固定作为一台 web 设备登录 FluxCloud，与宿主 App 账户状态无关。 */
export const CLOUD_DEVICE_PLATFORM = 'web'

/** RFC 4122 UUID v4 via crypto.getRandomValues() —— 不同于 crypto.randomUUID()，
 *  getRandomValues() 无 Secure Context 限制，NAS/Docker 面板经明文 HTTP（非
 *  localhost）访问时 randomUUID 缺失会直接抛错（见 issue #204），此处规避。 */
function randomUuidV4(): string {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  bytes[6] = (bytes[6] & 0x0f) | 0x40 // version 4
  bytes[8] = (bytes[8] & 0x3f) | 0x80 // variant 10
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

/** 客户端持久设备标识（UUID v4），首次调用生成并落盘，此后永久不变 —— 服务端
 *  devices 表识别"同一设备"的唯一依据。 */
export function cloudDeviceId(): string {
  const existing = localStorage.getItem(DEVICE_ID_KEY)
  if (existing) return existing
  const id = randomUuidV4()
  localStorage.setItem(DEVICE_ID_KEY, id)
  return id
}

/** 默认设备名探测：UA 解析浏览器 + 操作系统，如 "Chrome · Windows"；
 *  解析失败返回空串，交由服务端按 devicePlatform 兜底（见契约）。 */
export function cloudDefaultDeviceName(): string {
  const ua = navigator.userAgent
  const browser = detectBrowser(ua)
  const os = detectOs(ua)
  if (browser && os) return `${browser} · ${os}`
  return browser || os || ''
}

function detectBrowser(ua: string): string {
  if (/Edg\//.test(ua)) return 'Edge'
  if (/OPR\//.test(ua) || /Opera/.test(ua)) return 'Opera'
  if (/Firefox\//.test(ua)) return 'Firefox'
  if (/Chrome\//.test(ua) && !/Chromium/.test(ua)) return 'Chrome'
  if (/Safari\//.test(ua) && /Version\//.test(ua)) return 'Safari'
  return ''
}

function detectOs(ua: string): string {
  if (/Windows/.test(ua)) return 'Windows'
  if (/Mac OS X/.test(ua)) return 'macOS'
  if (/Android/.test(ua)) return 'Android'
  if (/iPhone|iPad|iPod/.test(ua)) return 'iOS'
  if (/Linux/.test(ua)) return 'Linux'
  return ''
}
