import { describe, expect, test } from 'bun:test'
import { markdownBlocks, parseInline } from './markdown'

describe('markdownBlocks', () => {
  test('headings, lists and paragraphs come from the shared release-note splitter', () => {
    expect(markdownBlocks('## Title\n\n- a\n- b\n\nplain\ntext')).toEqual([
      { kind: 'heading', text: 'Title' },
      { kind: 'list', items: [{ text: 'a', depth: 0 }, { text: 'b', depth: 0 }] },
      { kind: 'paragraph', lines: ['plain', 'text'] },
    ])
  })

  test('fenced code is kept verbatim and never parsed as markdown', () => {
    expect(markdownBlocks('before\n```ts\n# not a heading\n- not a list\n```\nafter')).toEqual([
      { kind: 'paragraph', lines: ['before'] },
      { kind: 'code', text: '# not a heading\n- not a list', lang: 'ts' },
      { kind: 'paragraph', lines: ['after'] },
    ])
  })

  test('an unterminated fence swallows the rest as code', () => {
    expect(markdownBlocks('```\nx')).toEqual([{ kind: 'code', text: 'x', lang: '' }])
  })

  test('empty input has no blocks', () => {
    expect(markdownBlocks('')).toEqual([])
  })

  test('windows line endings and CJK text', () => {
    expect(markdownBlocks('第一行\r\n第二行\r\n\r\n- 项目')).toEqual([
      { kind: 'paragraph', lines: ['第一行', '第二行'] },
      { kind: 'list', items: [{ text: '项目', depth: 0 }] },
    ])
  })
})

describe('parseInline', () => {
  test('code, bold and links', () => {
    expect(parseInline('use `aria2c` and **fast** [docs](https://example.com/a?b=1)')).toEqual([
      { type: 'text', text: 'use ' },
      { type: 'code', text: 'aria2c' },
      { type: 'text', text: ' and ' },
      { type: 'bold', text: 'fast' },
      { type: 'text', text: ' ' },
      { type: 'link', text: 'docs', url: 'https://example.com/a?b=1' },
    ])
  })

  test('double underscores are bold too', () => {
    expect(parseInline('__x__')).toEqual([{ type: 'bold', text: 'x' }])
  })

  test('non-http links stay literal text', () => {
    expect(parseInline('[x](javascript:alert(1))')).toEqual([{ type: 'text', text: '[x](javascript:alert(1))' }])
    expect(parseInline('[x](ftp://a/b)')).toEqual([{ type: 'text', text: '[x](ftp://a/b)' }])
  })

  test('unpaired markers and stray asterisks stay literal', () => {
    expect(parseInline('2 * 3 * 4 and `open')).toEqual([{ type: 'text', text: '2 * 3 * 4 and `open' }])
    expect(parseInline('**open')).toEqual([{ type: 'text', text: '**open' }])
  })

  test('code spans protect markers inside them', () => {
    expect(parseInline('`**x**`')).toEqual([{ type: 'code', text: '**x**' }])
  })

  test('plain text, empty text and long urls', () => {
    expect(parseInline('')).toEqual([])
    expect(parseInline('hello 世界')).toEqual([{ type: 'text', text: 'hello 世界' }])
    const url = `https://example.com/${'a'.repeat(500)}?q=${'b'.repeat(500)}`
    expect(parseInline(`[l](${url})`)).toEqual([{ type: 'link', text: 'l', url }])
  })
})
