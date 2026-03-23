/**
 * GET /api/download/:filename?tag=v1.2.3
 *
 * 代理下载私有仓库的 Release 资产。
 * - 若提供 ?tag= 参数，则在对应 tag 的 release 中查找 asset
 * - 若不提供 tag，则在最新的正式 release 中查找 asset
 *
 * 流程：定位目标 Release → 找到匹配 asset → 用 token 获取带签名的 CDN URL → 302 重定向
 * 用户浏览器直接从 GitHub CDN 下载，不经过 Vercel serverless 中转流量。
 */

import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO, CF_R2_PUBLIC_URL } from "astro:env/server";

export const prerender = false;

interface GitHubAsset {
  name: string;
  url: string;
  browser_download_url: string;
}

interface GitHubRelease {
  tag_name: string;
  draft: boolean;
  prerelease: boolean;
  assets: GitHubAsset[];
}

const GITHUB_HEADERS = {
  Authorization: `Bearer ${GITHUB_TOKEN}`,
  Accept: "application/vnd.github+json",
  "X-GitHub-Api-Version": "2022-11-28",
};

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
 * 获取最新正式 release（非 draft、非 prerelease）。
 * 拉取前几页后取第一个符合条件的即可，无需全量拉取。
 */
async function fetchLatestRelease(): Promise<GitHubRelease | null> {
  const res = await fetch(
    `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=10`,
    { headers: GITHUB_HEADERS },
  );

  if (!res.ok) {
    throw new Error(`GitHub API error ${res.status}: ${await res.text()}`);
  }

  const releases: GitHubRelease[] = await res.json();
  return releases.find((r) => !r.draft && !r.prerelease) ?? null;
}

/**
 * 检查 R2 上是否存在对应文件，存在则返回公开 URL，否则返回 null。
 * 文件路径格式: {tag}/{filename}，例如: v0.3.0/FluxDown-0.3.0-windows-x64-setup.exe
 */
async function resolveR2Url(tag: string, filename: string): Promise<string | null> {
  if (!CF_R2_PUBLIC_URL) {
    return null;
  }

  const key = `${tag}/${filename}`;
  const publicUrl = `${CF_R2_PUBLIC_URL.replace(/\/$/, "")}/${key}`;

  try {
    // 用 HEAD 请求验证文件确实存在于 R2（防止 404 重定向）
    const res = await fetch(publicUrl, { method: "HEAD" });
    if (res.ok) {
      return publicUrl;
    }
  } catch {
    // R2 不可达时静默降级到 GitHub CDN
  }

  return null;
}

/**
 * 通过 GitHub Asset API URL 获取带签名的 CDN 下载地址。
 * GitHub 会返回 302，Location 即为有效期约 10 分钟的临时 CDN URL。
 */
async function resolveAssetDownloadUrl(
  assetApiUrl: string,
): Promise<string | null> {
  const res = await fetch(assetApiUrl, {
    headers: {
      ...GITHUB_HEADERS,
      Accept: "application/octet-stream",
    },
    redirect: "manual", // 捕获 302，不自动跟随
  });

  return res.headers.get("Location");
}

export const GET: APIRoute = async ({ params, url }) => {
  const { filename } = params;

  if (!filename) {
    return new Response(JSON.stringify({ error: "Missing filename" }), {
      status: 400,
      headers: { "Content-Type": "application/json" },
    });
  }

  if (!GITHUB_TOKEN) {
    return new Response(
      JSON.stringify({ error: "Server misconfigured: missing GITHUB_TOKEN" }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
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
      release = await fetchLatestRelease();
      if (!release) {
        return new Response(
          JSON.stringify({ error: "No published release found" }),
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

    // ── 3. 优先尝试 R2 镜像 URL（中国大陆友好，Cloudflare CDN 加速）──
    const r2Url = await resolveR2Url(release.tag_name, filename);
    if (r2Url) {
      return new Response(null, {
        status: 302,
        headers: {
          Location: r2Url,
          "Cache-Control": "private, no-cache",
          "X-Download-Source": "r2",
        },
      });
    }

    // ── 4. Fallback：解析 GitHub CDN 带签名 URL ──
    const downloadUrl = await resolveAssetDownloadUrl(asset.url);

    if (!downloadUrl) {
      return new Response(
        JSON.stringify({ error: "Failed to get download URL from GitHub" }),
        { status: 502, headers: { "Content-Type": "application/json" } },
      );
    }

    // ── 5. 302 重定向到 GitHub CDN 临时签名 URL ──
    return new Response(null, {
      status: 302,
      headers: {
        Location: downloadUrl,
        "Cache-Control": "private, no-cache",
        "X-Download-Source": "github",
      },
    });
  } catch (err) {
    return new Response(
      JSON.stringify({ error: "Download failed", detail: String(err) }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
