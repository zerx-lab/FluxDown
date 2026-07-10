/**
 * 文件/资源类型 → 内联 SVG 图标（lucide 风格线性图标，stroke=currentColor）。
 *
 * popup 任务卡片与资源列表共用；配色由容器上的 `icon-<kind>` class 控制
 * （见 popup/style.css），SVG 本身不携带颜色。
 */

import type { ResourceType } from './resource-types';

export type FileIconKind =
  | 'video'
  | 'audio'
  | 'archive'
  | 'executable'
  | 'disc'
  | 'document'
  | 'image'
  | 'mobile'
  | 'torrent'
  | 'subtitle'
  | 'file';

/** 扩展名 → 图标 kind（与原 emoji 映射同一套规则）。 */
const EXT_KINDS: Array<[RegExp, FileIconKind]> = [
  [/\.(mp4|mkv|avi|mov|wmv|flv|webm|m4v|ts)$/i, 'video'],
  [/\.(mp3|flac|wav|aac|ogg|m4a|wma)$/i, 'audio'],
  [/\.(zip|rar|7z|tar|gz|bz2|xz|tgz)$/i, 'archive'],
  [/\.(exe|msi|dmg|deb|rpm|appimage)$/i, 'executable'],
  [/\.(iso|img)$/i, 'disc'],
  [/\.(pdf|docx?|rtf|xlsx?|csv|pptx?|txt|md)$/i, 'document'],
  [/\.(png|jpe?g|gif|webp|bmp|svg|heic)$/i, 'image'],
  [/\.(apk|ipa)$/i, 'mobile'],
  [/\.torrent$/i, 'torrent'],
  [/\.(srt|vtt|ass|ssa)$/i, 'subtitle'],
];

export function fileIconKind(name: string): FileIconKind {
  for (const [re, kind] of EXT_KINDS) {
    if (re.test(name)) return kind;
  }
  return 'file';
}

/** 嗅探资源类型 → 图标 kind（stream 视作视频，magnet 与 torrent 同图标）。 */
const RESOURCE_KINDS: Record<ResourceType, FileIconKind> = {
  video: 'video',
  audio: 'audio',
  document: 'document',
  archive: 'archive',
  image: 'image',
  executable: 'executable',
  torrent: 'torrent',
  stream: 'video',
  subtitle: 'subtitle',
  magnet: 'torrent',
  other: 'file',
};

export function resourceIconKind(type: ResourceType): FileIconKind {
  return RESOURCE_KINDS[type] ?? 'file';
}

/** lucide 图标 path 数据（24x24 viewBox）。 */
const ICON_PATHS: Record<FileIconKind, string> = {
  video: '<path d="m22 8-6 4 6 4V8Z"/><rect x="2" y="6" width="14" height="12" rx="2"/>',
  audio:
    '<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>',
  archive:
    '<rect x="2" y="3" width="20" height="5" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"/><path d="M10 12h4"/>',
  executable:
    '<polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/>',
  disc: '<circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="2"/>',
  document:
    '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/>',
  image:
    '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/>',
  mobile:
    '<rect x="5" y="2" width="14" height="20" rx="2"/><path d="M12 18h.01"/>',
  torrent:
    '<path d="m6 15-4-4 6.75-6.77a7.79 7.79 0 0 1 11 11L13 22l-4-4 6.39-6.36a2.14 2.14 0 1 0-3-3L6 15"/><path d="m5 8 4 4"/><path d="m12 15 4 4"/>',
  subtitle:
    '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M7 15h4"/><path d="M15 15h2"/><path d="M7 11h2"/><path d="M13 11h4"/>',
  file: '<path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z"/><polyline points="14 2 14 8 20 8"/>',
};

/** 下载完成勾选图标（lucide check-circle）。 */
export const ICON_CHECK_CIRCLE =
  '<svg xmlns="http://www.w3.org/2000/svg" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.27"/></svg>';

export function fileIconSvg(kind: FileIconKind, size = 15): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${ICON_PATHS[kind]}</svg>`;
}
