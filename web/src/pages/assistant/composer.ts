// 输入区的纯逻辑：图片附件的校验，以及提问文本里 `[Image #N]` 占位符的写入 / 删除 / 重新编号。
// 占位符约定与 FluxCloud 解析一致（`assistantImagePlaceholder`）：第 N 张附图（从 1 起）在文本里写成
// `[Image #N]`，云端把图片紧跟在占位符之后发给模型；这里不碰 React，也不读文件。

import { assistantImagePlaceholder } from '../../lib/rpc/protocol'
import type { AssistantConfigDto } from '../../lib/rpc/protocol'

/** 云端接受的图片类型（`AssistantAskParams.images` 的 `data:image/<type>;base64,…`）。 */
export const ATTACHABLE_IMAGE_TYPES: readonly string[] = ['image/png', 'image/jpeg', 'image/gif', 'image/webp']

/** 一张待发送的附图（编号 = 它在附件列表里的位置 + 1）。 */
export interface Attachment {
  /** 稳定的界面键（删除后其余附件的键不变）。 */
  key: number
  /** `data:image/...;base64,...`。 */
  dataUrl: string
  /** 原文件名（缩略图的无障碍标签；剪贴板图片可能为空）。 */
  name: string
  bytes: number
}

/** 输入草稿与待发送附图（提升到页面层：切换会话 / 移动端换栏时不丢）。 */
export interface ComposerState {
  draft: string
  attachments: Attachment[]
}

export const EMPTY_COMPOSER: ComposerState = { draft: '', attachments: [] }

/** 附图能力：云端配置声明支持且允许至少一张。不满足时输入区不提供任何图片入口。 */
export function imagesEnabled(config: AssistantConfigDto | undefined): boolean {
  return config !== undefined && config.imagesSupported && config.maxImages > 0
}

/** 附图失败的原因（对应提示文案）。 */
export type AttachError = 'unsupported' | 'type' | 'tooMany' | 'tooLarge'

/**
 * 校验再添加一张图片：能力、类型、张数（`maxImages`）与单张字节数（`maxImageBytes`，`0` 不在本地限制，
 * 交给云端校验）。`count` 为已有附件数；通过返回 null。
 */
export function checkAttachable(
  config: AssistantConfigDto | undefined,
  count: number,
  file: { type: string; size: number },
): AttachError | null {
  if (config === undefined || !imagesEnabled(config)) return 'unsupported'
  if (!ATTACHABLE_IMAGE_TYPES.includes(file.type.toLowerCase())) return 'type'
  if (count >= config.maxImages) return 'tooMany'
  if (config.maxImageBytes > 0 && file.size > config.maxImageBytes) return 'tooLarge'
  return null
}

/** 文本里全部 `[Image #N]` 占位符（N 为十进制数字，不带符号 / 前导空白）。 */
const PLACEHOLDER = /\[Image #(\d+)\]/g

export interface TextEdit {
  text: string
  /** 编辑后的光标位置（UTF-16 下标，与 `textarea.selectionStart` 同口径）。 */
  caret: number
}

function clampCaret(text: string, caret: number): number {
  return Math.min(text.length, Math.max(0, caret))
}

/** 在光标处写入第 `number` 张图片的占位符，光标落在占位符之后。 */
export function insertPlaceholder(text: string, caret: number, number: number): TextEdit {
  const at = clampCaret(text, caret)
  const placeholder = assistantImagePlaceholder(number)
  return { text: text.slice(0, at) + placeholder + text.slice(at), caret: at + placeholder.length }
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

/** 文本里引用了第 `number` 张图片（用户删掉占位符后缩略图仍在，云端会把它附在末尾）。 */
export function isReferenced(text: string, number: number): boolean {
  return text.includes(assistantImagePlaceholder(number))
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
