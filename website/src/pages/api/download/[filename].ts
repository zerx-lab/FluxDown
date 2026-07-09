/**
 * GET /api/download/:filename?tag=v1.2.3[&source=github|mirror]
 *
 * Release 资产的地域感知下载路由（仓库已开源，asset 可公开直连）。
 * - 若提供 ?tag= 参数，则在对应 tag 的 release 中查找 asset
 * - 若不提供 tag，则在最新的正式 release 中查找 asset
 *
 * 路由策略（302 重定向，本服务不中转下载流量）：
 * - 中国大陆用户（Cloudflare `CF-IPCountry: CN`，无该头时降级用
 *   Accept-Language 含 zh-CN 判断）：GitHub 加速镜像 → GitHub 直连。
 *   镜像做服务端 HEAD 健康检查，结果按镜像缓存 10 分钟，失效自动切下一个。
 * - 其他地区：GitHub 直连（官方 CDN 全球最快）。
 * - ?source= 可显式指定来源，用于调试与前端手动切换。
 *
 * 桌面 App 自升级同样经由本端点（/api/release 返回的 download_url 指向这里），
 * App 发起下载时携带自身 IP，地域判定对网站与 App 两个入口统一生效。
 * 镜像与 GitHub CDN 均支持 Range（已验证 206），App 的多线程分段升级下载
 * 透过 302 正常工作。
 */

import type { APIRoute } from "astro";
import {
  GITHUB_TOKEN,
  GITHUB_REPO,
  DOWNLOAD_MIRRORS,
} from "astro:env/server";

export const prerender = false;

interface GitHubAsset {
  name: string;
  url: string;
  size: number;
  browser_download_url: string;
}

interface GitHubRelease {
  tag_name: string;
  draft: boolean;
  prerelease: boolean;
  assets: GitHubAsset[];
}

/** 仓库已公开：token 仅用于提高 GitHub API 速率限制，缺失时匿名访问。 */
const GITHUB_HEADERS: Record<string, string> = {
  Accept: "application/vnd.github+json",
  "X-GitHub-Api-Version": "2022-11-28",
  ...(GITHUB_TOKEN ? { Authorization: `Bearer ${GITHUB_TOKEN}` } : {}),
};

/**
 * 中国大陆常用的 GitHub 下载加速镜像（前缀完整 GitHub URL 使用）。
 * 默认使用 ghproxy.net（hunshcn/gh-proxy 公共实例，release/archive 走
 * Cloudflare 加速）——遵守 `<镜像>/<完整GitHub直链>` 前缀契约，实测本仓
 * Release 资产可正常代理（200 + content-length 一致）。Google Safe
 * Browsing 透明度报告状态为 5（安全，无危险标志），Chrome 不会弹红。
 * 可用性随时间变化，且可能被 Safe Browsing 拉黑（Chrome 会弹全屏
 * "危险网站"警告）——入选前须人工核查
 * https://transparencyreport.google.com/safe-browsing/search?url=<域名>。
 * 按顺序健康检查、自动降级，也可通过 DOWNLOAD_MIRRORS 环境变量
 * （逗号分隔）覆盖，无需改代码。
 *
 * 注意 github.akams.cn / ghproxy.link 之类靠页面 JS 生成节点或"地址发布页"
 * 的站点不是前缀代理本体，前缀 URL 会落到 404 / HTML 公告页——健康检查
 * 校验 content-length 恰好防住这类不兼容镜像。
 */
const DEFAULT_MIRRORS = ["https://ghproxy.net"];

function mirrorList(): string[] {
  const raw = DOWNLOAD_MIRRORS?.trim();
  if (!raw) return DEFAULT_MIRRORS;
  return raw
    .split(",")
    .map((s) => s.trim().replace(/\/+$/, ""))
    .filter(Boolean);
}

/** 镜像健康状态缓存：按镜像域名（非按文件）缓存，TTL 内不重复探测。 */
const mirrorHealth = new Map<string, { ok: boolean; checkedAt: number }>();
const MIRROR_HEALTH_TTL_MS = 10 * 60 * 1000;
const MIRROR_PROBE_TIMEOUT_MS = 3000;

/**
 * 按配置顺序找到第一个健康的镜像，返回镜像化的下载 URL。
 * 健康标准：HEAD 2xx 且 content-length 与 GitHub asset 大小一致——
 * 只看状态码会被"200 + HTML 公告页"的假镜像骗过。
 * 全部不可用时返回 null（调用方降级到 GitHub 直连）。
 */
async function resolveMirrorUrl(
  directUrl: string,
  expectedSize: number,
): Promise<string | null> {
  for (const mirror of mirrorList()) {
    const candidate = `${mirror}/${directUrl}`;
    const cached = mirrorHealth.get(mirror);
    const fresh = cached && Date.now() - cached.checkedAt < MIRROR_HEALTH_TTL_MS;

    if (fresh) {
      if (cached.ok) return candidate;
      continue;
    }

    let ok = false;
    try {
      const res = await fetch(candidate, {
        method: "HEAD",
        redirect: "follow",
        signal: AbortSignal.timeout(MIRROR_PROBE_TIMEOUT_MS),
      });
      const len = Number(res.headers.get("content-length"));
      ok = res.ok && (expectedSize <= 0 || len === expectedSize);
    } catch {
      // 超时/网络错误 → 视为不健康
    }

    mirrorHealth.set(mirror, { ok, checkedAt: Date.now() });
    if (ok) return candidate;
  }

  return null;
}

/**
 * 判断请求是否来自中国大陆。
 * 优先使用 Cloudflare 的 CF-IPCountry 头（需在 CF 面板开启 IP Geolocation）；
 * 头缺失时（直连源站/未开启开关）降级用 Accept-Language 含 zh-CN 启发式判断——
 * 误判成本低：镜像本身是全球可达的透传代理，只是非大陆用户走镜像会稍慢。
 */
function isMainlandChina(request: Request): boolean {
  const country = request.headers.get("cf-ipcountry");
  if (country) return country.toUpperCase() === "CN";

  const lang = request.headers.get("accept-language") ?? "";
  return /\bzh-CN\b/i.test(lang);
}

/**
 * 通过 tag 名称获取指定 release。
 * GitHub API: GET /repos/{owner}/{repo}/releases/tags/{tag}
 */
async function fetchReleaseByTag(tag: string): Promise<GitHubRelease | null> {
  const res = await fetch(
    `https://api.github.com/repos/${GITHUB_REPO}/releases/tags/${encodeURIComponent(tag)}`,
    { headers: GITHUB_HEADERS },
  );

  if (res.status === 404) return null;

  if (!res.ok) {
    throw new Error(`GitHub API error ${res.status}: ${await res.text()}`);
  }

  return res.json() as Promise<GitHubRelease>;
}

/**
 * 获取包含指定 asset 的最新正式 release（非 draft、非 prerelease）。
 * Release 已按组件拆分（v* / extension-v* / website-v*），列表首个 release
 * 不一定包含请求的文件，须按 asset 名定位。
 */
async function fetchLatestReleaseWithAsset(
  filename: string,
): Promise<GitHubRelease | null> {
  const res = await fetch(
    `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=30`,
    { headers: GITHUB_HEADERS },
  );

  if (!res.ok) {
    throw new Error(`GitHub API error ${res.status}: ${await res.text()}`);
  }

  const releases: GitHubRelease[] = await res.json();
  return (
    releases.find(
      (r) =>
        !r.draft &&
        !r.prerelease &&
        r.assets.some((a) => a.name === filename),
    ) ?? null
  );
}

/** 302 到最终下载地址；X-Download-Source 标记来源便于观测。 */
function redirectTo(location: string, source: string): Response {
  return new Response(null, {
    status: 302,
    headers: {
      Location: location,
      "Cache-Control": "private, no-cache",
      "X-Download-Source": source,
    },
  });
}

export const GET: APIRoute = async ({ params, url, request }) => {
  const { filename } = params;

  if (!filename) {
    return new Response(JSON.stringify({ error: "Missing filename" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });
  }

  const tag = url.searchParams.get("tag")?.trim() || "";
  const source = url.searchParams.get("source")?.trim().toLowerCase() || "";

  try {
    // ── 1. 定位目标 Release ──
    let release: GitHubRelease | null;

    if (tag) {
      release = await fetchReleaseByTag(tag);
      if (!release) {
        return new Response(
          JSON.stringify({ error: `Release "${tag}" not found` }),
          { status: 404, headers: { "Content-Type": "application/json" } },
        );
      }
      // 拒绝下载草稿或预发布版本中的 asset
      if (release.draft || release.prerelease) {
        return new Response(
          JSON.stringify({
            error: `Release "${tag}" is not a published release`,
          }),
          { status: 403, headers: { "Content-Type": "application/json" } },
        );
      }
    } else {
      release = await fetchLatestReleaseWithAsset(filename);
      if (!release) {
        return new Response(
          JSON.stringify({
            error: `No published release contains asset "${filename}"`,
          }),
          { status: 404, headers: { "Content-Type": "application/json" } },
        );
      }
    }

    // ── 2. 在 release 中查找对应 asset ──
    const asset = release.assets.find((a) => a.name === filename);

    if (!asset) {
      return new Response(
        JSON.stringify({
          error: `Asset "${filename}" not found in release "${release.tag_name}"`,
        }),
        { status: 404, headers: { "Content-Type": "application/json" } },
      );
    }

    // 仓库已公开，browser_download_url 无需 token 签名即可直连
    const directUrl = asset.browser_download_url;

    // ── 3. 显式 source 覆盖（调试 / 前端手动切换）──
    if (source === "github") {
      return redirectTo(directUrl, "github");
    }

    // ── 4. 地域路由 ──
    const preferMirror = source === "mirror" || isMainlandChina(request);

    if (preferMirror) {
      // 中国大陆：加速镜像 → GitHub 直连
      const mirrorUrl = await resolveMirrorUrl(directUrl, asset.size);
      if (mirrorUrl) return redirectTo(mirrorUrl, "mirror");
    }

    // ── 5. 其他地区（或大陆全链路降级）：GitHub 官方 CDN 直连 ──
    return redirectTo(directUrl, "github");
  } catch (err) {
    return new Response(
      JSON.stringify({ error: "Download failed", detail: String(err) }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
