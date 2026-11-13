/**
 * 下载镜像（dl-cn，回源私有 OSS 按流量计费）的准入判定：只有「像正常客户端」的请求才拿镜像链接，
 * 其余一律交给 GitHub，不消耗镜像流量。
 *
 * 正常客户端下载前都会先读发布清单：官网下载页 / 首页 / 更新日志在浏览器里请求 /api/release
 * 或 /api/changelog，应用内更新（含不带 User-Agent 的旧版客户端）先查 /api/release 再下载。
 * 因此以「同一访客 IP 在 24 小时内读过发布清单」为准入条件；直接批量请求 /api/download 的
 * 脚本拿不到 CDN 链接。常见脚本 / HTTP 库的 User-Agent 即使读过清单也不准入。
 * 浏览器等非更新器请求另须带验证码通行证（needsDownloadPass，见 download-pass.ts）；应用内更新不受影响，
 * 但只能拿当前更新清单里的资产（mirrorAssetAllowed），冒充更新器 UA 也扫不到历史版本。
 *
 * 准入之后还有成本闸（checkMirrorQuota）：每个 IP 24 小时内的镜像文件数 / 字节数配额与全站每日预算，
 * 超额一律回落 GitHub（真人仍能下到，只是慢）。镜像链接绑定访客 IPv4 /24（mirrorNetworkId），
 * 签给谁只有谁的网段能用，转发、贴到论坛或解析站都会被边缘 403。
 *
 * 状态在进程内存（单容器部署），重启后清空：期间的首次下载回落 GitHub，下次读清单后恢复；配额与预算
 * 计数随之归零（重启当天预算放宽，可接受）。
 */

/** 读过发布清单后的准入时长：覆盖「检查到更新后隔几小时才点下载」。 */
const VIEWER_TTL_MS = 24 * 3600 * 1000;
/** 记录上限，超出时淘汰最早写入的条目。 */
const MAX_VIEWERS = 300_000;

const viewers = new Map<string, number>();

/** 发布清单接口调用：记录访客 IP。 */
export function markReleaseViewer(ip: string, now = Date.now()): void {
  if (!ip || ip === "unknown") return;
  // 先删再写，让最近访问的 IP 排到 Map 末尾，淘汰时从头部开始。
  viewers.delete(ip);
  viewers.set(ip, now + VIEWER_TTL_MS);
  while (viewers.size > MAX_VIEWERS) {
    const oldest = viewers.keys().next();
    if (oldest.done) break;
    viewers.delete(oldest.value);
  }
}

function recentViewer(ip: string, now: number): boolean {
  const until = viewers.get(ip);
  if (until === undefined) return false;
  if (until > now) return true;
  viewers.delete(ip);
  return false;
}

const SCRIPT_USER_AGENT =
  /curl|wget|python|aiohttp|httpx|go-http-client|java\/|okhttp|apache-httpclient|libwww|node-fetch|undici|axios|got\b|powershell|scrapy|headless|phantomjs|bot\b|spider|crawler/i;

/** 是否为常见脚本 / HTTP 库 / 爬虫的 User-Agent（空 UA 不算：旧版客户端的更新器不带 UA）。 */
export function isScriptUserAgent(userAgent: string | null): boolean {
  return !!userAgent && SCRIPT_USER_AGENT.test(userAgent);
}

/** 能拿 123 直链的最低客户端版本：此后的更新器 UA 固定为 `fluxdown-<agent|android>/<版本>`。 */
const PAN123_MIN_VERSION = [0, 6, 0] as const;
const PAN123_UPDATER_USER_AGENT = /^fluxdown-(?:agent|android)\/v?(\d+)\.(\d+)\.(\d+)(?:[-+][0-9A-Za-z.+-]*)?$/;

/**
 * 是否为可以拿 123 直链的应用内更新：User-Agent 严格匹配 `fluxdown-agent/<版本>`（桌面 / 服务器）
 * 或 `fluxdown-android/<版本>`（native/agent/src/update/mod.rs::build），且版本核心号 ≥ 0.6.0。
 * 不带 User-Agent 的旧版更新器不再给 123（大陆回落 dl-cn，其余 GitHub）。
 */
export function isPan123Updater(userAgent: string | null): boolean {
  const match = userAgent ? PAN123_UPDATER_USER_AGENT.exec(userAgent) : null;
  if (!match) return false;
  for (let i = 0; i < PAN123_MIN_VERSION.length; i++) {
    const part = Number(match[i + 1]);
    if (part !== PAN123_MIN_VERSION[i]) return part > PAN123_MIN_VERSION[i];
  }
  return true;
}

/** `/api/release` 各渠道当前下发给更新器的资产（`<tag>/<文件名>`）；只有它们能拿 123 直链。 */
const updateAssets = new Map<string, ReadonlySet<string>>();

/** `/api/release` 生成某渠道清单时整体替换该渠道的可更新资产（桌面 / 服务器 / Android）。 */
export function recordUpdateAssets(channel: string, assets: Iterable<{ tag: string; name: string }>): void {
  updateAssets.set(channel, new Set(Array.from(assets, (a) => `${a.tag}/${a.name}`)));
}

/** 该资产是否在某渠道当前的更新清单里：历史版本与非更新组件（扩展 / CLI / iOS）不给 123。 */
export function isUpdateAsset(tag: string, name: string): boolean {
  const key = `${tag}/${name}`;
  for (const assets of updateAssets.values()) {
    if (assets.has(key)) return true;
  }
  return false;
}

/** 同一 IP 24 小时内最多签发的 123 直链数：覆盖同一出口下几台设备各自更新与断点重试。 */
const PAN123_LINKS_PER_IP = 10;
const PAN123_WINDOW_MS = 24 * 3600 * 1000;
const pan123Issued = new Map<string, number[]>();

function recentIssued(ip: string, now: number): number[] {
  const issued = (pan123Issued.get(ip) ?? []).filter((at) => now - at < PAN123_WINDOW_MS);
  if (issued.length) pan123Issued.set(ip, issued);
  else pan123Issued.delete(ip);
  return issued;
}

/** 该 IP 是否还能拿 123 直链（超额回落 dl-cn / GitHub，不报错）。 */
export function hasPan123Quota(ip: string, now = Date.now()): boolean {
  return recentIssued(ip, now).length < PAN123_LINKS_PER_IP;
}

/** 记一次 123 直链签发。 */
export function recordPan123Link(ip: string, now = Date.now()): void {
  const issued = recentIssued(ip, now);
  issued.push(now);
  pan123Issued.delete(ip);
  pan123Issued.set(ip, issued);
  while (pan123Issued.size > MAX_VIEWERS) {
    const oldest = pan123Issued.keys().next();
    if (oldest.done) break;
    pan123Issued.delete(oldest.value);
  }
}

/** FluxDown 应用内更新器的 User-Agent：`<产品>/<版本>`（fluxdown-agent / fluxdown-android，见 native/agent/src/update）。 */
const UPDATER_USER_AGENT = /^fluxdown-[a-z]+\//i;

/**
 * 拿镜像前是否须带验证码通行证：浏览器与其余带 User-Agent 的请求要；FluxDown 更新器与
 * 不带 User-Agent 的旧版更新器不要（它们无法过验证码，自升级不能因此变慢或失败）。
 */
export function needsDownloadPass(userAgent: string | null): boolean {
  return !!userAgent && !UPDATER_USER_AGENT.test(userAgent);
}

/** 该请求能否使用 CDN 镜像；`hasPass` = 带有效验证码通行证或未启用通行证。 */
export function mayUseMirror(ip: string, userAgent: string | null, hasPass: boolean, now = Date.now()): boolean {
  return !isScriptUserAgent(userAgent) && recentViewer(ip, now) && (hasPass || !needsDownloadPass(userAgent));
}

/** 更新器类请求（FluxDown 更新器与不带 UA 的旧版更新器）只能经镜像拿当前更新清单里的资产；浏览器不限（受配额约束）。 */
export function mirrorAssetAllowed(userAgent: string | null, tag: string, name: string): boolean {
  return needsDownloadPass(userAgent) || isUpdateAsset(tag, name);
}

/**
 * 镜像链接绑定的访客网段标识：IPv4 /24 的前三段，各两位十六进制（`114.114.114.x` → `727272`），写进
 * URL 鉴权 A 方式的 uid 段参与签名，ESA 边缘函数按 `ali-cdn-real-ip` 重算比对。IPv6 与非法地址返回 null：
 * ESA 站点未开 IPv6，dl-cn 只会收到 IPv4，IPv6 签发（旧域名经 Cloudflare 的双栈访客）的链接在边缘对不上，
 * 这类请求直接走 GitHub。规则与 cdn/esa-dl-cn.js::networkId 一致，改一处须同步；站点若开 IPv6 须先改这里。
 */
export function mirrorNetworkId(ip: string): string | null {
  const match = /^(?:::ffff:)?(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/i.exec(ip);
  if (!match) return null;
  const octets = match.slice(1).map(Number);
  if (octets.some((n) => n > 255)) return null;
  return octets.slice(0, 3).map((n) => n.toString(16).padStart(2, "0")).join("");
}

/** 每个 IP 24 小时内可拿镜像的不同文件数与总字节数：覆盖一台机器装全平台包或几台设备共用出口。 */
export const MIRROR_FILES_PER_IP = 6;
export const MIRROR_BYTES_PER_IP = 2 * 1024 ** 3;
/** 同一 IP 同一文件 24 小时内最多重签次数：链接 3 小时过期后续传 / 重试要重新签，全天连续续传也够。 */
export const MIRROR_REISSUES_PER_FILE = 8;
/** 更新器在全站预算用到这一比例后改走 GitHub，余量留给浏览器下载（真人在页面上等，更新器在后台）。 */
export const UPDATER_BUDGET_SHARE = 0.8;
const MIRROR_WINDOW_MS = 24 * 3600 * 1000;
/** 预算按北京时间自然日计（与阿里云出账一致）。 */
const BUDGET_DAY_OFFSET_MS = 8 * 3600 * 1000;

interface MirrorGrant { first: number; issued: number; size: number }

/** 访客 IP → 对象键 → 首次签发时刻 / 签发次数 / 文件大小。 */
const mirrorGrants = new Map<string, Map<string, MirrorGrant>>();
let budgetDay = -1;
let budgetUsed = 0;

function liveGrants(ip: string, now: number): Map<string, MirrorGrant> {
  const grants = mirrorGrants.get(ip);
  if (!grants) return new Map();
  for (const [key, grant] of grants) {
    if (now - grant.first >= MIRROR_WINDOW_MS) grants.delete(key);
  }
  if (!grants.size) mirrorGrants.delete(ip);
  return grants;
}

function budgetUsedToday(now: number): number {
  const day = Math.floor((now + BUDGET_DAY_OFFSET_MS) / (24 * 3600 * 1000));
  if (day !== budgetDay) {
    budgetDay = day;
    budgetUsed = 0;
  }
  return budgetUsed;
}

/** 镜像成本闸的判定：放行，或超出每 IP 配额 / 全站每日预算（两者都回落 GitHub）。 */
export type MirrorQuota = "ok" | "ip-quota" | "budget";

/**
 * 这次签发是否在配额内。同一 IP 已签过的文件（续传、重试、多线程重连）不重复计字节与预算，只受重签次数限制；
 * 新文件须同时满足每 IP 文件数 / 字节数配额与全站预算。`budgetBytes` ≤ 0 表示不设预算；`updater` 为更新器类请求，
 * 预算用到 `UPDATER_BUDGET_SHARE` 即止。只判定不记账，302 前确认资产存在后再 `recordMirrorLink`。
 */
export function checkMirrorQuota(
  ip: string, key: string, size: number, updater: boolean, budgetBytes: number, now = Date.now(),
): MirrorQuota {
  const grants = liveGrants(ip, now);
  const granted = grants.get(key);
  if (granted) return granted.issued < MIRROR_REISSUES_PER_FILE ? "ok" : "ip-quota";
  let bytes = size;
  for (const grant of grants.values()) bytes += grant.size;
  if (grants.size >= MIRROR_FILES_PER_IP || bytes > MIRROR_BYTES_PER_IP) return "ip-quota";
  if (budgetBytes > 0) {
    const limit = updater ? budgetBytes * UPDATER_BUDGET_SHARE : budgetBytes;
    if (budgetUsedToday(now) + size > limit) return "budget";
  }
  return "ok";
}

/** 记一次镜像签发（新文件计入该 IP 配额与当日预算，重签只加次数）。 */
export function recordMirrorLink(ip: string, key: string, size: number, now = Date.now()): void {
  const grants = liveGrants(ip, now);
  const granted = grants.get(key);
  if (granted) {
    granted.issued++;
  } else {
    grants.set(key, { first: now, issued: 1, size });
    budgetUsedToday(now);
    budgetUsed += size;
  }
  // 先删再写，让最近签发的 IP 排到 Map 末尾，淘汰时从头部开始。
  mirrorGrants.delete(ip);
  mirrorGrants.set(ip, grants);
  while (mirrorGrants.size > MAX_VIEWERS) {
    const oldest = mirrorGrants.keys().next();
    if (oldest.done) break;
    mirrorGrants.delete(oldest.value);
  }
}

/** 测试用：清空记录。 */
export function resetReleaseViewers(): void {
  viewers.clear();
  updateAssets.clear();
  pan123Issued.clear();
  mirrorGrants.clear();
  budgetDay = -1;
  budgetUsed = 0;
}
