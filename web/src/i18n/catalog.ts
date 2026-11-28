// 翻译目录：与 GPUI（crates/i18n）同一份 assets/i18n/*.json，语义逐条镜像：
// locale 精确匹配 → 主语言匹配 → 英文键级回退（空值同缺键）→ 键名；`{name}` 占位插值。
// 语言表构建期自动发现（virtual:i18n-tables），新增语言只需新增 JSON 文件。

import TABLES from 'virtual:i18n-tables'

export type Params = Readonly<Record<string, string | number>>

/** 最终回退语言（与 Rust `FALLBACK_LOCALE` 一致）。 */
export const FALLBACK_LOCALE = 'en'

function normalizeLocale(locale: string): string {
  return locale.trim().toLowerCase().replaceAll('_', '-')
}

function localeRank(locale: string): number {
  return locale === 'en' ? 0 : locale === 'zh' ? 1 : 2
}

/** 可用 locale：`en`、`zh`，随后按代码排序（与 Rust 一致）。 */
export const AVAILABLE_LOCALES: readonly string[] = Object.keys(TABLES).sort(
  (a, b) => localeRank(a) - localeRank(b) || (a < b ? -1 : a > b ? 1 : 0),
)

/** 把系统/用户 locale 解析为实际可用代码。 */
export function resolveLocale(locale: string): string {
  const normalized = normalizeLocale(locale)
  if (normalized in TABLES) return normalized
  const [rawPrefix = normalized, scriptOrRegion] = normalized.split('-')
  // ISO 639 废弃码：旧版 Java / Android WebView 仍上报 `in`（= `id` 印尼语）。
  const prefix = rawPrefix === 'in' ? 'id' : rawPrefix
  if (prefix === 'zh') {
    return scriptOrRegion === 'hant' || scriptOrRegion === 'tw' || scriptOrRegion === 'hk' || scriptOrRegion === 'mo'
      ? 'zh-tw'
      : 'zh'
  }
  if (prefix in TABLES) return prefix
  for (const code of AVAILABLE_LOCALES) {
    if ((code.split('-')[0] ?? code) === prefix) return code
  }
  return FALLBACK_LOCALE
}

/** 语言文件声明的自称；缺失回退 locale 代码。 */
export function nativeName(locale: string): string {
  const value = TABLES[locale]?.languageNativeName
  return value ? value : locale
}

/** 查表；空值/缺键回退英文，再回退键名。 */
export function lookup(locale: string, key: string): string {
  const own = TABLES[locale]?.[key]
  if (own) return own
  const fallback = TABLES[FALLBACK_LOCALE]?.[key]
  return fallback ? fallback : key
}

/** `{name}` 占位插值：按参数顺序逐个全量替换（同 Rust `text_with`）。 */
export function interpolate(template: string, params?: Params): string {
  if (!params) return template
  let value = template
  for (const [name, replacement] of Object.entries(params)) {
    value = value.replaceAll(`{${name}}`, String(replacement))
  }
  return value
}

export function translate(locale: string, key: string, params?: Params): string {
  return interpolate(lookup(resolveLocale(locale), key), params)
}
