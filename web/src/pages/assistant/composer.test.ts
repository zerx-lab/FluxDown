import { describe, expect, test } from 'bun:test'
import type { AssistantConfigDto } from '../../lib/rpc/protocol'
import {
  ATTACHABLE_IMAGE_TYPES,
  EMPTY_COMPOSER,
  checkAttachable,
  imagesEnabled,
  insertPlaceholder,
  mergeStashedDraft,
  orphanedImages,
  pasteTakesImages,
  placeholderRemnant,
  prepareImage,
  reconcileDraft,
  removePlaceholder,
  settleSent,
  startEdit,
  stripPlaceholders,
} from './composer'
import type { Attachment, ImageCodec } from './composer'

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

const attachment = (key: number): Attachment => ({ key, dataUrl: `data:image/png;base64,${key}`, name: `${key}.png`, bytes: 1 })

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
    expect(checkAttachable(config(), 0, { type: 'application/pdf', size: 1 })).toBe('type')
    expect(checkAttachable(config(), 0, { type: 'video/mp4', size: 1 })).toBe('type')
    expect(checkAttachable(config(), 3, png)).toBe('tooMany')
    expect(checkAttachable(config(), 0, { type: 'image/png', size: 1_001 })).toBe('tooLarge')
    // 张数先于大小：已满时不再报大小。
    expect(checkAttachable(config(), 3, { type: 'image/png', size: 9_999 })).toBe('tooMany')
  })

  test('images the cloud does not take are left to the converter, not rejected up front', () => {
    for (const type of ['image/bmp', 'image/tiff', 'image/avif', 'image/svg+xml', '']) {
      expect(checkAttachable(config(), 0, { type, size: 50_000 })).toBeNull()
    }
  })

  test('a zero byte limit is not checked locally', () => {
    expect(checkAttachable(config({ maxImageBytes: 0 }), 0, { type: 'image/png', size: 50_000_000 })).toBeNull()
  })
})

describe('prepareImage', () => {
  const blob = (type: string, size: number): Blob => new Blob([new Uint8Array(size)], { type })
  const codec = (extra: Partial<ImageCodec> = {}): ImageCodec & { converted: number } => {
    const state = {
      converted: 0,
      async toPng(_file: Blob): Promise<Blob> {
        state.converted += 1
        return blob('image/png', 40)
      },
      async toDataUrl(file: Blob): Promise<string> {
        return `data:${file.type};base64,${file.size}`
      },
      ...extra,
    }
    return state
  }

  test('formats the cloud accepts are sent as they are', async () => {
    const fake = codec()
    expect(await prepareImage(config(), blob('image/jpeg', 100), fake)).toEqual({ ok: true, dataUrl: 'data:image/jpeg;base64,100', bytes: 100 })
    expect(fake.converted).toBe(0)
  })

  test('other formats are converted to PNG and measured after conversion', async () => {
    const fake = codec()
    expect(await prepareImage(config(), blob('image/bmp', 5_000), fake)).toEqual({ ok: true, dataUrl: 'data:image/png;base64,40', bytes: 40 })
    expect(fake.converted).toBe(1)
    // 转码后仍超限才算过大。
    const big = codec({ toPng: async () => blob('image/png', 2_000) })
    expect(await prepareImage(config(), blob('image/tiff', 10), big)).toEqual({ ok: false, error: 'tooLarge' })
  })

  test('an undecodable file is a type error, an unreadable one a read error', async () => {
    const broken = codec({
      toPng: async () => {
        throw new Error('decode')
      },
    })
    expect(await prepareImage(config(), blob('', 10), broken)).toEqual({ ok: false, error: 'type' })
    const unreadable = codec({
      toDataUrl: async () => {
        throw new Error('read')
      },
    })
    expect(await prepareImage(config(), blob('image/png', 10), unreadable)).toEqual({ ok: false, error: 'read' })
  })
})

describe('pasteTakesImages', () => {
  const shot = { name: 'image.png' }

  test('pure images are taken, plain text is left alone', () => {
    expect(pasteTakesImages([shot], '')).toBe(true)
    expect(pasteTakesImages([shot], '  \n ')).toBe(true)
    expect(pasteTakesImages([], 'hello')).toBe(false)
    expect(pasteTakesImages([], '')).toBe(false)
  })

  test('text wins over a screenshot that rides along (Word / Excel / web pages)', () => {
    expect(pasteTakesImages([shot], 'hello')).toBe(false)
    expect(pasteTakesImages([shot], 'a\nb')).toBe(false)
  })

  test('a file copied in the file manager is still taken even though its name rides along as text', () => {
    expect(pasteTakesImages([{ name: '1.png' }, { name: '2.jpg' }], '1.png\n2.jpg')).toBe(true)
    expect(pasteTakesImages([{ name: '1.png' }], '/Users/a/1.png')).toBe(true)
    expect(pasteTakesImages([{ name: '1.png' }], 'C:\\a\\1.png')).toBe(true)
    expect(pasteTakesImages([{ name: '1.png' }], '1.png and more')).toBe(false)
    expect(pasteTakesImages([{ name: '' }], 'x')).toBe(false)
  })
})

describe('insertPlaceholder', () => {
  test('writes the placeholder at the caret and moves the caret behind it', () => {
    expect(insertPlaceholder('see this', 4, 4, 1)).toEqual({ text: 'see [Image #1] this', caret: 15 })
    expect(insertPlaceholder('', 0, 0, 1)).toEqual({ text: '[Image #1] ', caret: 11 })
    expect(insertPlaceholder('abc', 3, 3, 2)).toEqual({ text: 'abc [Image #2] ', caret: 15 })
  })

  test('replaces the selection like a paste does', () => {
    expect(insertPlaceholder('see this one', 4, 8, 1)).toEqual({ text: 'see [Image #1] one', caret: 14 })
    expect(insertPlaceholder('abc', 3, 0, 1).text).toBe('[Image #1] ')
  })

  test('clamps a selection outside the text', () => {
    expect(insertPlaceholder('ab', 99, 99, 1)).toEqual({ text: 'ab [Image #1] ', caret: 14 })
    expect(insertPlaceholder('ab', -5, -5, 1)).toEqual({ text: '[Image #1] ab', caret: 11 })
  })

  test('keeps CJK and emoji around the caret intact', () => {
    // 光标下标是 UTF-16 码元：「界」1 个码元，🙂 2 个。
    expect(insertPlaceholder('界🙂x', 3, 3, 1).text).toBe('界🙂 [Image #1] x')
  })
})

describe('removePlaceholder', () => {
  test('removes the placeholder and renumbers the ones behind it', () => {
    const text = 'a [Image #1] b [Image #2] c [Image #3]'
    expect(removePlaceholder(text, text.length, 2, 3).text).toBe('a [Image #1] b c [Image #2]')
    expect(removePlaceholder(text, text.length, 1, 3).text).toBe('a b [Image #1] c [Image #2]')
    expect(removePlaceholder(text, text.length, 3, 3).text).toBe('a [Image #1] b [Image #2] c ')
  })

  test('removes every reference of the removed number', () => {
    expect(removePlaceholder('[Image #1] x [Image #1] [Image #2]', 0, 1, 2).text).toBe('x [Image #1]')
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
    // 光标在末尾：#1 连同后面的空格被删（-11）、#2 → #1（-0）。
    expect(removePlaceholder(text, text.length, 1, 2)).toEqual({ text: 'ab cd [Image #1] ef', caret: text.length - 11 })
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
    const first = insertPlaceholder('hello world', 5, 5, 1)
    const second = insertPlaceholder(first.text, first.caret, first.caret, 2)
    expect(second.text).toBe('hello [Image #1] [Image #2] world')
    const after = removePlaceholder(second.text, second.caret, 1, 2)
    expect(after.text).toBe('hello [Image #1] world')
  })
})

describe('orphanedImages', () => {
  test('images whose placeholder is gone are orphaned', () => {
    expect(orphanedImages('a [Image #1] b [Image #3]', 3)).toEqual([2])
    expect(orphanedImages('[Image #2] [Image #1]', 2)).toEqual([])
    // 残缺占位符与超出图片数的编号都不算。
    expect(orphanedImages('[Image #1 [Image #5]', 2)).toEqual([1, 2])
    expect(orphanedImages('', 0)).toEqual([])
    // 前导 0 不是占位符。
    expect(orphanedImages('[Image #01]', 1)).toEqual([1])
  })
})

describe('placeholderRemnant', () => {
  /** 从 `old` 删掉 `[start, end)` 后，补删占位符残余得到的文本。 */
  const deleteRange = (old: string, start: number, end: number, count: number): string => {
    let next = old.slice(0, start) + old.slice(end)
    const remnant = placeholderRemnant(old, next, count)
    if (remnant !== null) next = next.slice(0, remnant.start) + next.slice(remnant.end)
    return next
  }

  test('a partially deleted placeholder is removed whole', () => {
    // 退格删掉 `]`。
    expect(deleteRange('a [Image #1] b', 11, 12, 1)).toBe('a b')
    // 从中间删一个字符。
    expect(deleteRange('[Image #1] hi', 3, 4, 1)).toBe('hi')
    // 选区跨过占位符一部分和后面的文字。
    expect(deleteRange('x [Image #1] yz', 8, 14, 1)).toBe('x z')
    // 多字节文字紧贴占位符。
    expect(deleteRange('看[Image #1]图', 10, 11, 1)).toBe('看图')
  })

  test('deletions that do not cut a placeholder are left alone', () => {
    const old = 'a [Image #1] b'
    // 整个占位符被删：没有残余。
    expect(placeholderRemnant(old, 'a  b', 1)).toBeNull()
    // 删的是普通文字。
    expect(placeholderRemnant(old, 'a [Image #1] ', 1)).toBeNull()
    // 编号超出图片数：是用户自己的文字。
    expect(placeholderRemnant(old, 'a [Image #1 b', 0)).toBeNull()
    expect(placeholderRemnant('[Image #2]', '[Image #2', 1)).toBeNull()
    // 不是纯删除（替换）。
    expect(placeholderRemnant(old, 'a [Image #X] b', 1)).toBeNull()
    expect(placeholderRemnant(old, 'a [Image #1] bc', 1)).toBeNull()
  })

  test('never splits a surrogate pair', () => {
    // 🙂 与 🙃 共用高代理：公共前缀会停在代理对中间。
    expect(deleteRange('🙂[Image #1]', 0, 2, 1)).toBe('[Image #1]')
    expect(placeholderRemnant('🙂🙃[Image #1]', '🙂[Image #1]', 1)).toBeNull()
  })
})

describe('reconcileDraft', () => {
  const two = [attachment(1), attachment(2)]

  test('removing a placeholder by hand drops its image and renumbers the rest', () => {
    const previous = 'a [Image #1] b [Image #2]'
    const next = 'a  b [Image #2]'
    const result = reconcileDraft(previous, next, 2, two)
    expect(result.draft).toBe('a  b [Image #1]')
    expect(result.attachments.map((item) => item.key)).toEqual([2])
  })

  test('a half deleted placeholder goes away whole together with its image', () => {
    const previous = 'a [Image #1] b'
    const next = 'a [Image #1 b'
    const result = reconcileDraft(previous, next, 11, [attachment(1)])
    expect(result.draft).toBe('a b')
    expect(result.attachments).toEqual([])
    expect(result.caret).toBe(2)
  })

  test('typing around intact placeholders changes nothing', () => {
    const result = reconcileDraft('[Image #1]', '[Image #1]x', 11, [attachment(1)])
    expect(result.draft).toBe('[Image #1]x')
    expect(result.attachments).toHaveLength(1)
  })

  test('several placeholders vanishing at once are removed from the largest down', () => {
    const three = [attachment(1), attachment(2), attachment(3)]
    const result = reconcileDraft('[Image #1] [Image #2] [Image #3]', '[Image #2]', 10, three)
    expect(result.draft).toBe('[Image #1]')
    expect(result.attachments.map((item) => item.key)).toEqual([2])
  })

  test('without attachments the text is returned as typed', () => {
    expect(reconcileDraft('a', '', 0, [])).toEqual({ draft: '', caret: 0, attachments: [] })
  })
})

describe('placeholder edge cases shared with GPUI and the website', () => {
  test('removing swallows one following space only after a boundary', () => {
    expect(removePlaceholder('see[Image #1] now', 0, 1, 1).text).toBe('see now')
    expect(removePlaceholder('see [Image #1]', 0, 1, 1).text).toBe('see ')
    expect(removePlaceholder('[Image #1] [Image #2] what', 0, 1, 2).text).toBe('[Image #1] what')
  })

  test('a half deleted placeholder takes the following space with it', () => {
    const old = 'a [Image #1] b'
    const next = 'a [Image #1 b'
    expect(placeholderRemnant(old, next, 1)).toEqual({ start: 2, end: 12 })
    expect(reconcileDraft(old, next, 11, [attachment(1)]).draft).toBe('a b')
  })

  test('look-alikes with a leading zero or an out of range number are plain text', () => {
    const text = '[Image #9] [Image #01] [Image #] [Image #1 [Image #x] [Image #2]'
    expect(removePlaceholder(text, 0, 1, 2).text).toBe('[Image #9] [Image #01] [Image #] [Image #1 [Image #x] [Image #1]')
    expect(orphanedImages('[Image #0] [Image #01]', 1)).toEqual([1])
  })

  test('insertion pads each side only where needed', () => {
    expect(insertPlaceholder('see ', 4, 4, 2).text).toBe('see [Image #2] ')
    expect(insertPlaceholder('a  b', 2, 2, 1).text).toBe('a [Image #1] b')
    expect(insertPlaceholder('ab', 1, 1, 1).text).toBe('a [Image #1] b')
    expect(insertPlaceholder('看图', 1, 1, 1).text).toBe('看 [Image #1] 图')
  })
})

describe('settleSent', () => {
  const sent = { ...EMPTY_COMPOSER, draft: 'question', attachments: [attachment(1), attachment(2)] }

  test('an untouched composer is cleared after the send', () => {
    expect(settleSent(sent, { ...sent })).toEqual(EMPTY_COMPOSER)
  })

  test('text typed while the request was in flight is kept', () => {
    const typed = { ...sent, draft: 'question and more' }
    expect(settleSent(sent, typed)).toBe(typed)
  })

  test('images added or removed while the request was in flight keep the composer', () => {
    const more = { ...sent, attachments: [...sent.attachments, attachment(3)] }
    expect(settleSent(sent, more)).toBe(more)
    const swapped = { ...sent, attachments: [attachment(1), attachment(9)] }
    expect(settleSent(sent, swapped)).toBe(swapped)
  })

  test('a sent edit restores the draft from before the edit', () => {
    const before = { ...EMPTY_COMPOSER, draft: 'half written' }
    const editing = startEdit(before, { sessionId: 's', questionId: 'q1', text: 'old question', imageCount: 0 })
    expect(settleSent(editing, { ...editing })).toEqual(before)
  })

  test('new text typed during a sent edit stays and leaves the edit state', () => {
    const editing = startEdit(EMPTY_COMPOSER, { sessionId: 's', questionId: 'q1', text: 'old question', imageCount: 0 })
    const typed = { ...editing, draft: 'old question, edited again' }
    const result = settleSent(editing, typed)
    expect(result.draft).toBe('old question, edited again')
    expect(result.editing).toBeNull()
  })

  describe('a sent edit whose input changed meanwhile never loses the stashed draft', () => {
    const edit = (stash: { draft: string; attachments: Attachment[] }, imageCount = 0) =>
      startEdit({ ...EMPTY_COMPOSER, ...stash }, { sessionId: 's', questionId: 'q1', text: 'old question', imageCount })

    test('the new input stays and the stash follows after a blank line', () => {
      const sentEdit = edit({ draft: 'half written', attachments: [] })
      const result = settleSent(sentEdit, { ...sentEdit, draft: 'new words  ' })
      expect(result).toEqual({ draft: 'new words\n\nhalf written', attachments: [], editing: null })
    })

    test('an empty stash or an emptied input needs no separator', () => {
      const none = edit({ draft: '   ', attachments: [] })
      expect(settleSent(none, { ...none, draft: 'new words' }).draft).toBe('new words')
      const some = edit({ draft: 'half written', attachments: [] })
      expect(settleSent(some, { ...some, draft: '  ' }).draft).toBe('half written')
    })

    test('the stashed images come back when no image is left in the composer', () => {
      const stash = { draft: 'see [Image #1] please', attachments: [attachment(5)] }
      const sentEdit = edit(stash, 1)
      const result = settleSent(sentEdit, { ...sentEdit, draft: 'typed' })
      expect(result.draft).toBe('typed\n\nsee [Image #1] please')
      expect(result.attachments).toEqual(stash.attachments)
      expect(result.editing).toBeNull()
    })

    test('with images still in the composer the stashed ones are dropped together with their placeholders', () => {
      const stash = { draft: 'look [Image #1] here', attachments: [attachment(5)] }
      const sentEdit = { ...edit(stash), draft: 'old question', attachments: [attachment(9)] }
      const result = settleSent(sentEdit, { ...sentEdit, draft: 'typed' })
      expect(result.draft).toBe('typed\n\nlook here')
      expect(result.attachments).toEqual([attachment(9)])
      expect(result.editing).toBeNull()
    })

    test('an untouched composer still restores the stash whole, images included', () => {
      const stash = { draft: 'see [Image #1] please', attachments: [attachment(5)] }
      const sentEdit = edit(stash, 1)
      expect(settleSent(sentEdit, { ...sentEdit })).toEqual({ ...stash, editing: null })
    })

    test('an edit that already ended or was replaced is not settled by this send', () => {
      const sentEdit = edit({ draft: 'half written', attachments: [] })
      // 发送期间切走会话：编辑已结束、暂存草稿已还原，输入区原样保留。
      const ended = { ...EMPTY_COMPOSER, draft: 'half written' }
      expect(settleSent(sentEdit, ended)).toBe(ended)
      // 被另一条编辑取代：它自己的暂存草稿不能被这次发送吞掉。
      const other = startEdit(EMPTY_COMPOSER, { sessionId: 's', questionId: 'q2', text: 'another', imageCount: 0 })
      expect(settleSent(sentEdit, other)).toBe(other)
      // 普通提问发出期间开始了编辑：同样不动。
      const plain = { ...EMPTY_COMPOSER, draft: 'question' }
      expect(settleSent(plain, other)).toBe(other)
    })
  })

  describe('merge helpers', () => {
    test('mergeStashedDraft joins by a blank line and drops placeholders it cannot restore', () => {
      expect(mergeStashedDraft('a', 'b', 0, true)).toBe('a\n\nb')
      expect(mergeStashedDraft('a  \n', 'b', 0, true)).toBe('a\n\nb')
      expect(mergeStashedDraft('a', ' ', 0, true)).toBe('a')
      expect(mergeStashedDraft(' ', 'b', 0, true)).toBe('b')
      expect(mergeStashedDraft('a', 'x [Image #1] y', 1, false)).toBe('a\n\nx y')
      expect(mergeStashedDraft('a', '[Image #1]', 1, false)).toBe('a')
    })

    test('stripPlaceholders removes numbers 1..=count and leaves the rest as text', () => {
      expect(stripPlaceholders('a [Image #1] b [Image #2] c', 2)).toBe('a b c')
      expect(stripPlaceholders('[Image #1] [Image #9]', 1)).toBe('[Image #9]')
      expect(stripPlaceholders('plain', 3)).toBe('plain')
    })
  })
})
