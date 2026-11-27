import { describe, expect, test } from 'bun:test'
import type { AssistantConfigDto } from '../../lib/rpc/protocol'
import { ATTACHABLE_IMAGE_TYPES, checkAttachable, imagesEnabled, insertPlaceholder, isReferenced, removePlaceholder } from './composer'

const config = (extra: Partial<AssistantConfigDto> = {}): AssistantConfigDto => ({
  available: true,
  maxMessageChars: 4000,
  imagesSupported: true,
  maxImages: 3,
  maxImageBytes: 1_000,
  dailyLimit: 0,
  usedToday: 0,
  toolsEnabled: true,
  ...extra,
})

describe('imagesEnabled', () => {
  test('needs a declared capability and room for at least one image', () => {
    expect(imagesEnabled(undefined)).toBe(false)
    expect(imagesEnabled(config())).toBe(true)
    expect(imagesEnabled(config({ imagesSupported: false }))).toBe(false)
    expect(imagesEnabled(config({ maxImages: 0 }))).toBe(false)
  })
})

describe('checkAttachable', () => {
  const png = { type: 'image/png', size: 500 }

  test('accepts an image within every limit', () => {
    expect(checkAttachable(config(), 0, png)).toBeNull()
    expect(checkAttachable(config(), 2, png)).toBeNull()
    for (const type of ATTACHABLE_IMAGE_TYPES) expect(checkAttachable(config(), 0, { type, size: 1 })).toBeNull()
    expect(checkAttachable(config(), 0, { type: 'IMAGE/PNG', size: 1 })).toBeNull()
  })

  test('rejects by capability, type, count and size in that order', () => {
    expect(checkAttachable(undefined, 0, png)).toBe('unsupported')
    expect(checkAttachable(config({ imagesSupported: false }), 0, png)).toBe('unsupported')
    expect(checkAttachable(config(), 0, { type: 'image/svg+xml', size: 1 })).toBe('type')
    expect(checkAttachable(config(), 0, { type: '', size: 1 })).toBe('type')
    expect(checkAttachable(config(), 3, png)).toBe('tooMany')
    expect(checkAttachable(config(), 0, { type: 'image/png', size: 1_001 })).toBe('tooLarge')
    // 张数先于大小：已满时不再报大小。
    expect(checkAttachable(config(), 3, { type: 'image/png', size: 9_999 })).toBe('tooMany')
  })

  test('a zero byte limit is not checked locally', () => {
    expect(checkAttachable(config({ maxImageBytes: 0 }), 0, { type: 'image/png', size: 50_000_000 })).toBeNull()
  })
})

describe('insertPlaceholder', () => {
  test('writes the placeholder at the caret and moves the caret behind it', () => {
    expect(insertPlaceholder('see this', 4, 1)).toEqual({ text: 'see [Image #1]this', caret: 14 })
    expect(insertPlaceholder('', 0, 1)).toEqual({ text: '[Image #1]', caret: 10 })
    expect(insertPlaceholder('abc', 3, 2)).toEqual({ text: 'abc[Image #2]', caret: 13 })
  })

  test('clamps a caret outside the text', () => {
    expect(insertPlaceholder('ab', 99, 1)).toEqual({ text: 'ab[Image #1]', caret: 12 })
    expect(insertPlaceholder('ab', -5, 1)).toEqual({ text: '[Image #1]ab', caret: 10 })
  })

  test('keeps CJK and emoji around the caret intact', () => {
    // 光标下标是 UTF-16 码元：「界」1 个码元，🙂 2 个。
    expect(insertPlaceholder('界🙂x', 3, 1).text).toBe('界🙂[Image #1]x')
  })
})

describe('removePlaceholder', () => {
  test('removes the placeholder and renumbers the ones behind it', () => {
    const text = 'a [Image #1] b [Image #2] c [Image #3]'
    expect(removePlaceholder(text, text.length, 2, 3).text).toBe('a [Image #1] b  c [Image #2]')
    expect(removePlaceholder(text, text.length, 1, 3).text).toBe('a  b [Image #1] c [Image #2]')
    expect(removePlaceholder(text, text.length, 3, 3).text).toBe('a [Image #1] b [Image #2] c ')
  })

  test('removes every reference of the removed number', () => {
    expect(removePlaceholder('[Image #1] x [Image #1] [Image #2]', 0, 1, 2).text).toBe(' x  [Image #1]')
  })

  test('leaves numbers outside the attachment range alone', () => {
    // 5 超出附件数（4）：用户手写的无效引用不动；0 同理。
    expect(removePlaceholder('[Image #2] [Image #5] [Image #0] [Image #3]', 0, 1, 4).text).toBe(
      '[Image #1] [Image #5] [Image #0] [Image #2]',
    )
  })

  test('renumbering crosses digit counts', () => {
    expect(removePlaceholder('[Image #10] [Image #11]', 0, 9, 11).text).toBe('[Image #9] [Image #10]')
  })

  test('ignores look-alikes that are not placeholders', () => {
    const text = '[Image 1] [image #1] [Image #x] [Image #1'
    expect(removePlaceholder(text, 0, 1, 1).text).toBe(text)
  })

  test('the caret follows the text', () => {
    const text = 'ab [Image #1] cd [Image #2] ef'
    // 光标在末尾：#1 被删（-10）、#2 → #1（-0）。
    expect(removePlaceholder(text, text.length, 1, 2)).toEqual({ text: 'ab  cd [Image #1] ef', caret: text.length - 10 })
    // 光标在被删占位符之前：不动。
    expect(removePlaceholder(text, 1, 1, 2).caret).toBe(1)
    // 光标落在被删占位符中间：回到它的起点。
    expect(removePlaceholder(text, 8, 1, 2).caret).toBe(3)
  })

  test('caret shrinks with a renumbered placeholder before it', () => {
    const text = '[Image #10] [Image #11] tail'
    const result = removePlaceholder(text, text.length, 9, 11)
    expect(result.text).toBe('[Image #9] [Image #10] tail')
    expect(result.caret).toBe(result.text.length)
  })

  test('insert and remove round trip', () => {
    const first = insertPlaceholder('hello world', 5, 1)
    const second = insertPlaceholder(first.text, first.caret, 2)
    expect(second.text).toBe('hello[Image #1][Image #2] world')
    const after = removePlaceholder(second.text, second.caret, 1, 2)
    expect(after.text).toBe('hello[Image #1] world')
  })
})

describe('isReferenced', () => {
  test('tells whether the text still mentions the image', () => {
    expect(isReferenced('look [Image #2]', 2)).toBe(true)
    expect(isReferenced('look [Image #2]', 1)).toBe(false)
    expect(isReferenced('', 1)).toBe(false)
  })
})
