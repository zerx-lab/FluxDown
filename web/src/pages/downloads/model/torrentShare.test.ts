import { describe, expect, test } from 'bun:test'
import { batchExportRow, parseEntries } from '../dialogs/model'
import { canCopyUrls, canShareTorrent, exportTaskInfo, taskInfoRow, taskInfoText, torrentFileName } from './actions'
import type { DownloadTaskView } from './task'

const view = (source: string, protocol: string) => ({ source, protocol }) as unknown as DownloadTaskView

describe('torrentFileName', () => {
  test('清理非法字符并补扩展名', () => {
    expect(torrentFileName('a/b:c')).toBe('a_b_c.torrent')
    expect(torrentFileName('x.TORRENT')).toBe('x.TORRENT')
    expect(torrentFileName('  ')).toBe('torrent.torrent')
  })
})

describe('canShareTorrent', () => {
  test('仅单选本地 BT', () => {
    expect(canShareTorrent([view('local', 'bt')])).toBe(true)
    expect(canShareTorrent([view('local', 'http')])).toBe(false)
    expect(canShareTorrent([view('remote', 'bt')])).toBe(false)
    expect(canShareTorrent([view('local', 'bt'), view('local', 'bt')])).toBe(false)
  })
})

describe('canCopyUrls', () => {
  const task = (source: string, protocol: string, url: string, originUrl = '') =>
    ({ source, protocol, url, originUrl }) as unknown as DownloadTaskView
  const magnet = task('local', 'bt', 'magnet:?xt=urn:btih:abc')
  const imported = task('local', 'bt', 'torrent-file://local')

  test('单选本机磁力与导入种子隐藏，其余保留', () => {
    expect(canCopyUrls([magnet])).toBe(false)
    expect(canCopyUrls([imported])).toBe(false)
    expect(canCopyUrls([task('local', 'bt', 'torrent-file://local', 'https://rss.example/a.torrent')])).toBe(true)
    expect(canCopyUrls([task('remote', 'bt', 'magnet:?xt=urn:btih:abc')])).toBe(true)
    expect(canCopyUrls([task('local', 'http', 'https://e.com/a.zip')])).toBe(true)
  })

  test('多选有一项可复制即显示', () => {
    expect(canCopyUrls([magnet, imported])).toBe(true)
    expect(canCopyUrls([imported, imported])).toBe(false)
  })
})

test('task info keeps rows without links and escapes cell boundaries', () => {
  expect(taskInfoText([
    { name: '=报告\tpart\n2', url: 'https://example.com/resolved', originUrl: 'https://example.com/original', sizeBytes: 1 },
    { name: 'file.bin', url: 'torrent-file://local', originUrl: '', sizeBytes: 0 },
  ].map(taskInfoRow))).toBe("Name\tURL\tSize (bytes)\n'=报告\\tpart\\n2\thttps://example.com/original\t1\nfile.bin\t\t")
})

test('downloaded TXT preserves task fields and the complete parsed batch', async () => {
  const blobs: Blob[] = []
  const originalCreateUrl = URL.createObjectURL
  URL.createObjectURL = (blob: Blob | MediaSource) => {
    if (blob instanceof Blob) blobs.push(blob)
    return `blob:task-export-${blobs.length}`
  }
  const downloaded: string[] = []
  const anchor = { download: '', setAttribute() {}, click() { downloaded.push(this.download) }, remove() {} }
  const documentDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'document')
  Object.defineProperty(globalThis, 'document', {
    configurable: true,
    value: { createElement: () => anchor, body: { append() {} } },
  })
  try {
    exportTaskInfo([
      { name: '=报告\\x\tpart\r\n2', url: 'https://example.com/resolved', originUrl: 'https://example.com/original', sizeBytes: 4294967296 },
      { name: 'file.bin', url: 'torrent-file://local', originUrl: '', sizeBytes: 0 },
    ].map(taskInfoRow))
    const entries = parseEntries([
      'https://example.com/a%20b.zip?key=v',
      'ftp://example.com/f',
      '  out=报告.zip',
      'https://example.com/',
      'MAGNET:?xt=urn:btih:a&dn=My+Show%21',
      'ED2K://|FILE|f%20x.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/',
      'magnet:?xt=urn:btih:b',
      'ed2k://|file||100|h|/',
      'ed2k://|file|ignored|100|1555B7DCA052B5958EE68DB58A42408D|/',
      '  out=keep%20name',
      'ed2k://|file|empty.txt|0|31D6CFE0D16AE931B73C59D7E0C089C0|/',
      'ed2k://|file|invalid.txt|0|1555B7DCA052B5958EE68DB58A42408D|/',
    ].join('\n'), false)
    exportTaskInfo(entries.map(batchExportRow))
    expect(downloaded).toEqual(['fluxdown-tasks.txt', 'fluxdown-tasks.txt'])
    expect(blobs[0]?.type).toBe('text/plain;charset=utf-8')
    const readRows = async (blob: Blob | undefined) => (await blob?.text())?.split('\n').slice(1).map((line) =>
      line.split('\t').map((cell) => cell.replace(/\\([\\tnr])/g, (_, ch: string) =>
        ch === 't' ? '\t' : ch === 'n' ? '\n' : ch === 'r' ? '\r' : '\\',
      )),
    )
    expect(await readRows(blobs[0])).toEqual([
      ['=报告\\x\tpart\r\n2', 'https://example.com/original', '4294967296'],
      ['file.bin', '', ''],
    ])
    expect(await readRows(blobs[1])).toEqual([
      ['a b.zip', 'https://example.com/a%20b.zip?key=v', ''],
      ['报告.zip', 'ftp://example.com/f', ''],
      ['', 'https://example.com/', ''],
      ['My+Show!', 'MAGNET:?xt=urn:btih:a&dn=My+Show%21', ''],
      ['f x.iso', 'ED2K://|FILE|f%20x.iso|5044211712|1555B7DCA052B5958EE68DB58A42408D|/', '5044211712'],
      ['', 'magnet:?xt=urn:btih:b', ''],
      ['', 'ed2k://|file||100|h|/', ''],
      ['keep%20name', 'ed2k://|file|ignored|100|1555B7DCA052B5958EE68DB58A42408D|/', '100'],
      ['empty.txt', 'ed2k://|file|empty.txt|0|31D6CFE0D16AE931B73C59D7E0C089C0|/', '0'],
      ['invalid.txt', 'ed2k://|file|invalid.txt|0|1555B7DCA052B5958EE68DB58A42408D|/', ''],
    ])
  } finally {
    URL.createObjectURL = originalCreateUrl
    if (documentDescriptor) Object.defineProperty(globalThis, 'document', documentDescriptor)
    else Reflect.deleteProperty(globalThis, 'document')
  }
})
