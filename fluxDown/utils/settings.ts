/**
 * 插件设置管理模块
 */

import { browser } from "wxt/browser";
import { normalizeDomainList } from "./domain-exclusion";
import {
  DEFAULT_SITE_PROFILE,
  loadSiteProfiles,
  profileFor,
  saveSiteProfiles,
} from "./site-profiles";
import type { SiteProfile } from "./site-profiles";
import { RemoteSettingsStore } from "./remote-settings";
import type { RemoteSettings } from "./remote-settings";
import type { RemotePingResult, RemoteServerConfig } from "./remote-server";
import { SNIFF_MAX_RESOURCES_DEFAULT, normalizeSniffMaxResources } from "./sniff-limit";

/**
 * 拦截模式：
 * - 'smart': 智能模式 — 结合扩展名 + MIME 类型 + 文件名 + 文件大小综合判断
 * - 'all': 拦截所有下载（除站点配置为「不接管」的站点外）
 */
export type InterceptMode = "smart" | "all";

/**
 * 远程下载源投递模式：
 * - 'off': 仅走桌面 NMH 通道（默认，行为与远程功能上线前完全一致）
 * - 'fallback': 桌面优先，NMH 不可达时投递远程 HTTP 下载源
 * - 'always': 仅走远程 HTTP 下载源，不再尝试连接桌面 App
 */
export type RemoteMode = "off" | "fallback" | "always";

/** 按住后点击链接跳过本次接管（交给浏览器下载）的修饰键；"off" = 关闭。 */
export type BypassKey = "alt" | "shift" | "off";

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
  /**
   * 排除的文件扩展名列表（含点，小写，如 ".torrent"/".crx"）。命中即不
   * 拦截，即便同时命中 customExtensions/BUILTIN_EXTENSIONS 或 smart 模式
   * 的 MIME/大小正向规则——优先级高于所有正向匹配（#177）。
   */
  excludeExtensions: string[];
  /**
   * 是否接管页面内 magnet: 链接点击（阻止浏览器交给系统默认磁力
   * 处理程序，改由 FluxDown 下载）。关闭后点击磁力链接走系统默认
   * 处理程序（如 qBittorrent）；资源面板仍会展示探测到的磁力资源，
   * 供手动选择用 FluxDown 下载。默认开启。
   */
  interceptMagnet: boolean;
  /**
   * IDM 式「按住修饰键点击跳过接管」：按住期间发起的请求（及其重定向）
   * 产生的下载一律交给浏览器，magnet 点击也交还系统处理程序。默认 Alt（macOS 为 Option）。
   */
  bypassKey: BypassKey;

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
  /** 是否嗅探图片资源（默认关闭，开启后显示 >100KB 的图片） */
  sniffImages: boolean;
  /** 是否显示页内悬浮球与资源面板 */
  showPageDot: boolean;
  /** 视频浮钮触发方式：hover = 悬停在播放器上时显示；always = 正在播放的视频常显 */
  floatButtonTrigger: "hover" | "always";
  /** 视频浮钮贴在播放元素的哪个角 */
  floatButtonPosition: "top-right" | "top-left" | "bottom-right" | "bottom-left";
  /** 是否在资源列表中显示低置信度项 */
  showLowConfidence: boolean;
  /**
   * 单页面 DOM 扫描最多上报的资源数（#288：长列表页无节制扫描会卡顿）。fetch / XHR 嗅探到的媒体不计入。
   * 取值经 `normalizeSniffMaxResources`（utils/sniff-limit.ts）规整到 1…99999。
   */
  sniffMaxResources: number;

  // === 失败回退与询问 ===

  /** 桌面端业务拒绝 / 创建失败时回退浏览器下载 */
  fallbackOnReject: boolean;
  /** 用户在桌面端取消（拒绝）确认框后回退浏览器下载（需桌面端 v2 且非远程模式） */
  fallbackOnCancel: boolean;
  /** 同一站点 7 天内被取消 ≥ 3 次后询问是否不再接管该站点 */
  askExcludeAfterCancels: boolean;
  /** 是否创建右键菜单 */
  contextMenuEnabled: boolean;

  // === 嗅探规则同步 ===

  /** 是否从桌面端同步官方 / 社区规则包 */
  ruleSyncEnabled: boolean;
  /** 是否启用社区规则包 */
  communityRulesEnabled: boolean;

  // === 远程下载源设置 ===

  /** 远程下载源投递模式 */
  remoteMode: RemoteMode;
  /** 远程 fluxdown_server 地址，如 http://192.168.1.10:17800（保存时会去除尾部斜杠） */
  remoteUrl: string;
  /** 远程 fluxdown_server 鉴权 token */
  remoteToken: string;
  /**
   * 本机已通过连接验证的 remoteUrl/remoteToken 的 SHA-256 指纹。
   * 仅验证通过后 UI 才允许选择 fallback/always 模式；
   * 读取时指纹不匹配即视为未验证，空串表示未验证。
   */
  remoteVerified: string;

  // === 自定义协议 ===

  /**
   * 启用 fluxdown:// 自定义协议下载（仅桌面端生效；Android 忽略此开关）。
   * 开启后拦截下载时不再投递 NMH/远端，而是导航到
   * fluxdown://download?url=... 协议 URL，由系统唤起已注册该协议的
   * FluxDown 客户端（Windows 注册表 / Linux x-scheme-handler / macOS
   * CFBundleURLTypes 协议处理器）。
   * 限制：不经 NMH，Cookie/Headers/method/body 无法携带；
   * 音视频分轨（audioUrl）请求会回退浏览器下载。
   * 桌面端 NMH 链路功能是协议模式的严格超集，建议仅在 NMH 不可用的
   * 受限环境开启。Android 版浏览器始终走 Intent 唤起
   * （background/android-handoff.ts，可携带 Cookie / 来源页 / 请求头），不读取此开关。默认关闭。
   */
  enableFluxdownProtocol: boolean;
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
  excludeExtensions: [],
  interceptMagnet: true,
  bypassKey: "alt",

  // 任务发送通知（本地默认关闭，按需开启；远程默认开启）
  notifyLocalTask: false,
  notifyRemoteTask: true,

  // 资源嗅探 & 页面内 UI
  resourceSniffing: true,
  showFloatingButton: true,
  showPageDot: true,
  floatButtonTrigger: "hover",
  floatButtonPosition: "top-right",
  showLowConfidence: false,
  sniffImages: false,
  sniffMaxResources: SNIFF_MAX_RESOURCES_DEFAULT,

  // 失败回退与询问
  fallbackOnReject: true,
  fallbackOnCancel: false,
  askExcludeAfterCancels: true,
  contextMenuEnabled: true,

  // 嗅探规则同步
  ruleSyncEnabled: true,
  communityRulesEnabled: true,

  // 远程下载源
  remoteMode: "off",
  remoteUrl: "",
  remoteToken: "",
  remoteVerified: "",

  // 自定义协议
  enableFluxdownProtocol: false,
};

const remoteSettingsStore = new RemoteSettingsStore(browser.storage.local);

function pickRemote(s: FluxDownSettings): RemoteSettings {
  return {
    remoteMode: s.remoteMode,
    remoteUrl: s.remoteUrl,
    remoteToken: s.remoteToken,
    remoteVerified: s.remoteVerified,
  };
}

function omitRemote(s: FluxDownSettings): Partial<FluxDownSettings> {
  const {
    remoteMode: _m,
    remoteUrl: _u,
    remoteToken: _t,
    remoteVerified: _v,
    ...rest
  } = s;
  return rest;
}

function hasRemoteFields(s: object): boolean {
  return (
    "remoteMode" in s ||
    "remoteUrl" in s ||
    "remoteToken" in s ||
    "remoteVerified" in s
  );
}

/** v1 把「悬浮球可见」单独存在 local，v2 并入 settings.showPageDot。 */
const LEGACY_DOT_VISIBLE_KEY = "fluxdown_dot_visible";

type StoredSettings = Partial<Omit<FluxDownSettings, "remoteVerified">> & {
  remoteVerified?: string | boolean;
  /** v1 遗留字段 */
  excludeDomains?: unknown;
  showResourcePanel?: unknown;
};

let legacyMigration: Promise<boolean> | null = null;

/**
 * v1 → v2 一次性迁移（依据旧字段是否存在，幂等）：
 * 1. sync 的 `excludeDomains` 逐项写成「不接管」站点配置，已有的站点不覆盖；
 * 2. local 的 `fluxdown_dot_visible === false` 转为 `showPageDot = false` 并删除该键；
 * 3. 去掉旧字段后回写 sync。
 */
async function migrateLegacyFields(
  stored: StoredSettings | undefined,
): Promise<boolean> {
  const hasLegacy =
    !!stored && ("excludeDomains" in stored || "showResourcePanel" in stored);
  const dotRaw =
    !stored || hasLegacy
      ? (await browser.storage.local.get(LEGACY_DOT_VISIBLE_KEY))?.[
          LEGACY_DOT_VISIBLE_KEY
        ]
      : undefined;
  if (!hasLegacy && dotRaw === undefined) return false;

  const legacyDomains = Array.isArray(stored?.excludeDomains)
    ? normalizeDomainList(
        stored.excludeDomains.filter((d): d is string => typeof d === "string"),
      )
    : [];
  if (legacyDomains.length > 0) {
    const profiles = await loadSiteProfiles();
    const now = Date.now();
    for (const domain of legacyDomains) {
      if (domain in profiles) continue;
      profiles[domain] = { ...DEFAULT_SITE_PROFILE, intercept: "never", updatedAt: now };
    }
    await saveSiteProfiles(profiles);
  }

  // 回写前重读最新 sync 值，只删旧字段（不用读取时的快照整份覆盖），
  // 避免并发的其它上下文迁移或用户刚保存的设置被回滚。
  const latest = (await browser.storage.sync.get("settings"))?.settings as
    | StoredSettings
    | undefined;
  const latestHasLegacy =
    !!latest && ("excludeDomains" in latest || "showResourcePanel" in latest);
  if (latestHasLegacy || dotRaw === false) {
    const { excludeDomains: _d, showResourcePanel: _p, ...rest } = latest ?? {};
    const next: StoredSettings = { ...rest };
    if (dotRaw === false) next.showPageDot = false;
    await browser.storage.sync.set({
      settings: omitRemote({ ...DEFAULT_SETTINGS, ...next } as FluxDownSettings),
    });
  }
  if (dotRaw !== undefined) {
    await browser.storage.local.remove(LEGACY_DOT_VISIBLE_KEY);
  }
  return true;
}

/**
 * 加载设置
 */
export async function loadSettings(): Promise<FluxDownSettings> {
  let syncResult = await browser.storage.sync.get("settings");
  let stored = syncResult?.settings as StoredSettings | undefined;
  const hasLegacyRemote = stored && hasRemoteFields(stored);
  const remote = await remoteSettingsStore.load(
    hasLegacyRemote ? stored : undefined,
  );

  // 迁移：本机尚无 local 副本时只采用 sync 中的地址、模式与密钥，不采用验证结论。
  // 无论是否采用，都从 sync 删除远程字段，此后以 local 为准。
  if (hasLegacyRemote) {
    await browser.storage.sync.set({
      settings: omitRemote({ ...DEFAULT_SETTINGS, ...stored, remoteVerified: "" }),
    });
  }

  // v1 → v2 迁移（并发的 loadSettings 共用同一次迁移）。
  legacyMigration ??= migrateLegacyFields(stored).finally(() => {
    legacyMigration = null;
  });
  if (await legacyMigration) {
    syncResult = await browser.storage.sync.get("settings");
    stored = syncResult?.settings as StoredSettings | undefined;
  }

  return { ...migrateSettings(stored), ...remote };
}

/**
 * 把任意来源（备份导入、旧版本落盘）的设置对象规整为当前版本：
 * 合并默认值、去掉 v1 遗留字段、归一化已移除的拦截模式。纯函数。
 */
export function migrateSettings(raw: unknown): FluxDownSettings {
  const source =
    raw && typeof raw === "object" && !Array.isArray(raw)
      ? (raw as StoredSettings)
      : {};
  const { excludeDomains: _d, showResourcePanel: _p, ...current } = source;
  const merged = { ...DEFAULT_SETTINGS, ...current } as FluxDownSettings;
  if ((merged.interceptMode as string) === "extension") {
    merged.interceptMode = "smart";
  }
  merged.sniffMaxResources = normalizeSniffMaxResources(merged.sniffMaxResources);
  return merged;
}

/**
 * 按字段归属落盘：远程配置写 storage.local，其余写 storage.sync。
 * `previous` 给出时仅在远程配置确有变化时才写 local。
 */
export async function persistSettings(
  next: FluxDownSettings,
  previous?: FluxDownSettings,
): Promise<void> {
  await browser.storage.sync.set({ settings: omitRemote(next) });
  const nextRemote = pickRemote(next);
  if (
    !previous ||
    JSON.stringify(pickRemote(previous)) !== JSON.stringify(nextRemote)
  ) {
    await remoteSettingsStore.persist(nextRemote);
  }
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
  // 远程连接信息变更后必须重新验证，普通设置写入不能给新连接授予验证指纹。
  const remoteChanged =
    merged.remoteUrl !== current.remoteUrl ||
    merged.remoteToken !== current.remoteToken;
  if (remoteChanged) {
    merged.remoteVerified = "";
  }
  await persistSettings(merged, current);
}

/** 本机测试当前连接，只有实际成功且凭据未变时才落盘验证指纹。 */
export async function verifyRemoteSettings(
  config: RemoteServerConfig,
): Promise<RemotePingResult> {
  return remoteSettingsStore.verify(config);
}

/** 投递需要远程时自动补验；失败后节流，避免每次捕获都等待远端超时。 */
export async function ensureRemoteSettingsVerified(
  settings: FluxDownSettings,
): Promise<FluxDownSettings> {
  const remote = await remoteSettingsStore.ensureVerified(pickRemote(settings));
  return { ...settings, ...remote };
}

/**
 * 重置设置
 */
export async function resetSettings(): Promise<void> {
  await persistSettings({ ...DEFAULT_SETTINGS });
}

/**
 * 下载项的额外信息，用于综合判断
 */
export interface DownloadItemInfo {
  url: string;
  fileSize?: number;
  mime?: string;
  filename?: string;
  /**
   * 下载请求的来源页面 URL（对应 downloads.DownloadItem.referrer / webRequest 的
   * originUrl/documentUrl）。用于识别由 FluxDown 自身远程 Web UI 发起的下载，
   * 避免拦截后再次投递回同一远端形成死循环（#658）。
   */
  referrerUrl?: string;
}

/**
 * 判断是否应该拦截下载
 */
export function shouldIntercept(
  item: DownloadItemInfo,
  settings: FluxDownSettings,
  profiles: Record<string, SiteProfile> = {},
): boolean {
  if (!settings.enabled) return false;

  const { url, fileSize, mime, filename, referrerUrl } = item;

  // #658：FluxDown 远程 Web UI（settings.remoteUrl 指向的 fluxdown_server）上发起
  // 的"下载已完成文件到本地"请求，不应被扩展再次拦截并回投同一远端，
  // 否则会形成持续创建任务的死循环。命中依据：下载来源页（referrer）或下载
  // URL 本身的 origin 与已配置的远程服务地址一致；或下载 URL 的路径就是
  // fluxdown_server 的文件导出端点（覆盖经反代/不同主机名访问 Web UI 的情况）。
  if (isFluxDownServerDownload(url, referrerUrl, settings.remoteUrl)) {
    return false;
  }

  // 站点配置：下载 URL 主机或来源页主机任一为 never 即放行（「不接管当前站点」
  // 对托管在 CDN/对象存储上的下载同样有效）；任一为 always 则跳过 smart 判定。
  let forceIntercept = false;
  let urlHost: string;
  try {
    urlHost = new URL(url).hostname;
  } catch {
    // URL 解析失败，不拦截
    return false;
  }
  const hosts = [urlHost];
  if (referrerUrl) {
    try {
      hosts.push(new URL(referrerUrl).hostname);
    } catch {
      // referrer 解析失败：忽略
    }
  }
  for (const host of hosts) {
    const { intercept } = profileFor(profiles, host).profile;
    if (intercept === "never") return false;
    if (intercept === "always") forceIntercept = true;
  }

  // 检查排除扩展名（#177）：优先级高于 customExtensions/BUILTIN_EXTENSIONS
  // 及 smart 模式下的 MIME/大小正向匹配——命中即不拦截。
  if (
    settings.excludeExtensions.length > 0 &&
    matchByExtension(url, filename, settings.excludeExtensions)
  ) {
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

  if (forceIntercept || settings.interceptMode === "all") {
    // 「拦截所有」模式 / 站点强制接管：只要过了大小与排除扩展名过滤就拦截
    return true;
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
 * 点击链接时（响应头到达前）是否像一次会被接管的下载：用于提前预热 NMH，让 App 冷启动与
 * 对方服务器首字节等待并行。只认强信号——`<a download>` 或 URL 扩展名命中生效拦截列表；
 * 预热会拉起 App，普通网页链接绝不能触发。站点 never、排除扩展名、自身远程 Web UI 一律否。
 */
export function isLikelyDownloadClick(
  url: string,
  hasDownloadAttr: boolean,
  pageUrl: string | undefined,
  settings: FluxDownSettings,
  profiles: Record<string, SiteProfile> = {},
): boolean {
  if (!settings.enabled) return false;
  if (isFluxDownServerDownload(url, pageUrl, settings.remoteUrl)) return false;
  for (const target of [url, pageUrl]) {
    if (!target) continue;
    let host: string;
    try {
      host = new URL(target).hostname;
    } catch {
      if (target === url) return false;
      continue;
    }
    if (profileFor(profiles, host).profile.intercept === "never") return false;
  }
  if (
    settings.excludeExtensions.length > 0 &&
    matchByExtension(url, undefined, settings.excludeExtensions)
  ) {
    return false;
  }
  return hasDownloadAttr || matchByExtension(url, undefined, effectiveExtensions(settings));
}

/**
 * fluxdown headless 服务（native/agent server_mode）给自身 Web UI 提供的
 * 文件/导出下载端点；旧的 /api/v1 路由（冻结的 native/server）一并保留兼容。
 */
const FLUXDOWN_EXPORT_PATH_RE =
  /^\/api\/(?:web\/(?:files\/tasks\/[^/]+|exports\/[^/]+|logs\/export)|v1\/(?:tasks\/[^/]+\/file|logs\/export))$/;

/**
 * 判断一次浏览器下载是否由 FluxDown 自身的远程 Web UI 发起（#658）。
 * 命中任一条件即为真：
 * - 下载 URL 路径是 fluxdown_server 的文件导出端点；
 * - 已配置 remoteUrl，且 referrer 或下载 URL 的 origin 与其一致。
 */
export function isFluxDownServerDownload(
  url: string,
  referrerUrl: string | undefined,
  remoteUrl: string,
): boolean {
  let parsedUrl: URL | null = null;
  try {
    parsedUrl = new URL(url);
  } catch {
    parsedUrl = null;
  }
  if (parsedUrl && FLUXDOWN_EXPORT_PATH_RE.test(parsedUrl.pathname)) {
    return true;
  }
  if (!remoteUrl) return false;
  let fluxDownOrigin: string;
  try {
    fluxDownOrigin = new URL(remoteUrl).origin;
  } catch {
    return false;
  }
  if (parsedUrl && parsedUrl.origin === fluxDownOrigin) return true;
  if (referrerUrl) {
    try {
      if (new URL(referrerUrl).origin === fluxDownOrigin) return true;
    } catch {
      // referrer 解析失败：忽略
    }
  }
  return false;
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
