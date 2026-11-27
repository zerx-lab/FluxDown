import { describe, expect, test } from 'bun:test'
import { daemonConfigField } from './rpc/protocol/config'
import { parseSiteUserAgentRules, SITE_USER_AGENT_RULES_KEY } from './siteUserAgentRules'

const exact = { domain: 'cdn.xn--bcher-kva.example', includeSubdomains: false, userAgent: 'Custom/1.0  (desktop)' }
const parent = { domain: 'xn--bcher-kva.example', includeSubdomains: true, userAgent: 'Browser/2.0' }

describe('site User-Agent rules wire format', () => {
  test('daemon catalog owns a text config key with an empty default', () => {
    expect(daemonConfigField(SITE_USER_AGENT_RULES_KEY)).toEqual({ key: SITE_USER_AGENT_RULES_KEY, kind: 'text', default: '' })
    expect(parseSiteUserAgentRules(undefined)).toEqual([])
    expect(parseSiteUserAgentRules('')).toEqual([])
    expect(parseSiteUserAgentRules('[]')).toEqual([])
  })

  test('decoding preserves priority, subdomain choice, duplicates, and header spacing', () => {
    const list = [exact, parent, exact]
    expect(parseSiteUserAgentRules(JSON.stringify(list))).toEqual(list)
    expect(parseSiteUserAgentRules(JSON.stringify([parent, exact]))).toEqual([parent, exact])
  })

  test('malformed wire entries are not silently filtered or defaulted', () => {
    const invalid = [null, 1, [], { domain: 'example.com', userAgent: 'Browser/1' }, { ...exact, includeSubdomains: 'false' }, { ...exact, extra: true }]
    for (const row of invalid) expect(parseSiteUserAgentRules(JSON.stringify([exact, row]))).toBeNull()
    expect(parseSiteUserAgentRules('{')).toBeNull()
    expect(parseSiteUserAgentRules('{}')).toBeNull()
  })
})
