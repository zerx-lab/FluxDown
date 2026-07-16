/**
 * GET /api/components/ffmpeg
 * GET /api/components/ytdlp
 *
 * 代理外部组件（ffmpeg / yt-dlp）的 GitHub Release 版本列表，为客户端规避
 * GitHub 匿名 REST API 的每 IP 60 次/小时限流（表现为 403 Forbidden）。
 *
 * 客户端「设置 → 扩展 → 组件」页每次打开都会拉版本列表，中国大陆用户还叠加
 * api.github.com 网络不稳定。经官网转发后：
 *   - 官网出口 IP 承担配额，且服务端可选持有 GITHUB_TOKEN（5000/h），
 *     叠加缓存后对 GitHub 的实际请求量降到每天数次；
 *   - 缓存分档禁止过频更新：有 token → 12h，无 token → 24h；
 *   - Vercel/官网作为可达代理，缓解直连 api.github.com 的网络问题。
 *
 * 返回：GitHub Release API 原样 JSON（客户端解析逻辑不变）。
 *   ffmpeg → BtbN/FFmpeg-Builds 的 releases/latest（单对象）
 *   ytdlp  → yt-dlp/yt-dlp 的 releases?per_page=30（数组）
 *
 * 客户端在本端点失败时回退直连 GitHub，故此路由不可用不影响功能、只退化体验。
 */

import type { APIRoute } from "astro";
import { GITHUB_TOKEN } from "astro:env/server";
import { getCached, setCached } from "../../../lib/api-cache";

export const prerender = false;

// 缓存分档：有 GITHUB_TOKEN → 12h（配额充足但仍禁止过频更新），
// 无 token → 24h（匿名 60/h 限流，更长 TTL 进一步保护）。
// 内存 TTL 与边缘 s-maxage 同步取此值。
const CACHE_TTL_MS = (GITHUB_TOKEN ? 12 : 24) * 60 * 60 * 1000;

// 组件名 → 上游 GitHub Release API 端点（与客户端
// native/engine/src/components/{ffmpeg,ytdlp}.rs 的 RELEASE_API 一致）。
const UPSTREAM: Record<string, string> = {
  ffmpeg: "https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/latest",
  ytdlp: "https://api.github.com/repos/yt-dlp/yt-dlp/releases?per_page=30",
};

export const GET: APIRoute = async ({ params }) => {
  const name = params.name ?? "";
  const upstream = UPSTREAM[name];
  if (!upstream) {
    return new Response(JSON.stringify({ error: `unknown component: ${name}` }), {
      status: 404,
      headers: { "Content-Type": "application/json" },
    });
  }

  const cacheKey = `components:${name}`;
  const cached = getCached<unknown>(cacheKey, CACHE_TTL_MS);
  if (cached) {
    return json(cached, "HIT");
  }

  try {
    // GITHUB_TOKEN 可选：有则提升配额到 5000/h，无则匿名（缓存已足够保护）。
    const headers: Record<string, string> = {
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "FluxDown-Website",
    };
    if (GITHUB_TOKEN) headers.Authorization = `Bearer ${GITHUB_TOKEN}`;

    const res = await fetch(upstream, { headers });
    if (!res.ok) {
      const detail = await res.text();
      return new Response(
        JSON.stringify({ error: `GitHub API error: ${res.status}`, detail }),
        { status: 502, headers: { "Content-Type": "application/json" } },
      );
    }

    const data: unknown = await res.json();
    setCached(cacheKey, data);
    return json(data, "MISS");
  } catch (err) {
    return new Response(
      JSON.stringify({ error: "upstream fetch failed", detail: String(err) }),
      { status: 502, headers: { "Content-Type": "application/json" } },
    );
  }
};

/** 统一 JSON 响应：边缘 s-maxage 与内存 TTL 同步分档 + stale-while-revalidate 兜底。 */
function json(data: unknown, cacheStatus: "HIT" | "MISS"): Response {
  const sMaxAge = Math.floor(CACHE_TTL_MS / 1000);
  return new Response(JSON.stringify(data), {
    status: 200,
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": `public, s-maxage=${sMaxAge}, stale-while-revalidate=604800`,
      "X-Cache": cacheStatus,
    },
  });
}
