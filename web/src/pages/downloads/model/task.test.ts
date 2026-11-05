import { describe, expect, test } from 'bun:test'
import { sourcePageUrl } from './task'

describe('sourcePageUrl（与 GPUI source_page_url 同组用例）', () => {
  test('只放行有 host 的 http(s) 来源页', () => {
    for (const web of ['https://a.example/p?q=1', 'HTTP://a.example', 'http://[::1]:8080/']) {
      expect(sourcePageUrl({ referrer: web })).toBe(web)
    }
  })

  test('其他协议、空 host 与前导空白只显示不打开', () => {
    for (const blocked of [
      '',
      'https://',
      'file:///etc/passwd',
      'javascript:alert(1)//https://x',
      'ftp://a.example/',
      'chrome-extension://abc/page.html',
      ' https://a.example/',
    ]) {
      expect(sourcePageUrl({ referrer: blocked })).toBeNull()
    }
  })
})
