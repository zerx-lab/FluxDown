// 输入区（GPUI `render_composer`）：大圆角卡片，上方是图片缩略图，中间是多行输入（默认 3 行，随内容增高到 8 行），
// 底栏左侧是「+」附件与提问 / 反馈切换，右侧是圆形的发送 / 停止按钮。Enter 发送，Shift+Enter 换行（输入法组字中的 Enter 不发送）。
// 图片附件写入 `[Image #N]` 占位符（纯逻辑见 `composer.ts`），云端把第 N 张图紧跟在占位符后发给模型：
// 粘贴时文字优先于附带的截图、非云端直收的格式转 PNG、读取 / 转码期间预占名额并显示转圈、拖入时卡片高亮、
// 发送中不能增删图片、占位符被手动删掉时对应图片自动移除。编辑历史提问时（`value.editing`）输入框换成原提问，发送即重发。

import { ArrowUp, Pencil, Plus, Square, X } from 'lucide-react'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { ChangeEvent, ClipboardEvent, DragEvent, KeyboardEvent } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import type { AssistantConfigDto, AssistantSessionKind } from '../../lib/rpc'
import { Button, Card, FieldError, Icon, Spinner, Tooltip } from '../../ui'
import { formatBytes } from '../downloads/dialogs/utils'
import {
  ATTACHABLE_IMAGE_TYPES,
  BROWSER_IMAGE_CODEC,
  checkAttachable,
  endEdit,
  imagesEnabled,
  insertPlaceholder,
  pasteTakesImages,
  prepareImage,
  reconcileDraft,
  removePlaceholder,
  settleSent,
} from './composer'
import type { AttachError, Attachment, ComposerState } from './composer'
import type { AssistantController, ViewState } from './controller'
import { PreviewableImage } from './ImagePreview'
import { ACTION_FAILURE_KEY, SEND_FAILURE_KEY, canSend, charCount, currentKind, currentSession, sessionLimitState } from './model'

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

function Thumbnail({ attachment, number, removable, onRemove }: { attachment: Attachment; number: number; removable: boolean; onRemove: () => void }) {
  const t = useT()
  const label = t('assistantImageLabel', { n: number })
  return (
    <div className="relative size-14 shrink-0">
      <PreviewableImage
        src={attachment.dataUrl}
        alt={attachment.name || label}
        number={number}
        className="size-full"
        imageClassName="size-full rounded-md border border-hairline object-cover"
      />
      <span className="pointer-events-none absolute bottom-0.5 left-0.5 rounded-sm bg-background/80 px-1 text-caption font-medium text-foreground tabular">#{number}</span>
      <button
        type="button"
        aria-label={t('assistantImageRemove', { n: number })}
        disabled={!removable}
        onClick={onRemove}
        className="absolute -top-1.5 -right-1.5 grid size-5 place-items-center rounded-full border border-hairline bg-surface text-muted-foreground hover:text-foreground disabled:pointer-events-none disabled:opacity-40 coarse:size-6"
      >
        <Icon icon={X} size="sm" />
      </button>
    </div>
  )
}

/** 正在读取 / 转码的图片占位（名额已预占）。 */
function PendingThumbnail() {
  const t = useT()
  return (
    <div role="status" aria-label={t('assistantLoading')} className="grid size-14 shrink-0 place-items-center rounded-md border border-hairline bg-muted">
      <Spinner size="md" />
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
  /** 读取 / 转码中的图片数（预占张数名额）。 */
  const attachingRef = useRef(0)
  const dragDepth = useRef(0)
  const [attaching, setAttaching] = useState(0)
  const [dragging, setDragging] = useState(false)
  const [attachFailure, setAttachFailure] = useState<AttachFailure | null>(null)

  const editing = value.editing
  const editActive = editing !== null && editing.sessionId === view.current

  useEffect(() => {
    latest.current = value
  }, [value])

  // 编辑的是别的会话里的提问（切走了 / 会话没了）：退出编辑，恢复进入前的草稿。
  useEffect(() => {
    if (editing !== null && !editActive) onChange(endEdit(latest.current))
  }, [editing, editActive, onChange])

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

  // 进入编辑后把光标放到原提问末尾。
  const editQuestion = editActive ? editing.questionId : null
  useEffect(() => {
    const element = textareaRef.current
    if (editQuestion === null || !element) return
    element.focus()
    element.setSelectionRange(element.value.length, element.value.length)
  }, [editQuestion])

  const stale = view.env.stale
  const run = controller.activeRun()
  // 提问请求还在途中（尚未被接受）：本会话里也算「进行中」，可以停止。
  const outgoing = view.sending && view.sendingFrom === view.current
  const running = run !== undefined || outgoing

  // 额度用尽：换成提示卡；但当天最后一问还在进行时（云端把进行中的执行也计入额度）仍保留输入区与停止按钮。
  if (quotaExhausted && !running) {
    return (
      <div className="flex w-full flex-none flex-col items-center px-4 pt-1 pb-3">
        <Card className="flex w-full max-w-3xl flex-col gap-1 p-3">
          <div className="text-sm font-medium text-foreground">{t('assistantQuotaTitle')}</div>
          <div className="text-xs text-text-tertiary">{t('assistantQuotaDesc', { limit: config?.dailyLimit ?? 0 })}</div>
        </Card>
      </div>
    )
  }

  const sessionLimit = sessionLimitState(config, view.current, view.detail.questionsUsed, running)
  const failureKey =
    view.sendFailure !== null ? SEND_FAILURE_KEY[view.sendFailure] : view.actionFailure !== null ? ACTION_FAILURE_KEY[view.actionFailure] : null

  if (sessionLimit.kind === 'reached') {
    return (
      <div className="flex w-full flex-none flex-col items-center px-4 pt-1 pb-3">
        <Card className="flex w-full max-w-3xl flex-col gap-2 p-3">
          <div className="flex flex-col gap-1">
            <div className="text-sm font-medium text-foreground">{t('assistantSessionLimitTitle')}</div>
            <div className="text-xs text-text-tertiary">{t('assistantSessionLimitDesc', { limit: sessionLimit.limit })}</div>
          </div>
          {failureKey !== null ? <FieldError>{t(failureKey)}</FieldError> : null}
          <div>
            <Button
              variant="primary"
              disabled={stale}
              onClick={() => {
                controller.clearFailure()
                controller.newChat()
              }}
            >
              {t('assistantNewChat')}
            </Button>
          </div>
        </Card>
      </div>
    )
  }

  const trimmed = value.draft.trim()
  const maxChars = config?.maxMessageChars ?? 0
  const count = charCount(value.draft)
  const tooLong = maxChars > 0 && count > maxChars
  const sendable = canSend({
    gate: { kind: 'open', quotaExhausted },
    stale,
    sending: view.sending,
    hasActiveRun: running,
    cooldown: view.cooldown,
    draft: trimmed,
    maxMessageChars: maxChars,
  })
  const images = imagesEnabled(config)
  // 发送中不能增删图片；重发沿用原附图时也不能改图。
  const keepImages = editActive && editing.keepImages
  const canAttach = images && !stale && !view.sending && !keepImages
  const removable = !stale && !view.sending
  const kind: AssistantSessionKind = currentKind(view.current, view.kind, currentSession(view.current, view.detail.session, view.sessions.items))

  const submit = () => {
    if (!sendable) return
    const target = editActive ? editing : null
    const sent = value
    const dataUrls = value.attachments.map((attachment) => attachment.dataUrl)
    const request =
      target === null
        ? controller.send(trimmed, dataUrls)
        : controller.send(trimmed, target.keepImages ? [] : dataUrls, { replaceFrom: target.questionId, keepImages: target.keepImages })
    // 发送期间用户新写的内容不能被清掉：只有输入仍是发送时的样子才清空。
    void request.then((ok) => {
      if (ok) onChange(settleSent(sent, latest.current))
    })
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Escape' && editActive) {
      event.preventDefault()
      onChange(endEdit(latest.current))
      return
    }
    if (event.key !== 'Enter' || event.shiftKey || event.nativeEvent.isComposing || event.keyCode === 229) return
    event.preventDefault()
    submit()
  }

  const addFiles = async (files: readonly File[]) => {
    if (config === undefined || !canAttach || files.length === 0) return
    setAttachFailure(null)
    // 同步预检并预占名额（张数计入读取中的），再并行读取 / 转码，最后一次性写入文本与附件列表。
    const accepted: File[] = []
    let failure: AttachFailure | null = null
    for (const file of files) {
      const problem = checkAttachable(config, latest.current.attachments.length + attachingRef.current + accepted.length, file)
      if (problem === 'tooMany') {
        failure = problem
        break
      }
      if (problem !== null) failure ??= problem
      else accepted.push(file)
    }
    if (accepted.length > 0) {
      attachingRef.current += accepted.length
      setAttaching(attachingRef.current)
      const prepared = await Promise.all(accepted.map((file) => prepareImage(config, file, BROWSER_IMAGE_CODEC)))
      attachingRef.current -= accepted.length
      setAttaching(attachingRef.current)
      const loaded: Attachment[] = []
      prepared.forEach((result, index) => {
        if (result.ok) loaded.push({ key: nextKey.current++, dataUrl: result.dataUrl, name: accepted[index]?.name ?? '', bytes: result.bytes })
        else failure ??= result.error
      })
      if (loaded.length > 0) {
        const current = latest.current
        const element = textareaRef.current
        let text = current.draft
        let start = Math.min(element?.selectionStart ?? text.length, text.length)
        let end = Math.min(element?.selectionEnd ?? start, text.length)
        const attachments = [...current.attachments]
        for (const attachment of loaded) {
          attachments.push(attachment)
          const edit = insertPlaceholder(text, start, end, attachments.length)
          text = edit.text
          start = edit.caret
          end = edit.caret
        }
        caretRef.current = start
        onChange({ ...current, draft: text, attachments })
      }
    }
    if (failure !== null) setAttachFailure(failure)
  }

  const removeAttachment = (index: number) => {
    if (!removable) return
    const caret = textareaRef.current?.selectionStart ?? value.draft.length
    const edit = removePlaceholder(value.draft, caret, index + 1, value.attachments.length)
    caretRef.current = edit.caret
    setAttachFailure(null)
    onChange({ ...value, draft: edit.text, attachments: value.attachments.filter((_, position) => position !== index) })
  }

  const onDraftChange = (event: ChangeEvent<HTMLTextAreaElement>) => {
    const next = event.target.value
    // 删到一半的占位符整体删掉；占位符不在文本里的图片随之移除（GPUI `input_changed`）。
    const change = reconcileDraft(value.draft, next, event.target.selectionStart ?? next.length, value.attachments)
    if (change.draft !== next || change.attachments.length !== value.attachments.length) caretRef.current = change.caret
    onChange({ ...value, draft: change.draft, attachments: change.attachments })
  }

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = [...event.clipboardData.files].filter((file) => file.type.startsWith('image/'))
    // 不能附图时一律照常粘贴；剪贴板里有文字时文字优先（Word / 网页复制常带一张截图），只有纯图片才由附件接管。
    if (files.length === 0 || !canAttach) return
    if (!pasteTakesImages(files, event.clipboardData.getData('text/plain'))) return
    event.preventDefault()
    void addFiles(files)
  }

  const hasFiles = (event: DragEvent) => [...event.dataTransfer.types].includes('Files')
  const onDragEnter = (event: DragEvent) => {
    if (!hasFiles(event)) return
    event.preventDefault()
    dragDepth.current += 1
    if (canAttach) setDragging(true)
  }
  const onDragOver = (event: DragEvent) => {
    if (!hasFiles(event)) return
    // 一律接管文件拖放：放开后浏览器会把图片当页面打开，丢掉整个面板。
    event.preventDefault()
    event.dataTransfer.dropEffect = canAttach ? 'copy' : 'none'
  }
  const onDragLeave = (event: DragEvent) => {
    if (!hasFiles(event)) return
    dragDepth.current = Math.max(0, dragDepth.current - 1)
    if (dragDepth.current === 0) setDragging(false)
  }
  const onDrop = (event: DragEvent) => {
    if (!hasFiles(event)) return
    event.preventDefault()
    dragDepth.current = 0
    setDragging(false)
    // 非图片文件也交给 `addFiles` 校验：报「只能附加 PNG…」而不是悄悄忽略。
    void addFiles([...event.dataTransfer.files])
  }

  const openPicker = () => {
    const slots = (config?.maxImages ?? 0) - value.attachments.length - attachingRef.current
    if (slots <= 0) {
      setAttachFailure('tooMany')
      return
    }
    fileRef.current?.click()
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
    <div
      className="flex w-full flex-none flex-col items-center px-4 pt-1 pb-3"
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <div className="flex w-full max-w-3xl min-w-0 flex-col gap-1.5">
        {failureKey !== null ? <FieldError>{t(failureKey)}</FieldError> : null}
        {attachMessage !== null ? <FieldError>{attachMessage}</FieldError> : null}
        {editActive ? (
          <div className="flex min-w-0 items-center gap-2 rounded-md bg-muted px-2.5 py-1 text-xs text-muted-foreground">
            <Icon icon={Pencil} size="sm" className="shrink-0" />
            <span className="min-w-0 flex-1 break-words">
              {t('assistantEditing')}
              {editing.keepImages ? ` · ${t('assistantImages', { count: editing.imageCount })}` : ''}
            </span>
            <Button variant="ghost" className="h-6 px-1.5 text-xs" onClick={() => onChange(endEdit(latest.current))}>
              {t('assistantEditCancel')}
            </Button>
          </div>
        ) : null}
        <div
          className={cn(
            'flex min-w-0 flex-col rounded-2xl border bg-surface transition-colors focus-within:border-ring',
            dragging ? 'border-ring ring-1 ring-ring' : 'border-input',
          )}
        >
          {value.attachments.length > 0 || attaching > 0 ? (
            <div className="flex gap-2 overflow-x-auto px-3 pt-3 pb-1">
              {value.attachments.map((attachment, index) => (
                <Thumbnail key={attachment.key} attachment={attachment} number={index + 1} removable={removable} onRemove={() => removeAttachment(index)} />
              ))}
              {Array.from({ length: attaching }, (_, index) => (
                <PendingThumbnail key={`pending:${index}`} />
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
            onChange={onDraftChange}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            className="w-full resize-none bg-transparent px-3.5 pt-3 pb-1 text-sm leading-relaxed text-foreground outline-none placeholder:text-text-tertiary"
          />
          <div className="flex min-w-0 flex-wrap items-center gap-1.5 px-2 pb-2">
            {images ? (
              <>
                <input ref={fileRef} type="file" accept={ACCEPT} multiple hidden onChange={onPick} />
                <Tooltip content={t('assistantAttach')}>
                  <Button variant="ghost" iconOnly className="rounded-full" aria-label={t('assistantAttach')} disabled={!canAttach} onClick={openPicker}>
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
                  {view.cancelling ? (
                    <Spinner size="md" className="text-primary-foreground" />
                  ) : (
                    <Icon icon={running ? Square : ArrowUp} size="md" className={running ? 'fill-current' : undefined} />
                  )}
                </button>
              </span>
            </Tooltip>
          </div>
        </div>
        {sessionLimit.kind === 'remaining' ? <div className="px-1 text-xs text-text-tertiary">{t('assistantSessionRemaining', { count: sessionLimit.count })}</div> : null}
      </div>
    </div>
  )
}
