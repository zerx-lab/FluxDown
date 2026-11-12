// 单行路径文本：宽度不够时在中间省略（前段先省略，末段目录名尽量保留），悬停显示完整路径。
// 保留连续空格（`white-space: pre`），适配含空格 / 非 ASCII 字符的 Windows 路径。

import { splitPathTail } from '../lib/pathDisplay'
import { cn } from '../lib/cn'

export function PathText({ path, title = path, className }: { path: string; title?: string; className?: string }) {
  const { head, tail } = splitPathTail(path)
  return (
    <span className={cn('flex min-w-0', className)} title={title}>
      {head ? (
        <span className="min-w-0 overflow-hidden text-ellipsis" style={{ whiteSpace: 'pre' }}>
          {head}
        </span>
      ) : null}
      <span className="max-w-full shrink-0 overflow-hidden text-ellipsis" style={{ whiteSpace: 'pre' }}>
        {tail}
      </span>
    </span>
  )
}
