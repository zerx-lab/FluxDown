// eD2K 详情「来源」「网络」页签的纯模型：DTO 缺省归一、状态 → 文案键、同时连接来源数选项、列布局档。

import type {
  Ed2kKadPhase,
  Ed2kNetworkDto,
  Ed2kPeerDto,
  Ed2kPeerOrigin,
  Ed2kPeerState,
  Ed2kServerDto,
  Ed2kServerStatus,
  Ed2kStatusDto,
  Ed2kTaskPhase,
} from '../../../lib/rpc'
import { MAX_PEER_ROWS } from './btStatus'

/** 时间戳是否在未来（0 / 缺省 = 未知，视为否）。 */
export function isFuture(at: number | undefined, nowSecs: number): boolean {
  return typeof at === 'number' && at > 0 && at > nowSecs
}

// ── DTO 归一 ──

const num = (value: unknown): number => (typeof value === 'number' && Number.isFinite(value) ? value : 0)
const str = (value: unknown): string => (typeof value === 'string' ? value : '')

function normalizeNetwork(n: Partial<Ed2kNetworkDto> | undefined): Ed2kNetworkDto {
  return {
    id: n?.id ?? 'none',
    clientId: num(n?.clientId),
    server: str(n?.server),
    listenPort: num(n?.listenPort),
    upnp: n?.upnp ?? 'disabled',
    externalIp: str(n?.externalIp),
    upnpEnabled: n?.upnpEnabled === true,
    kadEnabled: n?.kadEnabled === true,
    kadNodes: num(n?.kadNodes),
    serverCount: num(n?.serverCount),
    error: str(n?.error),
  }
}

/** 补齐缺省字段（服务端默认值不上线）；旧宿主 / 截断 DTO 也不崩。 */
export function normalizeEd2kStatus(dto: Partial<Ed2kStatusDto>): Ed2kStatusDto {
  const c: Partial<Ed2kStatusDto['peerCounts']> = dto.peerCounts ?? {}
  const kad: Partial<Ed2kStatusDto['kad']> = dto.kad ?? {}
  return {
    phase: dto.phase ?? 'idle',
    nextSearchAt: num(dto.nextSearchAt),
    fileHash: str(dto.fileHash),
    partsTotal: num(dto.partsTotal),
    partsDone: num(dto.partsDone),
    maxSources: num(dto.maxSources),
    segments: num(dto.segments),
    network: normalizeNetwork(dto.network),
    peerCounts: {
      total: num(c.total),
      transferring: num(c.transferring),
      connected: num(c.connected),
      queued: num(c.queued),
      connecting: num(c.connecting),
      cooldown: num(c.cooldown),
      failed: num(c.failed),
      banned: num(c.banned),
    },
    peers: dto.peers ?? [],
    servers: dto.servers ?? [],
    kad: {
      phase: kad.phase ?? 'idle',
      bootstrapNodes: num(kad.bootstrapNodes),
      responded: num(kad.responded),
      discovered: num(kad.discovered),
      sources: num(kad.sources),
      lastSearchAt: num(kad.lastSearchAt),
      nodes: kad.nodes ?? [],
    },
  }
}

// ── 文案键 ──

const PHASE_KEY: Record<Ed2kTaskPhase, string> = {
  idle: 'ed2kPhaseIdle',
  searching: 'ed2kPhaseSearching',
  downloading: 'ed2kPhaseDownloading',
  waiting: 'ed2kPhaseWaiting',
  verifying: 'ed2kPhaseVerifying',
}
export const phaseKey = (phase: Ed2kTaskPhase): string => PHASE_KEY[phase] ?? PHASE_KEY.idle

const KAD_PHASE_KEY: Record<Ed2kKadPhase, string> = {
  idle: 'ed2kKadPhaseIdle',
  bootstrapping: 'ed2kKadPhaseBootstrapping',
  findingNodes: 'ed2kKadPhaseFindingNodes',
  searching: 'ed2kKadPhaseSearching',
  done: 'ed2kKadPhaseDone',
}
export const kadPhaseKey = (phase: Ed2kKadPhase): string => KAD_PHASE_KEY[phase] ?? KAD_PHASE_KEY.idle

const SERVER_STATUS_KEY: Record<Ed2kServerStatus, string> = {
  querying: 'ed2kServerStatusQuerying',
  working: 'ed2kServerStatusWorking',
  notWorking: 'ed2kServerStatusNotWorking',
}
export const serverStatusKey = (status: Ed2kServerStatus): string => SERVER_STATUS_KEY[status] ?? SERVER_STATUS_KEY.querying

export const peerOriginKey = (origin: Ed2kPeerOrigin): string => (origin === 'kad' ? 'ed2kPeerOriginKad' : 'ed2kPeerOriginServer')

/** 状态文案：`key` + 参数；`retryAt` 非空时由调用方把相对时间填进 `time`。 */
export interface PeerStateLabel {
  key: string
  params?: Record<string, number>
  /** 冷却结束时间（仅 `ed2kPeerStateCooldownUntil`）。 */
  retryAt?: number
}

export function peerStateLabel(peer: Pick<Ed2kPeerDto, 'state' | 'queueRank' | 'part' | 'retryAt'>, nowSecs: number): PeerStateLabel {
  switch (peer.state) {
    case 'known':
      return { key: 'ed2kPeerStateKnown' }
    case 'connecting':
      return { key: 'ed2kPeerStateConnecting' }
    case 'queued':
      return peer.queueRank ? { key: 'ed2kPeerStateQueuedRank', params: { rank: peer.queueRank } } : { key: 'ed2kPeerStateQueued' }
    case 'transferring':
      return typeof peer.part === 'number' && peer.part >= 0
        ? { key: 'ed2kPeerStateTransferringPart', params: { part: peer.part + 1 } }
        : { key: 'ed2kPeerStateTransferring' }
    case 'connected':
      return { key: 'ed2kPeerStateConnected' }
    case 'cooldown':
      return isFuture(peer.retryAt, nowSecs) ? { key: 'ed2kPeerStateCooldownUntil', retryAt: peer.retryAt! } : { key: 'ed2kPeerStateCooldown' }
    case 'failed':
      return { key: 'ed2kPeerStateFailed' }
    case 'banned':
      return { key: 'ed2kPeerStateBanned' }
    default:
      return { key: 'ed2kPeerStateKnown' }
  }
}

/** 状态文字色（主题令牌）。 */
export function peerStateTone(state: Ed2kPeerState): string {
  switch (state) {
    case 'transferring':
      return 'text-success'
    case 'failed':
    case 'banned':
      return 'text-destructive'
    case 'known':
    case 'cooldown':
      return 'text-text-tertiary'
    default:
      return 'text-muted-foreground'
  }
}

// ── 同时连接来源数 ──

export const MAX_ED2K_SOURCES = 8

/** 选项：0 = 自动，1…8。 */
export const MAX_SOURCES_OPTIONS: readonly number[] = [0, 1, 2, 3, 4, 5, 6, 7, 8]

/** 任务线程数设置 → 选项值：≤0 / 非法 = 自动，>8 视为 8。 */
export function normalizeMaxSources(segments: number | undefined): number {
  if (typeof segments !== 'number' || !Number.isFinite(segments) || segments <= 0) return 0
  return Math.min(MAX_ED2K_SOURCES, Math.floor(segments))
}

// ── 来源行 ──

export interface Ed2kPeerRow {
  key: string
  /** HighID 源地址；LowID 源为 `''`。 */
  addr: string
  /** LowID 源的 client ID；HighID 源为 null。 */
  lowId: number | null
  origin: Ed2kPeerOrigin
  state: Ed2kPeerState
  queueRank: number
  part: number | undefined
  retryAt: number
  client: string
  nickname: string
  downloaded: number
  downBps: number
  /** 对端持有块数；未知 = null。 */
  parts: number | null
  message: string
}

/** 取前 `MAX_PEER_ROWS` 条（服务端顺序）；key 唯一。 */
export function mapEd2kPeers(peers: readonly Ed2kPeerDto[]): Ed2kPeerRow[] {
  const seen = new Map<string, number>()
  return peers.slice(0, MAX_PEER_ROWS).map((p) => {
    const addr = str(p.addr).trim()
    const lowId = addr === '' && p.lowId ? p.lowId : null
    const base = addr !== '' ? addr : `low:${lowId ?? '?'}`
    const nth = seen.get(base) ?? 0
    seen.set(base, nth + 1)
    return {
      key: nth === 0 ? base : `${base}#${nth}`,
      addr,
      lowId,
      origin: p.origin,
      state: p.state,
      queueRank: num(p.queueRank),
      part: typeof p.part === 'number' ? p.part : undefined,
      retryAt: num(p.retryAt),
      client: str(p.client).trim(),
      nickname: str(p.nickname).trim(),
      downloaded: num(p.downloaded),
      downBps: num(p.downBps),
      parts: typeof p.parts === 'number' ? p.parts : null,
      message: str(p.message).trim(),
    }
  })
}

/** 持有分块：未知 → null（显示 —）。 */
export function partsText(parts: number | null, partsTotal: number): string | null {
  return parts === null ? null : `${parts} / ${partsTotal}`
}

export type Ed2kPeerColumn = 'addr' | 'origin' | 'client' | 'state' | 'parts' | 'downloaded' | 'speed'
export type Ed2kLayout = 'card' | 'compact' | 'full'

/** 容器宽度 → 版式（与 BT 连接页同档位）：<480 卡片；<780 紧凑；其余全列。 */
export function ed2kLayout(width: number): Ed2kLayout {
  if (width < 480) return 'card'
  if (width < 780) return 'compact'
  return 'full'
}

export function ed2kPeerColumns(layout: Ed2kLayout): Ed2kPeerColumn[] {
  switch (layout) {
    case 'card':
      return ['addr', 'state', 'speed']
    case 'compact':
      return ['addr', 'state', 'parts', 'speed']
    case 'full':
      return ['addr', 'origin', 'client', 'state', 'parts', 'downloaded', 'speed']
  }
}

// ── 服务器 / Kad ──

export interface Ed2kServerRow {
  key: string
  addr: string
  source: Ed2kServerDto['source']
  status: Ed2kServerStatus
  connected: boolean
  sources: number | null
  message: string
  lastQueryAt: number
}

export function mapEd2kServers(servers: readonly Ed2kServerDto[]): Ed2kServerRow[] {
  const seen = new Map<string, number>()
  return servers.map((s) => {
    const nth = seen.get(s.addr) ?? 0
    seen.set(s.addr, nth + 1)
    return {
      key: nth === 0 ? s.addr : `${s.addr}#${nth}`,
      addr: s.addr,
      source: s.source,
      status: s.status,
      connected: s.connected === true,
      sources: typeof s.sources === 'number' ? s.sources : null,
      message: str(s.message).trim(),
      lastQueryAt: num(s.lastQueryAt),
    }
  })
}

/** Kad 段显示什么：关闭 / 无引导节点 / 汇总 + 表格。 */
export function kadView(network: Pick<Ed2kNetworkDto, 'kadEnabled' | 'kadNodes'>): 'off' | 'noBootstrap' | 'table' {
  if (!network.kadEnabled) return 'off'
  if (network.kadNodes === 0) return 'noBootstrap'
  return 'table'
}
