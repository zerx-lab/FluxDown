// 未鉴权的 HTTP 探针：`/ping`、首次运行向导 `/api/v1/setup{,/status}`。
// `POST /api/v1/setup` 在密钥仍空时必须另带一次性引导码，不能匿名落盘。

export interface SetupStatus {
  setupRequired: boolean
  minLength: number
  /** 密钥仍空且服务端已装入一次性引导码。响应不包含密文。 */
  needsBootstrap: boolean
}

export interface PingInfo {
  /** 服务端语言回退（FLUXDOWN_LANG）；空串 = 未设置。 */
  language: string
}

export class SetupError extends Error {
  readonly status: number
  constructor(status: number, message: string) {
    super(message)
    this.name = 'SetupError'
    this.status = status
  }
}

export async function fetchSetupStatus(signal?: AbortSignal): Promise<SetupStatus> {
  const res = await fetch('/api/v1/setup/status', { signal, cache: 'no-store' })
  if (!res.ok) throw new SetupError(res.status, `setup status: HTTP ${res.status}`)
  return (await res.json()) as SetupStatus
}

/** 提交首次运行访问密钥。须带上 data_dir 里的一次性引导码。400 = 策略拒绝，401 = 引导码错误，409 = 已完成初始化。 */
export async function submitSetup(token: string, bootstrap: string): Promise<void> {
  const res = await fetch('/api/v1/setup', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token, bootstrap }),
  })
  if (res.ok) return
  let message = `HTTP ${res.status}`
  try {
    const body = (await res.json()) as { error?: unknown }
    if (typeof body.error === 'string') message = body.error
  } catch {
    // 非 JSON 响应体，保留状态码文案。
  }
  throw new SetupError(res.status, message)
}

/** `/ping`（免鉴权）。服务不可达返回 null。 */
export async function fetchPing(signal?: AbortSignal): Promise<PingInfo | null> {
  try {
    const res = await fetch('/ping', { signal, cache: 'no-store' })
    if (!res.ok) return null
    const body = (await res.json()) as { language?: unknown }
    return { language: typeof body.language === 'string' ? body.language : '' }
  } catch {
    return null
  }
}

/**
 * 用访问密钥探测一个鉴权 HTTP 接口，用于把 WS 升级前的失败（浏览器拿不到 HTTP 状态）
 * 区分为「密钥错误」「服务需要初始化」「网络/其他」。
 */
export type TokenProbe = 'ok' | 'unauthorized' | 'setupRequired' | 'unreachable' | 'unknown'

export async function probeToken(token: string, signal?: AbortSignal): Promise<TokenProbe> {
  try {
    const res = await fetch('/api/v1/info', {
      headers: { Authorization: `Bearer ${token}` },
      signal,
      cache: 'no-store',
    })
    if (res.ok) return 'ok'
    if (res.status === 401) return 'unauthorized'
    if (res.status === 403) return 'setupRequired'
    return 'unknown'
  } catch {
    return 'unreachable'
  }
}
