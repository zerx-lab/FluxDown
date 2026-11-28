import { forwardRef } from 'react'
import type { InputHTMLAttributes, ReactNode, TextareaHTMLAttributes } from 'react'
import { cn } from '../lib/cn'

const FIELD =
  'w-full min-w-0 rounded-md border border-input bg-transparent text-foreground placeholder:text-text-tertiary ' +
  'transition-colors hover:border-ring/60 focus-visible:border-ring focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring ' +
  'disabled:cursor-not-allowed disabled:opacity-50'

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  invalid?: boolean
  /** 输入框内右侧附件（如显示/隐藏密码按钮）。 */
  trailing?: ReactNode
}

/** 单行输入：`density.control` 高（触屏 44px）。 */
export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { invalid, trailing, className, ...rest },
  ref,
) {
  const field = (
    <input
      ref={ref}
      aria-invalid={invalid || undefined}
      className={cn(
        FIELD,
        'h-control px-2.5 coarse:min-h-touch',
        invalid && 'border-destructive hover:border-destructive focus-visible:border-destructive focus-visible:ring-destructive',
        trailing ? 'pr-10' : undefined,
        className,
      )}
      {...rest}
    />
  )
  if (!trailing) return field
  return (
    <div className="relative w-full min-w-0">
      {field}
      {/* 输入框内的附件按钮缩到 24px（触屏 36px）：28px 的按钮会盖住输入框上下边框。 */}
      <div className="absolute inset-y-0 right-0 flex items-center pr-0.5 [&>button]:size-6 coarse:[&>button]:size-9 coarse:[&>button]:min-h-9 coarse:[&>button]:min-w-9">
        {trailing}
      </div>
    </div>
  )
})

export interface TextareaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  invalid?: boolean
}

export const Textarea = forwardRef<HTMLTextAreaElement, TextareaProps>(function Textarea(
  { invalid, className, rows = 3, ...rest },
  ref,
) {
  return (
    <textarea
      ref={ref}
      rows={rows}
      aria-invalid={invalid || undefined}
      className={cn(
        FIELD,
        'resize-y px-2.5 py-1.5',
        invalid && 'border-destructive hover:border-destructive focus-visible:border-destructive focus-visible:ring-destructive',
        className,
      )}
      {...rest}
    />
  )
})
