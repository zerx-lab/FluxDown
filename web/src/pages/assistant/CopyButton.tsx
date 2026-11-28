// 复制按钮：写入剪贴板后图标变为勾并显示「已复制」1.5 秒（GPUI `render_copy_turn`）。文本在点击时才取，
// 失败（非安全上下文且兼容写入也被拒）弹提示。

import { Check, Copy } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { copyText } from '../../lib/copy'
import { Button, Icon, Tooltip, toast } from '../../ui'

/** 「已复制」状态保持的时长。 */
const COPIED_MS = 1500

export function CopyButton({ getText, label, className }: { getText: () => string; label: string; className?: string }) {
  const t = useT()
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => () => clearTimeout(timer.current), [])

  const copy = () => {
    void copyText(getText()).then((ok) => {
      if (!ok) {
        toast.key('copyFileFailed', 'error')
        return
      }
      setCopied(true)
      clearTimeout(timer.current)
      timer.current = window.setTimeout(() => setCopied(false), COPIED_MS)
    })
  }

  const text = copied ? t('assistantCopied') : label
  return (
    <Tooltip content={text}>
      <Button variant="ghost" iconOnly aria-label={text} className={cn('text-muted-foreground', copied && 'text-success', className)} onClick={copy}>
        <Icon icon={copied ? Check : Copy} size="md" />
      </Button>
    </Tooltip>
  )
}
