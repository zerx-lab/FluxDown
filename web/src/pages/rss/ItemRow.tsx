// 条目行（GPUI `RssView::render_item`）：桌面 64px 单行（选择 | 标题+元信息+原因 | 状态徽标 120px | 操作），
// 移动端 76px 三行（标题 / 元信息 / 状态徽标+原因）+ 右侧 44px 操作，无 hover 依赖。

import { Download, EllipsisVertical, EyeOff, Image as ImageIcon } from 'lucide-react'
import { memo } from 'react'
import { useFileSizeUnits } from '../../lib/fileSizeUnits'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import type { RssItemDto } from '../../lib/rpc'
import { RSS_ITEM_STATUS } from '../../lib/rpc'
import { ActionMenu, Badge, Button, Checkbox, Tooltip } from '../../ui'
import type { MenuEntry } from '../../ui'
import { reasonKey } from './filter'
import { bytesText, dateText } from './format'
import type { ItemAction } from './useRssItems'

export const ROW_HEIGHT_DESKTOP = 64
export const ROW_HEIGHT_MOBILE = 76

export interface ItemRowProps {
  item: RssItemDto
  /** 状态徽标 i18n 键（已按关联任务真实状态解析）。 */
  statusKey: string
  selected: boolean
  busy: boolean
  stale: boolean
  /** 连接就绪且未在处理。 */
  canAct: boolean
  mobile: boolean
  onToggle: (guid: string) => void
  onAct: (guids: readonly string[], action: ItemAction) => void
  onViewImages: (item: RssItemDto) => void
}

function ItemRowImpl({ item, statusKey, selected, busy, canAct, mobile, onToggle, onAct }: ItemRowProps) {
  const units = useFileSizeUnits()
  const t = useT()
  const reason = reasonKey(item.reason)
  const date = dateText(item.pubDate)
  const size = bytesText(item.enclosureLength, units)
  const meta = [date !== '' ? `${t('rssPublishedAt')} · ${date}` : '', size].filter((part) => part !== '').join(' · ')
  const canIgnore = item.status === RSS_ITEM_STATUS.new
  const hasImages = (item.imageCount ?? 0) > 0
  const taskMissing = item.status === RSS_ITEM_STATUS.downloaded && statusKey === 'rssTaskMissing'
  const downloadLabel = busy ? t('rssActionPreparing') : item.status === RSS_ITEM_STATUS.downloaded ? t('rssActionRedownload') : t('rssActionDownload')
  const status = (
    <Badge tone={taskMissing ? 'destructive' : 'neutral'} className="max-w-full truncate">
      {t(statusKey)}
    </Badge>
  )
  const select = (
    <label className="flex size-8 shrink-0 cursor-pointer items-center justify-center coarse:size-11">
      <Checkbox checked={selected} onCheckedChange={() => onToggle(item.guid)} aria-label={t('rssSelectVisible')} />
    </label>
  )

  if (mobile) {
    const menu: MenuEntry[] = [
      { type: 'item', key: 'download', label: downloadLabel, icon: Download, disabled: !canAct, onSelect: () => onAct([item.guid], 'download') },
      ...(canIgnore
        ? [{ type: 'item', key: 'ignore', label: t('rssActionIgnore'), icon: EyeOff, disabled: !canAct, onSelect: () => onAct([item.guid], 'ignore') } satisfies MenuEntry]
        : []),
      ...(hasImages
        ? [{ type: 'item', key: 'images', label: t('rssViewImages'), icon: ImageIcon, disabled: stale, onSelect: () => onViewImages(item) } satisfies MenuEntry]
        : []),
    ]
    return (
      <div className={cn('flex h-full w-full items-center gap-1 pr-1 pl-2', selected && 'bg-accent')}>
        {select}
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <div className="truncate text-sm text-foreground">{item.title}</div>
          {meta !== '' ? <div className="truncate text-xs text-text-tertiary tabular">{meta}</div> : null}
          <div className="flex min-w-0 items-center gap-2">
            <div className="shrink-0">{status}</div>
            {reason ? <span className="truncate text-xs text-text-tertiary">{t(reason)}</span> : null}
          </div>
        </div>
        <ActionMenu
          title={item.title}
          trigger={<Button variant="ghost" iconOnly icon={EllipsisVertical} aria-label={t('moreActions')} loading={busy} />}
          entries={menu}
        />
      </div>
    )
  }

  return (
    <div className={cn('group flex h-full w-full items-center gap-3 px-4', selected ? 'bg-accent' : 'hover:bg-row-hover')}>
      {select}
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <div className="truncate text-sm text-foreground" title={item.title}>
          {item.title}
        </div>
        {meta !== '' ? <div className="truncate text-xs text-text-tertiary tabular">{meta}</div> : null}
        {reason ? <div className="truncate text-xs text-text-tertiary">{t(reason)}</div> : null}
      </div>
      <div className="flex w-[120px] shrink-0 justify-end">{status}</div>
      {hasImages ? (
        <Tooltip content={t('rssViewImages')}>
          <Button variant="ghost" iconOnly icon={ImageIcon} aria-label={t('rssViewImages')} disabled={stale} onClick={() => onViewImages(item)} />
        </Tooltip>
      ) : null}
      <div
        className={cn(
          'flex shrink-0 items-center justify-end gap-0.5 transition-opacity',
          // 悬停 / 键盘聚焦 / 触屏指针时显示；选中或处理中常显，保证状态可见。
          !selected && !busy && 'opacity-0 group-hover:opacity-100 focus-within:opacity-100 coarse:opacity-100',
        )}
      >
        <Button variant="ghost" disabled={!canAct} onClick={() => onAct([item.guid], 'download')}>
          {downloadLabel}
        </Button>
        {canIgnore ? (
          <Button variant="ghost" disabled={!canAct} onClick={() => onAct([item.guid], 'ignore')}>
            {t('rssActionIgnore')}
          </Button>
        ) : null}
      </div>
    </div>
  )
}

export const ItemRow = memo(ItemRowImpl)
