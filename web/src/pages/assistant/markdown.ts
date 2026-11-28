// 助手回答的 Markdown 解析（纯函数，不引入依赖）：块级（标题、段落、围栏代码、有序 / 无序 / 任务列表、引用、
// GFM 表格、分隔线）与行内（代码、粗体、斜体、删除线、链接、裸 URL、转义）。按 CommonMark 的主干规则实现，
// 刻意不做「懒惰续行」：列表项之后没有缩进的行就是新块，不会被吞进上一项。不解析原始 HTML，也不渲染图片
// （`![x](u)` 退化为链接），链接只认 http(s)；回答是模型数据，输出一律当文本渲染。

export type TableAlign = 'left' | 'center' | 'right' | null

export interface ListItem {
  /** 任务列表项（`- [ ]` / `- [x]`）的勾选状态；普通项为 null。 */
  checked: boolean | null
  blocks: MarkdownBlock[]
}

export type MarkdownBlock =
  | { kind: 'heading'; level: number; text: string }
  | { kind: 'paragraph'; lines: string[] }
  | { kind: 'code'; text: string; lang: string }
  | { kind: 'list'; ordered: boolean; start: number; items: ListItem[] }
  | { kind: 'quote'; blocks: MarkdownBlock[] }
  | { kind: 'table'; header: string[]; align: TableAlign[]; rows: string[][] }
  | { kind: 'rule' }

const FENCE = /^ {0,3}(`{3,}|~{3,})[ \t]*([^\s`]*)[^`]*$/
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/
const QUOTE = /^ {0,3}>/
const LIST_MARKER = /^( {0,3})([-+*]|\d{1,9}[.)])( +|\t|$)/
const TABLE_DELIMITER = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/
const TASK = /^\[([ xX])\][ \t]+/

function isBlank(line: string): boolean {
  return line.trim() === ''
}

/** 行首缩进的列数（制表符按 4 列对齐）。 */
function indentOf(line: string): number {
  let columns = 0
  for (const char of line) {
    if (char === ' ') columns += 1
    else if (char === '\t') columns += 4 - (columns % 4)
    else break
  }
  return columns
}

/** 去掉行首至多 `columns` 列缩进。 */
function dedent(line: string, columns: number): string {
  let removed = 0
  let index = 0
  while (index < line.length && removed < columns) {
    const char = line[index]
    if (char === ' ') removed += 1
    else if (char === '\t') {
      const width = 4 - (removed % 4)
      if (removed + width > columns) break
      removed += width
    } else break
    index += 1
  }
  return line.slice(index)
}

interface Marker {
  indent: number
  /** 标记字符（`-` `+` `*` / 有序列表的分隔符 `.` `)`）：变化就是新列表。 */
  kind: string
  ordered: boolean
  number: number
  /** 内容起始列（子块相对它缩进）。 */
  contentIndent: number
  /** 标记后的同行文字。 */
  rest: string
}

function matchMarker(line: string): Marker | null {
  const match = LIST_MARKER.exec(line)
  if (!match) return null
  const indent = (match[1] ?? '').length
  const marker = match[2] ?? ''
  const spaces = match[3] ?? ''
  const rest = line.slice(match[0].length)
  const ordered = /\d/.test(marker)
  // 标记后空 5 列以上算缩进代码，内容从标记后一格开始；行尾标记（空项）内容从标记后一格开始。
  const gap = rest === '' || spaces.length > 4 ? 1 : spaces === '\t' ? 4 : spaces.length
  return {
    indent,
    kind: ordered ? marker.slice(-1) : marker,
    ordered,
    number: ordered ? Number.parseInt(marker, 10) : 0,
    contentIndent: indent + marker.length + gap,
    rest: spaces.length > 4 && rest !== '' ? ' '.repeat(spaces.length - 1) + rest : rest,
  }
}

function splitRow(line: string): string[] {
  let text = line.trim()
  if (text.startsWith('|')) text = text.slice(1)
  if (text.endsWith('|') && !text.endsWith('\\|')) text = text.slice(0, -1)
  const cells: string[] = []
  let current = ''
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index] ?? ''
    if (char === '\\' && text[index + 1] === '|') {
      current += '|'
      index += 1
    } else if (char === '|') {
      cells.push(current.trim())
      current = ''
    } else current += char
  }
  cells.push(current.trim())
  return cells
}

function tableAlign(cell: string): TableAlign {
  const left = cell.startsWith(':')
  const right = cell.endsWith(':')
  if (left && right) return 'center'
  if (right) return 'right'
  return left ? 'left' : null
}

/** `lines[index]` 起是否是表格头（含 `|` 的一行 + 列数一致的分隔行）。 */
function tableStarts(lines: readonly string[], index: number): boolean {
  const head = lines[index]
  const delimiter = lines[index + 1]
  if (head === undefined || delimiter === undefined || !head.includes('|') || !delimiter.includes('-')) return false
  if (!TABLE_DELIMITER.test(delimiter)) return false
  return splitRow(head).length === splitRow(delimiter).length
}

/** 这一行能否打断一个段落（开启新块）。 */
function interruptsParagraph(lines: readonly string[], index: number): boolean {
  const line = lines[index] ?? ''
  if (FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line)) return true
  const marker = matchMarker(line)
  // 列表只在项目非空、且有序列表从 1 起时才能打断段落（避免「2024. 年……」被当成列表）。
  if (marker !== null && marker.rest.trim() !== '' && (!marker.ordered || marker.number === 1)) return true
  return tableStarts(lines, index)
}

function parseFence(lines: readonly string[], start: number): { block: MarkdownBlock; next: number } | null {
  const open = FENCE.exec(lines[start] ?? '')
  if (!open) return null
  const fence = open[1] ?? '```'
  const char = fence.charAt(0)
  const body: string[] = []
  let index = start + 1
  // 围栏缩进（至多 3 列）从内容里同样去掉。
  const fenceIndent = indentOf(lines[start] ?? '')
  for (; index < lines.length; index += 1) {
    const line = lines[index] ?? ''
    const trimmed = line.trim()
    if (trimmed.length >= fence.length && trimmed.split('').every((c) => c === char)) {
      index += 1
      break
    }
    body.push(dedent(line, fenceIndent))
  }
  return { block: { kind: 'code', text: body.join('\n'), lang: open[2] ?? '' }, next: index }
}

function parseList(lines: readonly string[], start: number): { block: MarkdownBlock; next: number } {
  const first = matchMarker(lines[start] ?? '') as Marker
  const items: ListItem[] = []
  let index = start
  while (index < lines.length) {
    const marker = matchMarker(lines[index] ?? '')
    if (marker === null || marker.ordered !== first.ordered || marker.kind !== first.kind) break
    // 同一列表的兄弟项：缩进不超过首项内容缩进（更深的是嵌套，已被上一项吞掉）。
    if (marker.indent >= first.contentIndent) break
    const body: string[] = [marker.rest]
    index += 1
    let pendingBlank = 0
    // 子块的缩进基准；嵌套列表缩进不够内容列时（如有序项下缩进 2 格的 `- x`）放宽为它自己的缩进。
    let strip = marker.contentIndent
    while (index < lines.length) {
      const line = lines[index] ?? ''
      if (isBlank(line)) {
        pendingBlank += 1
        index += 1
        continue
      }
      const lineIndent = indentOf(line)
      const child = lineIndent < strip ? matchMarker(line) : null
      if (lineIndent >= strip || (child !== null && child.indent > marker.indent)) {
        if (lineIndent < strip) strip = lineIndent
        for (let blank = 0; blank < pendingBlank; blank += 1) body.push('')
        pendingBlank = 0
        body.push(dedent(line, strip))
        index += 1
        continue
      }
      break
    }
    // 项目之间允许空行；列表结束时把没用掉的空行还给外层。
    const next = lines[index]
    const sibling = next === undefined ? null : matchMarker(next)
    const continues = sibling !== null && sibling.ordered === first.ordered && sibling.kind === first.kind && sibling.indent < first.contentIndent
    if (!continues) index -= pendingBlank
    items.push(intoItem(body))
    if (!continues) break
  }
  return { block: { kind: 'list', ordered: first.ordered, start: first.number, items }, next: index }
}

function intoItem(body: string[]): ListItem {
  const head = body[0] ?? ''
  const task = TASK.exec(head)
  if (task) {
    const rest = [head.slice(task[0].length), ...body.slice(1)]
    return { checked: task[1] !== ' ', blocks: parseBlocks(rest) }
  }
  return { checked: null, blocks: parseBlocks(body) }
}

function parseTable(lines: readonly string[], start: number): { block: MarkdownBlock; next: number } {
  const header = splitRow(lines[start] ?? '')
  const align = splitRow(lines[start + 1] ?? '').map(tableAlign)
  const rows: string[][] = []
  let index = start + 2
  for (; index < lines.length; index += 1) {
    const line = lines[index] ?? ''
    if (isBlank(line) || !line.includes('|') || FENCE.test(line) || HEADING.test(line) || QUOTE.test(line)) break
    const cells = splitRow(line)
    while (cells.length < header.length) cells.push('')
    rows.push(cells.slice(0, header.length))
  }
  return { block: { kind: 'table', header, align, rows }, next: index }
}

function parseBlocks(lines: readonly string[]): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = []
  let index = 0
  while (index < lines.length) {
    const line = lines[index] ?? ''
    if (isBlank(line)) {
      index += 1
      continue
    }
    const fence = parseFence(lines, index)
    if (fence) {
      blocks.push(fence.block)
      index = fence.next
      continue
    }
    const heading = HEADING.exec(line)
    if (heading) {
      blocks.push({ kind: 'heading', level: (heading[1] ?? '#').length, text: (heading[2] ?? '').trim() })
      index += 1
      continue
    }
    if (RULE.test(line)) {
      blocks.push({ kind: 'rule' })
      index += 1
      continue
    }
    if (QUOTE.test(line)) {
      // 引用只收连续的 `>` 行（不做懒惰续行）。
      const quoted: string[] = []
      while (index < lines.length && QUOTE.test(lines[index] ?? '')) {
        quoted.push((lines[index] ?? '').replace(/^ {0,3}> ?/, ''))
        index += 1
      }
      blocks.push({ kind: 'quote', blocks: parseBlocks(quoted) })
      continue
    }
    if (matchMarker(line) !== null) {
      const list = parseList(lines, index)
      blocks.push(list.block)
      index = list.next
      continue
    }
    if (tableStarts(lines, index)) {
      const table = parseTable(lines, index)
      blocks.push(table.block)
      index = table.next
      continue
    }
    const paragraph: string[] = [line.trim()]
    index += 1
    while (index < lines.length && !isBlank(lines[index] ?? '') && !interruptsParagraph(lines, index)) {
      paragraph.push((lines[index] ?? '').trim())
      index += 1
    }
    blocks.push({ kind: 'paragraph', lines: paragraph })
  }
  return blocks
}

/** 把回答拆成块。 */
export function markdownBlocks(source: string): MarkdownBlock[] {
  return parseBlocks(source.split(/\r\n|\r|\n/))
}

// ───────────────────────── 行内 ─────────────────────────

export type InlineSegment =
  | { type: 'text'; text: string }
  | { type: 'code'; text: string }
  | { type: 'link'; url: string; children: InlineSegment[] }
  | { type: 'bold' | 'italic' | 'strike'; children: InlineSegment[] }

type Token =
  | { t: 'node'; segment: InlineSegment }
  | { t: 'delim'; char: string; count: number; original: number; canOpen: boolean; canClose: boolean }

const PUNCTUATION = /[\p{P}\p{S}]/u
const ESCAPABLE = /[!-/:-@[-`{-~]/
const URL_START = /https?:\/\/[^\s<>]+/iy

function isSpace(char: string | undefined): boolean {
  return char === undefined || /\s/u.test(char)
}

function isPunctuation(char: string | undefined): boolean {
  return char !== undefined && PUNCTUATION.test(char)
}

function charBefore(text: string, index: number): string | undefined {
  if (index <= 0) return undefined
  const low = text.charCodeAt(index - 1)
  if (low >= 0xdc00 && low <= 0xdfff && index >= 2) return text.slice(index - 2, index)
  return text.charAt(index - 1)
}

function charAfter(text: string, index: number): string | undefined {
  return index >= text.length ? undefined : String.fromCodePoint(text.codePointAt(index) ?? 0)
}

/** 链接目标的长度上限(与 GPUI / 官网一致)。 */
const MAX_LINK_CHARS = 2048

/**
 * 链接目标只认 http / https:非空主机、不带 userinfo(`https://docs.example.com@evil.test/` 这类伪装)、
 * 无空白 / 控制字符、长度 ≤ 2048。其余不成为链接,按纯文本显示。
 */
export function safeUrl(url: string): string | null {
  if (url.length > MAX_LINK_CHARS || /[\s<>\p{Cc}]/u.test(url)) return null
  const match = /^https?:\/\/([^/?#]*)/i.exec(url)
  if (match === null) return null
  const host = match[1] ?? ''
  if (host === '' || host.includes('@')) return null
  return url
}

/** `[` 在 `from` 处：返回匹配的 `]` 位置（计嵌套与转义，跳过代码段），找不到返回 -1。 */
function closingBracket(text: string, from: number): number {
  let depth = 0
  for (let index = from; index < text.length; index += 1) {
    const char = text[index]
    if (char === '\\') {
      index += 1
    } else if (char === '`') {
      let run = 1
      while (text[index + run] === '`') run += 1
      const close = text.indexOf('`'.repeat(run), index + run)
      index = close < 0 ? index + run - 1 : close + run - 1
    } else if (char === '[') depth += 1
    else if (char === ']') {
      depth -= 1
      if (depth === 0) return index
    }
  }
  return -1
}

/** `(` 在 `open` 处：解析链接目标与可选标题，返回目标与 `)` 之后的位置；不合法返回 null。 */
function linkTarget(text: string, open: number): { url: string; end: number } | null {
  let index = open + 1
  while (/\s/u.test(text[index] ?? 'x')) index += 1
  let url = ''
  if (text[index] === '<') {
    const close = text.indexOf('>', index + 1)
    if (close < 0 || /[\n]/.test(text.slice(index, close))) return null
    url = text.slice(index + 1, close)
    index = close + 1
  } else {
    let depth = 0
    const begin = index
    while (index < text.length) {
      const char = text[index] ?? ''
      if (/\s/u.test(char)) break
      if (char === '\\') index += 1
      else if (char === '(') depth += 1
      else if (char === ')') {
        if (depth === 0) break
        depth -= 1
      }
      index += 1
    }
    url = text.slice(begin, index)
  }
  while (/\s/u.test(text[index] ?? 'x')) index += 1
  const quote = text[index]
  if (quote === '"' || quote === "'" || quote === '(') {
    const closer = quote === '(' ? ')' : quote
    const close = text.indexOf(closer, index + 1)
    if (close < 0) return null
    index = close + 1
    while (/\s/u.test(text[index] ?? 'x')) index += 1
  }
  return text[index] === ')' ? { url, end: index + 1 } : null
}

/** 裸 URL 末尾的标点与不配对的 `)` 不属于链接。 */
function trimUrl(url: string): string {
  let end = url.length
  while (end > 0) {
    const char = url[end - 1] ?? ''
    if (/[.,:;!?'"*_~]/.test(char)) end -= 1
    else if (char === ')') {
      const body = url.slice(0, end)
      const opens = body.split('(').length - 1
      const closes = body.split(')').length - 1
      if (closes > opens) end -= 1
      else break
    } else break
  }
  return url.slice(0, end)
}

function pushText(tokens: Token[], text: string): void {
  if (text === '') return
  const last = tokens[tokens.length - 1]
  if (last?.t === 'node' && last.segment.type === 'text') last.segment.text += text
  else tokens.push({ t: 'node', segment: { type: 'text', text } })
}

function delimiter(text: string, start: number, char: string): { token: Token; end: number } {
  let end = start
  while (text[end] === char) end += 1
  const before = charBefore(text, start)
  const after = charAfter(text, end)
  const left = !isSpace(after) && (!isPunctuation(after) || isSpace(before) || isPunctuation(before))
  const right = !isSpace(before) && (!isPunctuation(before) || isSpace(after) || isPunctuation(after))
  const underscore = char === '_'
  const count = end - start
  return {
    token: {
      t: 'delim',
      char,
      count,
      original: count,
      canOpen: underscore ? left && (!right || isPunctuation(before)) : left,
      canClose: underscore ? right && (!left || isPunctuation(after)) : right,
    },
    end,
  }
}

function tokenize(text: string): Token[] {
  const tokens: Token[] = []
  let index = 0
  let plain = ''
  const flush = () => {
    pushText(tokens, plain)
    plain = ''
  }
  while (index < text.length) {
    const char = text[index] ?? ''
    if (char === '\\' && index + 1 < text.length && ESCAPABLE.test(text[index + 1] ?? '')) {
      plain += text[index + 1]
      index += 2
      continue
    }
    if (char === '`') {
      let run = 1
      while (text[index + run] === '`') run += 1
      const ticks = '`'.repeat(run)
      let close = text.indexOf(ticks, index + run)
      // 闭合反引号串必须与开头等长：跳过更长的串。
      while (close >= 0 && text[close + run] === '`') {
        let longer = run
        while (text[close + longer] === '`') longer += 1
        close = text.indexOf(ticks, close + longer)
      }
      if (close >= 0) {
        flush()
        let code = text.slice(index + run, close).replace(/\n/g, ' ')
        if (code.length > 2 && code.startsWith(' ') && code.endsWith(' ') && code.trim() !== '') code = code.slice(1, -1)
        tokens.push({ t: 'node', segment: { type: 'code', text: code } })
        index = close + run
        continue
      }
      plain += ticks
      index += run
      continue
    }
    if (char === '[') {
      const close = closingBracket(text, index)
      if (close > 0 && text[close + 1] === '(') {
        const target = linkTarget(text, close + 1)
        const url = target === null ? null : safeUrl(target.url)
        if (target !== null && url !== null) {
          flush()
          const label = text.slice(index + 1, close)
          tokens.push({ t: 'node', segment: { type: 'link', url, children: label === '' ? [{ type: 'text', text: url }] : parseInline(label) } })
          index = target.end
          continue
        }
      }
      plain += char
      index += 1
      continue
    }
    if ((char === 'h' || char === 'H') && !/[\p{L}\p{N}]/u.test(charBefore(text, index) ?? '')) {
      URL_START.lastIndex = index
      const match = URL_START.exec(text)
      if (match) {
        const url = trimUrl(match[0])
        if (safeUrl(url) !== null) {
          flush()
          tokens.push({ t: 'node', segment: { type: 'link', url, children: [{ type: 'text', text: url }] } })
          index += url.length
          continue
        }
      }
    }
    if (char === '*' || char === '_' || char === '~') {
      const run = delimiter(text, index, char)
      if (char === '~' && run.end - index < 2) {
        plain += text.slice(index, run.end)
      } else {
        flush()
        tokens.push(run.token)
      }
      index = run.end
      continue
    }
    plain += char
    index += 1
  }
  flush()
  return tokens
}

function toSegments(tokens: readonly Token[]): InlineSegment[] {
  const segments: InlineSegment[] = []
  const push = (segment: InlineSegment) => {
    const last = segments[segments.length - 1]
    if (segment.type === 'text' && last?.type === 'text') last.text += segment.text
    else segments.push(segment)
  }
  for (const token of tokens) {
    if (token.t === 'node') push(token.segment)
    else if (token.count > 0) push({ type: 'text', text: token.char.repeat(token.count) })
  }
  return segments
}

/** CommonMark「处理强调」：把配对的 `*` / `_` / `~~` 定界串收成节点，没配上的留作文字。 */
function resolveEmphasis(tokens: Token[]): Token[] {
  let closerIndex = 0
  while (closerIndex < tokens.length) {
    const closer = tokens[closerIndex]
    if (closer?.t !== 'delim' || !closer.canClose || closer.count === 0) {
      closerIndex += 1
      continue
    }
    let openerIndex = -1
    for (let index = closerIndex - 1; index >= 0; index -= 1) {
      const candidate = tokens[index]
      if (candidate?.t !== 'delim' || candidate.char !== closer.char || !candidate.canOpen || candidate.count === 0) continue
      if (closer.char === '~') {
        if (candidate.count < 2 || closer.count < 2) continue
      } else if ((candidate.canClose || closer.canOpen) && (candidate.original + closer.original) % 3 === 0 && !(candidate.original % 3 === 0 && closer.original % 3 === 0)) {
        continue
      }
      openerIndex = index
      break
    }
    const opener = openerIndex < 0 ? undefined : tokens[openerIndex]
    if (opener?.t !== 'delim') {
      closerIndex += 1
      continue
    }
    const use = closer.char === '~' || (opener.count >= 2 && closer.count >= 2) ? 2 : 1
    const type = closer.char === '~' ? 'strike' : use === 2 ? 'bold' : 'italic'
    const inner = toSegments(tokens.slice(openerIndex + 1, closerIndex))
    opener.count -= use
    closer.count -= use
    const node: Token = { t: 'node', segment: { type, children: inner } }
    tokens.splice(openerIndex + 1, closerIndex - openerIndex - 1, node)
    closerIndex = openerIndex + 2
  }
  return tokens
}

/** 行内语法切分；没有标记的文字、未配对的标记原样留在 `text` 段里。 */
export function parseInline(text: string): InlineSegment[] {
  return toSegments(resolveEmphasis(tokenize(text)))
}
