// 输入区的纯逻辑：图片附件的校验 / 转码，以及提问文本里 `[Image #N]` 占位符的写入 / 删除 / 重新编号 / 自动清理。
// 占位符约定与 FluxCloud 解析一致（`assistantImagePlaceholder`）：第 N 张附图（从 1 起）在文本里写成
// `[Image #N]`，云端把图片紧跟在占位符之后发给模型；占位符与剪贴板判定是 GPUI `crates/assistant`
// （`model.rs` / `attachment.rs`）的移植。这里不碰 React；浏览器 API（FileReader / canvas）只在默认编解码器里用。

import { assistantImagePlaceholder } from '../../lib/rpc/protocol'
import type { AssistantConfigDto } from '../../lib/rpc/protocol'

/** 云端接受的图片类型（`AssistantAskParams.images` 的 `data:image/<type>;base64,…`）；其他图片先转 PNG。 */
export const ATTACHABLE_IMAGE_TYPES: readonly string[] = ['image/png', 'image/jpeg', 'image/gif', 'image/webp']

/** 一张待发送的附图（编号 = 它在附件列表里的位置 + 1）。 */
export interface Attachment {
  /** 稳定的界面键（删除后其余附件的键不变）。 */
  key: number
  /** `data:image/...;base64,...`。 */
  dataUrl: string
  /** 原文件名（缩略图的无障碍标签；剪贴板图片可能为空）。 */
  name: string
  /** 实际发送的字节数（转码后的大小）。 */
  bytes: number
}

/** 编辑后重发：被编辑的用户消息；`previous` 是进入编辑前输入区的内容（取消 / 发送后恢复）。 */
export interface EditTarget {
  sessionId: string
  /** 被替换的用户消息 ID（`AssistantAskParams.replaceFrom`）。 */
  questionId: string
  /** 原提问有附图：沿用（此时不能再增删图片，文本里的 `[Image #N]` 照旧指向原附图）。 */
  keepImages: boolean
  imageCount: number
  previous: { draft: string; attachments: Attachment[] }
}

/** 输入草稿与待发送附图（提升到页面层：切换会话 / 移动端换栏时不丢）。 */
export interface ComposerState {
  draft: string
  attachments: Attachment[]
  /** 非 null = 正在编辑一条历史提问，发送即重发。 */
  editing: EditTarget | null
}

export const EMPTY_COMPOSER: ComposerState = { draft: '', attachments: [], editing: null }

/** 进入编辑：输入框换成原提问（保留进入前的草稿以便取消）。 */
export function startEdit(
  state: ComposerState,
  target: { sessionId: string; questionId: string; text: string; imageCount: number },
): ComposerState {
  const previous = state.editing?.previous ?? { draft: state.draft, attachments: state.attachments }
  return {
    draft: target.text,
    attachments: [],
    editing: {
      sessionId: target.sessionId,
      questionId: target.questionId,
      keepImages: target.imageCount > 0,
      imageCount: target.imageCount,
      previous,
    },
  }
}

/** 退出编辑（取消、发送成功、会话已切走）：恢复进入前的草稿与附图；没有在编辑时原样返回。 */
export function endEdit(state: ComposerState): ComposerState {
  return state.editing === null ? state : { ...state.editing.previous, editing: null }
}

/** 附图能力：云端配置声明支持且允许至少一张。不满足时输入区不提供任何图片入口。 */
export function imagesEnabled(config: AssistantConfigDto | undefined): boolean {
  return config !== undefined && config.imagesSupported && config.maxImages > 0
}

/** 附图失败的原因（对应提示文案）。 */
export type AttachError = 'unsupported' | 'type' | 'tooMany' | 'tooLarge'

/** 云端能直接接受的格式（其余图片转 PNG 再发）。 */
export function isDirectImageType(type: string): boolean {
  return ATTACHABLE_IMAGE_TYPES.includes(type.toLowerCase())
}

/**
 * 解码前的预检：能力、类型、张数（`maxImages`，`count` 含已附加与读取中的）与云端直收格式的字节数
 * （`maxImageBytes`，`0` 不在本地限制，交给云端校验）。类型只拒绝明确不是图片的（pdf、视频……）；
 * 云端不收的图片格式（bmp / tiff……）与空类型交给 `prepareImage` 解码转 PNG，解码失败才算类型错误。
 * 通过返回 null。
 */
export function checkAttachable(
  config: AssistantConfigDto | undefined,
  count: number,
  file: { type: string; size: number },
): AttachError | null {
  if (config === undefined || !imagesEnabled(config)) return 'unsupported'
  const type = file.type.toLowerCase()
  if (type !== '' && !type.startsWith('image/')) return 'type'
  if (count >= config.maxImages) return 'tooMany'
  if (isDirectImageType(type) && config.maxImageBytes > 0 && file.size > config.maxImageBytes) return 'tooLarge'
  return null
}

/** 图片的编解码（默认用浏览器 API；测试注入替身）。 */
export interface ImageCodec {
  /** 解码任意浏览器能显示的图片并编码为 PNG；无法解码时拒绝。 */
  toPng(file: Blob): Promise<Blob>
  toDataUrl(blob: Blob): Promise<string>
}

export type PreparedImage = { ok: true; dataUrl: string; bytes: number } | { ok: false; error: AttachError | 'read' }

/**
 * 把一张图处理成可发送的附件：云端接受的格式原样使用；其他格式解码后转 PNG，并按转码后的字节数检查
 * `maxImageBytes`。解码失败 → `type`，读取失败 → `read`。调用前应已通过 `checkAttachable`。
 */
export async function prepareImage(config: AssistantConfigDto, file: Blob, codec: ImageCodec): Promise<PreparedImage> {
  let blob = file
  if (!isDirectImageType(file.type)) {
    try {
      blob = await codec.toPng(file)
    } catch {
      return { ok: false, error: 'type' }
    }
    if (config.maxImageBytes > 0 && blob.size > config.maxImageBytes) return { ok: false, error: 'tooLarge' }
  }
  try {
    return { ok: true, dataUrl: await codec.toDataUrl(blob), bytes: blob.size }
  } catch {
    return { ok: false, error: 'read' }
  }
}

/** `File` → `data:` URL（`FileReader`；读取失败时拒绝）。 */
export function readAsDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => (typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('unreadable image')))
    reader.onerror = () => reject(reader.error ?? new Error('unreadable image'))
    reader.readAsDataURL(file)
  })
}

/** 解码成可画到 canvas 的位图：优先 `createImageBitmap`，旧浏览器退回 `<img>`。 */
async function decodeImage(file: Blob): Promise<{ source: CanvasImageSource; width: number; height: number; release: () => void }> {
  if (typeof createImageBitmap === 'function') {
    const bitmap = await createImageBitmap(file)
    return { source: bitmap, width: bitmap.width, height: bitmap.height, release: () => bitmap.close() }
  }
  const url = URL.createObjectURL(file)
  try {
    const image = new Image()
    image.src = url
    await image.decode()
    return { source: image, width: image.naturalWidth, height: image.naturalHeight, release: () => URL.revokeObjectURL(url) }
  } catch (error) {
    URL.revokeObjectURL(url)
    throw error
  }
}

/** 浏览器解码 + canvas 编码为 PNG（bmp / tiff / avif 等云端不收的格式）。 */
async function convertToPng(file: Blob): Promise<Blob> {
  const decoded = await decodeImage(file)
  try {
    if (decoded.width === 0 || decoded.height === 0) throw new Error('empty image')
    const canvas = document.createElement('canvas')
    canvas.width = decoded.width
    canvas.height = decoded.height
    const context = canvas.getContext('2d')
    if (context === null) throw new Error('canvas unavailable')
    context.drawImage(decoded.source, 0, 0)
    return await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob((blob) => (blob === null ? reject(new Error('png encode failed')) : resolve(blob)), 'image/png'),
    )
  } finally {
    decoded.release()
  }
}

export const BROWSER_IMAGE_CODEC: ImageCodec = { toPng: convertToPng, toDataUrl: readAsDataUrl }

// ───────────────────────── 剪贴板 ─────────────────────────

/**
 * 粘贴是否由图片附件接管（GPUI `clipboard_text_wins_over_an_attached_image`）：剪贴板里没有图片文件 → 否；
 * 没有文字 → 是；有文字 → 文字优先（Word / Excel / 网页复制时常带一张截图），除非文字只是这些图片的文件名
 * （文件管理器复制文件时会同时带上文件名），此时仍由附件接管。
 */
export function pasteTakesImages(images: readonly { name: string }[], text: string): boolean {
  if (images.length === 0) return false
  const lines = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== '')
  if (lines.length === 0) return true
  return lines.every((line) =>
    images.some((image) => image.name !== '' && (line === image.name || line.endsWith(`/${image.name}`) || line.endsWith(`\\${image.name}`))),
  )
}

// ───────────────────────── 占位符 ─────────────────────────

export interface TextEdit {
  text: string
  /** 编辑后的光标位置（UTF-16 下标，与 `textarea.selectionStart` 同口径）。 */
  caret: number
}

function clampCaret(text: string, caret: number): number {
  return Math.min(text.length, Math.max(0, caret))
}

/** 不超过 `index` 的最大字符边界（不把代理对切成两半）。 */
function floorBoundary(text: string, index: number): number {
  const at = clampCaret(text, index)
  const low = text.charCodeAt(at)
  const high = text.charCodeAt(at - 1)
  return at > 0 && low >= 0xdc00 && low <= 0xdfff && high >= 0xd800 && high <= 0xdbff ? at - 1 : at
}

/** 一处 `[Image #n]` 占位符：`[start, end)` 区间与编号。 */
interface PlaceholderSpan {
  start: number
  end: number
  number: number
}

const OPEN = '[Image #'

/** 文本里全部形如 `[Image #n]` 的占位符（`n` 为不带前导 0 的正整数），按出现顺序。 */
function placeholderSpans(text: string): PlaceholderSpan[] {
  const spans: PlaceholderSpan[] = []
  let from = 0
  for (;;) {
    const start = text.indexOf(OPEN, from)
    if (start < 0) return spans
    const digitsStart = start + OPEN.length
    let digitsEnd = digitsStart
    while (digitsEnd < text.length && text.charCodeAt(digitsEnd) >= 0x30 && text.charCodeAt(digitsEnd) <= 0x39) digitsEnd += 1
    const digits = text.slice(digitsStart, digitsEnd)
    if (digits !== '' && !digits.startsWith('0') && text[digitsEnd] === ']') {
      spans.push({ start, end: digitsEnd + 1, number: Number(digits) })
      from = digitsEnd + 1
    } else {
      from = digitsStart
    }
  }
}

/** 文本里全部 `[Image #N]` 占位符（N 为十进制数字，不带符号 / 前导空白）。 */
const PLACEHOLDER = /\[Image #(\d+)\]/g

/** 在选区 `[start, end)` 处写入第 `number` 张图片的占位符（选区被替换，与粘贴一致），光标落在占位符之后。 */
export function insertPlaceholder(text: string, start: number, end: number, number: number): TextEdit {
  const from = clampCaret(text, Math.min(start, end))
  const to = clampCaret(text, Math.max(start, end))
  const placeholder = assistantImagePlaceholder(number)
  return { text: text.slice(0, from) + placeholder + text.slice(to), caret: from + placeholder.length }
}

/**
 * 删除第 `number` 张图片后更新文本：该编号的占位符全部移除，其后编号（`number < N <= count`，`count` 为删除前的
 * 附件数）各减一，保持与剩余附件列表一一对应；超出范围的编号是用户手写的无效引用，原样保留。
 * 光标跟随文本移动（落在被移除的占位符里时回到它的起点）。
 */
export function removePlaceholder(text: string, caret: number, number: number, count: number): TextEdit {
  const at = clampCaret(text, caret)
  let out = ''
  let cursor = 0
  let nextCaret = at
  for (const match of text.matchAll(PLACEHOLDER)) {
    const start = match.index
    const end = start + match[0].length
    const n = Number(match[1])
    let replacement = match[0]
    if (n === number) replacement = ''
    else if (n > number && n <= count) replacement = assistantImagePlaceholder(n - 1)
    out += text.slice(cursor, start) + replacement
    cursor = end
    const delta = replacement.length - match[0].length
    if (delta !== 0) {
      if (end <= at) nextCaret += delta
      else if (start < at) nextCaret = out.length - replacement.length
    }
  }
  out += text.slice(cursor)
  return { text: out, caret: clampCaret(out, nextCaret) }
}

/** 编号 1..=`count` 里在文本中已找不到占位符的图片编号（升序）：用户删掉了占位符，对应图片随之移除。 */
export function orphanedImages(text: string, count: number): number[] {
  const present = new Set(placeholderSpans(text).map((span) => span.number))
  const orphaned: number[] = []
  for (let number = 1; number <= count; number += 1) {
    if (!present.has(number)) orphaned.push(number)
  }
  return orphaned
}

/**
 * 一次纯删除（退格、选区删除等）只删掉了某个占位符的一部分时，删除后文本里该占位符的残余区间 `[start, end)`；
 * 删掉它让占位符整体消失，与删掉一个字符的手感一致。只认编号在 `count` 以内的占位符。不是纯删除、或没有碰到
 * 占位符的局部时返回 null。
 */
export function placeholderRemnant(previous: string, next: string, count: number): { start: number; end: number } | null {
  if (count === 0 || next.length >= previous.length) return null
  let prefix = 0
  while (prefix < next.length && previous.charCodeAt(prefix) === next.charCodeAt(prefix)) prefix += 1
  const start = floorBoundary(previous, prefix)
  let suffix = 0
  while (
    suffix < next.length - start &&
    previous.charCodeAt(previous.length - 1 - suffix) === next.charCodeAt(next.length - 1 - suffix)
  ) {
    suffix += 1
  }
  const previousEnd = previous.length - suffix
  // 新文本在 `start` 之后若还有新增内容就不是纯删除。
  if (next.length - suffix !== start || floorBoundary(previous, previousEnd) !== previousEnd) return null
  let lo = start
  let hi = previousEnd
  for (const span of placeholderSpans(previous)) {
    const overlaps = span.start < previousEnd && start < span.end
    const covered = start <= span.start && span.end <= previousEnd
    if (span.number <= count && overlaps && !covered) {
      lo = Math.min(lo, span.start)
      hi = Math.max(hi, span.end)
    }
  }
  if (lo === start && hi === previousEnd) return null
  let end = start + (hi - previousEnd)
  // 与 GPUI 一致：占位符前是行首 / 空白时顺带吞掉后面的一个空格。
  const before = next.slice(0, lo)
  const atBoundary = before === '' || /\s$/.test(before)
  if (atBoundary && next[end] === ' ') end += 1
  return { start: lo, end }
}

/** 输入框文本变化后的收敛结果。 */
export interface DraftChange {
  draft: string
  caret: number
  attachments: Attachment[]
}

/**
 * 输入框文本变了（GPUI `input_changed`）：删到一半的占位符整体删掉；占位符已不在文本里的图片随之移除
 * （手动删除、剪切、撤销插入等），其后占位符与图片编号依次减一。`caret` 为 `next` 里的光标位置。
 */
export function reconcileDraft(previous: string, next: string, caret: number, attachments: readonly Attachment[]): DraftChange {
  let text = next
  let at = clampCaret(next, caret)
  const remnant = placeholderRemnant(previous, next, attachments.length)
  if (remnant !== null) {
    text = next.slice(0, remnant.start) + next.slice(remnant.end)
    if (at >= remnant.end) at -= remnant.end - remnant.start
    else if (at > remnant.start) at = remnant.start
  }
  const kept = [...attachments]
  // 从大到小移除，前面的编号才不受重排影响。
  for (const number of orphanedImages(text, kept.length).reverse()) {
    const edit = removePlaceholder(text, at, number, kept.length)
    text = edit.text
    at = edit.caret
    kept.splice(number - 1, 1)
  }
  return { draft: text, caret: clampCaret(text, at), attachments: kept }
}
