/**
 * 插件设置管理模块
 */

/**
 * 拦截模式：
 * - 'extension': 仅按文件扩展名拦截（旧行为）
 * - 'smart': 智能模式 — 结合扩展名 + MIME 类型 + 文件名 + 文件大小综合判断
 * - 'all': 拦截所有下载（除排除域名外）
 */
export type InterceptMode = 'extension' | 'smart' | 'all';

export interface FluxDownSettings {
  /** 是否启用下载拦截 */
  enabled: boolean;
  /** 拦截模式 */
  interceptMode: InterceptMode;
  /** 最小文件大小（字节），小于此值的文件不拦截 */
  minFileSize: number;
  /** 拦截的文件扩展名列表（为空则拦截所有） */
  interceptExtensions: string[];
  /** 拦截的 MIME 类型前缀列表（smart 模式下生效） */
  interceptMimeTypes: string[];
  /** 排除的域名列表 */
  excludeDomains: string[];

  // === 资源嗅探 & 页面内 UI 设置 ===

  /** 是否启用资源嗅探（检测页面中的可下载资源） */
  resourceSniffing: boolean;
  /** 是否在视频元素上显示浮动下载按钮 */
  showFloatingButton: boolean;
  /** 是否显示底部资源面板 */
  showResourcePanel: boolean;
  /** 是否嗅探图片资源（默认关闭，开启后显示 >100KB 的图片） */
  sniffImages: boolean;
}

const DEFAULT_SETTINGS: FluxDownSettings = {
  enabled: true,
  interceptMode: 'smart',
  minFileSize: 0, // 不限
  interceptExtensions: [
    // 压缩文件
    '.zip', '.rar', '.7z', '.tar', '.gz', '.bz2', '.xz',
    // 安装程序
    '.exe', '.msi', '.dmg', '.deb', '.rpm', '.appimage',
    // 磁盘镜像
    '.iso', '.img',
    // 视频
    '.mp4', '.mkv', '.avi', '.mov', '.wmv', '.flv', '.webm',
    // 音频
    '.mp3', '.flac', '.wav', '.aac', '.ogg',
    // 文档
    '.pdf', '.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx',
    // 其他大文件
    '.bin', '.apk', '.ipa', '.torrent',
  ],
  interceptMimeTypes: [
    // 通用二进制/下载
    'application/octet-stream',
    'application/x-download',
    'application/force-download',
    // 压缩
    'application/zip',
    'application/x-rar-compressed',
    'application/x-7z-compressed',
    'application/gzip',
    'application/x-tar',
    'application/x-bzip2',
    'application/x-xz',
    // 安装程序
    'application/x-msdownload',
    'application/x-msi',
    'application/x-apple-diskimage',
    'application/vnd.debian.binary-package',
    // 磁盘镜像
    'application/x-iso9660-image',
    'application/x-raw-disk-image',
    // 视频
    'video/',
    // 音频
    'audio/',
    // 文档
    'application/pdf',
    'application/msword',
    'application/vnd.openxmlformats-officedocument',
    'application/vnd.ms-excel',
    'application/vnd.ms-powerpoint',
    // APK
    'application/vnd.android.package-archive',
    // torrent
    'application/x-bittorrent',
  ],
  excludeDomains: [],

  // 资源嗅探 & 页面内 UI
  resourceSniffing: true,
  showFloatingButton: true,
  showResourcePanel: true,
  sniffImages: false,
};

/**
 * 加载设置
 */
export async function loadSettings(): Promise<FluxDownSettings> {
  const result = await chrome.storage.sync.get('settings') ?? {};
  if (result.settings) {
    return { ...DEFAULT_SETTINGS, ...result.settings };
  }
  return { ...DEFAULT_SETTINGS };
}

/**
 * 保存设置
 */
export async function saveSettings(settings: Partial<FluxDownSettings>): Promise<void> {
  const current = await loadSettings();
  const merged = { ...current, ...settings };
  await chrome.storage.sync.set({ settings: merged });
}

/**
 * 重置设置
 */
export async function resetSettings(): Promise<void> {
  await chrome.storage.sync.set({ settings: DEFAULT_SETTINGS });
}

/**
 * 下载项的额外信息，用于综合判断
 */
export interface DownloadItemInfo {
  url: string;
  fileSize?: number;
  mime?: string;
  filename?: string;
}

/**
 * 判断是否应该拦截下载
 */
export function shouldIntercept(
  item: DownloadItemInfo,
  settings: FluxDownSettings,
): boolean {
  if (!settings.enabled) return false;

  const { url, fileSize, mime, filename } = item;

  // 检查域名排除
  try {
    const hostname = new URL(url).hostname;
    if (settings.excludeDomains.some((d) => hostname.includes(d))) {
      return false;
    }
  } catch {
    // URL 解析失败，不拦截
    return false;
  }

  // 检查文件大小下限（仅当文件大小已知时）
  if (fileSize !== undefined && fileSize > 0 && fileSize < settings.minFileSize) {
    return false;
  }

  // === 按模式判断 ===

  if (settings.interceptMode === 'all') {
    // "拦截所有"模式：只要过了域名和大小过滤就拦截
    return true;
  }

  if (settings.interceptMode === 'extension') {
    // "仅扩展名"模式：只看 URL 路径或 filename 的扩展名
    return matchByExtension(url, filename, settings.interceptExtensions);
  }

  // === smart 模式（默认）===
  // 1. 先看扩展名匹配（URL 路径或 filename）
  if (matchByExtension(url, filename, settings.interceptExtensions)) {
    return true;
  }

  // 2. 再看 MIME 类型匹配
  if (mime && matchByMime(mime, settings.interceptMimeTypes)) {
    return true;
  }

  // 3. 如果文件大小已知且超过阈值，但 MIME 未知，也拦截
  //    （很多动态下载链接没有扩展名，MIME 可能是 application/octet-stream 或空）
  if (fileSize !== undefined && fileSize >= settings.minFileSize) {
    // 排除明显不需要拦截的 MIME（如网页、脚本、样式表）
    if (mime && isWebResourceMime(mime)) {
      return false;
    }
    return true;
  }

  return false;
}

/**
 * 通过扩展名匹配（检查 URL 路径和 filename）
 */
function matchByExtension(url: string, filename: string | undefined, extensions: string[]): boolean {
  if (extensions.length === 0) return false;

  // 从 filename 提取扩展名（优先，因为 Content-Disposition 给的文件名更准确）
  if (filename) {
    const lowerFilename = filename.toLowerCase();
    if (extensions.some((ext) => lowerFilename.endsWith(ext))) {
      return true;
    }
  }

  // 从 URL 路径提取扩展名
  try {
    const urlPath = new URL(url).pathname.toLowerCase();
    // 去掉查询参数干扰，只看路径部分
    if (extensions.some((ext) => urlPath.endsWith(ext))) {
      return true;
    }
  } catch {
    // ignore
  }

  return false;
}

/**
 * 通过 MIME 类型匹配
 * 支持前缀匹配，如 'video/' 可匹配 'video/mp4'
 */
function matchByMime(mime: string, mimeTypes: string[]): boolean {
  const lowerMime = mime.toLowerCase();
  return mimeTypes.some((pattern) => {
    const lowerPattern = pattern.toLowerCase();
    // 前缀匹配（如 'video/' 匹配 'video/mp4'）
    if (lowerPattern.endsWith('/')) {
      return lowerMime.startsWith(lowerPattern);
    }
    return lowerMime === lowerPattern;
  });
}

/**
 * 判断是否为网页资源类型的 MIME（不应该拦截的类型）
 */
function isWebResourceMime(mime: string): boolean {
  const lowerMime = mime.toLowerCase();
  const webMimes = [
    'text/html',
    'text/css',
    'text/javascript',
    'application/javascript',
    'application/json',
    'application/xml',
    'text/xml',
    'image/svg+xml',
    'text/plain',
    // 常见小图片不需要拦截
    'image/png',
    'image/jpeg',
    'image/gif',
    'image/webp',
    'image/x-icon',
    'image/bmp',
  ];
  return webMimes.some((wm) => lowerMime === wm);
}

export { DEFAULT_SETTINGS };
