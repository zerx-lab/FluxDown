// 「文件已存在」聚合对话框：所有待确认的 `fileExists` 请求归为一个对话框。
// 单条 = 已有 / 新下载对比卡 + 三个动作；多条 = 列表（每行动作）+ 底部批量按钮。
// 关闭 / Esc = 稍后决定（本地隐藏，不发 RPC，请求继续等到超时按默认重命名）；
// 只有「取消下载」才发 `cancelled`。回答遇到 conflict / not-found（别处已答或已超时）静默移除该行；
// 其它错误（网络 / 传输）保留该行并弹错误提示，用户可重试。

import { Copy, Replace, SkipForward } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../../i18n'
import type { TFunction } from '../../../i18n'
import { RpcError, rpc } from '../../../lib/rpc'
import type { FileExistsAction, SelectionOutcome } from '../../../lib/rpc'
import { cn } from '../../../lib/cn'
import { toastRpcError } from '../../../lib/rpcToast'
import { Button, Card, Dialog, FieldHint, Icon, Tooltip } from '../../../ui'
import { formatDateTime } from '../model/task'
import {
  bulkActions,
  dismissFileConflicts,
  earliestDeadline,
  hasUndismissed,
  useDismissedFileConflicts,
  useFileConflicts,
} from './fileConflict'
import type { FileConflict, FileExistsKind } from './fileConflict'
import { useFileSizeUnits, type FileSizeUnits } from '../../../lib/fileSizeUnits'
import { formatBytes, useNow } from './utils'

const ACTION_ICON: Record<FileExistsAction, LucideIcon> = { rename: Copy, overwrite: Replace, skip: SkipForward }

/** 宿主挂载一次；无可见请求时不渲染（也就没有倒计时定时器）。 */
export function FileConflictDialog() {
  const conflicts = useFileConflicts()
  const dismissed = useDismissedFileConflicts()
  const [resolved, setResolved] = useState<ReadonlySet<string>>(new Set())
  const pending = conflicts.filter((conflict) => !resolved.has(conflict.requestId))
  if (pending.length === 0 || !hasUndismissed(pending, dismissed)) return null
  return (
    <ConflictDialogBody
      conflicts={pending}
      onResolved={(ids) => setResolved((current) => new Set([...current, ...ids]))}
    />
  )
}

function ConflictDialogBody({ conflicts, onResolved }: { conflicts: readonly FileConflict[]; onResolved: (ids: readonly string[]) => void }) {
  const t = useT()
  const now = useNow(1000)
  const [busy, setBusy] = useState(false)
  const single = conflicts.length === 1 ? conflicts[0] : undefined
  const deadline = earliestDeadline(conflicts)
  const remaining = deadline === null ? 0 : Math.max(0, Math.floor((deadline - now) / 1000))
  const bulk = bulkActions(conflicts)

  // 逐个回答；conflict / not-found 视为已被处理，移出该行；其它错误保留并提示，可重试。
  const resolve = async (targets: readonly FileConflict[], outcomeFor: (conflict: FileConflict) => SelectionOutcome) => {
    if (busy) return
    setBusy(true)
    const results = await Promise.allSettled(
      targets.map((conflict) => rpc.daemon.selection.resolve({ requestId: conflict.requestId, outcome: outcomeFor(conflict) })),
    )
    const done: string[] = []
    let failure: unknown = null
    results.forEach((result, index) => {
      const target = targets[index]
      if (!target) return
      if (result.status === 'fulfilled' || (result.reason instanceof RpcError && (result.reason.is('conflict') || result.reason.is('notFound')))) {
        done.push(target.requestId)
      } else {
        failure ??= result.reason
      }
    })
    if (done.length > 0) onResolved(done)
    if (failure !== null) toastRpcError(failure)
    setBusy(false)
  }
  const choose = (target: FileConflict, action: FileExistsAction) => resolve([target], () => ({ kind: 'fileExists', action }))
  const chooseAll = (action: FileExistsAction) => resolve(conflicts, () => ({ kind: 'fileExists', action }))
  const cancelAll = () => resolve(conflicts, () => ({ kind: 'cancelled' }))
  const later = () => dismissFileConflicts(conflicts)

  const bulkLabel: Record<FileExistsAction, string> = {
    rename: t('fileConflictRenameAll'),
    overwrite: t('fileConflictOverwriteAll'),
    skip: t('fileConflictSkipAll'),
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) later()
      }}
      size={single ? 'md' : 'lg'}
      title={single ? t('fileConflictTitle') : t('fileConflictTitleMany', { count: conflicts.length })}
      description={t('fileConflictDesc')}
      footer={
        <div className="flex flex-col gap-3">
          {single ? null : (
            <div className="flex flex-wrap items-center gap-2 narrow:[&>*]:flex-1">
              {bulk.map((action) => (
                <Button key={action} variant={action === 'rename' ? 'primary' : 'outline'} disabled={busy} onClick={() => void chooseAll(action)}>
                  {bulkLabel[action]}
                </Button>
              ))}
            </div>
          )}
          <div className="flex flex-col gap-2 desktop:flex-row desktop:items-center desktop:justify-between">
            <Tooltip content={t('fileConflictCancelHint')} side="top">
              <Button variant="outline" disabled={busy} onClick={() => void cancelAll()} title={t('fileConflictCancelHint')}>
                {single ? t('fileConflictCancelDownload') : t('fileConflictCancelAll')}
              </Button>
            </Tooltip>
            <div className="flex flex-wrap items-center justify-between gap-2 desktop:justify-end">
              <FieldHint className="tabular">{t('fileConflictAutoRenameIn', { seconds: remaining })}</FieldHint>
              <Button variant="ghost" onClick={later}>
                {t('fileConflictLater')}
              </Button>
            </div>
          </div>
        </div>
      }
    >
      {single ? (
        <SingleConflict t={t} conflict={single} busy={busy} onChoose={(action) => void choose(single, action)} />
      ) : (
        <div className="flex flex-col gap-2">
          {conflicts.map((conflict) => (
            <ConflictRow key={conflict.requestId} t={t} conflict={conflict} busy={busy} onChoose={(action) => void choose(conflict, action)} />
          ))}
        </div>
      )}
    </Dialog>
  )
}

/** 已有文件大小；未知显示「大小未知」。 */
function existingSizeLabel(t: TFunction, kind: FileExistsKind, units: FileSizeUnits): string {
  return kind.existingSize === undefined ? t('fileConflictSizeUnknown') : formatBytes(kind.existingSize, units)
}

function incomingSizeLabel(t: TFunction, kind: FileExistsKind, units: FileSizeUnits): string {
  return kind.incomingSize === undefined || kind.incomingSize <= 0 ? t('fileConflictSizeUnknown') : formatBytes(kind.incomingSize, units)
}

function modifiedLabel(t: TFunction, kind: FileExistsKind): string | null {
  return kind.existingModifiedUnixMs === undefined ? null : t('fileConflictModified', { time: formatDateTime(kind.existingModifiedUnixMs / 1000) })
}

function SingleConflict({ t, conflict, busy, onChoose }: { t: TFunction; conflict: FileConflict; busy: boolean; onChoose: (action: FileExistsAction) => void }) {
  const units = useFileSizeUnits()
  const kind = conflict.kind
  const modified = modifiedLabel(t, kind)
  return (
    <div className="flex flex-col gap-3">
      <div className="min-w-0">
        <div className="break-all text-sm font-medium text-foreground">{kind.fileName}</div>
        <FieldHint className="break-all">{kind.saveDir}</FieldHint>
      </div>
      <Card className="grid grid-cols-1 divide-y divide-hairline desktop:grid-cols-2 desktop:divide-x desktop:divide-y-0">
        <div className="flex min-w-0 flex-col gap-0.5 p-3">
          <span className="text-caption text-text-tertiary">{t('fileConflictExisting')}</span>
          <span className="tabular text-sm text-foreground">{existingSizeLabel(t, kind, units)}</span>
          {modified ? <span className="text-xs text-text-tertiary">{modified}</span> : null}
        </div>
        <div className="flex min-w-0 flex-col gap-0.5 p-3">
          <span className="text-caption text-text-tertiary">{t('fileConflictIncoming')}</span>
          <span className="tabular text-sm text-foreground">{incomingSizeLabel(t, kind, units)}</span>
        </div>
      </Card>
      <div className="flex flex-col gap-2">
        <ActionOption
          action="rename"
          title={t('fileConflictRename')}
          caption={t('fileConflictRenameAs', { name: kind.renamePreview })}
          primary
          disabled={busy}
          onClick={() => onChoose('rename')}
        />
        <ActionOption action="overwrite" title={t('fileConflictOverwrite')} caption={t('fileConflictOverwriteHint')} warning disabled={busy} onClick={() => onChoose('overwrite')} />
        {kind.actions.includes('skip') ? (
          <ActionOption action="skip" title={t('fileConflictSkip')} caption={t('fileConflictSkipHint')} disabled={busy} onClick={() => onChoose('skip')} />
        ) : null}
      </div>
    </div>
  )
}

function ActionOption({
  action,
  title,
  caption,
  primary,
  warning,
  disabled,
  onClick,
}: {
  action: FileExistsAction
  title: string
  caption: string
  primary?: boolean
  warning?: boolean
  disabled: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cn(
        'flex min-h-touch w-full items-center gap-3 rounded-md border px-3 py-2 text-left transition-colors disabled:pointer-events-none disabled:opacity-50',
        primary ? 'border-primary bg-accent hover:bg-accent/80' : 'border-border hover:bg-row-hover active:bg-nav-hover',
      )}
    >
      <Icon icon={ACTION_ICON[action]} className={cn('shrink-0', warning ? 'text-warning' : primary ? 'text-primary' : 'text-muted-foreground')} />
      <span className="min-w-0 flex-1">
        <span className={cn('block text-sm font-medium', warning ? 'text-warning' : 'text-foreground')}>{title}</span>
        <span className="block break-all text-xs text-text-tertiary">{caption}</span>
      </span>
    </button>
  )
}

function ConflictRow({ t, conflict, busy, onChoose }: { t: TFunction; conflict: FileConflict; busy: boolean; onChoose: (action: FileExistsAction) => void }) {
  const units = useFileSizeUnits()
  const kind = conflict.kind
  const modified = modifiedLabel(t, kind)
  const meta = [`${t('fileConflictExisting')} ${existingSizeLabel(t, kind, units)}`, modified, `${t('fileConflictIncoming')} ${incomingSizeLabel(t, kind, units)}`]
    .filter((part): part is string => part !== null)
    .join(' · ')
  return (
    <Card className="flex flex-col gap-2 p-3">
      <div className="min-w-0">
        <div className="break-all text-sm font-medium text-foreground">{kind.fileName}</div>
        <FieldHint className="tabular break-all">{meta}</FieldHint>
        <FieldHint className="break-all">{t('fileConflictRenameAs', { name: kind.renamePreview })}</FieldHint>
      </div>
      <div className="flex flex-wrap gap-2 narrow:[&>*]:flex-1">
        <Button variant="primary" icon={ACTION_ICON.rename} disabled={busy} onClick={() => onChoose('rename')}>
          {t('fileConflictRename')}
        </Button>
        <Button variant="outline" icon={ACTION_ICON.overwrite} disabled={busy} onClick={() => onChoose('overwrite')} className="text-warning">
          {t('fileConflictOverwrite')}
        </Button>
        {kind.actions.includes('skip') ? (
          <Button variant="outline" icon={ACTION_ICON.skip} disabled={busy} onClick={() => onChoose('skip')}>
            {t('fileConflictSkip')}
          </Button>
        ) : null}
      </div>
    </Card>
  )
}
