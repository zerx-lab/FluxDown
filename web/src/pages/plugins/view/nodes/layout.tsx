import { Inbox } from 'lucide-react'
import type { CSSProperties, ReactNode } from 'react'
import { cn } from '../../../../lib/cn'
import type { ViewNode } from '../../../../lib/rpc'
import { EmptyState, RevealCallout } from '../../../../ui'
import { toneBackground, toneColor } from '../tones'
import { bool, num, optStr, records, str } from './props'

const GAP: Record<string, string> = {
  none: '0px',
  xs: 'var(--fx-spacing-xs)',
  sm: 'var(--fx-spacing-sm)',
  md: 'var(--fx-spacing-md)',
  lg: 'var(--fx-spacing-lg)',
  xl: 'var(--fx-spacing-xl)',
}

const ALIGN: Record<string, CSSProperties['alignItems']> = { start: 'flex-start', center: 'center', end: 'flex-end', stretch: 'stretch' }
const JUSTIFY: Record<string, CSSProperties['justifyContent']> = {
  start: 'flex-start',
  center: 'center',
  end: 'flex-end',
  between: 'space-between',
}

/** 间距档位 → CSS 值（未知档位按 `fallback`）。 */
function spacing(name: string, fallback: string): string {
  return GAP[name] ?? GAP[fallback] ?? '0px'
}

export function StackNode({ node, children }: { node: ViewNode; children: ReactNode }) {
  const p = node.p
  const row = str(p, 'direction', 'column') === 'row'
  const grow = bool(p, 'grow')
  const style: CSSProperties = {
    display: 'flex',
    flexDirection: row ? 'row' : 'column',
    gap: spacing(str(p, 'gap', 'sm'), 'sm'),
    alignItems: ALIGN[str(p, 'align', 'stretch')],
    justifyContent: JUSTIFY[str(p, 'justify', 'start')],
    flexWrap: bool(p, 'wrap') ? 'wrap' : 'nowrap',
    padding: spacing(str(p, 'padding', 'none'), 'none'),
    minWidth: 0,
    ...(grow ? { flex: '1 1 0%', minHeight: 0 } : null),
  }
  return <div data-fill-view={node.id === 'root' && grow && !row ? '' : undefined} style={style}>{children}</div>
}

export function ScrollNode({ node, children }: { node: ViewNode; children: ReactNode }) {
  const maxHeight = num(node.p, 'maxHeight')
  return (
    <div className={cn('flex min-h-0 flex-col overflow-auto', maxHeight === undefined && 'view-scroll-fill')} style={{ maxHeight }}>
      {children}
    </div>
  )
}

export function CardNode({ node, children }: { node: ViewNode; children: ReactNode }) {
  const title = optStr(node.p, 'title')
  return (
    <div
      className="flex flex-col gap-2 rounded-[var(--fx-components-card-radius,var(--fx-radius-md))] border border-hairline bg-surface"
      style={{ padding: spacing(str(node.p, 'padding', 'md'), 'md') }}
    >
      {title ? <div className="text-sm font-semibold text-foreground">{title}</div> : null}
      {children}
    </div>
  )
}

export function SectionNode({ node, children }: { node: ViewNode; children: ReactNode }) {
  const description = optStr(node.p, 'description')
  return (
    <section className="flex flex-col gap-2">
      <div>
        <h3 className="text-md font-semibold text-foreground">{str(node.p, 'title')}</h3>
        {description ? <div className="text-xs text-muted-foreground">{description}</div> : null}
      </div>
      {children}
    </section>
  )
}

export function DividerNode() {
  return <hr className="border-0 border-t border-hairline" />
}

export function EmptyNode({ node }: { node: ViewNode }) {
  return <EmptyState icon={Inbox} title={str(node.p, 'title')} description={optStr(node.p, 'description')} />
}

export function KvNode({ node }: { node: ViewNode }) {
  const items = records(node.p, 'items')
  return (
    <dl className="grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
      {items.map((item, index) => (
        <div key={`${str(item, 'key')}:${index}`} className="contents">
          <dt className="text-muted-foreground">{str(item, 'key')}</dt>
          <dd className="min-w-0 break-words text-foreground">{str(item, 'value')}</dd>
        </div>
      ))}
    </dl>
  )
}

export function CalloutNode({ node }: { node: ViewNode }) {
  const tone = str(node.p, 'tone', 'info')
  const title = optStr(node.p, 'title')
  const text = str(node.p, 'text')
  if (tone === 'warning' || tone === 'danger') {
    return <RevealCallout visible tone={tone} title={title ?? text} body={title ? text : ''} contentClassName="" />
  }
  return (
    <div
      role="status"
      className={cn('flex flex-col gap-0.5 rounded-md border border-l-[3px] px-3 py-2')}
      style={{ color: toneColor(tone), backgroundColor: toneBackground(tone), borderColor: toneColor(tone) }}
    >
      {title ? <div className="text-sm font-semibold">{title}</div> : null}
      <div className="text-xs">{text}</div>
    </div>
  )
}
