import { describe, expect, test } from 'bun:test'
import { appendLiteralEntries, batchExportRow, buildBatch, buildRequests, captureEntry, capturedReferrer, entryToText, parseEntries, parseManualInput } from './model'
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

describe('manual HTTP filename ranges', () => {
  const urls = (text: string) => parseManualInput(text).entries.map((item) => item.url)

  test('expands an inclusive series and previews the endpoints without rewriting URL data', () => {
    const prefix = 'HTTPS://[2001:db8::1]/目录/组合e\u0301😀'
    const suffix = '.tar.gz?token=a%2Fb&next=[003-004]#part[005-006]'
    const parsed = parseManualInput(`${prefix}[001-215]${suffix}`)
    expect(parsed.entries).toHaveLength(215)
    expect(parsed.rangeCount).toBe(215)
    expect(parsed.rangeLimitExceeded).toBe(false)
    const first = `${prefix}001${suffix}`
    const last = `${prefix}215${suffix}`
    expect(parsed.entries[0].url).toBe(first)
    expect(parsed.entries[214].url).toBe(last)
    expect(parsed.rangePreview).toEqual({ first, last })
  })

  test('uses the start width, supports zero and does not truncate growing numbers', () => {
    expect(urls('https://a/[9-011].zip')).toEqual(['https://a/9.zip', 'https://a/10.zip', 'https://a/11.zip'])
    expect(urls('https://a/[000-2].zip')).toEqual(['https://a/000.zip', 'https://a/001.zip', 'https://a/002.zip'])
    expect(urls('https://a/[9007199254740992-9007199254740993].zip')).toEqual([
      'https://a/9007199254740992.zip', 'https://a/9007199254740993.zip',
    ])
    const padding = '0'.repeat(80)
    expect(urls(`https://a/[${padding}1-2].zip`)).toEqual([
      `https://a/${padding}1.zip`, `https://a/${padding}2.zip`,
    ])
  })

  test('unsupported locations, protocols, descending and malformed ranges stay literal', () => {
    for (const url of [
      'https://a/[001-002]/file.zip',
      'https://a/file.zip?part=[001-002]',
      'https://a/file.zip#[001-002]',
      'https://[2001:db8::1]',
      'https://a/%5B001-002%5D.zip',
      'ftp://a/[001-002].zip',
      'magnet:?xt=urn:btih:[001-002]',
      'ed2k://|file|[001-002].zip|1|ff|/',
      'https://a/[002-001].zip',
      'https://a/[1-2][3-4].zip',
      'https://a/[[1-2]].zip',
      'https://a/[1-2]extra].zip',
      'https://a/[1-2',
      'https://a/[1-].zip',
      'https://a/[-1-2].zip',
      'https://a/[+1-2].zip',
      'https://a/[1.0-2].zip',
      'https://a/[1–2].zip',
      'https://a/[１-２].zip',
    ]) {
      const parsed = parseManualInput(url)
      expect(parsed.entries.map((item) => item.url)).toEqual([url])
      expect(parsed.rangeCount).toBe(0)
      expect(parsed.rangePreview).toBeNull()
    }
  })

  test('counts generated entries cumulatively, without consuming the limit for ordinary URLs', () => {
    const parsed = parseManualInput('https://a/plain.zip\nhttps://a/[001-500].zip\nhttps://b/[501-1000].zip\nhttps://b/plain.zip')
    expect(parsed.entries).toHaveLength(1002)
    expect(parsed.rangeCount).toBe(1000)
    expect(parsed.rangeLimitExceeded).toBe(false)
    expect(parsed.rangePreview).toEqual({ first: 'https://a/001.zip', last: 'https://b/1000.zip' })
    expect(urls(Array.from({ length: 1001 }, (_, index) => `https://a/${index}.zip`).join('\n'))).toHaveLength(1001)
  })

  test('rejects the entire form above the cumulative limit', () => {
    for (const text of [
      'https://a/plain.zip\nhttps://a/[1-1001].zip',
      'https://a/[1-500].zip\nhttps://b/plain.zip\nhttps://b/[1-501].zip',
      'https://a/[1-999999999999999999999999999999999999].zip',
    ]) {
      const parsed = parseManualInput(text)
      expect(parsed.entries).toEqual([])
      expect(parsed.rangeLimitExceeded).toBe(true)
      expect(parsed.rangePreview).toBeNull()
    }
  })

  test('does not reinterpret an aria2c URL even when an identical standalone URL expands', () => {
    const url = 'https://a/[01-02].zip'
    const parsed = parseManualInput(`aria2c -U CustomAgent \\\n  -o same.zip '${url}'\n  checksum=md5=ff\n${url}`)
    expect(parsed.entries).toEqual([
      { url, fileName: 'same.zip', checksum: 'md5=ff' },
      { url: 'https://a/01.zip', fileName: '', checksum: '' },
      { url: 'https://a/02.zip', fileName: '', checksum: '' },
    ])
    expect(parsed.hints.userAgent).toBe('CustomAgent')
    expect(parsed.rangeCount).toBe(2)
  })

  test('appends literal imports without changing a duplicate manual range', () => {
    const manual = 'https://c/[05-06].zip'
    const capture = captureEntry('https://a/[01-02].zip', 'captured.zip')
    const imported = parseEntries('see https://b/[03-04].\n  out=dir/imported.zip\n  checksum=md5=aa', true)
    expect(imported).toEqual([{ url: 'https://b/[03-04]', fileName: 'dir/imported.zip', checksum: 'md5=aa' }])
    const draft = appendLiteralEntries({ text: manual, literalUrls: new Set() }, [capture, ...imported, captureEntry(manual, '')])
    const parsed = parseManualInput(draft.text, draft.literalUrls)
    expect(parsed.entries).toEqual([
      { url: 'https://c/05.zip', fileName: '', checksum: '' },
      { url: 'https://c/06.zip', fileName: '', checksum: '' },
      capture,
      ...imported,
    ])
    expect(parsed.rangeCount).toBe(2)
    expect(parseEntries(capture.url, false)).toEqual([{ url: capture.url, fileName: '', checksum: '' }])
  })

  test('serialized command failures keep their literal meaning when retried', () => {
    const url = 'https://a/[1-1001].zip'
    const command = parseManualInput(`aria2c -o retry.zip '${url}'`)
    expect(command.entries).toEqual([{ url, fileName: 'retry.zip', checksum: '' }])
    expect(command.rangeLimitExceeded).toBe(false)
    const retry = parseManualInput(command.entries.map(entryToText).join('\n'), new Set([url]))
    expect(retry.entries).toEqual(command.entries)
    expect(retry.rangeLimitExceeded).toBe(false)
  })

  test('keeps per-item overrides, export names and single-file overrides without cloning options', () => {
    const options: DraftOptions = {
      saveDir: '/dl', segments: 0, cookies: '', referrer: '', proxyUrl: '', userAgent: '', queueId: 'main',
      checksum: 'md5=form', ignoreTlsErrors: false, headers: {}, startPaused: false,
      rename: 'form.zip', httpUser: '', httpPassword: '', saveSiteAuth: false,
    }
    const { entries } = parseManualInput(
      'https://a/[01-02].zip\n  out=same.zip\n  checksum=md5=template\nhttps://a/plain.zip\n  out=dir/plain.zip\n  checksum=md5=aa',
    )
    expect(buildRequests(entries, options).map((item) => [item.url, item.fileName, item.checksum])).toEqual([
      ['https://a/01.zip', '', ''], ['https://a/02.zip', '', ''], ['https://a/plain.zip', 'dir/plain.zip', 'md5=aa'],
    ])
    const batch = buildBatch(entries, options)
    expect(batch.common.fileName).toBe('')
    expect(batch.common.checksum).toBe('')
    expect(batch.items).toEqual(entries)
    expect(entries.map((item) => batchExportRow(item).name)).toEqual(['01.zip', '02.zip', 'dir/plain.zip'])
    const singleton = parseManualInput('https://a/[001-001].zip\n  out=one.zip\n  checksum=md5=one')
    expect(singleton.entries).toEqual([{ url: 'https://a/001.zip', fileName: 'one.zip', checksum: 'md5=one' }])
    expect(buildRequests(singleton.entries, options)[0].fileName).toBe('form.zip')
    expect(buildRequests(singleton.entries, options)[0].checksum).toBe('md5=form')
  })
})
