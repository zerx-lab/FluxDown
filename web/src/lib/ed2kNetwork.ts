// eD2K 本机网络（HighID / LowID、UPnP、Kad 引导）的文案键、参数拼装与结果判定：
// 任务详情「网络」页签与设置 → eD2K 共用。

import type {
  Ed2kIdCheckParams,
  Ed2kIdCheckTarget,
  Ed2kIdKind,
  Ed2kKadBootstrapDto,
  Ed2kKadBootstrapParams,
  Ed2kKadBootstrapSource,
  Ed2kKadNodeOrigin,
  Ed2kKadNodeState,
  Ed2kNetworkDto,
  Ed2kUpnpStatus,
} from './rpc'

type Translate = (key: string, params?: Record<string, string | number>) => string

const ID_KEY: Record<Ed2kIdKind, string> = { high: 'ed2kIdHigh', low: 'ed2kIdLow', none: 'ed2kIdNone' }
const UPNP_KEY: Record<Ed2kUpnpStatus, string> = {
  disabled: 'ed2kUpnpDisabled',
  pending: 'ed2kUpnpPending',
  mapped: 'ed2kUpnpMapped',
  failed: 'ed2kUpnpFailed',
}

export const ed2kIdKey = (id: Ed2kIdKind): string => ID_KEY[id] ?? ID_KEY.none
export const ed2kUpnpKey = (status: Ed2kUpnpStatus): string => UPNP_KEY[status] ?? UPNP_KEY.disabled

/** 失败且有原因时（trim 后非空）返回原因，否则 null。 */
export function ed2kUpnpFailureReason(network: Pick<Ed2kNetworkDto, 'upnp' | 'upnpError'>): string | null {
  const reason = (network.upnpError ?? '').trim()
  return network.upnp === 'failed' && reason !== '' ? reason : null
}

/** UPnP 状态文本：失败且有原因 →「失败: 原因」（与 BT 端口映射一致），否则状态名。 */
export function ed2kUpnpStatusText(t: Translate, network: Pick<Ed2kNetworkDto, 'upnp' | 'upnpError'>): string {
  const reason = ed2kUpnpFailureReason(network)
  return reason === null ? t(ed2kUpnpKey(network.upnp)) : `${t('ed2kUpnpFailed')}: ${reason}`
}

/** ID 下方的说明：HighID / LowID 各一句，未登录无。 */
export function ed2kIdHintKey(id: Ed2kIdKind): string | null {
  if (id === 'high') return 'ed2kIdHighHint'
  if (id === 'low') return 'ed2kIdLowHint'
  return null
}

/** `ed2kIdCheckFailed`，有 `error` 时附在后面。 */
export function ed2kIdCheckFailedText(t: Translate, error: string | undefined): string {
  const detail = (error ?? '').trim()
  return detail === '' ? t('ed2kIdCheckFailed') : `${t('ed2kIdCheckFailed')}: ${detail}`
}

// ── 主机 + 端口输入 ──

/**
 * 主机 / 端口输入 → 端点；主机只 trim、不解析域名。主机为空、含 `:`（IPv6 字面量无法拼 `host:port`）
 * 或含空白，端口不是 1–65535 的十进制整数 → null。
 */
export function parseHostPort(host: string, port: string): { host: string; port: number } | null {
  const h = host.trim()
  const p = port.trim()
  if (h === '' || h.includes(':') || /\s/.test(h) || !/^\d{1,5}$/.test(p)) return null
  const n = Number.parseInt(p, 10)
  return n >= 1 && n <= 65_535 ? { host: h, port: n } : null
}

// ── HighID / LowID 检测 ──

/** 设置页可选的检测目标（auto 只给任务详情的不带参数调用）。 */
export type Ed2kIdCheckChoice = Exclude<Ed2kIdCheckTarget, 'auto'>

/** 选项 → 请求参数；选「指定服务器」而主机 / 端口非法 → null（不发请求，提示 `ed2kIdCheckNeedServer`）。 */
export function ed2kIdCheckParams(target: Ed2kIdCheckChoice, host: string, port: string): Ed2kIdCheckParams | null {
  if (target !== 'server') return { target }
  const endpoint = parseHostPort(host, port)
  return endpoint === null ? null : { target, server: `${endpoint.host}:${endpoint.port}` }
}

/** 检测结果：成功 = 摘要行（+ LowID 提示）；未登录 / 失败 = 一行说明。 */
export type Ed2kIdCheckOutcome = { ok: true; summary: string; hint: string | null } | { ok: false; message: string }

export function ed2kIdCheckOutcome(t: Translate, network: Ed2kNetworkDto, target: Ed2kIdCheckTarget = 'auto'): Ed2kIdCheckOutcome {
  if (network.id !== 'high' && network.id !== 'low') {
    // 只看当前会话却没有会话：不是失败，是没登录。
    if (target === 'session' && (network.error ?? '').trim() === '') return { ok: false, message: t('ed2kIdCheckNoSession') }
    return { ok: false, message: ed2kIdCheckFailedText(t, network.error) }
  }
  const summary = t('ed2kIdCheckResult', {
    id: t(ed2kIdKey(network.id)),
    server: network.server || '—',
    port: network.listenPort ? String(network.listenPort) : '—',
    upnp: ed2kUpnpStatusText(t, network),
  })
  return { ok: true, summary, hint: network.id === 'low' ? t('ed2kIdLowHint') : null }
}

// ── Kad 引导 ──

/** 选项 → 请求参数；选「IP 或域名地址」而主机 / 端口非法 → null（提示 `ed2kKadBootstrapNeedNode`）。URL 只 trim，可留空。 */
export function ed2kKadBootstrapParams(source: Ed2kKadBootstrapSource, host: string, port: string, url: string): Ed2kKadBootstrapParams | null {
  switch (source) {
    case 'known':
      return { source }
    case 'url': {
      const u = url.trim()
      return u === '' ? { source } : { source, url: u }
    }
    case 'node': {
      const endpoint = parseHostPort(host, port)
      return endpoint === null ? null : { source, host: endpoint.host, port: endpoint.port }
    }
  }
}

export type Ed2kKadBootstrapOutcome =
  | { ok: true; summary: string; noReply: string | null }
  | { ok: false; message: string }

export function ed2kKadBootstrapFailedText(t: Translate, error: string): string {
  return t('ed2kKadBootstrapFailed', { error: error.trim() || '—' })
}

export function ed2kKadBootstrapOutcome(t: Translate, dto: Ed2kKadBootstrapDto): Ed2kKadBootstrapOutcome {
  const error = (dto.error ?? '').trim()
  if (error !== '') return { ok: false, message: ed2kKadBootstrapFailedText(t, error) }
  return {
    ok: true,
    summary: t('ed2kKadBootstrapResult', { contacted: dto.contacted, responded: dto.responded, discovered: dto.discovered, total: dto.nodeCount }),
    noReply: dto.responded === 0 ? t('ed2kKadBootstrapNoReply') : null,
  }
}

// ── Kad 节点表 ──

const KAD_NODE_STATE_KEY: Record<Ed2kKadNodeState, string> = {
  known: 'ed2kKadNodeStateKnown',
  queried: 'ed2kKadNodeStateQueried',
  responded: 'ed2kKadNodeStateResponded',
}
export const kadNodeStateKey = (state: Ed2kKadNodeState): string => KAD_NODE_STATE_KEY[state] ?? KAD_NODE_STATE_KEY.known

export const kadNodeOriginKey = (origin: Ed2kKadNodeOrigin): string =>
  origin === 'bootstrap' ? 'ed2kKadNodeOriginBootstrap' : 'ed2kKadNodeOriginDiscovered'

/** Kad ID 显示：空或全 0（未知）→ null（显示 —）。 */
export function kadNodeIdText(id: string): string | null {
  const v = id.trim()
  return v === '' || /^0+$/.test(v) ? null : v
}

/** 节点表渲染上限（协议侧至多 100 个）。 */
export const MAX_KAD_NODE_ROWS = 100
