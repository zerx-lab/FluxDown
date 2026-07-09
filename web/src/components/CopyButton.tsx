// 图标复制按钮：点击写入剪贴板后切换为绿色对勾 1.5s 作为视觉反馈。
// 外观（尺寸/hover）由父级选择器（.copy-row button / .token-box button）提供。

import { useEffect, useRef, useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { useI18n } from '../lib/i18n'

export function CopyButton({ value, title }: { value: string; title?: string }) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])

  return (
    <button
      type="button"
      className={copied ? 'copied' : undefined}
      title={copied ? t('common.copied') : (title ?? t('common.copy'))}
      onClick={() => {
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
