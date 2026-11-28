import { createConfigListPatcher } from './configList'

export const SITE_REQUEST_HEADERS_KEY = 'site_request_headers'

export interface SiteRequestHeadersRule {
  origin: string
  headers: Record<string, string>
}

function isRule(value: unknown): value is SiteRequestHeadersRule {
  if (typeof value !== 'object' || value === null || !('origin' in value) || typeof value.origin !== 'string'
    || !('headers' in value)) return false
  const { headers } = value
  return typeof headers === 'object' && headers !== null && !Array.isArray(headers)
    && Object.values(headers).every((header) => typeof header === 'string')
}

export function parseSiteRequestHeaders(raw: string | undefined): SiteRequestHeadersRule[] | null {
  if (raw === undefined) return null
  if (raw.trim() === '') return []
  try {
    const parsed: unknown = JSON.parse(raw)
    return Array.isArray(parsed) && parsed.every(isRule) ? parsed : null
  } catch {
    return null
  }
}

export function siteRequestHeadersDraft(origin: string, referer: string, extra: string): SiteRequestHeadersRule | null {
  const headers: [string, string][] = []
  const names = new Set(['referer'])
  if (referer.trim() !== '') headers.push(['Referer', referer.trim()])
  for (const line of extra.split(/\r?\n/)) {
    if (line.trim() === '') continue
    const separator = line.indexOf(':')
    if (separator < 0) return null
    const name = line.slice(0, separator).trim()
    const folded = name.toLowerCase()
    if (name === '' || names.has(folded)) return null
    names.add(folded)
    headers.push([name, line.slice(separator + 1).trim()])
  }
  if (origin.trim() === '' || headers.length === 0) return null
  return { origin: origin.trim(), headers: Object.fromEntries(headers) }
}

export function siteRequestHeadersFields(rule: SiteRequestHeadersRule | null) {
  let referer = ''
  const extra: string[] = []
  for (const [name, value] of Object.entries(rule?.headers ?? {})) {
    if (name.toLowerCase() === 'referer') referer = value
    else extra.push(`${name}: ${value}`)
  }
  return { origin: rule?.origin ?? '', referer, extra: extra.join('\n') }
}

export function updateSiteRequestHeaders(
  list: SiteRequestHeadersRule[], original: string | null, draft: SiteRequestHeadersRule | null,
): SiteRequestHeadersRule[] | null {
  if (original === null) return draft ? [...list, draft] : null
  const index = list.findIndex((rule) => rule.origin === original)
  if (index < 0) return null
  return draft ? list.map((rule, i) => i === index ? draft : rule) : list.filter((_, i) => i !== index)
}

const patchList = createConfigListPatcher(
  SITE_REQUEST_HEADERS_KEY,
  (raw) => {
    const list = parseSiteRequestHeaders(raw)
    if (!list) throw new Error('Could not read site request headers')
    return list
  },
  (list) => list.length === 0 ? '' : JSON.stringify(list),
)

export function patchSiteRequestHeaders(original: string | null, draft: SiteRequestHeadersRule | null): Promise<void> {
  return patchList((list) => updateSiteRequestHeaders(list, original, draft))
}
