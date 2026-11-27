// 助手回答的 Markdown 渲染（解析见 `markdown.ts`）。回答是用户 / 模型数据：长 URL、长单词必须换行而不撑破版面。

import { useMemo } from 'react'
import { markdownBlocks, parseInline } from './markdown'

function Inline({ text }: { text: string }) {
  return (
    <>
      {parseInline(text).map((segment, index) => {
        switch (segment.type) {
          case 'code':
            return (
              <code key={index} className="rounded-sm bg-muted px-1 font-mono text-xs break-all">
                {segment.text}
              </code>
            )
          case 'bold':
            return <strong key={index}>{segment.text}</strong>
          case 'link':
            return (
              <a key={index} href={segment.url} target="_blank" rel="noopener noreferrer" className="text-accent-text underline break-all">
                {segment.text}
              </a>
            )
          case 'text':
            return <span key={index}>{segment.text}</span>
        }
      })}
    </>
  )
}

export function Markdown({ text }: { text: string }) {
  const blocks = useMemo(() => markdownBlocks(text), [text])
  return (
    <div className="flex w-full min-w-0 flex-col gap-2 text-sm leading-relaxed break-words text-foreground">
      {blocks.map((block, index) => {
        switch (block.kind) {
          case 'heading':
            return (
              <div key={index} className="pt-1 font-semibold">
                <Inline text={block.text} />
              </div>
            )
          case 'list':
            return (
              <ul key={index} className="flex min-w-0 flex-col gap-1">
                {block.items.map((item, itemIndex) => (
                  <li key={itemIndex} className="list-disc" style={{ marginLeft: 20 + item.depth * 16 }}>
                    <Inline text={item.text} />
                  </li>
                ))}
              </ul>
            )
          case 'paragraph':
            return (
              <p key={index} className="whitespace-pre-line">
                <Inline text={block.lines.join('\n')} />
              </p>
            )
          case 'code':
            return (
              <pre key={index} className="max-w-full overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs whitespace-pre">
                <code>{block.text}</code>
              </pre>
            )
        }
      })}
    </div>
  )
}
