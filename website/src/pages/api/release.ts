/**
 * GET /api/release
 *
 * 代理 GitHub Release API，安全获取私有仓库的最新发布信息。
 * 服务端持有 GITHUB_TOKEN，前端无需暴露凭据。
 *
 * 返回格式:
 * {
 *   version: "1.0.0",
 *   published_at: "2025-01-01T00:00:00Z",
 *   assets: {
 *     setup: { name, size, download_url },
 *     portable: { name, size, download_url },
 *     extension: { name, size, download_url },
 *   }
 * }
 */

import type { APIRoute } from "astro";

export const prerender = false;

const GITHUB_REPO = import.meta.env.GITHUB_REPO || "user/x_down";
const GITHUB_TOKEN = import.meta.env.GITHUB_TOKEN || "";

// 缓存：避免每次请求都打 GitHub API（60 秒）
let cache: { data: unknown; timestamp: number } | null = null;
const CACHE_TTL = 60_000;

interface GitHubAsset {
  name: string;
  size: number;
  url: string; // API URL, 需要 token 才能下载
  browser_download_url: string;
}

interface GitHubRelease {
  tag_name: string;
  name: string;
  published_at: string;
  draft: boolean;
  prerelease: boolean;
  assets: GitHubAsset[];
}

export const GET: APIRoute = async () => {
  // 检查缓存
  if (cache && Date.now() - cache.timestamp < CACHE_TTL) {
    return new Response(JSON.stringify(cache.data), {
      status: 200,
      headers: {
        "Content-Type": "application/json",
        "Cache-Control": "public, s-maxage=60, stale-while-revalidate=300",
      },
    });
  }

  if (!GITHUB_TOKEN) {
    return new Response(
      JSON.stringify({ error: "Server misconfigured: missing GITHUB_TOKEN" }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }

  try {
    // 获取最新的非草稿 Release
    const res = await fetch(
      `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=5`,
      {
        headers: {
          Authorization: `Bearer ${GITHUB_TOKEN}`,
          Accept: "application/vnd.github+json",
          "X-GitHub-Api-Version": "2022-11-28",
        },
      },
    );

    if (!res.ok) {
      const text = await res.text();
      return new Response(
        JSON.stringify({ error: `GitHub API error: ${res.status}`, detail: text }),
        { status: 502, headers: { "Content-Type": "application/json" } },
      );
    }

    const releases: GitHubRelease[] = await res.json();

    // 找到第一个非草稿、非预发布的 Release
    const latest = releases.find((r) => !r.draft && !r.prerelease);

    if (!latest) {
      return new Response(
        JSON.stringify({ error: "No published release found" }),
        { status: 404, headers: { "Content-Type": "application/json" } },
      );
    }

    const version = latest.tag_name.replace(/^v/, "");

    // 匹配资产文件
    const setupAsset = latest.assets.find((a) => a.name.endsWith("-windows-setup.exe"));
    const portableAsset = latest.assets.find((a) => a.name.endsWith("-windows-portable.zip"));
    const extensionAsset = latest.assets.find((a) => a.name.endsWith("-extension.zip"));

    const formatAsset = (asset: GitHubAsset | undefined) => {
      if (!asset) return null;
      return {
        name: asset.name,
        size: asset.size,
        // 使用我们自己的代理下载端点，避免前端直接访问 GitHub
        download_url: `/api/download/${asset.name}`,
      };
    };

    const data = {
      version,
      tag: latest.tag_name,
      published_at: latest.published_at,
      assets: {
        setup: formatAsset(setupAsset),
        portable: formatAsset(portableAsset),
        extension: formatAsset(extensionAsset),
      },
    };

    // 更新缓存
    cache = { data, timestamp: Date.now() };

    return new Response(JSON.stringify(data), {
      status: 200,
      headers: {
        "Content-Type": "application/json",
        "Cache-Control": "public, s-maxage=60, stale-while-revalidate=300",
      },
    });
  } catch (err) {
    return new Response(
      JSON.stringify({ error: "Failed to fetch release info", detail: String(err) }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
