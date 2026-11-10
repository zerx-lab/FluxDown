// 多文件清单选择 → 建任务组（`daemon.group.create`）。
// 由新建下载在单条 http(s) 链接经 `daemon.group.resolvePreview` 命中插件清单后打开（嵌套对话框，
// 取消回到表单，确认建组后由调用方关闭表单）。交互采用下钻导航：
// 面包屑 + 目录行下钻 + 三态勾选 + 扩展名 chips + 全局搜索；resolverItem 恒为 item.id。

import { ArrowLeft, ChevronRight, File as FileIcon, Folder, Home, MoreHorizontal } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useT } from '../../../i18n'
import { rpc } from '../../../lib/rpc'
import type { CategoryRouteDto, QueueDto, ResolvePreviewResponse } from '../../../lib/rpc'
import { LATER_QUEUE_ID } from '../../../lib/rpc'
import { ActionMenu, Button, Checkbox, Dialog, DialogFooter, FieldHint, FormField, Icon, Input, InputWithAction } from '../../../ui'
import { toastRpcError } from '../../../lib/rpcToast'
import type { MenuEntry } from '../../../ui'
import { routeBaseDir, routeFollows } from '../../../lib/categoryRoute'
import { cn } from '../../../lib/cn'
import { FsPickerDialog } from './FsPickerDialog'
import {
  buildManifestBreadcrumb,
  buildManifestGroupItems,
  manifestDefaultGroupName,
  manifestDirRowCheckState,
  manifestInvertVisibleSelection,
  manifestRowsAt,
  manifestSelectAllVisible,
  manifestSelectionStat,
  manifestSourceHost,
  manifestToggleDirSubtree,
  manifestTopExtensions,
  manifestTotalSize,
  manifestUpPath,
} from './manifest'
import type { ManifestSortKey } from './manifest'
import { SplitButton } from './SplitButton'
import { formatBytes, queueLabel } from './utils'

/** 组级请求选项：沿用新建下载表单里已填的值，下发给全部子任务。 */
export interface ManifestBaseOptions {
  saveDir: string
  offeredSaveDir: string
  queueId: string
  segments: number
  cookies: string
  referrer: string
  userAgent: string
  proxyUrl: string
  headers: Record<string, string>
  ignoreTlsErrors: boolean
  categoryRoute?: CategoryRouteDto
}

export function ManifestSelectDialog({
  preview,
  sourceUrl,
  base,
  queues,
  onCancel,
  onCreated,
}: {
  preview: ResolvePreviewResponse
  sourceUrl: string
  base: ManifestBaseOptions
  queues: readonly QueueDto[]
  onCancel: () => void
  onCreated: () => void
}) {
  const t = useT()
  const items = preview.items
  const [cwd, setCwd] = useState('')
  const [selected, setSelected] = useState<Set<string>>(() => new Set())
  const [extFilter, setExtFilter] = useState<Set<string>>(() => new Set())
  const [search, setSearch] = useState('')
  const [sortKey, setSortKey] = useState<ManifestSortKey>('name')
  const [groupName, setGroupName] = useState(() => manifestDefaultGroupName(preview.name, sourceUrl))
  const [saveDir, setSaveDir] = useState(base.saveDir)
  const [pickerOpen, setPickerOpen] = useState(false)
  const [submitting, setSubmitting] = useState(false)

  const filter = { extFilter, search }
  const rowsResult = manifestRowsAt({ items, cwd, selectedItemIds: selected, extFilter, search, sortKey })
  const breadcrumb = buildManifestBreadcrumb({ items, cwd: rowsResult.cwd, extFilter, search })
  const topExtensions = useMemo(() => manifestTopExtensions(items), [items])
  const stat = manifestSelectionStat(items, selected)
  const totalSize = manifestTotalSize(items)
  const host = manifestSourceHost(sourceUrl)

  const navigate = (path: string, nextExt: Set<string> = extFilter) => {
    setCwd(manifestRowsAt({ items, cwd: path, selectedItemIds: selected, extFilter: nextExt, search: '', sortKey }).cwd)
  }

  const toggleFile = (id: string) => {
    const next = new Set(selected)
    if (!next.delete(id)) next.add(id)
    setSelected(next)
  }

  const toggleExt = (ext: string) => {
    const next = new Set(extFilter)
    if (!next.delete(ext)) next.add(ext)
    setExtFilter(next)
    navigate(rowsResult.cwd, next)
  }

  const submit = async (queueId: string, startPaused: boolean) => {
    if (submitting || stat.count === 0) return
    setSubmitting(true)
    try {
      const route = base.categoryRoute && routeFollows(base.categoryRoute, saveDir.trim(), base.offeredSaveDir)
        ? base.categoryRoute
        : undefined
      const submittedSaveDir = route
        ? routeBaseDir(route, saveDir.trim(), base.offeredSaveDir)
        : saveDir.trim()
      await rpc.daemon.group.create({
        sourceUrl,
        groupName: groupName.trim() || preview.name || undefined,
        saveDir: submittedSaveDir || undefined,
        queueId: queueId || undefined,
        segments: base.segments,
        cookies: base.cookies || undefined,
        referrer: base.referrer || undefined,
        userAgent: base.userAgent || undefined,
        proxyUrl: base.proxyUrl || undefined,
        extraHeaders: Object.keys(base.headers).length > 0 ? base.headers : undefined,
        ignoreTlsErrors: base.ignoreTlsErrors,
        startPaused,
        resolverPluginId: preview.pluginId || undefined,
        archiveMetadata: preview.archiveMetadata,
        categoryRoute: route,
        items: buildManifestGroupItems(items, selected),
      })
      onCreated()
    } catch (error) {
      toastRpcError(error)
      setSubmitting(false)
    }
  }

  const startMenu: MenuEntry[] = queues.map((queue) => ({
    type: 'item',
    key: queue.queueId,
    label: t('manifestStartToQueue', { name: queueLabel(t, queue) }),
    onSelect: () => void submit(queue.queueId, false),
  }))
  const laterMenu: MenuEntry[] = queues.map((queue) => ({
    type: 'item',
    key: queue.queueId,
    label: t('manifestLaterToQueue', { name: queueLabel(t, queue) }),
    onSelect: () => void submit(queue.queueId, true),
  }))
  const startQueue = queues.find((queue) => queue.queueId === base.queueId)
  const laterQueue = queues.find((queue) => queue.queueId === LATER_QUEUE_ID)

  const summary =
    stat.count === 0
      ? t('manifestNoSelection')
      : stat.unknownCount > 0
        ? `${t('manifestSelectedSummary', { count: stat.count, size: formatBytes(stat.size) })} ${t('manifestUnknownSizeNote', { count: stat.unknownCount })}`
        : t('manifestSelectedSummary', { count: stat.count, size: formatBytes(stat.size) })

  return (
    <>
      <Dialog
        open
        onOpenChange={(open) => !open && !submitting && onCancel()}
        size="xl"
        modalLocked={submitting}
        title={t('manifestDialogTitle')}
        description={`${t('manifestSummary', { count: items.length, size: formatBytes(totalSize) })}${host ? ` · ${host}` : ''}`}
        footer={
          <div className="flex flex-wrap items-center justify-between gap-2">
            <FieldHint className="tabular min-w-0 truncate">{summary}</FieldHint>
            <DialogFooter className="ml-auto flex-wrap">
              <Button variant="outline" className="mobile:hidden" disabled={submitting} onClick={onCancel}>
                {t('cancel')}
              </Button>
              <SplitButton
                label={t('downloadLater')}
                tooltip={laterQueue ? t('laterIntoQueueTooltip', { name: queueLabel(t, laterQueue) }) : undefined}
                disabled={stat.count === 0 || submitting}
                menu={laterMenu}
                menuTitle={t('downloadLater')}
                menuLabel={t('downloadLater')}
                onClick={() => void submit(LATER_QUEUE_ID, true)}
              />
              <SplitButton
                variant="primary"
                label={t('manifestStartDownloadWithCount', { count: stat.count })}
                tooltip={startQueue ? t('startIntoQueueTooltip', { name: queueLabel(t, startQueue) }) : undefined}
                disabled={stat.count === 0}
                loading={submitting}
                menu={startMenu}
                menuTitle={t('startDownload')}
                menuLabel={t('startDownload')}
                onClick={() => void submit(base.queueId, false)}
              />
            </DialogFooter>
          </div>
        }
      >
        <div className="flex flex-col gap-3">
          <FormField label={t('manifestGroupNameTooltip')} htmlFor="manifest-group-name">
            <Input id="manifest-group-name" value={groupName} onChange={(event) => setGroupName(event.target.value)} placeholder={t('manifestGroupNamePlaceholder')} spellCheck={false} />
          </FormField>
          <FormField label={t('saveDir')} htmlFor="manifest-save-dir">
            <InputWithAction
              input={<Input id="manifest-save-dir" value={saveDir} onChange={(event) => setSaveDir(event.target.value)} spellCheck={false} />}
              action={<Button onClick={() => setPickerOpen(true)}>{t('browse')}</Button>}
            />
          </FormField>

          <div className="flex flex-col gap-2">
            <Input value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t('manifestSearchPlaceholder')} spellCheck={false} />
            {topExtensions.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {topExtensions.map((chip) => (
                  <button
                    key={chip.ext}
                    type="button"
                    onClick={() => toggleExt(chip.ext)}
                    className={cn(
                      'inline-flex min-h-7 items-center gap-1 rounded-full border px-2.5 text-xs coarse:min-h-touch',
                      extFilter.has(chip.ext) ? 'border-primary bg-accent text-accent-text' : 'border-border text-muted-foreground hover:bg-row-hover',
                    )}
                  >
                    {chip.ext} <span className="tabular text-text-tertiary">{chip.count}</span>
                  </button>
                ))}
              </div>
            ) : null}
            <div className="flex flex-wrap items-center gap-1">
              <Button variant="ghost" onClick={() => setSelected(manifestSelectAllVisible(items, filter))}>
                {t('manifestSelectAll')}
              </Button>
              <Button variant="ghost" onClick={() => setSelected(manifestInvertVisibleSelection(items, selected, filter))}>
                {t('manifestInvertSelection')}
              </Button>
              <Button variant="ghost" onClick={() => setSelected(new Set())}>
                {t('manifestClearSelection')}
              </Button>
              <Button variant="ghost" className="ml-auto" onClick={() => setSortKey(sortKey === 'name' ? 'size' : 'name')}>
                {sortKey === 'size' ? t('manifestSortBySizeDesc') : t('manifestSortByName')}
              </Button>
            </div>
          </div>

          <nav className="flex min-h-control flex-wrap items-center gap-1 text-sm" aria-label="breadcrumb">
            {breadcrumb.searching ? (
              <span className="text-muted-foreground">{t('manifestSearchResultCount', { count: breadcrumb.searchResultCount })}</span>
            ) : (
              <>
                {breadcrumb.showUp ? (
                  <Button variant="ghost" iconOnly title={t('manifestBreadcrumbUpTooltip')} aria-label={t('manifestBreadcrumbUpTooltip')} onClick={() => navigate(manifestUpPath({ items, cwd: rowsResult.cwd, extFilter }))}>
                    <Icon icon={ArrowLeft} />
                  </Button>
                ) : null}
                {breadcrumb.segments.map((segment, index) => (
                  <span key={`${segment.kind}:${segment.path}:${index}`} className="inline-flex items-center gap-1">
                    {index > 0 ? <Icon icon={ChevronRight} size="sm" className="text-text-tertiary" /> : null}
                    {segment.kind === 'ellipsis' ? (
                      <ActionMenu
                        title={t('manifestBreadcrumbMoreTooltip')}
                        align="start"
                        entries={breadcrumb.overflowSegments.map((hidden) => ({ type: 'item' as const, key: hidden.path, label: hidden.label, onSelect: () => navigate(hidden.path) }))}
                        trigger={
                          <Button variant="ghost" iconOnly title={t('manifestBreadcrumbMoreTooltip')} aria-label={t('manifestBreadcrumbMoreTooltip')}>
                            <Icon icon={MoreHorizontal} />
                          </Button>
                        }
                      />
                    ) : (
                      <button
                        type="button"
                        disabled={segment.isLast}
                        onClick={() => navigate(segment.path)}
                        className={cn('inline-flex min-h-control items-center rounded-sm px-1.5 coarse:min-h-touch', segment.isLast ? 'font-medium text-foreground' : 'text-muted-foreground hover:bg-nav-hover')}
                      >
                        {segment.kind === 'home' ? <Icon icon={Home} /> : segment.label}
                      </button>
                    )}
                  </span>
                ))}
              </>
            )}
          </nav>

          <div className="rounded-md border border-hairline p-1">
            {rowsResult.rows.length === 0 ? (
              <div className="px-3 py-8 text-center text-xs text-muted-foreground">{t('manifestTreeEmpty')}</div>
            ) : (
              rowsResult.rows.map((entry) => {
                if (entry.kind === 'dir') {
                  const row = entry.row
                  return (
                    <div key={`d:${row.path}`} className="flex min-h-control items-center gap-2 rounded-sm px-2 hover:bg-row-hover coarse:min-h-touch">
                      <Checkbox
                        checked={manifestDirRowCheckState(row) === 'checked' ? true : manifestDirRowCheckState(row) === 'indeterminate' ? 'indeterminate' : false}
                        onCheckedChange={() => setSelected(manifestToggleDirSubtree({ items, dirPath: row.path, selectedItemIds: selected, extFilter, search }))}
                        aria-label={row.labels.join(' / ')}
                      />
                      <button type="button" className="flex min-h-control min-w-0 flex-1 items-center gap-2 text-left text-sm coarse:min-h-touch" onClick={() => navigate(row.path)}>
                        <Icon icon={Folder} className="text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate">
                          {row.labels.map((label, index) => (
                            <span key={`${label}:${index}`} className={index === row.labels.length - 1 ? 'font-medium' : 'text-muted-foreground'}>
                              {index > 0 ? ' / ' : ''}
                              {label}
                            </span>
                          ))}
                        </span>
                        <span className="tabular shrink-0 text-xs text-muted-foreground">
                          {t('manifestItemsCount', { count: row.count })} · {row.unknown && row.size === 0 ? t('manifestDirSizeUnknown') : formatBytes(row.size)}
                        </span>
                        <Icon icon={ChevronRight} size="md" className="text-text-tertiary" />
                      </button>
                    </div>
                  )
                }
                const { item, showPath } = entry.row
                const checked = selected.has(item.id)
                return (
                  <label
                    key={`f:${item.id}`}
                    className={cn('flex min-h-control cursor-pointer items-center gap-2 rounded-sm px-2 text-sm coarse:min-h-touch', checked ? 'bg-accent' : 'hover:bg-row-hover')}
                  >
                    <Checkbox checked={checked} onCheckedChange={() => toggleFile(item.id)} />
                    <Icon icon={FileIcon} className="text-muted-foreground" />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{item.name}</span>
                      {showPath && item.path !== '' ? <span className="block truncate text-xs text-text-tertiary">{item.path}</span> : null}
                    </span>
                    <span className="tabular shrink-0 text-xs text-muted-foreground">{item.size > 0 ? formatBytes(item.size) : t('manifestFileSizeUnknown')}</span>
                  </label>
                )
              })
            )}
          </div>
        </div>
      </Dialog>
      <FsPickerDialog open={pickerOpen} onOpenChange={setPickerOpen} initialPath={saveDir} onSelect={setSaveDir} />
    </>
  )
}
