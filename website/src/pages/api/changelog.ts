/**
 * GET /api/changelog?page=1&per_page=10&since=v0.0.2&channel=stable
 *
 * 代理 GitHub Releases API，获取私有仓库的发布记录（分页）。
 * 服务端持有 GITHUB_TOKEN，前端无需暴露凭据。
 *
 * Query params:
 *   page     - 页码，从 1 开始，默认 1
 *   per_page - 每页条数，默认 10，最大 100
 *   since    - 可选，起始版本号（含），如 "v0.0.2" 或 "0.0.2"
 *   channel  - 可选，更新渠道：stable（默认，仅稳定版 vX.Y.Z）
 *              或 frontier（仅预览预发布版 vX.Y.Z-rc.N）
 *
 * 返回格式:
 * {
 *   releases: [
 *     {
 *       tag, version, published_at, body,
 *       assets: [{ name, size, download_url }]
 *     }
 *   ],
 *   page: 1,
 *   per_page: 10,
 *   has_more: true
 * }
 */

import type { APIRoute } from "astro";
import { GITHUB_TOKEN, GITHUB_REPO } from "astro:env/server";
import { getCached, setCached } from "../../lib/api-cache";

export const prerender = false;

// ── 全量缓存：拉取 GitHub 所有 release 后缓存，分页在返回时切片 ──
// （5 分钟 TTL；release webhook 会主动清除）
const CACHE_KEY = "changelog";
const CACHE_TTL = 300_000; // 5 分钟

interface GitHubAsset {
  name: string;
  size: number;
  download_count: number;
  url: string;
  browser_download_url: string;
}

interface GitHubRelease {
  tag_name: string;
  name: string;
  published_at: string;
  draft: boolean;
  prerelease: boolean;
  body: string;
  assets: GitHubAsset[];
}

interface ReleaseAsset {
  name: string;
  size: number;
  download_url: string;
}

interface FilteredRelease {
  tag: string;
  version: string;
  published_at: string;
  body: string;
  /** 是否为预览预发布版（GitHub prerelease，tag 形如 vX.Y.Z-rc.N） */
  prerelease: boolean;
  assets: ReleaseAsset[];
}

/** 将 tag 转为可比较的版本数组，如 "v0.0.3" / "cli-v0.0.3" → [0, 0, 3] */
function parseVersion(tag: string): number[] {
  return tag
    // 去掉组件前缀（cli- / mobile- / server- / extension- / website-）再去掉 v
    .replace(/^(cli|mobile|server|extension|website)-/, "")
    .replace(/^v/, "")
    .split(".")
    .map((s) => {
      const n = parseInt(s, 10);
      return isNaN(n) ? 0 : n;
    });
}

/** 比较两个版本：a >= b 返回 true */
function versionGte(a: number[], b: number[]): boolean {
  const len = Math.max(a.length, b.length);
  for (let i = 0; i < len; i++) {
    const va = a[i] ?? 0;
    const vb = b[i] ?? 0;
    if (va > vb) return true;
    if (va < vb) return false;
  }
  return true;
}

/** 解析 GitHub Link header 中的 next URL */
function parseLinkNext(header: string | null): string | null {
  if (!header) return null;
  const match = header.match(/<([^>]+)>;\s*rel="next"/);
  return match ? match[1] : null;
}

/**
 * 判断某个 asset 是否是用户可下载的安装包/压缩包，
 * 过滤掉 .yml / .yaml / .sig / .sha256 等校验/元数据文件。
 */
function isDownloadableAsset(name: string): boolean {
  const lower = name.toLowerCase();
  // 排除校验/元数据文件
  if (
    lower.endsWith(".yml") ||
    lower.endsWith(".yaml") ||
    lower.endsWith(".sig") ||
    lower.endsWith(".sha256") ||
    lower.endsWith(".sha512") ||
    lower.endsWith(".asc") ||
    lower.endsWith(".blockmap")
  ) {
    return false;
  }
  // 保留常见可下载格式
  return (
    lower.endsWith(".exe") ||
    lower.endsWith(".zip") ||
    lower.endsWith(".xpi") ||
    lower.endsWith(".dmg") ||
    lower.endsWith(".pkg") ||
    lower.endsWith(".deb") ||
    lower.endsWith(".rpm") ||
    lower.endsWith(".appimage") ||
    lower.endsWith(".tar.gz") ||
    lower.endsWith(".tar.xz") ||
    lower.endsWith(".tar.bz2") ||
    lower.endsWith(".zst") ||
    lower.endsWith(".msi") ||
    lower.endsWith(".apk")
  );
}

/** 拉取 GitHub 全部 releases（自动跟随分页） */
async function fetchAllGitHubReleases(): Promise<GitHubRelease[]> {
  const all: GitHubRelease[] = [];
  let url: string | null =
    `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=100`;

  while (url) {
    const res = await fetch(url, {
      headers: {
        Authorization: `Bearer ${GITHUB_TOKEN}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
      },
    });

    if (!res.ok) {
      throw new Error(`GitHub API ${res.status}: ${await res.text()}`);
    }

    const page: GitHubRelease[] = await res.json();
    all.push(...page);

    url = parseLinkNext(res.headers.get("Link"));
  }

  return all;
}

/** 获取经过过滤和排序的全量 release 列表（带缓存），按渠道切分 */
async function getCachedReleases(
  since: string,
  channel: "stable" | "frontier",
): Promise<FilteredRelease[]> {
  let all = getCached<FilteredRelease[]>(CACHE_KEY, CACHE_TTL);
  if (!all) {
    const raw = await fetchAllGitHubReleases();

    // 只保留 v* 客户端 release（含预览预发布）；extension-v* / website-v*
    // 组件 release 不属于 App 更新日志（且其 tag 无法按 semver 解析）
    all = raw
      .filter((r) => !r.draft && /^v\d/.test(r.tag_name))
      .sort(
        (a, b) =>
          new Date(b.published_at).getTime() -
          new Date(a.published_at).getTime(),
      )
      .map((r) => ({
        tag: r.tag_name,
        version: r.tag_name.replace(/^v/, ""),
        published_at: r.published_at,
        body: r.body || "",
        prerelease: r.prerelease,
        assets: (r.assets || [])
          .filter((a) => isDownloadableAsset(a.name))
          .map((a) => ({
            name: a.name,
            size: a.size,
            // 通过我们自己的代理端点下载，携带 tag 参数定位到对应版本
            download_url: `/api/download/${encodeURIComponent(a.name)}?tag=${encodeURIComponent(r.tag_name)}`,
          })),
      }));

    setCached(CACHE_KEY, all);
  }

  // 渠道切分：稳定版 tab 只看正式版，预览版 tab 只看 rc 预发布
  let releases = all.filter((r) =>
    channel === "frontier" ? r.prerelease : !r.prerelease,
  );

  if (since) {
    const sinceVer = parseVersion(since);
    releases = releases.filter((r) =>
      versionGte(parseVersion(r.tag), sinceVer),
    );
  }

  return releases;
}

export const GET: APIRoute = async ({ url }) => {
  const sinceParam = url.searchParams.get("since")?.trim() || "";
  // 渠道：缺省 stable（与 /api/release 一致，未知值一律回落稳定版）
  const channel =
    url.searchParams.get("channel") === "frontier" ? "frontier" : "stable";
  const page = Math.max(
    1,
    parseInt(url.searchParams.get("page") || "1", 10) || 1,
  );
  const perPage = Math.min(
    100,
    Math.max(1, parseInt(url.searchParams.get("per_page") || "10", 10) || 10),
  );

  if (!GITHUB_TOKEN) {
    return new Response(
      JSON.stringify({ error: "Server misconfigured: missing GITHUB_TOKEN" }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }

  try {
    const all = await getCachedReleases(sinceParam, channel);
    const start = (page - 1) * perPage;
    const sliced = all.slice(start, start + perPage);

    const data = {
      releases: sliced,
      page,
      per_page: perPage,
      has_more: start + perPage < all.length,
    };

    return new Response(JSON.stringify(data), {
      status: 200,
      headers: {
        "Content-Type": "application/json",
        "Cache-Control": "public, s-maxage=300, stale-while-revalidate=600",
      },
    });
  } catch (err) {
    return new Response(
      JSON.stringify({
        error: "Failed to fetch releases",
        detail: String(err),
      }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
