import { describe, expect, test } from 'bun:test'
import { buildRequests, captureEntry, capturedReferrer } from './model'
import type { DraftOptions } from './model'

describe('captureEntry', () => {
  test('省略与 URL 末段（含百分号解码后）相同的 out=', () => {
    expect(captureEntry('https://x.example/a.zip?s=1', 'a.zip').fileName).toBe('')
    expect(captureEntry('https://x.example/%E4%B8%AD.zip', '中.zip').fileName).toBe('')
    expect(captureEntry('https://x.example/a%20b.zip', 'a b.zip').fileName).toBe('')
  })

  test('真正改名或解码失败时保留 out=', () => {
    expect(captureEntry('https://x.example/%E4%B8%AD.zip', 'other.zip').fileName).toBe('other.zip')
    expect(captureEntry('https://x.example/%B6%D4.zip', '对.zip').fileName).toBe('对.zip')
    expect(captureEntry('https://x.example/%B6%D4.zip', '%B6%D4.zip').fileName).toBe('')
    expect(captureEntry('https://x.example/', 'index.html').fileName).toBe('index.html')
  })
})

describe('capturedReferrer', () => {
  const one = [{ url: 'https://a.example/x', fileName: '', checksum: '' }]
  const two = [...one, { url: 'https://b.example/y', fileName: '', checksum: '' }]
  const page = { url: 'https://a.example/x', referrer: ' https://site.example/v/1 ' }

  test('仅单条捕获带入 http(s) 来源页', () => {
    expect(capturedReferrer(one, [page])).toBe('https://site.example/v/1')
    expect(capturedReferrer(two, [page])).toBe('')
    expect(capturedReferrer([{ url: 'https://other.example/z', fileName: '', checksum: '' }], [page])).toBe('')
  })

  test('占位 / 非 http(s) / `://` 后为空不带入，scheme 不分大小写', () => {
    for (const junk of ['', 'about:client', 'file:///etc/passwd', 'javascript://x', 'https://']) {
      expect(capturedReferrer(one, [{ url: 'https://a.example/x', referrer: junk }])).toBe('')
    }
    expect(capturedReferrer(one, [{ url: 'https://a.example/x', referrer: 'HTTP://Site.example/' }])).toBe('HTTP://Site.example/')
  })
})

describe('buildRequests referrer', () => {
  const options: DraftOptions = {
    saveDir: '/dl',
    segments: 0,
    cookies: '',
    referrer: 'https://site.example/video/1',
    proxyUrl: '',
    userAgent: '',
    queueId: 'main',
    checksum: '',
    ignoreTlsErrors: false,
    headers: {},
    startPaused: false,
    rename: '',
    httpUser: '',
    httpPassword: '',
    saveSiteAuth: false,
  }

  test('来源页进入每条请求，留空保持空串', () => {
    const entries = [
      { url: 'https://a/1', fileName: '', checksum: '' },
      { url: 'https://a/2', fileName: '', checksum: '' },
    ]
    expect(buildRequests(entries, options).map((request) => request.referrer)).toEqual(['https://site.example/video/1', 'https://site.example/video/1'])
    expect(buildRequests(entries, { ...options, referrer: '' })[0].referrer).toBe('')
  })
})
