// 助手回答的 Markdown 解析（纯函数，不引入依赖）：
// 围栏代码块沿用插件视图的 `splitMarkdown`，其余文本沿用更新说明的 `releaseNoteBlocks`
// （标题 / 列表 / 段落），这里只补行内语法：`代码`、**粗体**、[文字](http(s) 链接)。

import { releaseNoteBlocks } from '../../lib/releaseNotes'
import type { ReleaseNoteItem } from '../../lib/releaseNotes'
import { splitMarkdown } from '../plugins/view/markdownLite'

export type MarkdownBlock =
  | { kind: 'heading'; text: string }
  | { kind: 'list'; items: ReleaseNoteItem[] }
  | { kind: 'paragraph'; lines: string[] }
  | { kind: 'code'; text: string; lang: string }

/** 把回答拆成块：围栏代码块原样保留，其余按标题 / 列表 / 段落切分。 */
export function markdownBlocks(source: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = []
  for (const segment of splitMarkdown(source)) {
    if (segment.type === 'code') blocks.push({ kind: 'code', text: segment.text, lang: segment.lang })
    else blocks.push(...releaseNoteBlocks(segment.text))
  }
  return blocks
}

export type InlineSegment =
  | { type: 'text'; text: string }
  | { type: 'code'; text: string }
  | { type: 'bold'; text: string }
  | { type: 'link'; text: string; url: string }

/** 反引号代码 | `**粗体**` / `__粗体__` | `[文字](链接)`（只认 http / https，其余协议保持原文）。 */
const INLINE = /`([^`\n]+)`|\*\*([^*\n]+?)\*\*|__([^_\n]+?)__|\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g

/** 行内语法切分；没有标记的文字、未配对的标记原样留在 `text` 段里。 */
export function parseInline(text: string): InlineSegment[] {
  const segments: InlineSegment[] = []
  let last = 0
  for (const match of text.matchAll(INLINE)) {
    const start = match.index
    if (start > last) segments.push({ type: 'text', text: text.slice(last, start) })
    const [, code, starBold, underscoreBold, label, url] = match
    if (code !== undefined) segments.push({ type: 'code', text: code })
    else if (label !== undefined && url !== undefined) segments.push({ type: 'link', text: label, url })
    else segments.push({ type: 'bold', text: starBold ?? underscoreBold ?? '' })
    last = start + match[0].length
  }
  if (last < text.length) segments.push({ type: 'text', text: text.slice(last) })
  return segments
}
