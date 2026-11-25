import * as RadixDialog from '@radix-ui/react-dialog'
import { X } from 'lucide-react'
import { useRef } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../i18n'
import { cn } from '../lib/cn'
import { Button } from './Button'
import type { ButtonVariant } from './Button'
import { useIsMobile } from './hooks'
import { Icon } from './Icon'

export type DialogSize = 'sm' | 'md' | 'lg' | 'xl'

const WIDTH: Record<DialogSize, string> = {
  sm: 'desktop:w-[400px]',
  md: 'desktop:w-[520px]',
  lg: 'desktop:w-[640px]',
  xl: 'desktop:w-[760px]',
}

export interface DialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: ReactNode
  description?: ReactNode
  /** 底栏（用 [`DialogFooter`] / [`ConfirmFooter`]）；表单类对话框建议提供。 */
  footer?: ReactNode
  size?: DialogSize
  children?: ReactNode
  /** 禁止点遮罩 / Esc 关闭（进行中的操作）。 */
  modalLocked?: boolean
  /** 点遮罩不关闭（Esc / 关闭按钮 / 取消仍可关闭）；用于填写验证码等误触代价高的表单。 */
  persistent?: boolean
  className?: string
}

/**
 * 按下点是否落在对话框矩形内。对话框内打开 modal 下拉 / 选择框时，Radix 会把对话框内容设为
 * `pointer-events: none`，此时点对话框空白处，命中的是其下方的遮罩（`pointer-events: auto`）；
 * 对话框的外部点击又延迟到 `click` 才判定，那时菜单已关闭，这次点击就被当成「点遮罩」而关掉对话框。
 * 用几何位置兜底：只有真正落在对话框外的按下才允许关闭。
 */
function pressedInside(content: HTMLElement | null, event: { detail: { originalEvent: PointerEvent } }): boolean {
  if (!content) return false
  const { clientX, clientY } = event.detail.originalEvent
  const rect = content.getBoundingClientRect()
  return clientX >= rect.left && clientX <= rect.right && clientY >= rect.top && clientY <= rect.bottom
}

/**
 * 对话框：桌面居中（`components.dialog.radius`），移动端（<=820px）全屏——
 * 顶部标题栏带关闭按钮，底栏吸底并避开安全区。标题栏 15/20 半粗（GPUI `dialog_title`）。
 */
export function Dialog({ open, onOpenChange, title, description, footer, size = 'md', children, modalLocked, persistent, className }: DialogProps) {
  const t = useT()
  const mobile = useIsMobile()
  const contentRef = useRef<HTMLDivElement>(null)
  return (
    <RadixDialog.Root open={open} onOpenChange={onOpenChange}>
      <RadixDialog.Portal>
        <RadixDialog.Overlay className="animate-fx-fade data-[state=closed]:animate-fx-fade-out fixed inset-0 z-40 bg-black/40" />
        <RadixDialog.Content
          ref={contentRef}
          onPointerDownOutside={(event) => pressedInside(contentRef.current, event) && event.preventDefault()}
          onInteractOutside={(event) => (modalLocked || persistent) && event.preventDefault()}
          onEscapeKeyDown={(event) => modalLocked && event.preventDefault()}
          {...(description ? {} : { 'aria-describedby': undefined })}
          className={cn(
            'fixed z-50 flex flex-col bg-surface text-surface-foreground outline-none',
            mobile
              ? 'animate-fx-slide-up data-[state=closed]:animate-fx-slide-up-out inset-0 pt-safe pl-safe pr-safe'
              : cn(
                  'view-dialog animate-fx-zoom data-[state=closed]:animate-fx-zoom-out top-1/2 left-1/2 max-h-[85dvh] w-[calc(100vw-32px)] -translate-x-1/2 -translate-y-1/2 rounded-[var(--fx-components-dialog-radius)] border border-hairline shadow-lg',
                  WIDTH[size],
                ),
            className,
          )}
        >
          <div className="flex shrink-0 items-start gap-2 px-4 pt-4 pb-2 mobile:items-center mobile:border-b mobile:border-hairline mobile:pb-3">
            <div className="min-w-0 flex-1">
              <RadixDialog.Title className="text-title font-semibold text-foreground">{title}</RadixDialog.Title>
              {description ? (
                <RadixDialog.Description className="mt-0.5 text-xs text-muted-foreground">{description}</RadixDialog.Description>
              ) : null}
            </div>
            {!modalLocked ? (
              <RadixDialog.Close asChild>
                <Button
                  variant="ghost"
                  iconOnly
                  aria-label={t('close')}
                  title={t('close')}
                  className="text-muted-foreground hover:text-foreground desktop:-mt-1 desktop:-mr-1"
                >
                  <Icon icon={X} size="md" />
                </Button>
              </RadixDialog.Close>
            ) : null}
          </div>
          <div className="view-viewport min-h-0 flex-1 overflow-y-auto px-4 py-2">{children}</div>
          {footer ? (
            <div className="shrink-0 border-t border-hairline bg-chrome px-4 py-3 pb-[max(0.75rem,env(safe-area-inset-bottom))] desktop:rounded-b-[var(--fx-components-dialog-radius)]">
              {footer}
            </div>
          ) : null}
        </RadixDialog.Content>
      </RadixDialog.Portal>
    </RadixDialog.Root>
  )
}

/** 底栏容器：右对齐，取消在左、主操作在右；窄屏按钮等分整行。 */
export function DialogFooter({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn('flex items-center justify-end gap-2 narrow:[&>*]:flex-1', className)}>{children}</div>
}

export type DialogIntent = 'confirm' | 'destructive'

/** 统一底栏：取消（outline）+ 主操作（primary / danger）。 */
export function ConfirmFooter({
  cancelLabel,
  okLabel,
  intent = 'confirm',
  onCancel,
  onOk,
  okDisabled,
  loading,
  extra,
}: {
  cancelLabel?: string | null
  okLabel: string
  intent?: DialogIntent
  onCancel: () => void
  onOk: () => void
  okDisabled?: boolean
  loading?: boolean
  /** 左侧附加内容（如「稍后下载」）。 */
  extra?: ReactNode
}) {
  const t = useT()
  const variant: ButtonVariant = intent === 'destructive' ? 'danger' : 'primary'
  return (
    <DialogFooter>
      {extra ? <div className="mr-auto">{extra}</div> : null}
      {cancelLabel === null ? null : (
        <Button variant="outline" onClick={onCancel}>
          {cancelLabel ?? t('cancel')}
        </Button>
      )}
      <Button variant={variant} onClick={onOk} disabled={okDisabled} loading={loading}>
        {okLabel}
      </Button>
    </DialogFooter>
  )
}
