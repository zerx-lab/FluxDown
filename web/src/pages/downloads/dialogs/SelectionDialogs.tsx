// 引擎发起的交互选择（HLS 画质 / BT 文件 / 插件变体）。
// 数据来自 `DaemonSnapshot.pendingSelections`（连接 hello 已声明 `client.selections`），
// 回答用 `daemon.selection.resolve`；服务端 `SelectionResolved` 事件把请求移出快照后对话框自然关闭。
// 与 GPUI `pages/selection.rs` 对齐：默认选择取 `defaultChoice`，超过 `deadlineUnixMs` 服务端按默认自动决定，
// HLS 没有取消入口。

import { ChevronDown, ChevronRight } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useT } from '../../../i18n'
import { hasSelectionDeadline, rpc, useAgent, useTasks } from '../../../lib/rpc'
import type { BtFileDto, HlsQualityOptionDto, ResolveVariantOptionDto, SelectionKind, SelectionOutcome, SelectionRequestDto } from '../../../lib/rpc'
import { SAVE_DIR_HISTORY_ENABLED_PREF, SAVE_DIR_HISTORY_PREF, readSaveDirHistory, saveDirChoices } from '../../../lib/saveDirHistory'
import { Button, Checkbox, Dialog, DialogFooter, FieldHint, FormField, Icon, Input } from '../../../ui'
import { toastRpcError } from '../../../lib/rpcToast'
import { cn } from '../../../lib/cn'
import { formatBytes, useNow } from './utils'
import { PluginChoiceDialog } from './PluginChoiceDialog'
import { SaveDirCombo } from './SaveDirCombo'
import { btSelectionCanConfirm, btSelectionSaveDir } from './btSelection'

type ChoiceKind = Exclude<SelectionKind, { type: 'fileExists' | 'plugin' }>

/** 一次只展示队首请求；其余排队，前一个解决后自动轮到下一个。`fileExists` 由 `FileConflictDialog` 聚合处理，这里跳过。 */
export function SelectionHost({ requests }: { requests: readonly SelectionRequestDto[] }) {
  for (const request of requests) {
    const kind = request.kind
    if (kind.type === 'fileExists') continue
    if (kind.type === 'plugin') {
      return <PluginChoiceDialog key={request.requestId} request={request} site={kind.site} url={kind.url} candidates={kind.candidates} />
    }
    return <SelectionDialog key={request.requestId} request={request} kind={kind} />
  }
  return null
}

function defaultIndex(request: SelectionRequestDto, kind: 'hls' | 'variant', first: number): number {
  const choice = request.defaultChoice
  return choice.kind === kind ? choice.index : first
}

function SelectionDialog({ request, kind }: { request: SelectionRequestDto; kind: ChoiceKind }) {
  const t = useT()
  const tasks = useTasks()
  const hasDeadline = hasSelectionDeadline(request)
  // 无截止（用户关闭了自动选择默认项）时不显示倒计时，也不必每秒重绘文件列表。
  const now = useNow(hasDeadline ? 1000 : null)
  const [submitting, setSubmitting] = useState(false)
  const [hlsIndex, setHlsIndex] = useState(() => defaultIndex(request, 'hls', kind.type === 'hls' ? (kind.options[0]?.index ?? 0) : 0))
  const [variantIndex, setVariantIndex] = useState(() => defaultIndex(request, 'variant', kind.type === 'variant' ? (kind.options[0]?.index ?? 0) : 0))
  const [btSelected, setBtSelected] = useState<ReadonlySet<number>>(() => {
    const choice = request.defaultChoice
    if (choice.kind === 'bt' && choice.indices.length > 0) return new Set(choice.indices)
    return new Set(kind.type === 'bt' ? kind.files.map((file) => file.index) : [])
  })
  const btPrefill = kind.type === 'bt' ? (kind.saveDir ?? '') : ''
  const [btSaveDir, setBtSaveDir] = useState(btPrefill)
  const historyEnabled = useAgent((snapshot) => snapshot.preferences.values[SAVE_DIR_HISTORY_ENABLED_PREF] !== false, true)
  const historyValue = useAgent((snapshot) => snapshot.preferences.values[SAVE_DIR_HISTORY_PREF], undefined)
  const saveDirHistory = useMemo(() => (historyEnabled ? readSaveDirHistory(historyValue) : []), [historyEnabled, historyValue])

  const task = tasks.find((item) => item.taskId === request.taskId)
  const taskName = task?.fileName || task?.originUrl || task?.url || ''
  const remaining = hasDeadline ? Math.max(0, Math.floor((request.deadlineUnixMs - now) / 1000)) : null
  const cancellable = kind.type !== 'hls'

  const resolve = async (next: SelectionOutcome) => {
    if (submitting) return
    setSubmitting(true)
    try {
      await rpc.daemon.selection.resolve({ requestId: request.requestId, outcome: next })
    } catch (error) {
      toastRpcError(error)
      setSubmitting(false)
    }
  }

  const confirm = () => {
    if (kind.type === 'hls') void resolve({ kind: 'hls', index: hlsIndex })
    else if (kind.type === 'variant') void resolve({ kind: 'variant', index: variantIndex })
    else {
      const saveDir = btSelectionSaveDir(btPrefill, btSaveDir)
      void resolve({ kind: 'bt', indices: [...btSelected].sort((a, b) => a - b), ...(saveDir ? { saveDir } : {}) })
    }
  }

  let title: string
  let description: string
  if (kind.type === 'hls') {
    title = t('hlsQualityTitle')
    description = t('hlsQualityDesc')
  } else if (kind.type === 'bt') {
    title = t('btFileSelectTitle')
    description = kind.files.length === 1 ? t('btFileSelectDescSingle') : t('btFileSelectDesc', { count: kind.files.length })
  } else {
    title = t('resolveVariantTitle')
    description = t('resolveVariantDesc')
  }

  // BT：`indices` 为空数组在协议里表示「全部文件」，所以没有勾选时必须禁用确认。
  const btSize = kind.type === 'bt' ? kind.files.filter((file) => btSelected.has(file.index)).reduce((sum, file) => sum + Math.max(0, file.size), 0) : 0
  const confirmLabel = kind.type === 'bt' ? t('btFileSelectConfirm', { count: btSelected.size, size: formatBytes(btSize) }) : t('confirm')
  const canConfirm = kind.type !== 'bt' || btSelectionCanConfirm(btPrefill, btSaveDir, btSelected.size)

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && cancellable) void resolve({ kind: 'cancelled' })
      }}
      modalLocked={!cancellable}
      size={kind.type === 'bt' ? 'xl' : 'md'}
      title={title}
      description={taskName ? `${description} · ${taskName}` : description}
      footer={
        <div className="flex flex-wrap items-center justify-between gap-2">
          {remaining === null ? <span /> : <FieldHint className="tabular">{t('selectionAutoDefaultIn', { seconds: remaining })}</FieldHint>}
          <DialogFooter className="ml-auto">
            {cancellable ? (
              <Button variant="outline" disabled={submitting} onClick={() => void resolve({ kind: 'cancelled' })}>
                {t('cancel')}
              </Button>
            ) : null}
            <Button variant="primary" disabled={submitting || !canConfirm} onClick={confirm}>
              {confirmLabel}
            </Button>
          </DialogFooter>
        </div>
      }
    >
      {kind.type === 'hls' ? (
        <HlsOptions options={kind.options} value={hlsIndex} onChange={setHlsIndex} />
      ) : kind.type === 'variant' ? (
        <VariantOptions options={kind.options} value={variantIndex} onChange={setVariantIndex} />
      ) : (
        <div className="flex flex-col gap-3">
          <FormField label={t('saveDir')} htmlFor="bt-selection-save-dir">
            <SaveDirCombo
              id="bt-selection-save-dir"
              value={btSaveDir}
              placeholder={t('selectSaveDir')}
              choices={saveDirChoices([], saveDirHistory, btSaveDir)}
              label={t('saveDirRecent')}
              onChange={setBtSaveDir}
            />
          </FormField>
          <BtFiles files={kind.files} selected={btSelected} onSelectedChange={setBtSelected} />
        </div>
      )}
    </Dialog>
  )
}

// ── HLS / 变体：单选列表 ──

function OptionRowButton({ selected, onSelect, children }: { selected: boolean; onSelect: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      onClick={onSelect}
      className={cn(
        'flex min-h-control w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm coarse:min-h-touch',
        selected ? 'bg-accent' : 'hover:bg-row-hover',
      )}
    >
      <span
        className={cn('flex size-4 shrink-0 items-center justify-center rounded-full border', selected ? 'border-primary' : 'border-input')}
        aria-hidden
      >
        {selected ? <span className="size-2 rounded-full bg-primary" /> : null}
      </span>
      <span className="min-w-0 flex-1">{children}</span>
    </button>
  )
}

function HlsOptions({ options, value, onChange }: { options: readonly HlsQualityOptionDto[]; value: number; onChange: (index: number) => void }) {
  return (
    <div role="radiogroup" className="flex flex-col gap-0.5">
      {options.map((option) => (
        <OptionRowButton key={option.index} selected={option.index === value} onSelect={() => onChange(option.index)}>
          <span className="tabular">
            {option.width}×{option.height} · {Math.round(option.bandwidth / 1000)} kbps
          </span>
        </OptionRowButton>
      ))}
    </div>
  )
}

function VariantOptions({
  options,
  value,
  onChange,
}: {
  options: readonly ResolveVariantOptionDto[]
  value: number
  onChange: (index: number) => void
}) {
  return (
    <div role="radiogroup" className="flex flex-col gap-0.5">
      {options.map((option) => (
        <OptionRowButton key={option.index} selected={option.index === value} onSelect={() => onChange(option.index)}>
          <div className="truncate text-sm">{option.label}</div>
          <div className="tabular truncate text-xs text-muted-foreground">
            {[option.container, option.width > 0 && option.height > 0 ? `${option.width}×${option.height}` : '', option.totalBytes > 0 ? formatBytes(option.totalBytes) : '']
              .filter((part) => part !== '')
              .join(' · ')}
          </div>
        </OptionRowButton>
      ))}
    </div>
  )
}

// ── BT：文件树 ──

interface DirNode {
  name: string
  path: string
  dirs: Map<string, DirNode>
  files: BtFileDto[]
}

type TreeRow =
  | { type: 'dir'; key: string; name: string; depth: number; path: string; indices: number[]; size: number; collapsed: boolean }
  | { type: 'file'; key: string; depth: number; file: BtFileDto; name: string }

function buildTree(files: readonly BtFileDto[]): DirNode {
  const root: DirNode = { name: '', path: '', dirs: new Map(), files: [] }
  for (const file of files) {
    const parts = file.path.split('/').filter((part) => part !== '')
    let node = root
    for (const part of parts.slice(0, -1)) {
      let next = node.dirs.get(part)
      if (!next) {
        next = { name: part, path: node.path === '' ? part : `${node.path}/${part}`, dirs: new Map(), files: [] }
        node.dirs.set(part, next)
      }
      node = next
    }
    node.files.push(file)
  }
  return root
}

function collectVisible(node: DirNode, matches: (file: BtFileDto) => boolean): BtFileDto[] {
  const own = node.files.filter(matches)
  for (const child of node.dirs.values()) own.push(...collectVisible(child, matches))
  return own
}

function flatten(node: DirNode, depth: number, matches: (file: BtFileDto) => boolean, collapsed: ReadonlySet<string>, filtering: boolean, out: TreeRow[]): void {
  for (const child of node.dirs.values()) {
    const visible = collectVisible(child, matches)
    if (visible.length === 0) continue
    const isCollapsed = !filtering && collapsed.has(child.path)
    out.push({
      type: 'dir',
      key: `d:${child.path}`,
      name: child.name,
      depth,
      path: child.path,
      indices: visible.map((file) => file.index),
      size: visible.reduce((sum, file) => sum + Math.max(0, file.size), 0),
      collapsed: isCollapsed,
    })
    if (!isCollapsed) flatten(child, depth + 1, matches, collapsed, filtering, out)
  }
  for (const file of node.files) {
    if (!matches(file)) continue
    out.push({ type: 'file', key: `f:${file.index}`, depth, file, name: file.path.split('/').pop() ?? file.path })
  }
}

function BtFiles({
  files,
  selected,
  onSelectedChange,
}: {
  files: readonly BtFileDto[]
  selected: ReadonlySet<number>
  onSelectedChange: (selected: ReadonlySet<number>) => void
}) {
  const t = useT()
  const [filter, setFilter] = useState('')
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set())

  const tree = useMemo(() => buildTree(files), [files])
  const needle = filter.trim().toLowerCase()
  const rows = useMemo(() => {
    const out: TreeRow[] = []
    const matches = (file: BtFileDto) => needle === '' || file.path.toLowerCase().includes(needle)
    flatten(tree, 0, matches, collapsed, needle !== '', out)
    return out
  }, [tree, needle, collapsed])

  const selectedSize = files.filter((file) => selected.has(file.index)).reduce((sum, file) => sum + Math.max(0, file.size), 0)

  const setMany = (indices: readonly number[], checked: boolean) => {
    const next = new Set(selected)
    for (const index of indices) {
      if (checked) next.add(index)
      else next.delete(index)
    }
    onSelectedChange(next)
  }
  const visibleIndices = rows.flatMap((row) => (row.type === 'file' ? [row.file.index] : []))

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" onClick={() => onSelectedChange(new Set(files.map((file) => file.index)))}>
          {t('btFileSelectAll')}
        </Button>
        <Button variant="outline" onClick={() => onSelectedChange(new Set())}>
          {t('deselectAll')}
        </Button>
        <span className="tabular ml-auto text-xs text-muted-foreground">
          {t('selectedCount', { n: selected.size })} · {formatBytes(selectedSize)}
        </span>
      </div>
      {files.length > 8 ? (
        <Input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder={t('webSelectionFilterPlaceholder')} spellCheck={false} />
      ) : null}
      <div className="max-h-[52dvh] overflow-y-auto rounded-md border border-hairline p-1 mobile:max-h-none">
        {rows.map((row) => {
          const indent = { paddingLeft: `${4 + Math.min(row.depth, 6) * 16}px` }
          if (row.type === 'dir') {
            const count = row.indices.filter((index) => selected.has(index)).length
            const state = count === 0 ? false : count === row.indices.length ? true : 'indeterminate'
            return (
              <div key={row.key} className="flex min-h-control items-center gap-1 rounded-sm pr-2 hover:bg-row-hover coarse:min-h-touch" style={indent}>
                <button
                  type="button"
                  className="flex size-6 shrink-0 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground coarse:size-touch"
                  aria-label={row.name}
                  onClick={() =>
                    setCollapsed((prev) => {
                      const next = new Set(prev)
                      if (!next.delete(row.path)) next.add(row.path)
                      return next
                    })
                  }
                >
                  <Icon icon={row.collapsed ? ChevronRight : ChevronDown} size="md" />
                </button>
                <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 text-sm">
                  <Checkbox checked={state} onCheckedChange={(checked) => setMany(row.indices, checked)} />
                  <span className="min-w-0 flex-1 truncate font-medium">{row.name}</span>
                  <span className="tabular shrink-0 text-xs text-muted-foreground">{formatBytes(row.size)}</span>
                </label>
              </div>
            )
          }
          const checked = selected.has(row.file.index)
          return (
            <label
              key={row.key}
              className={cn('flex min-h-control cursor-pointer items-center gap-2 rounded-sm pr-2 text-sm coarse:min-h-touch', checked ? 'bg-accent' : 'hover:bg-row-hover')}
              style={{ ...indent, paddingLeft: `${4 + 24 + Math.min(row.depth, 6) * 16}px`, contentVisibility: 'auto', containIntrinsicSize: 'auto 32px' }}
            >
              <Checkbox checked={checked} onCheckedChange={(next) => setMany([row.file.index], next)} />
              <span className="min-w-0 flex-1 truncate" title={row.file.path}>
                {row.name}
              </span>
              <span className="tabular shrink-0 text-xs text-muted-foreground">{formatBytes(Math.max(0, row.file.size))}</span>
            </label>
          )
        })}
      </div>
      {needle !== '' && visibleIndices.length > 0 ? (
        <div className="flex gap-2">
          <Button variant="ghost" onClick={() => setMany(visibleIndices, true)}>
            {t('manifestSelectAll')}
          </Button>
          <Button variant="ghost" onClick={() => setMany(visibleIndices, false)}>
            {t('manifestClearSelection')}
          </Button>
        </div>
      ) : null}
    </div>
  )
}
