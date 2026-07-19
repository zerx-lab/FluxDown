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

/** ISO 时间字符串 → 相对时间（"3 分钟前"）；30 天以上回退本地日期，非法输入返回 '—'。
 *  用于 FluxCloud 设备列表的 lastSeenAt 等 ISO8601 时间戳（不同于 fmtTime 的 unix 秒）。 */
export function fmtRelativeTime(iso: string): string {
  const ms = Date.parse(iso)
  if (!Number.isFinite(ms)) return '—'
  const diffSecs = Math.floor((Date.now() - ms) / 1000)
  if (diffSecs < 60) return t('time.justNow')
  const diffMins = Math.floor(diffSecs / 60)
  if (diffMins < 60) return t('time.minutesAgo', { n: diffMins })
  const diffHours = Math.floor(diffMins / 60)
  if (diffHours < 24) return t('time.hoursAgo', { n: diffHours })
  const diffDays = Math.floor(diffHours / 24)
  if (diffDays < 30) return t('time.daysAgo', { n: diffDays })
  return new Date(ms).toLocaleDateString(getLocale() === 'zh' ? 'zh-CN' : 'en-US')
}

/** ISO 时间字符串 → 本地绝对时间（"2026/7/17 14:03:20"），用于设备详情的
 *  首次信任时间/最近活跃精确值（相对时间见 fmtRelativeTime）。非法输入返回 '—'。 */
export function fmtIsoTime(iso: string): string {
  const ms = Date.parse(iso)
  if (!Number.isFinite(ms)) return '—'
  return new Date(ms).toLocaleString(getLocale() === 'zh' ? 'zh-CN' : 'en-US', { hour12: false })
}

/** 任务耗时（开始→完成）：`23s` / `3m05s` / `1h02m03s`。无效输入返回 '—'。 */
export function fmtDuration(startSecs: string | number, endSecs: string | number): string {
  const a = typeof startSecs === 'string' ? parseInt(startSecs, 10) : startSecs
  const b = typeof endSecs === 'string' ? parseInt(endSecs, 10) : endSecs
  if (!Number.isFinite(a) || !Number.isFinite(b) || a <= 0 || b <= 0) return '—'
  const s = Math.max(0, b - a)
  const pad = (v: number) => v.toString().padStart(2, '0')
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m${pad(s % 60)}s`
  return `${Math.floor(s / 3600)}h${pad(Math.floor((s % 3600) / 60))}m${pad(s % 60)}s`
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

export type FileType = 'video' | 'audio' | 'document' | 'image' | 'program' | 'archive' | 'other'
export const TYPE_ORDER: ('all' | FileType)[] = ['all', 'video', 'audio', 'document', 'image', 'program', 'archive', 'other']
export function typeLabel(k: 'all' | FileType): string {
  const KEYS: Record<'all' | FileType, 'type.all' | 'type.video' | 'type.audio' | 'type.document' | 'type.image' | 'type.program' | 'type.archive' | 'type.other'> = {
    all: 'type.all',
    video: 'type.video',
    audio: 'type.audio',
    document: 'type.document',
    image: 'type.image',
    program: 'type.program',
    archive: 'type.archive',
    other: 'type.other',
  }
  return t(KEYS[k])
}

const VIDEO = ['mp4', 'mkv', 'avi', 'mov', 'wmv', 'flv', 'webm', 'm4v', 'mpg', 'mpeg', 'ts', 'm3u8', '3gp', 'rmvb', 'vob']
const AUDIO = ['mp3', 'wav', 'flac', 'aac', 'ogg', 'wma', 'm4a', 'opus', 'ape', 'mid']
const DOCUMENT = ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'txt', 'md', 'epub', 'mobi', 'csv', 'rtf', 'odt']
const IMAGE = ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'svg', 'ico', 'tif', 'tiff', 'heic', 'avif', 'raw']
const PROGRAM = ['exe', 'msi', 'msix', 'appx', 'apk', 'dmg', 'pkg', 'deb', 'rpm', 'appimage', 'snap', 'flatpak']
const ARCHIVE = ['zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'zst', 'iso', 'cab', 'lz', 'lzma']

export function fileType(fileName: string, url = ''): FileType {
  const name = fileName || url.split('?')[0].split('/').pop() || ''
  const ext = name.includes('.') ? name.split('.').pop()!.toLowerCase() : ''
  if (VIDEO.includes(ext) || /m3u8|magnet:/.test(url)) return 'video'
  if (AUDIO.includes(ext)) return 'audio'
  if (DOCUMENT.includes(ext)) return 'document'
  if (IMAGE.includes(ext)) return 'image'
  if (PROGRAM.includes(ext)) return 'program'
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

/** 队列显示名：内置队列（main/later）本地化，命名队列用存量名——
 *  所有队列名显示点（侧边栏/详情面板/右键菜单/选择器/附加信息列）的单一事实源。 */
export function queueDisplayName(q: { queueId: string; name: string }): string {
  if (q.queueId === 'main') return t('sidebar.queueMain')
  if (q.queueId === 'later') return t('sidebar.queueLater')
  return q.name
}
