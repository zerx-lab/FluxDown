import { useVirtualizer } from '@tanstack/react-virtual'
import { Files, Pen, SlidersHorizontal } from 'lucide-react'
import { useMemo, useRef, useState } from 'react'
import type { MouseEvent, RefObject } from 'react'
import { useT } from '../../../i18n'
import { cn } from '../../../lib/cn'
import { rpc } from '../../../lib/rpc'
import type { BtFilePriorityDto, BtTaskFileDto } from '../../../lib/rpc'
import { rpcErrorText } from '../../../lib/rpcErrorText'
import { toastRpcError } from '../../../lib/rpcToast'
import { ConfirmFooter, ContextMenuArea, Dialog, EmptyState, FieldError, Form, FormField, Input, ProgressBar, Select, toast } from '../../../ui'
import type { MenuEntry, SelectOption } from '../../../ui'
import { formatBytes } from '../model/task'
import type { Translate } from '../table/text'
import { PollError, SkeletonRows, useAncestorScroller, useMeasuredRoot } from './btParts'
import {
  applyChanges,
  clickSelection,
  EMPTY_FILE_SELECTION,
  leavesNothingToDownload,
  pathEllipsisParts,
  PRIORITY_LEVELS,
  priorityChanges,
  priorityLabelKey,
  priorityLevel,
  progressFraction,
  progressPercentText,
  pruneSelection,
  renameTarget,
  targetLevel,
  targetsFor,
} from './btFiles'
import type { FileSelection, PriorityLevel } from './btFiles'
import { useBtFiles } from './useBtFiles'

/** 低于此宽度（px）改用两行卡片：名称一行，大小 / 进度 / 优先级一行。 */
const TABLE_MIN_WIDTH = 560
const GRID = 'grid-cols-[minmax(0,1fr)_5.5rem_8rem_8.5rem]'
const ROW_OVERSCAN = 10
const ROW_ESTIMATE = { table: 40, card: 64 } as const
type FilesLayout = keyof typeof ROW_ESTIMATE

/** 一次改动的回调集合（行、菜单、批量下拉共用）。 */
interface FileActions {
  editable: boolean
  setPriority: (targets: ReadonlySet<number>, level: PriorityLevel) => void
  rename: (file: BtTaskFileDto) => void
}

/**
 * BT 任务详情「文件」页签：虚拟化文件表（名称中间省略、大小、进度、优先级）、多选批量改优先级、
 * 行右键菜单（优先级子菜单 + 单选「重命名…」）。`editable=false`（已完成 / 无元数据）时只读。
 * 只在页签可见时挂载，`useBtFiles` 随之 2 s 轮询。
 */
export function BtFilesTab({ taskId }: { taskId: string }) {
  const t = useT()
  const { data, error, update } = useBtFiles(taskId)
  const { ref, width } = useMeasuredRoot()
  const [rawSelection, setSelection] = useState<FileSelection>(EMPTY_FILE_SELECTION)
  const [renaming, setRenaming] = useState<BtTaskFileDto | null>(null)

  const order = useMemo(() => (data ? data.files.map((file) => file.index) : []), [data])
  // 轮询后下标消失的文件从选区剪除（派生，不回写状态）。
  const selection = useMemo(() => pruneSelection(rawSelection, order), [rawSelection, order])

  const send = (changes: BtFilePriorityDto[]) => {
    update((current) => ({ ...current, files: applyChanges(current.files, changes) }))
    rpc.daemon.task.setFilePriorities({ taskId, priorities: changes }).catch(toastRpcError)
  }

  const actions: FileActions = {
    editable: data?.editable ?? false,
    setPriority: (targets, level) => {
      if (!data || !data.editable) return
      const changes = priorityChanges(data.files, targets, level)
      if (changes.length === 0) return
      if (leavesNothingToDownload(data.files, changes)) {
        toast.warning(t('btFilesKeepOne'))
        return
      }
      send(changes)
    },
    rename: (file) => {
      if (data?.editable) setRenaming(file)
    },
  }

  const layout: FilesLayout = width === 0 || width >= TABLE_MIN_WIDTH ? 'table' : 'card'
  const options: SelectOption<PriorityLevel>[] = PRIORITY_LEVELS.map((level) => ({ value: level, label: t(priorityLabelKey(level)) }))

  let body
  if (data === null) {
    body = error === null ? <SkeletonRows rows={3} /> : null
  } else if (data.files.length === 0) {
    body = <EmptyState icon={Files} title={t('btFilesEmpty')} className="py-6" />
  } else {
    const batch = targetLevel(data.files, selection.selected)
    body = (
      <>
        {data.editable ? (
          <div className="flex items-center gap-2">
            <span className="text-xs text-text-tertiary">{t('btFileColumnPriority')}</span>
            <Select<PriorityLevel>
              value={batch === null || batch === 'mixed' ? '' : batch}
              placeholder={t('btFilePriorityMixed')}
              options={options}
              disabled={selection.selected.size === 0}
              aria-label={t('btFileColumnPriority')}
              className="w-36"
              onValueChange={(level) => actions.setPriority(selection.selected, level)}
            />
          </div>
        ) : null}
        <div className="flex flex-col">
          {layout === 'table' ? (
            <div className={cn('grid gap-x-3 border-b border-hairline px-1 pb-1 text-xs text-text-tertiary', GRID)}>
              <span>{t('btFileColumnName')}</span>
              <span className="text-right">{t('btFileColumnSize')}</span>
              <span>{t('btFileColumnProgress')}</span>
              <span>{t('btFileColumnPriority')}</span>
            </div>
          ) : null}
          <VirtualFiles
            files={data.files}
            order={order}
            layout={layout}
            selection={selection}
            onSelect={setSelection}
            actions={actions}
            options={options}
            t={t}
            rootRef={ref}
          />
        </div>
      </>
    )
  }

  return (
    <div ref={ref} className="flex flex-col gap-2">
      <PollError message={error} />
      {body}
      {renaming ? <RenameFileDialog taskId={taskId} file={renaming} onClose={() => setRenaming(null)} onRenamed={(path) => update((current) => ({ ...current, files: current.files.map((file) => (file.index === renaming.index ? { ...file, path } : file)) }))} /> : null}
    </div>
  )
}

function VirtualFiles({
  files,
  order,
  layout,
  selection,
  onSelect,
  actions,
  options,
  t,
  rootRef,
}: {
  files: readonly BtTaskFileDto[]
  order: readonly number[]
  layout: FilesLayout
  selection: FileSelection
  onSelect: (next: FileSelection) => void
  actions: FileActions
  options: readonly SelectOption<PriorityLevel>[]
  t: Translate
  rootRef: RefObject<HTMLDivElement | null>
}) {
  const listRef = useRef<HTMLDivElement | null>(null)
  const { scrollEl, scrollMargin } = useAncestorScroller(listRef, rootRef)
  const virtualizer = useVirtualizer({
    count: files.length,
    getScrollElement: () => scrollEl,
    estimateSize: () => ROW_ESTIMATE[layout],
    getItemKey: (position) => files[position]?.index ?? position,
    overscan: ROW_OVERSCAN,
    scrollMargin,
  })

  return (
    <div ref={listRef} role="listbox" aria-multiselectable className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
      {virtualizer.getVirtualItems().map((item) => {
        const file = files[item.index]
        if (file === undefined) return null
        return (
          <div
            key={item.key}
            ref={virtualizer.measureElement}
            data-index={item.index}
            className="absolute left-0 top-0 w-full"
            style={{ transform: `translateY(${item.start - scrollMargin}px)` }}
          >
            <FileRow
              file={file}
              files={files}
              layout={layout}
              selected={selection.selected.has(file.index)}
              onClick={(event) => onSelect(clickSelection(selection, order, file.index, event.shiftKey, event.metaKey || event.ctrlKey))}
              targets={() => targetsFor(selection, file.index)}
              actions={actions}
              options={options}
              t={t}
            />
          </div>
        )
      })}
    </div>
  )
}

function FileRow({
  file,
  files,
  layout,
  selected,
  onClick,
  targets,
  actions,
  options,
  t,
}: {
  file: BtTaskFileDto
  files: readonly BtTaskFileDto[]
  layout: FilesLayout
  selected: boolean
  onClick: (event: MouseEvent) => void
  targets: () => ReadonlySet<number>
  actions: FileActions
  options: readonly SelectOption<PriorityLevel>[]
  t: Translate
}) {
  const level = priorityLevel(file.priority)
  const fraction = progressFraction(file.downloaded, file.size)
  const menu = (): MenuEntry[] => {
    const set = targets()
    const current = targetLevel(files, set)
    const entries: MenuEntry[] = [
      {
        type: 'sub',
        key: 'priority',
        label: t('btFileColumnPriority'),
        icon: SlidersHorizontal,
        entries: PRIORITY_LEVELS.map((option) => ({
          type: 'check',
          key: option,
          label: t(priorityLabelKey(option)),
          checked: current === option,
          onSelect: () => actions.setPriority(set, option),
        })),
      },
    ]
    if (set.size === 1) entries.push({ type: 'item', key: 'rename', label: t('btFileRename'), icon: Pen, onSelect: () => actions.rename(file) })
    return entries
  }

  const priority = actions.editable ? (
    // 下拉自身的点击不改变行选区。
    <div onClick={(event) => event.stopPropagation()} onPointerDown={(event) => event.stopPropagation()}>
      <Select<PriorityLevel>
        value={level}
        options={options}
        aria-label={t('btFileColumnPriority')}
        className="h-7 w-full"
        onValueChange={(next) => actions.setPriority(targets(), next)}
      />
    </div>
  ) : (
    <span className="truncate text-xs text-muted-foreground">{t(priorityLabelKey(level))}</span>
  )
  const progress = (
    <div className="flex min-w-0 items-center gap-2" title={`${formatBytes(file.downloaded)} / ${formatBytes(file.size)}`}>
      <ProgressBar value={fraction} className="min-w-0 flex-1" />
      <span className="shrink-0 tabular-nums text-xs text-muted-foreground">{progressPercentText(fraction)}</span>
    </div>
  )

  return (
    <ContextMenuArea
      entries={menu}
      title={file.path}
      disabled={!actions.editable}
      className={cn('border-b border-hairline px-1 select-none', selected ? 'bg-accent' : 'hover:bg-row-hover')}
    >
      <div role="option" aria-selected={selected} onClick={onClick} className={layout === 'table' ? cn('grid items-center gap-x-3 py-1.5', GRID) : 'flex flex-col gap-1 py-2'}>
        <FileName path={file.path} />
        {layout === 'table' ? (
          <>
            <span className="text-right tabular-nums text-xs text-muted-foreground">{formatBytes(file.size)}</span>
            {progress}
            {priority}
          </>
        ) : (
          <div className="grid grid-cols-[5.5rem_minmax(0,1fr)_8.5rem] items-center gap-x-3">
            <span className="tabular-nums text-xs text-muted-foreground">{formatBytes(file.size)}</span>
            {progress}
            {priority}
          </div>
        )}
      </div>
    </ContextMenuArea>
  )
}

/** 单行文件路径：头部可截断、尾部（文件名末尾含扩展名）固定，悬停 / 长按菜单标题看全路径。含 RTL 文字时整体尾部截断。 */
function FileName({ path }: { path: string }) {
  const { head, tail } = pathEllipsisParts(path)
  return (
    <span className="flex min-w-0 text-sm" title={path}>
      {head === '' ? null : (
        <span className="min-w-0 truncate" dir={tail === '' ? 'auto' : undefined} style={{ whiteSpace: 'pre' }}>
          {head}
        </span>
      )}
      {tail === '' ? null : (
        <span className="shrink-0" style={{ whiteSpace: 'pre' }}>
          {tail}
        </span>
      )}
    </span>
  )
}

/** 「重命名文件」：输入任务目录内的新相对路径（可含子目录），原样下发 `daemon.task.renameBtFile`。 */
function RenameFileDialog({ taskId, file, onClose, onRenamed }: { taskId: string; file: BtTaskFileDto; onClose: () => void; onRenamed: (path: string) => void }) {
  const t = useT()
  const [value, setValue] = useState(file.path)
  const [error, setError] = useState('')
  const [pending, setPending] = useState(false)

  const confirm = async () => {
    if (pending) return
    const target = renameTarget(value, file.path)
    if (target.kind === 'invalid') return
    if (target.kind === 'unchanged') {
      onClose()
      return
    }
    setPending(true)
    setError('')
    try {
      await rpc.daemon.task.renameBtFile({ taskId, index: file.index, path: target.path })
      onRenamed(target.path)
      onClose()
    } catch (err) {
      setError(rpcErrorText(err, t))
      setPending(false)
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      size="md"
      title={t('btFileRenameTitle')}
      footer={<ConfirmFooter okLabel={t('confirm')} okDisabled={value.trim() === ''} loading={pending} onCancel={onClose} onOk={() => void confirm()} />}
    >
      <Form onSubmit={() => void confirm()}>
        <FormField label={t('btFileColumnName')} htmlFor="bt-file-rename" hint={t('btFileRenameHint')}>
          <Input
            id="bt-file-rename"
            value={value}
            spellCheck={false}
            autoFocus
            autoComplete="off"
            onFocus={(event) => {
              // 默认只选中文件名主干（不含扩展名），与系统文件管理器一致。
              const input = event.target
              const slash = input.value.lastIndexOf('/') + 1
              const dot = input.value.lastIndexOf('.')
              input.setSelectionRange(slash, dot > slash ? dot : input.value.length)
            }}
            onChange={(event) => setValue(event.target.value)}
          />
        </FormField>
        {error ? <FieldError className="break-all">{error}</FieldError> : null}
        <button type="submit" className="hidden" tabIndex={-1} aria-hidden />
      </Form>
    </Dialog>
  )
}
