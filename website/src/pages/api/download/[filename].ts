/**
 * GET /api/download/:filename?tag=v1.2.3&source=mirror|github
 *
 * Release 资产的下载路由（仓库已开源，asset 可公开直连）。
 * - 若提供 ?tag= 参数，则在对应 tag 的 release 中查找 asset
 * - 若不提供 tag，则在最新的正式 release 中查找 asset
 *
 * 路由策略（302 重定向，本服务不中转下载流量）：
 * - 中国大陆请求（x-vercel-ip-country / cf-ipcountry == CN）且镜像清单
 *   （mirror.qwld.cn/manifest.json，60s 内存缓存 + 2.5s 超时）确认持有
 *   该 tag+asset 时，302 到镜像；镜像端本地缺失时自身还会再 302 回 GitHub。
 * - 其余地域、镜像不可达或未持有该资产：302 到 GitHub 官方 CDN 直连。
 * - ?source=mirror|github 可显式覆盖地域判定（调试/用户手动切换源）。
 *
 * 桌面 App 自升级同样经由本端点（/api/release 返回的 download_url 指向这里）。
 * GitHub CDN 与镜像端均支持 Range（206），App 的多线程分段升级下载透过 302 正常工作。
 */

import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO, MIRROR_BASE_URL } from "astro:env/server";

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

// ── 镜像清单缓存：60s 内存 TTL，探测失败视为镜像不可用（回退 GitHub）──
interface MirrorManifest {
  releases?: { tag: string; assets?: { name: string }[] }[];
}
let mirrorCache: { at: number; manifest: MirrorManifest | null } | null = null;
const MIRROR_CACHE_TTL = 60 * 1000;

/** 拉取镜像清单（带缓存与 2.5s 超时）；任何失败返回 null。 */
async function fetchMirrorManifest(): Promise<MirrorManifest | null> {
  const now = Date.now();
  if (mirrorCache && now - mirrorCache.at < MIRROR_CACHE_TTL) {
    return mirrorCache.manifest;
  }
  let manifest: MirrorManifest | null = null;
  try {
    const res = await fetch(`${MIRROR_BASE_URL}/manifest.json`, {
      signal: AbortSignal.timeout(2500),
    });
    if (res.ok) manifest = (await res.json()) as MirrorManifest;
  } catch {
    // 镜像不可达 → null，调用方回退 GitHub
  }
  mirrorCache = { at: now, manifest };
  return manifest;
}

/** 镜像是否已持有指定 tag 的指定资产。 */
async function mirrorHasAsset(tag: string, filename: string): Promise<boolean> {
  const manifest = await fetchMirrorManifest();
  if (!manifest?.releases) return false;
  const rel = manifest.releases.find((r) => r.tag === tag);
  return !!rel?.assets?.some((a) => a.name === filename);
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
      // 仅拒绝草稿。预发布（预览版）资产允许经显式 ?tag= 下载：官网页面
      // 从不暴露预发布 tag（无 tag 的"最新"路径见下方仍只认正式版），只有
      // /api/release?channel=frontier 才会把预览 tag 交给客户端更新通道。
      if (release.draft) {
        return new Response(
          JSON.stringify({
            error: `Release "${tag}" is a draft`,
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
    const githubUrl = asset.browser_download_url;

    // ── 3. 地域分流：CN → 国内镜像（mirror.qwld.cn），其余 → GitHub ──
    // ?source= 显式覆盖：mirror 强制镜像，github 强制直连。
    const source = url.searchParams.get("source");
    const country =
      request.headers.get("x-vercel-ip-country") ??
      request.headers.get("cf-ipcountry") ??
      "";
    const preferMirror =
      source === "mirror" || (source !== "github" && country === "CN");

    if (preferMirror && (await mirrorHasAsset(release.tag_name, filename))) {
      const mirrorUrl = `${MIRROR_BASE_URL}/releases/${encodeURIComponent(release.tag_name)}/${encodeURIComponent(filename)}`;
      return redirectTo(mirrorUrl, "mirror");
    }

    // 镜像未持有该资产 / 镜像不可达 / 非 CN 地域：GitHub 官方 CDN 直连
    return redirectTo(githubUrl, "github");
  } catch (err) {
    return new Response(
      JSON.stringify({ error: "Download failed", detail: String(err) }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
