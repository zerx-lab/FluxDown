/**
 * 插件设置管理模块
 */

import { browser } from "wxt/browser";

/**
 * 拦截模式：
 * - 'extension': 仅按文件扩展名拦截（旧行为）
 * - 'smart': 智能模式 — 结合扩展名 + MIME 类型 + 文件名 + 文件大小综合判断
 * - 'all': 拦截所有下载（除排除域名外）
 */
export type InterceptMode = "extension" | "smart" | "all";

/**
 * 远程下载源投递模式：
 * - 'off': 仅走桌面 NMH 通道（默认，行为与远程功能上线前完全一致）
 * - 'fallback': 桌面优先，NMH 不可达时投递远程 HTTP 下载源
 * - 'always': 仅走远程 HTTP 下载源，不再尝试连接桌面 App
 */
export type RemoteMode = "off" | "fallback" | "always";

export interface FluxDownSettings {
  /** 是否启用下载拦截 */
  enabled: boolean;
  /** 拦截模式 */
  interceptMode: InterceptMode;
  /** 最小文件大小（字节），小于此值的文件不拦截 */
  minFileSize: number;
  /** 拦截的 MIME 类型前缀列表（smart 模式下生效，可在配置页增删/恢复默认） */
  interceptMimeTypes: string[];
  /** 用户自定义追加的可拦截扩展名（含点，小写，如 ".epub"；与内置列表合并生效） */
  customExtensions: string[];
  /** 排除的域名列表 */
  excludeDomains: string[];

  // === 任务发送通知 ===

  /** 发送到本地桌面 App 的任务通知（创建成功/失败） */
  notifyLocalTask: boolean;
  /** 发送到远程服务器的任务通知（创建成功/失败） */
  notifyRemoteTask: boolean;

  // === 资源嗅探 & 页面内 UI 设置 ===

  /** 是否启用资源嗅探（检测页面中的可下载资源） */
  resourceSniffing: boolean;
  /** 是否在视频元素上显示浮动下载按钮 */
  showFloatingButton: boolean;
  /** 是否显示底部资源面板 */
  showResourcePanel: boolean;
  /** 是否嗅探图片资源（默认关闭，开启后显示 >100KB 的图片） */
  sniffImages: boolean;

  // === 远程下载源设置 ===

  /** 远程下载源投递模式 */
  remoteMode: RemoteMode;
  /** 远程 fluxdown_server 地址，如 http://192.168.1.10:17800（保存时会去除尾部斜杠） */
  remoteUrl: string;
  /** 远程 fluxdown_server 鉴权 token */
  remoteToken: string;
  /**
   * 远程配置是否已通过「测试连接」验证（含 token 鉴权校验）。
   * 仅验证通过后 UI 才允许选择 fallback/always 模式；
   * remoteUrl/remoteToken 任一变更时自动复位为 false。
   */
  remoteVerified: boolean;
}

/**
 * 内置的可拦截文件扩展名列表（不可删除的基线）。
 * smart 模式作为已知下载类型的正向匹配；extension 模式与
 * `settings.customExtensions` 合并后为唯一来源（见 effectiveExtensions）。
 */
export const BUILTIN_EXTENSIONS: string[] = [
  // 压缩文件
  ".zip",
  ".rar",
  ".7z",
  ".tar",
  ".gz",
  ".bz2",
  ".xz",
  // 安装程序
  ".exe",
  ".msi",
  ".dmg",
  ".deb",
  ".rpm",
  ".appimage",
  // 磁盘镜像
  ".iso",
  ".img",
  // 视频
  ".mp4",
  ".mkv",
  ".avi",
  ".mov",
  ".wmv",
  ".flv",
  ".webm",
  // 音频
  ".mp3",
  ".flac",
  ".wav",
  ".aac",
  ".ogg",
  // 文档
  ".pdf",
  ".doc",
  ".docx",
  ".xls",
  ".xlsx",
  ".ppt",
  ".pptx",
  // 其他大文件
  ".bin",
  ".apk",
  ".ipa",
  ".torrent",
];

/**
 * 归一化用户输入的自定义扩展名：去空白、转小写、补前导点。
 * 非法输入（空 / 含非法字符 / 过长）返回 null。
 */
export function normalizeExtension(input: string): string | null {
  let s = input.trim().toLowerCase();
  if (!s) return null;
  if (!s.startsWith(".")) s = `.${s}`;
  // 允许多段后缀（如 .tar.zst）；总长限制防误粘贴整个文件名
  return /^\.[a-z0-9][a-z0-9.]{0,15}$/.test(s) && !s.endsWith(".") ? s : null;
}

/** 内置 + 用户自定义合并后的生效扩展名列表（去重，内置在前） */
export function effectiveExtensions(settings: FluxDownSettings): string[] {
  if (!settings.customExtensions?.length) return BUILTIN_EXTENSIONS;
  return [...new Set([...BUILTIN_EXTENSIONS, ...settings.customExtensions])];
}

const DEFAULT_SETTINGS: FluxDownSettings = {
  enabled: true,
  interceptMode: "smart",
  minFileSize: 0, // 不限
  interceptMimeTypes: [
    // 通用二进制/下载
    "application/octet-stream",
    "application/x-download",
    "application/force-download",
    // 压缩
    "application/zip",
    "application/x-rar-compressed",
    "application/x-7z-compressed",
    "application/gzip",
    "application/x-tar",
    "application/x-bzip2",
    "application/x-xz",
    // 安装程序
    "application/x-msdownload",
    "application/x-msi",
    "application/x-apple-diskimage",
    "application/vnd.debian.binary-package",
    // 磁盘镜像
    "application/x-iso9660-image",
    "application/x-raw-disk-image",
    // 视频
    "video/",
    // 音频
    "audio/",
    // 文档
    "application/pdf",
    "application/msword",
    "application/vnd.openxmlformats-officedocument",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    // APK
    "application/vnd.android.package-archive",
    // torrent
    "application/x-bittorrent",
  ],
  customExtensions: [],
  excludeDomains: [],

  // 任务发送通知
  notifyLocalTask: true,
  notifyRemoteTask: true,

  // 资源嗅探 & 页面内 UI
  resourceSniffing: true,
  showFloatingButton: true,
  showResourcePanel: true,
  sniffImages: false,

  // 远程下载源
  remoteMode: "off",
  remoteUrl: "",
  remoteToken: "",
  remoteVerified: false,
};

/**
 * 加载设置
 */
export async function loadSettings(): Promise<FluxDownSettings> {
  const result = (await browser.storage.sync.get("settings")) ?? {};
  if (result.settings) {
    const merged = { ...DEFAULT_SETTINGS, ...result.settings };
    // 迁移：旧版"仅扩展名"模式的设置项已移除，归一化为智能模式，
    // 否则 popup 下拉框（已无该选项）会显示空白、拦截逻辑走废弃分支。
    if ((merged.interceptMode as string) === "extension") {
      merged.interceptMode = "smart";
    }
    return merged;
  }
  return { ...DEFAULT_SETTINGS };
}

/**
 * 保存设置
 */
export async function saveSettings(
  settings: Partial<FluxDownSettings>,
): Promise<void> {
  const current = await loadSettings();
  const merged = { ...current, ...settings };
  // remoteUrl 保存时去除尾部斜杠，避免拼接 `/download` 等路径时出现 `//`。
  if (merged.remoteUrl) {
    merged.remoteUrl = merged.remoteUrl.replace(/\/+$/, "");
  }
  // 远程连接信息变更 → 旧的验证结论失效，除非本次调用显式给出新结论。
  const remoteChanged =
    merged.remoteUrl !== current.remoteUrl ||
    merged.remoteToken !== current.remoteToken;
  if (remoteChanged && !("remoteVerified" in settings)) {
    merged.remoteVerified = false;
  }
  await browser.storage.sync.set({ settings: merged });
}

/**
 * 重置设置
 */
export async function resetSettings(): Promise<void> {
  await browser.storage.sync.set({ settings: DEFAULT_SETTINGS });
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
  if (
    fileSize !== undefined &&
    fileSize > 0 &&
    fileSize < settings.minFileSize
  ) {
    return false;
  }

  // === 按模式判断 ===

  if (settings.interceptMode === "all") {
    // "拦截所有"模式：只要过了域名和大小过滤就拦截
    return true;
  }

  if (settings.interceptMode === "extension") {
    // "仅扩展名"模式：只看 URL 路径或 filename 的扩展名
    return matchByExtension(url, filename, effectiveExtensions(settings));
  }

  // === smart 模式（默认）===
  // 1. 先看扩展名匹配（URL 路径或 filename）
  if (matchByExtension(url, filename, effectiveExtensions(settings))) {
    return true;
  }

  // 2. 再看 MIME 类型匹配
  if (mime && matchByMime(mime, settings.interceptMimeTypes)) {
    return true;
  }

  // 3. 如果文件大小已知且超过阈值，但 MIME 未知，也拦截
  //    （很多动态下载链接没有扩展名，MIME 可能是 application/octet-stream 或空）
  if (fileSize !== undefined && fileSize >= settings.minFileSize) {
    if (mime && isWebResourceMime(mime)) {
      return false;
    }
    return true;
  }

  // 4. 如果 MIME 明确是网页资源类型（HTML/CSS/JS/小图片等），不拦截
  if (mime && isWebResourceMime(mime)) {
    return false;
  }

  // 5. 默认拦截 —— 浏览器既然触发了 downloads API 事件，
  //    说明它已经将该请求识别为"下载"操作。
  //    到这里意味着：扩展名不在列表中、MIME 不在列表中（或为空）、文件大小未知。
  //    典型场景：按钮点击触发的动态下载（URL 无扩展名、MIME 未知/为空）。
  //    同类产品（IDM/FDM）在此场景下也会拦截。
  //    如果用户不想拦截特定站点，可通过排除域名列表处理。
  return true;
}

/**
 * 通过扩展名匹配（检查 filename 和 URL 三策略）
 *
 * 策略顺序：
 * 1. filename — Content-Disposition 给的文件名最准确，优先检查
 * 2. URL pathname 末尾 — 覆盖 /path/file.pdf 场景
 * 3. URL 查询参数值  — 覆盖 /download?file=report.pdf 场景
 * 4. URL pathname 任意段 — 覆盖 /file.pdf/download 场景
 */
function matchByExtension(
  url: string,
  filename: string | undefined,
  extensions: string[],
): boolean {
  if (extensions.length === 0) return false;

  // 策略 1: filename（Content-Disposition 给的文件名，最准确）
  if (filename) {
    const lowerFilename = filename.toLowerCase();
    if (extensions.some((ext) => lowerFilename.endsWith(ext))) {
      return true;
    }
  }

  try {
    const u = new URL(url);
    const pathname = u.pathname.toLowerCase();

    // 策略 2: pathname 末尾（最常见场景）
    if (extensions.some((ext) => pathname.endsWith(ext))) {
      return true;
    }

    // 策略 3: 查询参数值中的扩展名（如 ?file=report.pdf&type=doc）
    for (const val of u.searchParams.values()) {
      const valLower = val.toLowerCase();
      if (extensions.some((ext) => valLower.endsWith(ext))) {
        return true;
      }
    }

    // 策略 4: pathname 任意段（如 /file.pdf/download）
    const segments = pathname.split("/");
    for (const seg of segments) {
      if (seg && extensions.some((ext) => seg.endsWith(ext))) {
        return true;
      }
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
    if (lowerPattern.endsWith("/")) {
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
    "text/html",
    "text/css",
    "text/javascript",
    "application/javascript",
    "application/json",
    "application/xml",
    "text/xml",
    "image/svg+xml",
    "text/plain",
    // 常见小图片不需要拦截
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/x-icon",
    "image/bmp",
  ];
  return webMimes.some((wm) => lowerMime === wm);
}

export { DEFAULT_SETTINGS };
