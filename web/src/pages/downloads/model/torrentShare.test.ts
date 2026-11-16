import { describe, expect, test } from 'bun:test'
import { canCopyUrls, canShareTorrent, torrentFileName } from './actions'
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
