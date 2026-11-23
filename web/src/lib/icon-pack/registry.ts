// 内置图标包与回退链（镜像 crates/icon_pack/src/registry.rs）。
// Web 取不到系统文件图标：链上的 `builtin:system` 环节直接跳过，用其后的包（与桌面取图失败时同一回退）。

import catppuccinText from '@icon-packs/catppuccin.json?raw'
import lucideText from '@icon-packs/lucide.json?raw'
import materialText from '@icon-packs/material.json?raw'
import thunderText from '@icon-packs/thunder.json?raw'
import { defaultIcon, isPackRef, matchedIcon, parseIconPack } from './pack'
import type { FileKind, IconPack, PackIcon } from './pack'

export const FILE_ICON_PACK_KEY = 'appearance.file_icon_pack'
export const SYSTEM_PACK_ID = 'system'
/** 未设置偏好时的选择（桌面 = 系统图标；Web 经回退显示 Lucide）。 */
export const DEFAULT_PACK = 'builtin:system'
export const FALLBACK_PACK = 'builtin:lucide'
const MAX_CHAIN = 8

/** 设置页展示顺序（同 `BUILTIN_PACK_IDS`）。 */
export const BUILTIN_PACK_IDS = [SYSTEM_PACK_ID, 'lucide', 'material', 'catppuccin', 'thunder'] as const

const BUILTIN_SOURCES: Record<string, string> = { lucide: lucideText, material: materialText, catppuccin: catppuccinText, thunder: thunderText }

const BUILTINS = new Map<string, IconPack>()
for (const [id, text] of Object.entries(BUILTIN_SOURCES)) {
  const parsed = parseIconPack(text)
  if (parsed.ok) BUILTINS.set(id, parsed.pack)
}

export function builtinPack(id: string): IconPack | undefined {
  return BUILTINS.get(id)
}

/** 选择 → 有序包链；首个引用不可用时为 `undefined`。 */
function buildChain(selection: string): IconPack[] | undefined {
  const chain: IconPack[] = []
  const visited: string[] = []
  let next: string | undefined = selection
  let usable = false
  while (next !== undefined && visited.length < MAX_CHAIN && !visited.includes(next) && isPackRef(next)) {
    const reference: string = next
    visited.push(reference)
    next = undefined
    if (reference === `builtin:${SYSTEM_PACK_ID}`) {
      usable = true
      continue
    }
    // 用户图标包库尚未落地：自定义引用一律不可用。
    const pack = reference.startsWith('builtin:') ? builtinPack(reference.slice('builtin:'.length)) : undefined
    if (!pack) break
    usable = true
    chain.push(pack)
    next = pack.extends
  }
  if (!usable) return undefined
  const fallback = builtinPack(FALLBACK_PACK.slice('builtin:'.length))
  if (!visited.includes(FALLBACK_PACK) && fallback) chain.push(fallback)
  return chain
}

export interface IconPackState {
  selection: string
  chain: IconPack[]
}

export function iconPackState(selection: string): IconPackState {
  return { selection, chain: buildChain(selection) ?? buildChain(DEFAULT_PACK) ?? [] }
}

/** 文件名（已小写）+ 大类 → 图标：链上第一个具体匹配，都没有时取第一个 `default`。 */
export function resolveFileIcon(state: IconPackState, lowerName: string, kind: FileKind): PackIcon | undefined {
  for (const pack of state.chain) {
    const matched = matchedIcon(pack, lowerName, kind)
    if (matched) return matched[1]
  }
  for (const pack of state.chain) {
    const fallback = defaultIcon(pack)
    if (fallback) return fallback[1]
  }
  return undefined
}

/** 偏好值 → 选择（非法值按默认）。 */
export function selectionOf(value: unknown): string {
  return typeof value === 'string' && isPackRef(value) ? value : DEFAULT_PACK
}
