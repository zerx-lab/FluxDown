// 输入区（GPUI `render_composer`）：大圆角卡片，上方是图片缩略图，中间是多行输入（默认 3 行，随内容增高到 8 行），
// 底栏左侧是「+」附件与提问 / 反馈切换，右侧是圆形的发送 / 停止按钮。Enter 发送，Shift+Enter 换行（输入法组字中的 Enter 不发送）。
// 图片附件写入 `[Image #N]` 占位符（纯逻辑见 `composer.ts`），云端把第 N 张图紧跟在占位符后发给模型。

import { ArrowUp, Plus, Square, X } from 'lucide-react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { ChangeEvent, ClipboardEvent, DragEvent, KeyboardEvent } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import type { AssistantConfigDto, AssistantSessionKind } from '../../lib/rpc'
import { Button, Card, FieldError, Icon, Spinner, Tooltip } from '../../ui'
import { formatBytes } from '../downloads/dialogs/utils'
import { ATTACHABLE_IMAGE_TYPES, EMPTY_COMPOSER, checkAttachable, imagesEnabled, insertPlaceholder, readAsDataUrl, removePlaceholder } from './composer'
import type { AttachError, Attachment, ComposerState } from './composer'
import type { AssistantController, ViewState } from './controller'
import { SEND_FAILURE_KEY, canSend, charCount, currentKind, currentSession } from './model'

/** 输入框最多增高到几行（GPUI `INPUT_MAX_ROWS`）。 */
const INPUT_MAX_ROWS = 8
const ACCEPT = ATTACHABLE_IMAGE_TYPES.join(',')

type AttachFailure = AttachError | 'read'

const ATTACH_ERROR_KEY: Record<AttachFailure, string> = {
  unsupported: 'assistantAttachErrUnsupported',
  type: 'assistantAttachErrType',
  tooMany: 'assistantAttachErrTooMany',
  tooLarge: 'assistantAttachErrTooLarge',
  read: 'assistantAttachErrRead',
}

function KindChip({ label, selected, onClick }: { label: string; selected: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      aria-pressed={selected}
      onClick={onClick}
      className={cn(
        'rounded-full px-2.5 py-1 text-xs transition-colors coarse:min-h-touch',
        selected ? 'bg-nav-selected font-medium text-nav-selected-foreground' : 'text-muted-foreground hover:bg-nav-hover',
      )}
    >
      {label}
    </button>
  )
}

function Thumbnail({ attachment, number, onRemove }: { attachment: Attachment; number: number; onRemove: () => void }) {
  const t = useT()
  const label = t('assistantImageLabel', { n: number })
  return (
    <div className="relative size-14 shrink-0">
      <img src={attachment.dataUrl} alt={attachment.name || label} title={attachment.name || label} className="size-full rounded-md border border-hairline object-cover" />
      <span className="absolute bottom-0.5 left-0.5 rounded-sm bg-background/80 px-1 text-caption font-medium text-foreground tabular">#{number}</span>
      <button
        type="button"
        aria-label={t('assistantImageRemove', { n: number })}
        onClick={onRemove}
        className="absolute -top-1.5 -right-1.5 grid size-5 place-items-center rounded-full border border-hairline bg-surface text-muted-foreground hover:text-foreground coarse:size-6"
      >
        <Icon icon={X} size="sm" />
      </button>
    </div>
  )
}

export function Composer({
  view,
  controller,
  config,
  quotaExhausted,
  newChat,
  value,
  onChange,
  autoFocus = false,
}: {
  view: ViewState
  controller: AssistantController
  config: AssistantConfigDto | undefined
  quotaExhausted: boolean
  /** 新对话且尚无内容：显示提问 / 反馈切换。 */
  newChat: boolean
  value: ComposerState
  onChange: (value: ComposerState) => void
  /** 挂载时聚焦输入框（桌面面板打开 / 切回对话视图）。 */
  autoFocus?: boolean
}) {
  const t = useT()
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const fileRef = useRef<HTMLInputElement>(null)
  const caretRef = useRef<number | null>(null)
  const latest = useRef(value)
  const nextKey = useRef(1)
  const [attachFailure, setAttachFailure] = useState<AttachFailure | null>(null)

  useEffect(() => {
    latest.current = value
  }, [value])

  // 输入框随内容增高（最多 INPUT_MAX_ROWS 行）；插入 / 删除占位符后把光标放回文本里对应的位置。
  useLayoutEffect(() => {
    const element = textareaRef.current
    if (!element) return
    element.style.height = 'auto'
    const style = getComputedStyle(element)
    const lineHeight = Number.parseFloat(style.lineHeight) || 20
    const padding = Number.parseFloat(style.paddingTop) + Number.parseFloat(style.paddingBottom)
    const limit = lineHeight * INPUT_MAX_ROWS + (Number.isFinite(padding) ? padding : 0)
    element.style.height = `${Math.min(element.scrollHeight, limit)}px`
    element.style.overflowY = element.scrollHeight > limit ? 'auto' : 'hidden'
    if (caretRef.current !== null) {
      element.setSelectionRange(caretRef.current, caretRef.current)
      caretRef.current = null
    }
  }, [value.draft])

  if (quotaExhausted) {
    return (
      <div className="flex w-full flex-none flex-col items-center px-4 pt-1 pb-3">
        <Card className="flex w-full max-w-3xl flex-col gap-1 p-3">
          <div className="text-sm font-medium text-foreground">{t('assistantQuotaTitle')}</div>
          <div className="text-xs text-text-tertiary">{t('assistantQuotaDesc', { limit: config?.dailyLimit ?? 0 })}</div>
        </Card>
      </div>
    )
  }

  const stale = view.env.stale
  const run = controller.activeRun()
  const running = run !== undefined
  const trimmed = value.draft.trim()
  const maxChars = config?.maxMessageChars ?? 0
  const count = charCount(value.draft)
  const tooLong = maxChars > 0 && count > maxChars
  const sendable = canSend({
    gate: { kind: 'open', quotaExhausted: false },
    stale,
    sending: view.sending,
    hasActiveRun: running,
    draft: trimmed,
    maxMessageChars: maxChars,
  })
  const images = imagesEnabled(config)
  const kind: AssistantSessionKind = currentKind(view.current, view.kind, currentSession(view.current, view.detail.session, view.sessions.items))
  const failureKey = view.sendFailure !== null ? SEND_FAILURE_KEY[view.sendFailure] : view.actionFailed ? 'assistantActionFailed' : null

  const submit = () => {
    if (!sendable) return
    const attachments = value.attachments
    void controller.send(
      trimmed,
      attachments.map((attachment) => attachment.dataUrl),
    ).then((ok) => {
      if (ok) onChange(EMPTY_COMPOSER)
    })
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== 'Enter' || event.shiftKey || event.nativeEvent.isComposing || event.keyCode === 229) return
    event.preventDefault()
    submit()
  }

  const addFiles = async (files: readonly File[]) => {
    if (!images || files.length === 0) return
    setAttachFailure(null)
    // 先校验并读完，再一次性写入文本与附件列表（读取期间用户可能继续输入）。
    const loaded: Attachment[] = []
    let failure: AttachFailure | null = null
    for (const file of files) {
      const problem = checkAttachable(config, latest.current.attachments.length + loaded.length, file)
      if (problem !== null) {
        failure = problem
        break
      }
      try {
        loaded.push({ key: nextKey.current++, dataUrl: await readAsDataUrl(file), name: file.name, bytes: file.size })
      } catch {
        failure = 'read'
        break
      }
    }
    if (loaded.length > 0) {
      const current = latest.current
      let text = current.draft
      let caret = textareaRef.current?.selectionStart ?? text.length
      const attachments = [...current.attachments]
      for (const attachment of loaded) {
        attachments.push(attachment)
        const edit = insertPlaceholder(text, caret, attachments.length)
        text = edit.text
        caret = edit.caret
      }
      caretRef.current = caret
      onChange({ draft: text, attachments })
    }
    if (failure !== null) setAttachFailure(failure)
  }

  const removeAttachment = (index: number) => {
    const caret = textareaRef.current?.selectionStart ?? value.draft.length
    const edit = removePlaceholder(value.draft, caret, index + 1, value.attachments.length)
    caretRef.current = edit.caret
    setAttachFailure(null)
    onChange({ draft: edit.text, attachments: value.attachments.filter((_, position) => position !== index) })
  }

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = [...event.clipboardData.files].filter((file) => file.type.startsWith('image/'))
    if (files.length === 0) return
    // 剪贴板里是图片：由附件接管（文本粘贴照常）。
    event.preventDefault()
    void addFiles(files)
  }

  const hasFiles = (event: DragEvent) => images && [...event.dataTransfer.types].includes('Files')
  const onDragOver = (event: DragEvent) => {
    if (hasFiles(event)) event.preventDefault()
  }
  const onDrop = (event: DragEvent) => {
    if (!hasFiles(event)) return
    event.preventDefault()
    void addFiles([...event.dataTransfer.files].filter((file) => file.type.startsWith('image/')))
  }

  const onPick = (event: ChangeEvent<HTMLInputElement>) => {
    const files = [...(event.target.files ?? [])]
    event.target.value = ''
    void addFiles(files)
  }

  const attachMessage =
    attachFailure === null
      ? null
      : t(ATTACH_ERROR_KEY[attachFailure], { max: config?.maxImages ?? 0, size: formatBytes(config?.maxImageBytes ?? 0) })

  const actionLabel = running ? t('assistantStop') : t('assistantSend')

  return (
    <div className="flex w-full flex-none flex-col items-center px-4 pt-1 pb-3" onDragOver={onDragOver} onDrop={onDrop}>
      <div className="flex w-full max-w-3xl min-w-0 flex-col gap-1.5">
        {failureKey !== null ? <FieldError>{t(failureKey)}</FieldError> : null}
        {attachMessage !== null ? <FieldError>{attachMessage}</FieldError> : null}
        <div className="flex min-w-0 flex-col rounded-2xl border border-input bg-surface transition-colors focus-within:border-ring">
          {value.attachments.length > 0 ? (
            <div className="flex gap-2 overflow-x-auto px-3 pt-3 pb-1">
              {value.attachments.map((attachment, index) => (
                <Thumbnail key={attachment.key} attachment={attachment} number={index + 1} onRemove={() => removeAttachment(index)} />
              ))}
            </div>
          ) : null}
          <textarea
            ref={textareaRef}
            rows={3}
            autoFocus={autoFocus}
            data-assistant-composer=""
            value={value.draft}
            placeholder={t('assistantInputPlaceholder')}
            aria-label={t('assistantInputPlaceholder')}
            aria-invalid={tooLong || undefined}
            onChange={(event) => onChange({ ...value, draft: event.target.value })}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            className="w-full resize-none bg-transparent px-3.5 pt-3 pb-1 text-sm leading-relaxed text-foreground outline-none placeholder:text-text-tertiary"
          />
          <div className="flex min-w-0 flex-wrap items-center gap-1.5 px-2 pb-2">
            {images ? (
              <>
                <input ref={fileRef} type="file" accept={ACCEPT} multiple hidden onChange={onPick} />
                <Tooltip content={t('assistantAttach')}>
                  <Button variant="ghost" iconOnly className="rounded-full" aria-label={t('assistantAttach')} disabled={stale} onClick={() => fileRef.current?.click()}>
                    <Icon icon={Plus} />
                  </Button>
                </Tooltip>
              </>
            ) : null}
            {newChat ? (
              <div className="flex items-center gap-1">
                <KindChip label={t('assistantKindAsk')} selected={kind === 'ask'} onClick={() => controller.setKind('ask')} />
                <KindChip label={t('assistantKindFeedback')} selected={kind === 'feedback'} onClick={() => controller.setKind('feedback')} />
              </div>
            ) : null}
            {tooLong ? <FieldError className="min-w-0 flex-1">{t('assistantTooLong', { count, max: maxChars })}</FieldError> : <span className="flex-1" />}
            <Tooltip content={actionLabel}>
              <span className="relative grid shrink-0 place-items-center">
                {running ? (
                  <>
                    <span aria-hidden className="pointer-events-none absolute inset-0 rounded-full border-2 border-primary animate-fx-halo-ring" />
                    <span
                      aria-hidden
                      className="pointer-events-none absolute inset-0 rounded-full border-2 border-primary animate-fx-halo-ring"
                      style={{ animationDelay: '0.9s' }}
                    />
                    <span
                      aria-hidden
                      className="pointer-events-none absolute -inset-1 rounded-full border-2 border-transparent border-t-primary animate-fx-halo-spin"
                    />
                  </>
                ) : null}
                <button
                  type="button"
                  aria-label={actionLabel}
                  disabled={running ? stale || view.cancelling : !sendable}
                  onClick={running ? () => controller.stop() : submit}
                  className="relative z-10 grid size-8 shrink-0 place-items-center rounded-full bg-primary text-primary-foreground transition-opacity hover:bg-primary/90 disabled:pointer-events-none disabled:opacity-40 coarse:size-10"
                >
                  {view.sending || view.cancelling ? (
                    <Spinner size="md" className="text-primary-foreground" />
                  ) : (
                    <Icon icon={running ? Square : ArrowUp} size="md" className={running ? 'fill-current' : undefined} />
                  )}
                </button>
              </span>
            </Tooltip>
          </div>
        </div>
      </div>
    </div>
  )
}
