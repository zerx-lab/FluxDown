// Kad 节点表：任务详情「网络」页签与设置 → eD2K →「Kad 节点」引导面板共用。

import { Copy } from 'lucide-react'
import { useT } from '../i18n'
import type { Ed2kKadNodeDto } from './rpc'
import { cn } from './cn'
import { copyText } from './copy'
import { kadNodeIdText, kadNodeOriginKey, kadNodeStateKey, MAX_KAD_NODE_ROWS } from './ed2kNetwork'
import { ContextMenuArea } from '../ui'
import type { MenuEntry } from '../ui'

const DASH = '—'

/** 窄布局只留地址 / ID / 状态三列；`maxHeightClass` 给出时表体限高滚动。 */
export function Ed2kKadNodeTable({ nodes, narrow, maxHeightClass }: { nodes: readonly Ed2kKadNodeDto[]; narrow: boolean; maxHeightClass?: string }) {
  const t = useT()
  const grid = narrow ? 'grid-cols-[minmax(0,1fr)_minmax(0,1fr)_5rem]' : 'grid-cols-[minmax(8rem,1fr)_minmax(0,1.2fr)_5rem_5rem_3.5rem]'
  return (
    <div className="flex min-w-0 flex-col">
      <div className={cn('grid gap-x-3 border-b border-hairline px-1 pb-1 text-xs text-text-tertiary', grid)}>
        <span className="truncate">{t('ed2kKadNodeAddr')}</span>
        <span className="truncate">{t('ed2kKadNodeId')}</span>
        <span className="truncate">{t('ed2kKadNodeState')}</span>
        {narrow ? null : <span className="truncate">{t('ed2kKadNodeOrigin')}</span>}
        {narrow ? null : <span className="truncate text-right">{t('ed2kKadNodeVersion')}</span>}
      </div>
      <div className={cn('flex flex-col', maxHeightClass && cn('overflow-y-auto', maxHeightClass))}>
        {nodes.slice(0, MAX_KAD_NODE_ROWS).map((node, index) => {
          const id = kadNodeIdText(node.id)
          const copy: MenuEntry = { type: 'item', key: 'copy', label: t('webCopy'), icon: Copy, onSelect: () => copyText(node.addr) }
          return (
            <ContextMenuArea key={`${node.addr}/${node.id}/${index}`} entries={[copy]} title={node.addr} className="border-b border-hairline hover:bg-row-hover">
              <div className={cn('grid items-center gap-x-3 px-1 py-1.5 text-xs', grid)}>
                <span title={node.addr} className="truncate font-mono text-foreground">
                  {node.addr || DASH}
                </span>
                <span title={id ?? undefined} className="truncate font-mono text-muted-foreground">
                  {id ?? DASH}
                </span>
                <span className="truncate text-muted-foreground">{t(kadNodeStateKey(node.state))}</span>
                {narrow ? null : <span className="truncate text-muted-foreground">{t(kadNodeOriginKey(node.origin))}</span>}
                {narrow ? null : <span className="tabular text-right text-muted-foreground">{node.version ? String(node.version) : DASH}</span>}
              </div>
            </ContextMenuArea>
          )
        })}
      </div>
    </div>
  )
}
