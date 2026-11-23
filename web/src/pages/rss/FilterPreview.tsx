// 过滤规则实时预览：用与引擎 `filter.rs` 逐条对齐的前端副本（./filter.ts）对样本条目整轮试跑。

import { useMemo } from 'react'
import { useFileSizeUnits } from '../../lib/fileSizeUnits'
import { useT } from '../../i18n'
import type { RssItemDto } from '../../lib/rpc'
import { Badge } from '../../ui'
import { previewRule, reasonKey } from './filter'
import type { RssFilterRule } from './filter'
import { bytesText } from './format'

/** 预览列表最多渲染的行数（汇总数字仍按全部样本统计）。 */
const MAX_ROWS = 200

export function FilterPreview({ rule, items }: { rule: RssFilterRule; items: readonly RssItemDto[] }) {
  const units = useFileSizeUnits()
  const t = useT()
  const rows = useMemo(() => previewRule(rule, [...items]), [rule, items])
  const hit = rows.filter((row) => row.verdict.accepted).length
  return (
    <div className="flex flex-col gap-2 rounded-[var(--fx-components-card-radius)] border border-hairline p-3">
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span>{t('rssPreviewHeader', { n: items.length })}</span>
        {items.length > 0 ? <span className="tabular">{t('rssPreviewSummary', { hit, miss: items.length - hit })}</span> : null}
      </div>
      {items.length === 0 ? (
        <p className="text-xs text-text-tertiary">{t('rssPreviewEmpty')}</p>
      ) : (
        <ul className="flex max-h-56 flex-col gap-1 overflow-y-auto">
          {rows.slice(0, MAX_ROWS).map(({ item, verdict }) => {
            const key = verdict.accepted ? null : reasonKey(verdict.reason)
            return (
              <li key={item.guid} className="flex min-w-0 items-center gap-2 text-xs">
                <Badge tone={verdict.accepted ? 'success' : 'neutral'} className="shrink-0">
                  {t(verdict.accepted ? 'rssPreviewWillDownload' : 'rssPreviewFiltered')}
                </Badge>
                <span className={verdict.accepted ? 'min-w-0 flex-1 truncate text-foreground' : 'min-w-0 flex-1 truncate text-text-tertiary'} title={item.title}>
                  {item.title}
                </span>
                <span className="max-w-[45%] shrink-0 truncate text-text-tertiary">{key ? t(key) : verdict.accepted ? bytesText(item.enclosureLength, units) : ''}</span>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
