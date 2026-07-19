// 图标复制按钮：点击写入剪贴板后切换为绿色对勾 1.5s 作为视觉反馈。
// 外观（尺寸/hover）由父级选择器（.copy-row button / .token-box button）或
// 传入的 className（如任务行 .task-act）提供；点击不冒泡（行内使用时不连带选中行）。

import { useEffect, useRef, useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { cn } from '../lib/cn'
import { useI18n } from '../lib/i18n'

export function CopyButton({ value, title, className }: { value: string; title?: string; className?: string }) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])

  return (
    <button
      type="button"
      className={cn(className, copied && 'copied')}
      title={copied ? t('common.copied') : (title ?? t('common.copy'))}
      onClick={(e) => {
        e.stopPropagation()
        void navigator.clipboard.writeText(value)
        setCopied(true)
        window.clearTimeout(timer.current)
        timer.current = window.setTimeout(() => setCopied(false), 1500)
      }}
    >
      {copied ? <Check /> : <Copy />}
    </button>
  )
}
