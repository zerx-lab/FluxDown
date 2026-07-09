// 数值/时间格式化与文件类型推断 —— fmtBytes/fmtEta 移植自 design/web/app.js。

import { getLocale, t } from './i18n'

const GB = 1024 ** 3
const MB = 1024 ** 2
const KB = 1024

export function fmtBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B'
  if (n >= GB) return (n / GB).toFixed(n / GB >= 100 ? 0 : 1) + ' GB'
  if (n >= MB) return (n / MB).toFixed(n / MB >= 100 ? 0 : 1) + ' MB'
  if (n >= KB) return (n / KB).toFixed(0) + ' KB'
  return n.toFixed(0) + ' B'
}

export function fmtSpeed(n: number): string {
  return fmtBytes(n) + '/s'
}

/** 剩余时间（秒 → 人类可读）。speed<=0 或未知总量时返回 '—'。 */
export function fmtEta(remainingBytes: number, speed: number): string {
  if (speed <= 0 || remainingBytes <= 0) return '—'
  const s = Math.round(remainingBytes / speed)
  if (s < 60) return t('time.secs', { s })
  if (s < 3600) return t('time.minSecs', { m: Math.floor(s / 60), s: s % 60 })
  return t('time.hourMin', { h: Math.floor(s / 3600), m: Math.floor((s % 3600) / 60) })
}

/** Unix 秒 → 本地时间字符串。 */
export function fmtTime(unixSecs: string | number): string {
  const n = typeof unixSecs === 'string' ? parseInt(unixSecs, 10) : unixSecs
  if (!Number.isFinite(n) || n <= 0) return '—'
  return new Date(n * 1000).toLocaleString(getLocale() === 'zh' ? 'zh-CN' : 'en-US', { hour12: false })
}

// ---- 时间分组（今天/昨天/本周/本月/更早，对齐桌面端） ----

export type TimeGroup = 'today' | 'yesterday' | 'thisWeek' | 'thisMonth' | 'older'
export const GROUP_ORDER: TimeGroup[] = ['today', 'yesterday', 'thisWeek', 'thisMonth', 'older']
export function groupLabel(g: TimeGroup): string {
  const KEYS: Record<TimeGroup, 'time.today' | 'time.yesterday' | 'time.thisWeek' | 'time.thisMonth' | 'time.older'> = {
    today: 'time.today',
    yesterday: 'time.yesterday',
    thisWeek: 'time.thisWeek',
    thisMonth: 'time.thisMonth',
    older: 'time.older',
  }
  return t(KEYS[g])
}

export function timeGroup(unixSecs: string | number): TimeGroup {
  const n = typeof unixSecs === 'string' ? parseInt(unixSecs, 10) : unixSecs
  if (!Number.isFinite(n) || n <= 0) return 'older'
  const d = new Date(n * 1000)
  const now = new Date()
  const startOfDay = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime()
  const today = startOfDay(now)
  const t = d.getTime()
  if (t >= today) return 'today'
  if (t >= today - 86400_000) return 'yesterday'
  // 本周：周一为第一天
  const weekday = (now.getDay() + 6) % 7
  const weekStart = today - weekday * 86400_000
  if (t >= weekStart) return 'thisWeek'
  const monthStart = new Date(now.getFullYear(), now.getMonth(), 1).getTime()
  if (t >= monthStart) return 'thisMonth'
  return 'older'
}

// ---- 文件类型推断（扩展名列表对齐桌面端 download_task.dart） ----

export type FileType = 'video' | 'audio' | 'document' | 'image' | 'archive' | 'other'
export const TYPE_ORDER: ('all' | FileType)[] = ['all', 'video', 'audio', 'document', 'image', 'archive', 'other']
export function typeLabel(k: 'all' | FileType): string {
  const KEYS: Record<'all' | FileType, 'type.all' | 'type.video' | 'type.audio' | 'type.document' | 'type.image' | 'type.archive' | 'type.other'> = {
    all: 'type.all',
    video: 'type.video',
    audio: 'type.audio',
    document: 'type.document',
    image: 'type.image',
    archive: 'type.archive',
    other: 'type.other',
  }
  return t(KEYS[k])
}

const VIDEO = ['mp4', 'mkv', 'avi', 'mov', 'wmv', 'flv', 'webm', 'm4v', 'mpg', 'mpeg', 'ts', 'm3u8', '3gp', 'rmvb', 'vob']
const AUDIO = ['mp3', 'wav', 'flac', 'aac', 'ogg', 'wma', 'm4a', 'opus', 'ape', 'mid']
const DOCUMENT = ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'txt', 'md', 'epub', 'mobi', 'csv', 'rtf', 'odt']
const IMAGE = ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'svg', 'ico', 'tif', 'tiff', 'heic', 'avif', 'raw']
const ARCHIVE = ['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'iso', 'dmg', 'pkg', 'deb', 'rpm', 'apk']

export function fileType(fileName: string, url = ''): FileType {
  const name = fileName || url.split('?')[0].split('/').pop() || ''
  const ext = name.includes('.') ? name.split('.').pop()!.toLowerCase() : ''
  if (VIDEO.includes(ext) || /m3u8|magnet:/.test(url)) return 'video'
  if (AUDIO.includes(ext)) return 'audio'
  if (DOCUMENT.includes(ext)) return 'document'
  if (IMAGE.includes(ext)) return 'image'
  if (ARCHIVE.includes(ext)) return 'archive'
  return 'other'
}

/** 协议标识（列表行的 proto 徽标）。 */
export function protoLabel(url: string): string {
  if (url.startsWith('magnet:') || url.endsWith('.torrent')) return 'BT'
  if (url.startsWith('ftp://') || url.startsWith('ftps://')) return 'FTP'
  if (url.startsWith('ed2k://')) return 'eD2K'
  if (/\.m3u8(\?|$)/.test(url)) return 'HLS'
  if (/\.mpd(\?|$)/.test(url)) return 'DASH'
  if (url.startsWith('https://')) return 'HTTPS'
  if (url.startsWith('http://')) return 'HTTP'
  return 'URL'
}
