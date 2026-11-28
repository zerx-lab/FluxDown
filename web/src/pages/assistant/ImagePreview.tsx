// 附图缩略图 + 大图预览：点缩略图在覆盖层里看原图。覆盖层是 Radix 模态对话框：焦点进入并被圈住、Esc / 点击任意处 /
// 关闭按钮都能关闭、关闭后焦点回到缩略图；缩略图按钮与对话框标题都带「查看第 N 张图片」标签。

import * as RadixDialog from '@radix-ui/react-dialog'
import { X } from 'lucide-react'
import { useState } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { Button, Icon } from '../../ui'

export function PreviewableImage({
  src,
  alt,
  number,
  className,
  imageClassName,
}: {
  src: string
  alt: string
  /** 第几张图（从 1 起），用于标签。 */
  number: number
  /** 缩略图按钮的样式。 */
  className?: string
  /** 缩略图 `<img>` 的样式。 */
  imageClassName?: string
}) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const label = t('assistantImagePreview', { n: number })
  return (
    <>
      <button
        type="button"
        aria-label={label}
        title={label}
        onClick={() => setOpen(true)}
        className={cn('max-w-full cursor-zoom-in rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring', className)}
      >
        <img src={src} alt={alt} className={imageClassName} />
      </button>
      <RadixDialog.Root open={open} onOpenChange={setOpen}>
        <RadixDialog.Portal>
          <RadixDialog.Overlay className="animate-fx-fade data-[state=closed]:animate-fx-fade-out fixed inset-0 z-40 bg-black/80" />
          <RadixDialog.Content
            aria-describedby={undefined}
            onClick={() => setOpen(false)}
            className="animate-fx-fade data-[state=closed]:animate-fx-fade-out fixed inset-0 z-50 grid cursor-zoom-out place-items-center p-4 pt-safe pb-safe outline-none"
          >
            <RadixDialog.Title className="sr-only">{label}</RadixDialog.Title>
            <img src={src} alt={alt} className="max-h-full max-w-full rounded-md object-contain" />
            <RadixDialog.Close asChild>
              <Button
                variant="ghost"
                iconOnly
                aria-label={t('assistantImagePreviewClose')}
                title={t('assistantImagePreviewClose')}
                className="absolute top-3 right-3 bg-black/50 text-white hover:bg-black/70 hover:text-white"
              >
                <Icon icon={X} size="md" />
              </Button>
            </RadixDialog.Close>
          </RadixDialog.Content>
        </RadixDialog.Portal>
      </RadixDialog.Root>
    </>
  )
}
