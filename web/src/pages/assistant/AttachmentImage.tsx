// 历史提问里的一张附图：`AssistantMessageDto.images` 是附件 ID，经 `agent.assistant.attachment` 取成 `data:` URL
// （控制器按 ID 缓存；失败不缓存，可点重试）。取图期间显示转圈占位，失败显示原因与重试。

import { ImageOff } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useT } from '../../i18n'
import { Button, Icon, Spinner } from '../../ui'
import type { AssistantController } from './controller'

type Load = { status: 'loading' } | { status: 'ready'; dataUrl: string } | { status: 'failed' }

export function AttachmentImage({
  attachmentId,
  number,
  controller,
  disabled,
}: {
  attachmentId: string
  number: number
  controller: AssistantController
  /** 与 agent 断开：不发起新的取图。 */
  disabled: boolean
}) {
  const t = useT()
  const [attempt, setAttempt] = useState(0)
  const [load, setLoad] = useState<Load>({ status: 'loading' })

  useEffect(() => {
    if (disabled) return
    let live = true
    setLoad({ status: 'loading' })
    controller.attachment(attachmentId).then(
      (dataUrl) => {
        if (live) setLoad({ status: 'ready', dataUrl })
      },
      () => {
        if (live) setLoad({ status: 'failed' })
      },
    )
    return () => {
      live = false
    }
  }, [controller, attachmentId, attempt, disabled])

  const label = t('assistantImageLabel', { n: number })
  if (load.status === 'ready') {
    return <img src={load.dataUrl} alt={label} className="max-h-40 max-w-full rounded-md border border-hairline object-contain" />
  }
  if (load.status === 'failed') {
    return (
      <div className="flex max-w-full items-center gap-1.5 rounded-md border border-hairline px-2 py-1 text-xs text-text-tertiary">
        <Icon icon={ImageOff} size="sm" className="shrink-0" />
        <span className="min-w-0 break-words">{t('assistantImageLoadFailed', { n: number })}</span>
        <Button variant="ghost" className="h-6 px-1.5 text-xs" onClick={() => setAttempt((value) => value + 1)}>
          {t('assistantRetry')}
        </Button>
      </div>
    )
  }
  return (
    <div role="status" aria-label={t('assistantLoading')} className="grid size-20 place-items-center rounded-md border border-hairline bg-muted">
      <Spinner size="md" />
    </div>
  )
}
