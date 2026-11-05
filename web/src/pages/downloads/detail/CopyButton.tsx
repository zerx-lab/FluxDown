import { useEffect, useRef, useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { Button, Icon, Tooltip } from '../../../ui'
import { copyText } from '../../../lib/copy'
import { cn } from '../../../lib/cn'

/** 详情页行内复制按钮：点击后 1.5s 勾选态（GPUI `flash_copied`）。 */
export function CopyButton({ text, label, copiedLabel }: { text: string; label: string; copiedLabel: string }) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])
  const current = copied ? copiedLabel : label
  return (
    <Tooltip content={current}>
      <Button
        variant="ghost"
        iconOnly
        aria-label={current}
        onClick={() => {
          copyText(text)
          setCopied(true)
          clearTimeout(timer.current)
          timer.current = setTimeout(() => setCopied(false), 1500)
        }}
      >
        <Icon icon={copied ? Check : Copy} className={copied ? 'text-success' : undefined} />
      </Button>
    </Tooltip>
  )
}

/** 来源页链接：http(s) 时新标签页打开（`noopener noreferrer`），否则纯文本。 */
export function SourcePageLink({ text, url, className }: { text: string; url: string | null; className?: string }) {
  if (url === null) return <span className={className}>{text}</span>
  return (
    <a href={url} target="_blank" rel="noopener noreferrer" title={url} className={cn('text-accent-text hover:underline', className)}>
      {text}
    </a>
  )
}
