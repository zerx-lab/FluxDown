// 「执行过程」折叠区、工具调用行与确认卡片（GPUI `render_steps` / `render_tool` / `render_confirm`）。
// 工具名、参数 JSON、输出预览都是模型 / 用户数据：单行摘要截断，展开后的 JSON 在块内横向滚动，不撑破版面。

import { ChevronDown, ChevronRight, CircleAlert, CircleCheck, CircleHelp, Clock, Globe, Pen, Trash2, X } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import type { AssistantToolCallDto, AssistantToolPreviewDto } from '../../lib/rpc'
import { Badge, Button, Icon, Spinner } from '../../ui'
import type { AssistantController, ViewState } from './controller'
import {
  argumentsPretty,
  argumentsSummary,
  confirmView,
  formatCountdown,
  formatDuration,
  formatToolOutput,
  previewActionKey,
  remainingSeconds,
  stepsOpen,
  toolCount,
  toolState,
} from './model'
import type { ConfirmView, ToolState, Turn } from './model'

interface Shared {
  view: ViewState
  controller: AssistantController
  now: number
}

/** 图标 + 文字的结果标签（已允许 / 已过期 / 已提交等）。 */
export function ResultLabel({ icon, text, tone }: { icon: LucideIcon; text: string; tone: string }) {
  return (
    <span className={cn('inline-flex min-w-0 items-center gap-1.5 text-xs', tone)}>
      <Icon icon={icon} size="sm" />
      <span className="min-w-0 break-words">{text}</span>
    </span>
  )
}

/** 可读预览（agent 用本机数据生成）：动作、受影响任务、设置变更、地址与补充说明；全部是数据，按文本节点输出。 */
function PreviewBlock({ preview }: { preview: AssistantToolPreviewDto }) {
  const t = useT()
  const actionKey = previewActionKey(preview.action)
  const tasks = preview.tasks ?? []
  const settings = preview.settings ?? []
  const urls = preview.urls ?? []
  const details = preview.details ?? []
  const moreTasks = preview.moreTasks ?? 0
  return (
    <div className="flex min-w-0 flex-col gap-2 rounded-sm bg-muted p-2">
      <div className="text-sm font-medium break-words text-foreground">{actionKey === null ? preview.action : t(actionKey)}</div>
      {preview.deleteFiles === true ? <ResultLabel icon={Trash2} text={t('assistantPreviewDeleteFiles')} tone="text-destructive" /> : null}
      {tasks.length > 0 ? (
        <PreviewList title={t('assistantPreviewTasks')}>
          {tasks.map((task, index) => (
            <li key={`${index}:${task}`} className="break-all">
              {task}
            </li>
          ))}
          {moreTasks > 0 ? <li className="list-none text-text-tertiary">{t('assistantPreviewMoreTasks', { n: moreTasks })}</li> : null}
        </PreviewList>
      ) : null}
      {settings.length > 0 ? (
        <PreviewList title={t('assistantPreviewSettings')}>
          {settings.map((change, index) => (
            <li key={`${index}:${change.key}`} className="break-all">
              <span className="font-mono">{change.key}</span>
              {': '}
              {change.from !== undefined && change.from !== null ? <span className="text-text-tertiary line-through">{change.from}</span> : null}
              {change.from !== undefined && change.from !== null ? ' → ' : null}
              <span className="font-medium">{change.to}</span>
            </li>
          ))}
        </PreviewList>
      ) : null}
      {urls.length > 0 ? (
        <PreviewList title={t('assistantPreviewUrls')}>
          {urls.map((url, index) => (
            <li key={`${index}:${url}`} className="font-mono break-all">
              {url}
            </li>
          ))}
        </PreviewList>
      ) : null}
      {details.map((line, index) => (
        <div key={`${index}:${line}`} className="text-xs break-all whitespace-pre-wrap text-muted-foreground">
          {line}
        </div>
      ))}
    </div>
  )
}

function PreviewList({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <div className="text-xs font-medium text-text-tertiary">{title}</div>
      <ul className="ml-4 flex list-disc flex-col gap-0.5 text-xs text-foreground">{children}</ul>
    </div>
  )
}

function ConfirmCard({
  call,
  rowKey,
  confirmation,
  observing,
  view,
  controller,
}: { call: AssistantToolCallDto; rowKey: string; confirmation: ConfirmView; observing: boolean } & Shared) {
  const t = useT()
  const requestId = call.requestId ?? null
  const busy = requestId !== null && view.confirming.has(requestId)
  const enabled = confirmation.kind === 'pending' && !view.env.stale && !busy && requestId !== null
  const destructive = call.destructive === true

  let status = null
  switch (confirmation.kind) {
    case 'pending':
      status =
        confirmation.remainingMs === null ? null : (
          <span className="text-xs text-text-tertiary tabular">
            {t('assistantConfirmRemaining', { time: formatCountdown(remainingSeconds(confirmation.remainingMs)) })}
          </span>
        )
      break
    case 'approved':
      status = <ResultLabel icon={CircleCheck} text={t('assistantConfirmApproved')} tone="text-success" />
      break
    case 'declined':
      status = <ResultLabel icon={X} text={t('assistantConfirmDeclined')} tone="text-muted-foreground" />
      break
    case 'expired':
      status = <ResultLabel icon={Clock} text={t('assistantConfirmExpired')} tone="text-warning" />
      break
  }

  const effect =
    call.effect === 'network'
      ? { icon: Globe, key: 'assistantEffectNetwork' }
      : call.effect === 'write'
        ? { icon: Pen, key: 'assistantEffectWrite' }
        : null
  const preview = call.preview ?? null
  const summary = preview === null ? argumentsSummary(call.arguments) : null
  const argsKey = `args:${rowKey}`
  const argsOpen = view.argsOpen.has(argsKey)
  const hasArguments = call.arguments !== undefined && call.arguments !== null

  return (
    <div
      className={cn(
        'flex w-full min-w-0 flex-col gap-3 rounded-[var(--fx-components-card-radius)] border bg-surface p-3',
        destructive ? 'border-destructive/60' : 'border-warning/50',
      )}
    >
      <div className="flex min-w-0 flex-col gap-2">
        <div className="flex flex-wrap items-center gap-1.5">
          <Icon icon={CircleAlert} size="sm" className={destructive ? 'text-destructive' : 'text-warning'} />
          <span className="text-sm font-medium text-foreground">{t('assistantConfirmTitle')}</span>
          {effect ? (
            <Badge>
              <span className="inline-flex items-center gap-1">
                <Icon icon={effect.icon} size="sm" />
                {t(effect.key)}
              </span>
            </Badge>
          ) : null}
        </div>
        <div className="font-mono text-sm break-all text-foreground">{call.name}</div>
        {destructive ? <div className="text-xs font-medium text-destructive">{t('assistantConfirmDestructive')}</div> : null}
        {preview !== null ? <PreviewBlock preview={preview} /> : null}
        {summary !== null ? (
          <div className={cn('font-mono text-xs text-text-tertiary', argsOpen ? 'break-all whitespace-pre-wrap' : 'truncate')} title={argsOpen ? undefined : summary}>
            {summary}
          </div>
        ) : null}
        {hasArguments ? (
          <>
            <button
              type="button"
              aria-expanded={argsOpen}
              onClick={() => controller.toggleArguments(argsKey)}
              className="flex w-fit items-center gap-1 text-xs text-accent-text coarse:min-h-touch"
            >
              <Icon icon={argsOpen ? ChevronDown : ChevronRight} size="sm" />
              {t(argsOpen ? 'assistantArgsHide' : 'assistantArgsShow')}
            </button>
            {argsOpen && call.arguments !== undefined && call.arguments !== null ? (
              <pre className="max-h-64 w-full max-w-full overflow-auto rounded-sm bg-muted p-2 font-mono text-xs whitespace-pre text-foreground">
                {argumentsPretty(call.arguments)}
              </pre>
            ) : null}
          </>
        ) : null}
      </div>
      {/* 旁观执行（其他设备 / 重启前发起）：本机不能确认它的待确认调用，不给确认 / 拒绝按钮（轮次顶部已有说明）。 */}
      {observing && confirmation.kind === 'pending' ? (
        <div className="min-w-0">{status}</div>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          <div className="min-w-0 flex-1">{status}</div>
          <Button variant="outline" disabled={!enabled} onClick={() => requestId !== null && controller.confirmTool(requestId, false)}>
            {t('assistantDeny')}
          </Button>
          <Button variant={destructive ? 'danger' : 'primary'} loading={busy} disabled={!enabled} onClick={() => requestId !== null && controller.confirmTool(requestId, true)}>
            {t('assistantAllow')}
          </Button>
        </div>
      )}
    </div>
  )
}

/** 状态文案与图标：待确认 / 未执行优先于调用状态本身。 */
function toolPresentation(state: ToolState, confirmation: ConfirmView | null): { labelKey: string; tone: string; icon: 'spinner' | LucideIcon } {
  if (confirmation?.kind === 'pending') return { labelKey: 'assistantToolWaiting', tone: 'text-warning', icon: Clock }
  if (confirmation?.kind === 'declined' || confirmation?.kind === 'expired') {
    return { labelKey: 'assistantToolSkipped', tone: 'text-text-tertiary', icon: CircleAlert }
  }
  switch (state) {
    case 'running':
      return { labelKey: 'assistantToolRunning', tone: 'text-text-tertiary', icon: 'spinner' }
    case 'succeeded':
      return { labelKey: 'assistantToolDone', tone: 'text-success', icon: CircleCheck }
    case 'failed':
      return { labelKey: 'assistantToolFailed', tone: 'text-destructive', icon: CircleAlert }
    case 'interrupted':
      return { labelKey: 'assistantToolInterrupted', tone: 'text-warning', icon: CircleAlert }
    case 'unknown':
      return { labelKey: 'assistantToolResultUnknown', tone: 'text-warning', icon: CircleHelp }
  }
}

function DetailSection({ title, text }: { title: string; text: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="text-xs font-medium text-text-tertiary">{title}</div>
      <pre className="m-0 font-mono text-xs break-all whitespace-pre-wrap text-foreground">{text}</pre>
    </div>
  )
}

function ToolRow({
  call,
  rowKey,
  runActive,
  observing,
  view,
  controller,
  now,
}: { call: AssistantToolCallDto; rowKey: string; runActive: boolean; observing: boolean } & Shared) {
  const t = useT()
  const state = toolState(call, runActive)
  const confirmation = confirmView(call, runActive, now)
  const client = call.kind === 'client'
  const { labelKey, tone, icon } = toolPresentation(state, confirmation)
  const detailKey = `detail:${rowKey}`
  // 需要确认的工具，参数已在确认卡片里；批准后的执行结果仍可在卡片下方展开。
  const detailArguments =
    confirmation === null && client && call.arguments !== undefined && call.arguments !== null && argumentsSummary(call.arguments) !== null
      ? argumentsPretty(call.arguments)
      : null
  const showsOutput = confirmation === null || confirmation.kind === 'approved'
  const detailOutput = showsOutput && client && call.outputPreview && call.outputPreview.trim() !== '' ? formatToolOutput(call.outputPreview) : null
  const expandable = detailArguments !== null || detailOutput !== null
  const open = expandable && view.argsOpen.has(detailKey)

  const header = (
    <>
      {icon === 'spinner' ? <Spinner size="sm" /> : <Icon icon={icon} size="sm" className={tone} />}
      <span className="min-w-0 font-mono text-sm break-all text-foreground">{call.name}</span>
      {client ? <Badge>{t('assistantToolLocal')}</Badge> : null}
      <span className={cn('text-xs', tone)}>{t(labelKey)}</span>
      {call.durationMs !== undefined && call.durationMs !== null ? (
        <span className="text-xs text-text-tertiary tabular">{formatDuration(call.durationMs)}</span>
      ) : null}
    </>
  )

  return (
    <div className="flex w-full min-w-0 flex-col gap-1">
      {confirmation !== null ? (
        <ConfirmCard call={call} rowKey={rowKey} confirmation={confirmation} observing={observing} view={view} controller={controller} now={now} />
      ) : null}
      {expandable ? (
        <button
          type="button"
          aria-expanded={open}
          aria-label={t(open ? 'assistantToolDetailsHide' : 'assistantToolDetailsShow')}
          title={t(open ? 'assistantToolDetailsHide' : 'assistantToolDetailsShow')}
          onClick={() => controller.toggleArguments(detailKey)}
          className="flex w-full min-w-0 flex-wrap items-center gap-1.5 rounded-sm px-1 py-0.5 text-left hover:bg-muted coarse:min-h-touch"
        >
          {header}
          <Icon icon={open ? ChevronDown : ChevronRight} size="sm" className="ml-auto text-text-tertiary" />
        </button>
      ) : (
        <div className="flex min-w-0 flex-wrap items-center gap-1.5 px-1 py-0.5">{header}</div>
      )}
      {open ? (
        <div className="flex min-w-0 flex-col gap-2 rounded-sm bg-muted p-2">
          {detailArguments !== null ? <DetailSection title={t('assistantToolArguments')} text={detailArguments} /> : null}
          {detailOutput !== null ? <DetailSection title={t('assistantToolOutput')} text={detailOutput} /> : null}
        </div>
      ) : null}
    </div>
  )
}

export function Steps({ turn, view, controller, now }: { turn: Turn } & Shared) {
  const t = useT()
  const running = turn.live !== null && (turn.live.status === 'starting' || turn.live.status === 'running')
  const open = stepsOpen(turn, view.stepsChoice.get(turn.key), now)
  const tools = toolCount(turn)

  return (
    <div className="flex w-full min-w-0 flex-col gap-0.5">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => controller.toggleSteps(turn.key, open)}
        className="flex w-fit max-w-full items-center gap-1 py-0.5 text-left coarse:min-h-touch"
      >
        <Icon icon={open ? ChevronDown : ChevronRight} size="sm" className="text-text-tertiary" />
        <span className="text-xs font-medium text-text-tertiary">{t('assistantStepsTitle')}</span>
        {tools > 0 ? <span className="text-xs text-text-tertiary">{t('assistantStepsCount', { count: tools })}</span> : null}
        {running && turn.answer === null ? <Spinner size="sm" /> : null}
      </button>
      {open ? (
        <div className="ml-1 flex min-w-0 flex-col gap-2 border-l border-hairline pl-3">
          {turn.steps.map((block, index) => {
            if (block.type === 'text') {
              return (
                <div key={index} className="text-sm break-words whitespace-pre-wrap text-muted-foreground">
                  {block.text}
                </div>
              )
            }
            // 问题单草稿在 `Turn.issues` 单独渲染，不进过程列表。
            if (block.type === 'tool') {
              // 历史里没有 `toolCallId` 的工具 ID 可能为空 / 重复：键与展开状态都带上位置。
              const rowKey = `${turn.key}:${index}:${block.call.callId}`
              return (
                <ToolRow
                  key={rowKey}
                  call={block.call}
                  rowKey={rowKey}
                  runActive={running}
                  observing={turn.live?.observing === true}
                  view={view}
                  controller={controller}
                  now={now}
                />
              )
            }
            return null
          })}
        </div>
      ) : null}
    </div>
  )
}
