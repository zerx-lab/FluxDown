// 端口映射状态行（GPUI `sections/bt.rs::port_mapping_segments` / `port_segment_text`）：纯数据 → 各段文字。

import type { BtPortStatusDto } from '../../../../lib/rpc'

/** `daemon.bt.portStatus` 轮询间隔（设置页「连接」可见时）。 */
export const PORT_STATUS_POLL_MS = 5000

export type PortSegmentState =
  /** 附显示用方法名（`UPnP` / `NAT-PMP` / `PCP`，未知取原值）。 */
  | { kind: 'mapped'; method: string }
  | { kind: 'pending' }
  | { kind: 'failed'; message: string }
  /** 未识别的 wire 状态，原样显示。 */
  | { kind: 'other'; raw: string }

export type PortSegment =
  /** BT 会话未运行：尚未请求映射。 */
  | { kind: 'idle' }
  /** `port` = `TCP 6881` / `UDP 6881 → 50123`。 */
  | { kind: 'mapping'; port: string; state: PortSegmentState }
  | { kind: 'externalIp'; ip: string }

/** wire 方法名 → 显示名；未知值原样返回。 */
export function methodLabel(method: string): string {
  switch (method) {
    case 'upnp':
      return 'UPnP'
    case 'natPmp':
      return 'NAT-PMP'
    case 'pcp':
      return 'PCP'
    default:
      return method
  }
}

/** 未运行一段「空闲」；否则每个映射一段，任一映射带外网地址时末尾追加一段（取第一个非空地址）。 */
export function portMappingSegments(status: BtPortStatusDto): PortSegment[] {
  if (!status.running) return [{ kind: 'idle' }]
  const mappings = status.mappings ?? []
  const segments: PortSegment[] = mappings.map((mapping) => {
    const external = mapping.externalPort ?? 0
    let port = `${mapping.protocol.toUpperCase()} ${mapping.internalPort}`
    if (mapping.state === 'mapped' && external !== 0 && external !== mapping.internalPort) port += ` → ${external}`
    const state: PortSegmentState =
      mapping.state === 'mapped'
        ? { kind: 'mapped', method: methodLabel(mapping.method ?? '') }
        : mapping.state === 'pending'
          ? { kind: 'pending' }
          : mapping.state === 'failed'
            ? { kind: 'failed', message: (mapping.message ?? '').trim() }
            : { kind: 'other', raw: mapping.state }
    return { kind: 'mapping', port, state }
  })
  const ip = mappings.map((mapping) => (mapping.externalIp ?? '').trim()).find((value) => value !== '')
  if (ip !== undefined) segments.push({ kind: 'externalIp', ip })
  return segments
}

type Translate = (key: string, params?: Record<string, string | number>) => string

/** 一段的显示文字 + 悬停全文（仅失败原因可能被截断，带全文）。失败原因压成单行。 */
export function portSegmentText(t: Translate, segment: PortSegment): { text: string; full: string | null } {
  switch (segment.kind) {
    case 'idle':
      return { text: t('btPortMappingIdle'), full: null }
    case 'externalIp':
      return { text: t('btPortMappingExternalIp', { ip: segment.ip }), full: null }
    case 'mapping': {
      const { port, state } = segment
      switch (state.kind) {
        case 'mapped':
          return { text: `${port} ${t('btPortMappingMapped', { method: state.method })}`, full: null }
        case 'pending':
          return { text: `${port} ${t('btPortMappingPending')}`, full: null }
        case 'other':
          return { text: `${port} ${state.raw}`, full: null }
        case 'failed': {
          const failed = t('btPortMappingFailed')
          if (state.message === '') return { text: `${port} ${failed}`, full: null }
          const line = state.message.split(/\s+/).filter(Boolean).join(' ')
          return { text: `${port} ${failed}: ${line}`, full: `${port} ${failed}: ${state.message}` }
        }
      }
    }
  }
}
