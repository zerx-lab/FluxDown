// 助手回答的 Markdown 渲染（解析见 `markdown.ts`）。回答是用户 / 模型数据：长 URL、长单词必须换行而不撑破版面，
// 所有文本都以文本节点输出（不插入 HTML）；链接只会是 http(s)。代码块右上角带复制按钮。

import { useMemo } from 'react'
import type { ReactNode } from 'react'
import { useT } from '../../i18n'
import { cn } from '../../lib/cn'
import { CopyButton } from './CopyButton'
import { markdownBlocks, parseInline } from './markdown'
import type { InlineSegment, MarkdownBlock } from './markdown'

function Segments({ segments }: { segments: readonly InlineSegment[] }) {
  return (
    <>
      {segments.map((segment, index) => {
        switch (segment.type) {
          case 'code':
            return (
              <code key={index} className="rounded-sm bg-muted px-1 font-mono text-xs break-all">
                {segment.text}
              </code>
            )
          case 'bold':
            return (
              <strong key={index} className="font-semibold">
                <Segments segments={segment.children} />
              </strong>
            )
          case 'italic':
            return (
              <em key={index}>
                <Segments segments={segment.children} />
              </em>
            )
          case 'strike':
            return (
              <del key={index}>
                <Segments segments={segment.children} />
              </del>
            )
          case 'link':
            return (
              <a key={index} href={segment.url} target="_blank" rel="noopener noreferrer" className="text-accent-text underline break-all">
                <Segments segments={segment.children} />
              </a>
            )
          case 'text':
            return <span key={index}>{segment.text}</span>
        }
      })}
    </>
  )
}

function Inline({ text }: { text: string }) {
  const segments = useMemo(() => parseInline(text), [text])
  return <Segments segments={segments} />
}

const HEADING_CLASS: Record<number, string> = {
  1: 'pt-1 text-base font-semibold',
  2: 'pt-1 text-base font-semibold',
  3: 'pt-1 text-sm font-semibold',
  4: 'pt-0.5 text-sm font-semibold',
  5: 'text-sm font-medium',
  6: 'text-sm font-medium text-muted-foreground',
}

const ALIGN_CLASS = { left: 'text-left', center: 'text-center', right: 'text-right' } as const

function CodeBlock({ text, lang }: { text: string; lang: string }) {
  const t = useT()
  return (
    <div className="group relative max-w-full min-w-0">
      <pre className="max-w-full overflow-x-auto rounded-md bg-muted p-3 pr-10 font-mono text-xs whitespace-pre">
        <code>{text}</code>
      </pre>
      {lang !== '' ? <span className="pointer-events-none absolute right-10 top-1.5 text-caption text-text-tertiary">{lang}</span> : null}
      <CopyButton getText={() => text} label={t('assistantCopyCode')} className="absolute top-0.5 right-0.5" />
    </div>
  )
}

function Blocks({ blocks, tight = false }: { blocks: readonly MarkdownBlock[]; tight?: boolean }) {
  return (
    <>
      {blocks.map((block, index) => {
        switch (block.kind) {
          case 'heading':
            return (
              <div key={index} role="heading" aria-level={block.level} className={HEADING_CLASS[block.level]}>
                <Inline text={block.text} />
              </div>
            )
          case 'paragraph':
            return (
              <p key={index} className={cn('min-w-0 whitespace-pre-line', tight && 'my-0')}>
                <Inline text={block.lines.join('\n')} />
              </p>
            )
          case 'code':
            return <CodeBlock key={index} text={block.text} lang={block.lang} />
          case 'rule':
            return <hr key={index} className="border-hairline" />
          case 'quote':
            return (
              <blockquote key={index} className="flex min-w-0 flex-col gap-2 border-l-2 border-hairline pl-3 text-muted-foreground">
                <Blocks blocks={block.blocks} />
              </blockquote>
            )
          case 'list': {
            const Tag = block.ordered ? 'ol' : 'ul'
            return (
              <Tag
                key={index}
                start={block.ordered ? block.start : undefined}
                className={cn('flex min-w-0 flex-col gap-1 pl-5', block.ordered ? 'list-decimal' : 'list-disc')}
              >
                {block.items.map((item, itemIndex) => (
                  <li key={itemIndex} className={cn('min-w-0', item.checked !== null && 'list-none')}>
                    <div className="flex min-w-0 flex-col gap-1">
                      {item.checked !== null ? (
                        <ItemTask checked={item.checked} blocks={item.blocks} />
                      ) : (
                        <Blocks blocks={item.blocks} tight />
                      )}
                    </div>
                  </li>
                ))}
              </Tag>
            )
          }
          case 'table':
            return (
              <div key={index} className="max-w-full overflow-x-auto">
                <table className="w-max min-w-full border-collapse text-xs">
                  <thead>
                    <tr>
                      {block.header.map((cell, cellIndex) => (
                        <th
                          key={cellIndex}
                          scope="col"
                          className={cn('border border-hairline bg-muted px-2 py-1 font-semibold', ALIGN_CLASS[block.align[cellIndex] ?? 'left'])}
                        >
                          <Inline text={cell} />
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {block.rows.map((row, rowIndex) => (
                      <tr key={rowIndex}>
                        {row.map((cell, cellIndex) => (
                          <td key={cellIndex} className={cn('border border-hairline px-2 py-1 align-top', ALIGN_CLASS[block.align[cellIndex] ?? 'left'])}>
                            <Inline text={cell} />
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )
        }
      })}
    </>
  )
}

/** 任务列表项：勾选框只读展示，正文照常渲染。 */
function ItemTask({ checked, blocks }: { checked: boolean; blocks: readonly MarkdownBlock[] }): ReactNode {
  return (
    <div className="flex min-w-0 items-start gap-2 -ml-5">
      <input type="checkbox" checked={checked} readOnly disabled className="mt-1 shrink-0" />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <Blocks blocks={blocks} tight />
      </div>
    </div>
  )
}

export function Markdown({ text }: { text: string }) {
  const blocks = useMemo(() => markdownBlocks(text), [text])
  return (
    <div className="flex w-full min-w-0 flex-col gap-2 text-sm leading-relaxed break-words text-foreground">
      <Blocks blocks={blocks} />
    </div>
  )
}
