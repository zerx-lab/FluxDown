import { describe, expect, test } from 'bun:test'
import {
  parseSiteRequestHeaders, siteRequestHeadersDraft, siteRequestHeadersFields, updateSiteRequestHeaders,
} from './siteRequestHeaders'

const existing = { origin: 'https://old.example', headers: { referer: 'https://page.example/', 'X-Key': 'old' } }
const other = { origin: 'https://other.example', headers: { 'X-Key': 'unrelated' } }
const replacement = { origin: 'https://new.example', headers: { 'X-Key': 'new' } }

describe('site request headers', () => {
  test('keeps the target origin separate and preserves colons and interior spaces in the header value', () => {
    expect(siteRequestHeadersDraft(
      ' https://例子.example:8443 ', 'https://cdn.example/ref?contains=a:b',
      'X-Hint: two  spaces:with colon\r\n\r\nX-Flag:',
    )).toEqual({
      origin: 'https://例子.example:8443',
      headers: { Referer: 'https://cdn.example/ref?contains=a:b', 'X-Hint': 'two  spaces:with colon', 'X-Flag': '' },
    })
    expect(siteRequestHeadersFields(existing)).toEqual({ origin: existing.origin, referer: existing.headers.referer, extra: 'X-Key: old' })
  })

  test('rejects malformed or ambiguous fields instead of silently dropping headers', () => {
    for (const extra of ['no separator', 'X-Key: a\nx-key: b', 'referer: a', ': a']) {
      expect(siteRequestHeadersDraft('https://target.example', 'https://ref.example/', extra)).toBeNull()
    }
    expect(siteRequestHeadersDraft('', '', 'X-Key: value')).toBeNull()
    expect(siteRequestHeadersDraft('https://target.example', '', '')).toBeNull()
  })

  test('distinguishes an empty supported config from missing or malformed config', () => {
    expect(parseSiteRequestHeaders('')).toEqual([])
    expect(parseSiteRequestHeaders(undefined)).toBeNull()
    expect(parseSiteRequestHeaders('[{"origin":"https://target.example","headers":{"X-Key":3}}]')).toBeNull()
    expect(parseSiteRequestHeaders(JSON.stringify([existing]))).toEqual([existing])
  })

  test('adds, edits and removes only the requested rule in the latest list', () => {
    expect(updateSiteRequestHeaders([other], null, existing)).toEqual([other, existing])
    expect(updateSiteRequestHeaders([other, existing], existing.origin, replacement)).toEqual([other, replacement])
    expect(updateSiteRequestHeaders([other, existing], existing.origin, null)).toEqual([other])
    expect(updateSiteRequestHeaders([other], existing.origin, replacement)).toBeNull()
  })
})
