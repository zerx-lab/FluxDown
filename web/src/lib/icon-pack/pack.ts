// 文件图标包（镜像 crates/icon_pack）：`fluxdown.icon-pack` v1 容错解析、SVG 安全校验、包内匹配。
// 两端共用用例 crates/icon_pack/tests/fixtures/cases.json（pack.test.ts）。

import kindTable from '@icon-packs/kinds.json'

export type FileKind = 'application' | 'diskImage' | 'mobile' | 'video' | 'audio' | 'document' | 'image' | 'archive' | 'other'

export const FILE_KINDS: readonly FileKind[] = ['application', 'diskImage', 'mobile', 'video', 'audio', 'document', 'image', 'archive', 'other']

const KIND_BY_EXTENSION: Record<string, FileKind> = Object.fromEntries(
  Object.entries(kindTable as Record<string, string[]>).flatMap(([kind, extensions]) => extensions.map((extension) => [extension, kind as FileKind])),
)

/** 文件名 → 大类（同 `FileKind::of_name`）：最后一个 `.` 之后的扩展名，大小写不敏感。 */
export function fileKindOf(name: string): FileKind {
  const dot = name.lastIndexOf('.')
  if (dot < 0) return 'other'
  const extension = name.slice(dot + 1).toLowerCase()
  if (extension === '' || extension.includes('/')) return 'other'
  return KIND_BY_EXTENSION[extension] ?? 'other'
}

export const ICON_PACK_FORMAT = 'fluxdown.icon-pack'
export const ICON_PACK_SCHEMA_VERSION = 1
export const MAX_SVG_BYTES = 64 * 1024
export const MAX_ICONS = 1024
const MAX_NAME_LEN = 64

const TOP_LEVEL_KEYS = ['format', 'schemaVersion', 'meta', 'extends', 'icons', 'fileNames', 'fileExtensions', 'kinds', 'default']
const ICON_KEYS = ['mode', 'svg', 'light', 'dark']
const META_KEYS = ['id', 'name', 'author', 'license', 'version', 'homepage'] as const

export type DiagnosticKind = 'invalidValue' | 'unknownKey' | 'newerVersion' | 'unsafeSvg'

export interface Diagnostic {
  path: string
  kind: DiagnosticKind
  message: string
}

export type IconPackParseError = 'invalidJson' | 'notAnObject' | 'unsupportedFormat'

export type IconMode = 'mask' | 'color'

/** 一段已校验的 SVG；`dataUrl` 解析时算一次，渲染直接用。 */
export interface IconSvg {
  text: string
  dataUrl: string
}

export interface PackIcon {
  mode: IconMode
  svg: IconSvg
  light?: IconSvg
  dark?: IconSvg
}

export type PackMeta = Record<(typeof META_KEYS)[number], string>

export interface IconPack {
  meta: PackMeta
  extends?: string
  icons: Map<string, PackIcon>
  fileNames: Map<string, string>
  fileExtensions: Map<string, string>
  kinds: Map<FileKind, string>
  default?: string
}

export type ParseResult = { ok: true; pack: IconPack; diagnostics: Diagnostic[] } | { ok: false; error: IconPackParseError }

/** 当前明暗模式下使用的 SVG。 */
export function iconVariant(icon: PackIcon, dark: boolean): IconSvg {
  return (dark ? icon.dark : icon.light) ?? icon.svg
}

/** 本包内的具体匹配：精确文件名 → 扩展名（复合优先）→ 大类；不含 `default`（同 `IconPack::matched_icon`）。 */
export function matchedIcon(pack: IconPack, lowerName: string, kind: FileKind): [string, PackIcon] | undefined {
  const base = lowerName.split(/[/\\]/).pop() ?? lowerName
  let name: string | undefined
  if (base !== '') {
    name = pack.fileNames.get(base)
    for (let index = base.indexOf('.'); name === undefined && index >= 0; index = base.indexOf('.', index + 1)) {
      name = pack.fileExtensions.get(base.slice(index + 1))
    }
  }
  name ??= pack.kinds.get(kind)
  const icon = name === undefined ? undefined : pack.icons.get(name)
  return name !== undefined && icon ? [name, icon] : undefined
}

export function defaultIcon(pack: IconPack): [string, PackIcon] | undefined {
  const icon = pack.default === undefined ? undefined : pack.icons.get(pack.default)
  return pack.default !== undefined && icon ? [pack.default, icon] : undefined
}

/** `builtin:<id>` / `custom:<id>`（同 `fluxdown_protocol::is_icon_pack_ref`）。 */
export function isPackRef(value: string): boolean {
  const colon = value.indexOf(':')
  if (colon < 0) return false
  const source = value.slice(0, colon)
  const id = value.slice(colon + 1)
  return (source === 'builtin' || source === 'custom') && id.length <= MAX_NAME_LEN && /^[a-z0-9_]+(-[a-z0-9_]+)*$/.test(id)
}

const FORBIDDEN = ['<script', '<foreignobject', '<iframe', '<object', '<embed', '<!entity', '<!doctype', 'javascript:', '@import']

/**
 * 拒收会执行脚本、读外部资源或展开实体的 SVG（同 `svg_is_safe`，逐条同一规则）。
 * 空白只认 ASCII（Rust `is_ascii_whitespace`）；判定用前瞻，等价于 Rust 的贪婪跳过，不会回溯误判。
 */
export function svgIsSafe(svg: string): boolean {
  const lower = svg.replace(/[A-Z]/g, (char) => char.toLowerCase())
  const start = lower.replace(/^[ \t\n\f\r]+/, '')
  if (!(start.startsWith('<svg') || start.startsWith('<?xml')) || !lower.includes('<svg')) return false
  if (FORBIDDEN.some((needle) => lower.includes(needle))) return false
  // href / xlink:href 只允许文档内片段引用（`#id`）或内嵌 PNG；url(...) 只允许片段引用。
  if (/href[ \t\n\f\r]*=(?![ \t\n\f\r]*["']?(?:#|data:image\/png;base64,))/.test(lower)) return false
  if (/url\((?![ \t\n\f\r"']*#)/.test(lower)) return false
  // 事件属性：空白后的 `on<字母>+` 紧跟 `=`。
  return !/[ \t\n\f\r]on[a-z]+[ \t\n\f\r]*=/.test(lower)
}

function svgEntry(text: string): IconSvg {
  return { text, dataUrl: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(text)}` }
}

type Json = unknown

function isObject(value: Json): value is Record<string, Json> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function parseIconPack(text: string): ParseResult {
  let root: Json
  try {
    root = JSON.parse(text)
  } catch {
    return { ok: false, error: 'invalidJson' }
  }
  if (!isObject(root)) return { ok: false, error: 'notAnObject' }
  if (root.format !== ICON_PACK_FORMAT) return { ok: false, error: 'unsupportedFormat' }

  const diagnostics: Diagnostic[] = []
  const invalid = (path: string, message: string) => diagnostics.push({ path, kind: 'invalidValue', message })
  const unknown = (path: string) => diagnostics.push({ path, kind: 'unknownKey', message: 'unknown key' })
  const objectAt = (path: string, value: Json): Record<string, Json> | undefined => {
    if (value === undefined) return undefined
    if (isObject(value)) return value
    invalid(path, 'must be an object')
    return undefined
  }

  for (const key of Object.keys(root)) if (!TOP_LEVEL_KEYS.includes(key)) unknown(key)
  const version = root.schemaVersion
  if (version !== undefined) {
    if (typeof version !== 'number' || !Number.isInteger(version) || version < 1) invalid('schemaVersion', 'must be a positive integer')
    else if (version > ICON_PACK_SCHEMA_VERSION)
      diagnostics.push({ path: 'schemaVersion', kind: 'newerVersion', message: `schema version ${version} is newer than ${ICON_PACK_SCHEMA_VERSION}` })
  }

  const meta: PackMeta = { id: '', name: '', author: '', license: '', version: '', homepage: '' }
  for (const [key, value] of Object.entries(objectAt('meta', root.meta) ?? {})) {
    const path = `meta.${key}`
    if (!(META_KEYS as readonly string[]).includes(key)) unknown(path)
    else if (typeof value === 'string') meta[key as keyof PackMeta] = value
    else invalid(path, 'must be a string')
  }

  let extendsRef: string | undefined
  if (root.extends !== undefined) {
    if (typeof root.extends === 'string' && isPackRef(root.extends)) extendsRef = root.extends
    else invalid('extends', 'must be builtin:<id> or custom:<id>')
  }

  const parseSvg = (path: string, value: Json, required: boolean): IconSvg | undefined => {
    if (value === undefined) {
      if (required) invalid(path, 'is required')
      return undefined
    }
    if (typeof value !== 'string') {
      invalid(path, 'must be a string')
      return undefined
    }
    if (new TextEncoder().encode(value).length > MAX_SVG_BYTES) {
      diagnostics.push({ path, kind: 'unsafeSvg', message: 'svg is too large' })
      return undefined
    }
    if (!svgIsSafe(value)) {
      diagnostics.push({ path, kind: 'unsafeSvg', message: 'svg contains scripts, external references or event handlers' })
      return undefined
    }
    return svgEntry(value)
  }

  const icons = new Map<string, PackIcon>()
  for (const [name, value] of Object.entries(objectAt('icons', root.icons) ?? {})) {
    const path = `icons.${name}`
    if (name === '' || new TextEncoder().encode(name).length > MAX_NAME_LEN) {
      invalid(path, 'icon name must be 1-64 bytes')
      continue
    }
    if (icons.size >= MAX_ICONS) {
      invalid(path, 'too many icons')
      continue
    }
    if (!isObject(value)) {
      invalid(path, 'must be an object')
      continue
    }
    for (const key of Object.keys(value)) if (!ICON_KEYS.includes(key)) unknown(`${path}.${key}`)
    let mode: IconMode = 'color'
    if (value.mode === 'mask') mode = 'mask'
    else if (value.mode !== undefined && value.mode !== 'color') invalid(`${path}.mode`, 'must be "mask" or "color"')
    const svg = parseSvg(`${path}.svg`, value.svg, true)
    if (!svg) continue
    const light = parseSvg(`${path}.light`, value.light, false)
    const dark = parseSvg(`${path}.dark`, value.dark, false)
    icons.set(name, { mode, svg, ...(light ? { light } : {}), ...(dark ? { dark } : {}) })
  }

  const mapping = (section: string, value: Json): Map<string, string> => {
    const result = new Map<string, string>()
    for (const [key, name] of Object.entries(objectAt(section, value) ?? {})) {
      const path = `${section}.${key}`
      if (typeof name !== 'string') invalid(path, 'must be a string')
      else if (key === '') invalid(path, 'key must not be empty')
      else if (icons.has(name)) result.set(key.toLowerCase(), name)
      else invalid(path, 'refers to an undefined icon')
    }
    return result
  }
  const fileNames = mapping('fileNames', root.fileNames)
  const fileExtensions = mapping('fileExtensions', root.fileExtensions)

  const kinds = new Map<FileKind, string>()
  for (const [key, name] of Object.entries(objectAt('kinds', root.kinds) ?? {})) {
    const path = `kinds.${key}`
    if (!(FILE_KINDS as readonly string[]).includes(key)) diagnostics.push({ path, kind: 'unknownKey', message: 'unknown file kind' })
    else if (typeof name !== 'string') invalid(path, 'must be a string')
    else if (icons.has(name)) kinds.set(key as FileKind, name)
    else invalid(path, 'refers to an undefined icon')
  }

  let defaultName: string | undefined
  if (root.default !== undefined) {
    if (typeof root.default !== 'string') invalid('default', 'must be a string')
    else if (icons.has(root.default)) defaultName = root.default
    else invalid('default', 'refers to an undefined icon')
  }

  return {
    ok: true,
    pack: { meta, ...(extendsRef ? { extends: extendsRef } : {}), icons, fileNames, fileExtensions, kinds, ...(defaultName ? { default: defaultName } : {}) },
    diagnostics,
  }
}
