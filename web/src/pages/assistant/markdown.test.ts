import { describe, expect, test } from 'bun:test'
import { markdownBlocks, parseInline, safeUrl } from './markdown'
import type { InlineSegment, MarkdownBlock } from './markdown'

const text = (value: string): InlineSegment => ({ type: 'text', text: value })
const item = (...blocks: MarkdownBlock[]) => ({ checked: null, blocks })
const para = (...lines: string[]): MarkdownBlock => ({ kind: 'paragraph', lines })

describe('markdownBlocks', () => {
  test('headings, lists and paragraphs', () => {
    expect(markdownBlocks('## Title\n\n- a\n- b\n\nplain\ntext')).toEqual([
      { kind: 'heading', level: 2, text: 'Title' },
      { kind: 'list', ordered: false, start: 0, items: [item(para('a')), item(para('b'))] },
      para('plain', 'text'),
    ])
  })

  test('heading levels, closing hashes and hash-tags that are not headings', () => {
    expect(markdownBlocks('# One\n###### Six ##\n#nospace\n####### seven')).toEqual([
      { kind: 'heading', level: 1, text: 'One' },
      { kind: 'heading', level: 6, text: 'Six' },
      para('#nospace', '####### seven'),
    ])
  })

  test('fenced code is kept verbatim and never parsed as markdown', () => {
    expect(markdownBlocks('before\n```ts\n# not a heading\n- not a list\n```\nafter')).toEqual([
      para('before'),
      { kind: 'code', text: '# not a heading\n- not a list', lang: 'ts' },
      para('after'),
    ])
  })

  test('tilde fences, longer closing fences and an unterminated fence', () => {
    expect(markdownBlocks('~~~\na\n~~~')).toEqual([{ kind: 'code', text: 'a', lang: '' }])
    expect(markdownBlocks('````md\n```\ninner\n```\n````')).toEqual([{ kind: 'code', text: '```\ninner\n```', lang: 'md' }])
    expect(markdownBlocks('```\nx')).toEqual([{ kind: 'code', text: 'x', lang: '' }])
  })

  test('empty input has no blocks', () => {
    expect(markdownBlocks('')).toEqual([])
    expect(markdownBlocks('\n\n  \n')).toEqual([])
  })

  test('windows line endings and CJK text', () => {
    expect(markdownBlocks('第一行\r\n第二行\r\n\r\n- 项目')).toEqual([
      para('第一行', '第二行'),
      { kind: 'list', ordered: false, start: 0, items: [item(para('项目'))] },
    ])
  })

  test('ordered lists keep their start number and delimiter style', () => {
    expect(markdownBlocks('1. one\n2. two\n3. three')).toEqual([
      { kind: 'list', ordered: true, start: 1, items: [item(para('one')), item(para('two')), item(para('three'))] },
    ])
    expect(markdownBlocks('3) c\n4) d')).toEqual([{ kind: 'list', ordered: true, start: 3, items: [item(para('c')), item(para('d'))] }])
  })

  test('a list can interrupt a paragraph only when it starts at 1 (or is a bullet)', () => {
    expect(markdownBlocks('steps:\n1. open\n2. close')).toEqual([
      para('steps:'),
      { kind: 'list', ordered: true, start: 1, items: [item(para('open')), item(para('close'))] },
    ])
    expect(markdownBlocks('released in\n2024. it was good')).toEqual([para('released in', '2024. it was good')])
    expect(markdownBlocks('intro\n- a')).toEqual([para('intro'), { kind: 'list', ordered: false, start: 0, items: [item(para('a'))] }])
  })

  test('no lazy continuation: unindented lines after a list item start new blocks', () => {
    expect(markdownBlocks('- a\n总之这样')).toEqual([{ kind: 'list', ordered: false, start: 0, items: [item(para('a'))] }, para('总之这样')])
    expect(markdownBlocks('- a\n1. 步骤')).toEqual([
      { kind: 'list', ordered: false, start: 0, items: [item(para('a'))] },
      { kind: 'list', ordered: true, start: 1, items: [item(para('步骤'))] },
    ])
  })

  test('indented continuation lines belong to the item', () => {
    expect(markdownBlocks('- a\n  more\n- b')).toEqual([{ kind: 'list', ordered: false, start: 0, items: [item(para('a', 'more')), item(para('b'))] }])
  })

  test('nested lists, including 2-space nesting under an ordered item', () => {
    expect(markdownBlocks('- a\n  - b\n    - c\n- d')).toEqual([
      {
        kind: 'list',
        ordered: false,
        start: 0,
        items: [
          item(para('a'), { kind: 'list', ordered: false, start: 0, items: [item(para('b'), { kind: 'list', ordered: false, start: 0, items: [item(para('c'))] })] }),
          item(para('d')),
        ],
      },
    ])
    expect(markdownBlocks('1. a\n  - b\n2. c')).toEqual([
      {
        kind: 'list',
        ordered: true,
        start: 1,
        items: [item(para('a'), { kind: 'list', ordered: false, start: 0, items: [item(para('b'))] }), item(para('c'))],
      },
    ])
  })

  test('blank lines between items keep one list; a different bullet starts a new list', () => {
    expect(markdownBlocks('- a\n\n- b')).toEqual([{ kind: 'list', ordered: false, start: 0, items: [item(para('a')), item(para('b'))] }])
    expect(markdownBlocks('- a\n* b')).toEqual([
      { kind: 'list', ordered: false, start: 0, items: [item(para('a'))] },
      { kind: 'list', ordered: false, start: 0, items: [item(para('b'))] },
    ])
  })

  test('code fences inside list items', () => {
    expect(markdownBlocks('1. run:\n   ```sh\n   fd --help\n   ```\n2. done')).toEqual([
      {
        kind: 'list',
        ordered: true,
        start: 1,
        items: [item(para('run:'), { kind: 'code', text: 'fd --help', lang: 'sh' }), item(para('done'))],
      },
    ])
  })

  test('task list items', () => {
    expect(markdownBlocks('- [ ] todo\n- [x] done')).toEqual([
      {
        kind: 'list',
        ordered: false,
        start: 0,
        items: [
          { checked: false, blocks: [para('todo')] },
          { checked: true, blocks: [para('done')] },
        ],
      },
    ])
  })

  test('block quotes, nested content, and no lazy continuation', () => {
    expect(markdownBlocks('> quoted\n> - item\nafter')).toEqual([
      { kind: 'quote', blocks: [para('quoted'), { kind: 'list', ordered: false, start: 0, items: [item(para('item'))] }] },
      para('after'),
    ])
  })

  test('thematic breaks are not list items', () => {
    expect(markdownBlocks('a\n\n---\n\n***\n\n- - -')).toEqual([para('a'), { kind: 'rule' }, { kind: 'rule' }, { kind: 'rule' }])
  })

  test('GFM tables with alignment, escaped pipes and short rows', () => {
    expect(markdownBlocks('| 名称 | 值 | 备注 |\n|:--|--:|:-:|\n| a | 1 | x \\| y |\n| b | 2 |')).toEqual([
      {
        kind: 'table',
        header: ['名称', '值', '备注'],
        align: ['left', 'right', 'center'],
        rows: [
          ['a', '1', 'x | y'],
          ['b', '2', ''],
        ],
      },
    ])
  })

  test('a table can follow a paragraph line without a blank line, and ends at a blank line', () => {
    expect(markdownBlocks('result:\na | b\n- | -\n1 | 2\n\nafter')).toEqual([
      para('result:'),
      { kind: 'table', header: ['a', 'b'], align: [null, null], rows: [['1', '2']] },
      para('after'),
    ])
  })

  test('a pipe line without a matching delimiter row is a paragraph', () => {
    expect(markdownBlocks('a | b\n--- ')).toEqual([para('a | b'), { kind: 'rule' }])
    expect(markdownBlocks('a | b | c\n|---|---|')).toEqual([para('a | b | c', '|---|---|')])
  })
})

describe('parseInline', () => {
  test('code, bold and links', () => {
    expect(parseInline('use `aria2c` and **fast** [docs](https://example.com/a?b=1)')).toEqual([
      text('use '),
      { type: 'code', text: 'aria2c' },
      text(' and '),
      { type: 'bold', children: [text('fast')] },
      text(' '),
      { type: 'link', url: 'https://example.com/a?b=1', children: [text('docs')] },
    ])
  })

  test('double underscores are bold, single markers are italic, tildes are strikethrough', () => {
    expect(parseInline('__x__')).toEqual([{ type: 'bold', children: [text('x')] }])
    expect(parseInline('*x* and _y_')).toEqual([{ type: 'italic', children: [text('x')] }, text(' and '), { type: 'italic', children: [text('y')] }])
    expect(parseInline('~~gone~~')).toEqual([{ type: 'strike', children: [text('gone')] }])
  })

  test('emphasis nests and combines', () => {
    expect(parseInline('**a *b* c**')).toEqual([{ type: 'bold', children: [text('a '), { type: 'italic', children: [text('b')] }, text(' c')] }])
    expect(parseInline('***both***')).toEqual([{ type: 'italic', children: [{ type: 'bold', children: [text('both')] }] }])
    expect(parseInline('**`code` in bold**')).toEqual([{ type: 'bold', children: [{ type: 'code', text: 'code' }, text(' in bold')] }])
  })

  test('emphasis works next to CJK text without spaces', () => {
    expect(parseInline('这是**粗体**文字')).toEqual([text('这是'), { type: 'bold', children: [text('粗体')] }, text('文字')])
  })

  test('intraword underscores and spaced asterisks stay literal', () => {
    expect(parseInline('snake_case_name')).toEqual([text('snake_case_name')])
    expect(parseInline('2 * 3 * 4')).toEqual([text('2 * 3 * 4')])
  })

  test('non-http links stay literal text', () => {
    expect(parseInline('[x](javascript:alert(1))')).toEqual([text('[x](javascript:alert(1))')])
    expect(parseInline('[x](ftp://a/b)')).toEqual([text('[x](ftp://a/b)')])
    expect(parseInline('[x](data:text/html,hi)')).toEqual([text('[x](data:text/html,hi)')])
  })

  test('links with userinfo, empty hosts or over-long targets stay literal text', () => {
    expect(safeUrl('https://docs.example.com@evil.test/')).toBeNull()
    expect(safeUrl('https://user:pw@e.com')).toBeNull()
    expect(safeUrl('https:///path')).toBeNull()
    expect(safeUrl('https://e.com/?next=a@b')).toBe('https://e.com/?next=a@b')
    expect(safeUrl(`https://e.com/${'a'.repeat(2048)}`)).toBeNull()
    expect(safeUrl(`https://e.com/${'a'.repeat(2048 - 'https://e.com/'.length)}`)).not.toBeNull()
    expect(parseInline('[x](https://docs.example.com@evil.test/)')).toEqual([text('[x](https://docs.example.com@evil.test/)')])
    expect(parseInline('go https://a@b.com now')).toEqual([text('go https://a@b.com now')])
  })

  test('images degrade to a plain link and never load remotely', () => {
    expect(parseInline('![alt](https://e.com/a.png)')).toEqual([text('!'), { type: 'link', url: 'https://e.com/a.png', children: [text('alt')] }])
  })

  test('link titles, angle-bracket targets and nested parentheses', () => {
    expect(parseInline('[a](https://e.com "title")')).toEqual([{ type: 'link', url: 'https://e.com', children: [text('a')] }])
    expect(parseInline('[a](<https://e.com/x>)')).toEqual([{ type: 'link', url: 'https://e.com/x', children: [text('a')] }])
    expect(parseInline('[w](https://e.com/a_(b))')).toEqual([{ type: 'link', url: 'https://e.com/a_(b)', children: [text('w')] }])
  })

  test('bare URLs become links without trailing punctuation', () => {
    expect(parseInline('see https://example.com/a. Next')).toEqual([
      text('see '),
      { type: 'link', url: 'https://example.com/a', children: [text('https://example.com/a')] },
      text('. Next'),
    ])
    expect(parseInline('(https://e.com/x)')).toEqual([text('('), { type: 'link', url: 'https://e.com/x', children: [text('https://e.com/x')] }, text(')')])
    expect(parseInline('nothttps://e.com')).toEqual([text('nothttps://e.com')])
  })

  test('backslash escapes produce literal punctuation', () => {
    expect(parseInline('\\*not italic\\* and \\`tick')).toEqual([text('*not italic* and `tick')])
  })

  test('unpaired markers stay literal', () => {
    expect(parseInline('**open')).toEqual([text('**open')])
    expect(parseInline('2 * 3 * 4 and `open')).toEqual([text('2 * 3 * 4 and `open')])
    expect(parseInline('~single~ tilde')).toEqual([text('~single~ tilde')])
  })

  test('code spans protect markers inside them and honour backtick run length', () => {
    expect(parseInline('`**x**`')).toEqual([{ type: 'code', text: '**x**' }])
    expect(parseInline('`` a`b ``')).toEqual([{ type: 'code', text: 'a`b' }])
  })

  test('plain text, empty text and long urls', () => {
    expect(parseInline('')).toEqual([])
    expect(parseInline('hello 世界')).toEqual([text('hello 世界')])
    const url = `https://example.com/${'a'.repeat(500)}?q=${'b'.repeat(500)}`
    expect(parseInline(`[l](${url})`)).toEqual([{ type: 'link', url, children: [text('l')] }])
  })

  test('raw HTML is just text', () => {
    expect(parseInline('<script>alert(1)</script>')).toEqual([text('<script>alert(1)</script>')])
  })
})
