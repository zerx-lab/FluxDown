import { createConfigListPatcher } from './configList'

export const SITE_USER_AGENT_RULES_KEY = 'site_user_agent_rules'

export interface SiteUserAgentRule {
  domain: string
  includeSubdomains: boolean
  userAgent: string
}

// Only decode the wire shape here. Domain and header validation belong to the daemon.
export function parseSiteUserAgentRules(raw: string | undefined): SiteUserAgentRule[] | null {
  if (!raw) return []
  let data: unknown
  try {
    data = JSON.parse(raw)
  } catch {
    return null
  }
  if (!Array.isArray(data)) return null
  const rules: SiteUserAgentRule[] = []
  for (const entry of data as unknown[]) {
    if (!entry || typeof entry !== 'object' || Object.keys(entry).length !== 3) return null
    const { domain, includeSubdomains, userAgent } = entry as Record<string, unknown>
    if (typeof domain !== 'string' || typeof includeSubdomains !== 'boolean' || typeof userAgent !== 'string') return null
    rules.push({ domain, includeSubdomains, userAgent })
  }
  return rules
}

const patchRules = createConfigListPatcher(SITE_USER_AGENT_RULES_KEY, (raw) => {
  const rules = parseSiteUserAgentRules(raw)
  if (rules === null) throw new Error('Invalid site User-Agent rules snapshot')
  return rules
}, (list) => JSON.stringify(list))

/** With no persistent row IDs, never apply an index-based edit to a different list. */
export async function editSiteUserAgentRules(
  expected: readonly SiteUserAgentRule[],
  edit: (list: SiteUserAgentRule[]) => SiteUserAgentRule[],
): Promise<boolean> {
  let stale = false
  await patchRules((list) => {
    if (list.length !== expected.length || list.some((rule, index) => {
      const original = expected[index]
      return rule.domain !== original.domain || rule.includeSubdomains !== original.includeSubdomains || rule.userAgent !== original.userAgent
    })) {
      stale = true
      return null
    }
    return edit(list)
  })
  return !stale
}
