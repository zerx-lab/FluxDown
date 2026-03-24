/**
 * GET /api/release
 *
 * 代理 GitHub Release API，安全获取私有仓库的最新发布信息。
 * 服务端持有 GITHUB_TOKEN，前端无需暴露凭据。
 *
 * 下载量计算：
 *   total_downloads = GitHub 所有 release 的 asset download_count 之和
 *                   + CF R2 的 GetObject 操作次数（B 类读取操作 = 实际下载）
 *
 *   当下载走 R2 路径（302 → R2 CDN）时，GitHub download_count 不会增加，
 *   因此需要额外查询 R2 Analytics 来补全这部分下载量。
 *   HeadObject（我们的文件存在性检查）等非下载 B 类操作会被排除。
 *
 * 返回格式:
 * {
 *   version: "1.0.0",
 *   published_at: "2025-01-01T00:00:00Z",
 *   total_downloads: 12345,
 *   assets: {
 *     setup: { name, size, download_url },
 *     portable: { name, size, download_url },
 *     extension: { name, size, download_url },        // Chrome zip
 *     firefox_extension: { name, size, download_url }, // Firefox XPI
 *   }
 * }
 */

import type { APIRoute } from "astro";
import {
  GITHUB_TOKEN,
  GITHUB_REPO,
  CF_R2_ENDPOINT,
  CF_R2_BUCKET,
  CF_API_TOKEN,
} from "astro:env/server";

export const prerender = false;

// ── 缓存：避免每次请求都打 GitHub API（60 秒）──
let cache: { data: unknown; timestamp: number } | null = null;
const CACHE_TTL = 60_000;

// ── R2 Analytics 独立缓存（5 分钟，减少对 CF GraphQL API 的请求频率）──
let r2Cache: { count: number; timestamp: number } | null = null;
const R2_CACHE_TTL = 300_000;

interface GitHubAsset {
  name: string;
  size: number;
  download_count: number;
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

// ── Cloudflare GraphQL Analytics 响应类型 ──
interface CfR2OperationsGroup {
  sum: { requests: number };
  dimensions: { actionType: string };
}

interface CfGraphQLResponse {
  data?: {
    viewer?: {
      accounts?: Array<{
        r2OperationsAdaptiveGroups?: CfR2OperationsGroup[];
      }>;
    };
  };
  errors?: Array<{ message: string }>;
}

/**
 * 被视为"实际下载"的 R2 操作类型。
 *
 * Cloudflare R2 操作分类：
 *   - A 类（写入）: PutObject, DeleteObject, ListObjects, ListBuckets, ...
 *   - B 类（读取）: GetObject, HeadObject, ...
 *
 * 我们只统计 GetObject（用户真正下载文件的操作），
 * 排除 HeadObject（/api/download/[filename] 中 resolveR2Url 的文件存在性检查）
 * 以及其他所有 A 类写入操作。
 *
 * 经测试确认 actionType 使用 S3 风格命名 ("GetObject")。
 */
const R2_DOWNLOAD_ACTION_TYPES = new Set(["GetObject"]);

/**
 * Cloudflare GraphQL Adaptive 数据集的查询限制：
 *   - 单次查询最大时间跨度: 4 周 4 天（~32 天）
 *   - 数据保留期: ~90 天
 *
 * 为安全起见，每个查询窗口设为 30 天，覆盖最近 90 天。
 */
const R2_QUERY_WINDOW_DAYS = 30;
const R2_LOOKBACK_DAYS = 90;

/**
 * 从 CF_R2_ENDPOINT 中解析 Cloudflare Account ID。
 * 端点格式: https://<account_id>.r2.cloudflarestorage.com
 */
function parseAccountIdFromEndpoint(endpoint: string): string | null {
  try {
    const host = new URL(endpoint).hostname; // e.g. "753e09f...".r2.cloudflarestorage.com
    const accountId = host.split(".")[0];
    return accountId && accountId.length > 0 ? accountId : null;
  } catch {
    return null;
  }
}

/**
 * 对单个时间窗口查询 R2 Analytics，返回 GetObject 请求数。
 * Cloudflare 限制单次查询跨度 ≤ 4w4d，此函数由调用方保证窗口合规。
 */
async function queryR2Window(
  accountId: string,
  startDate: string,
  endDate: string,
  bucketFilter: string,
): Promise<number> {
  const query = `
    query R2DownloadCount {
      viewer {
        accounts(filter: { accountTag: "${accountId}" }) {
          r2OperationsAdaptiveGroups(
            limit: 100
            filter: {
              datetime_geq: "${startDate}"
              datetime_leq: "${endDate}"
              ${bucketFilter}
            }
          ) {
            sum {
              requests
            }
            dimensions {
              actionType
            }
          }
        }
      }
    }
  `;

  const res = await fetch("https://api.cloudflare.com/client/v4/graphql", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${CF_API_TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ query }),
  });

  if (!res.ok) {
    const body = await res.text();
    const tokenStr = String(CF_API_TOKEN ?? "");
    const tokenPreview = tokenStr.slice(0, 10);
    const tokenSuffix = tokenStr.slice(-6);
    console.warn(
      `[R2 Analytics] Cloudflare GraphQL API 返回 ${res.status}，token="${tokenPreview}…${tokenSuffix}" len=${tokenStr.length}，body=${body}`,
    );
    return 0;
  }

  const json: CfGraphQLResponse = await res.json();

  if (json.errors?.length) {
    console.warn(
      "[R2 Analytics] GraphQL 错误:",
      json.errors.map((e) => e.message).join("; "),
    );
    return 0;
  }

  const groups =
    json.data?.viewer?.accounts?.[0]?.r2OperationsAdaptiveGroups ?? [];

  let count = 0;
  for (const group of groups) {
    if (R2_DOWNLOAD_ACTION_TYPES.has(group.dimensions.actionType)) {
      count += group.sum.requests;
    }
  }
  return count;
}

/**
 * 查询 CF R2 Analytics，获取 GetObject（实际下载）操作的请求总数。
 *
 * Cloudflare Adaptive 数据集限制单次查询 ≤ 32 天、数据保留 ~90 天，
 * 因此将最近 90 天拆分为多个 30 天窗口并行查询，累加结果。
 *
 * 环境变量缺失或查询失败时静默返回 0（优雅降级）。
 */
async function fetchR2DownloadCount(): Promise<number> {
  // 检查 R2 Analytics 缓存
  if (r2Cache && Date.now() - r2Cache.timestamp < R2_CACHE_TTL) {
    return r2Cache.count;
  }

  // 缺少必要凭证时跳过 R2 统计
  if (!CF_R2_ENDPOINT || !CF_API_TOKEN) {
    return 0;
  }

  const accountId = parseAccountIdFromEndpoint(CF_R2_ENDPOINT);
  if (!accountId) {
    console.warn("[R2 Analytics] 无法从 CF_R2_ENDPOINT 解析 Account ID");
    return 0;
  }

  try {
    const now = new Date();
    const lookbackStart = new Date(
      now.getTime() - R2_LOOKBACK_DAYS * 24 * 60 * 60 * 1000,
    );
    const bucketFilter = CF_R2_BUCKET ? `bucketName: "${CF_R2_BUCKET}",` : "";

    // 将 [lookbackStart, now] 拆分为 ≤30 天的窗口
    const windows: Array<{ start: string; end: string }> = [];
    let cursor = lookbackStart;
    while (cursor < now) {
      const windowEnd = new Date(
        Math.min(
          cursor.getTime() + R2_QUERY_WINDOW_DAYS * 24 * 60 * 60 * 1000,
          now.getTime(),
        ),
      );
      windows.push({
        start: cursor.toISOString(),
        end: windowEnd.toISOString(),
      });
      cursor = windowEnd;
    }

    // 并行查询所有窗口
    const results = await Promise.all(
      windows.map((w) =>
        queryR2Window(accountId, w.start, w.end, bucketFilter),
      ),
    );

    const downloadCount = results.reduce((sum, n) => sum + n, 0);

    // 更新缓存
    r2Cache = { count: downloadCount, timestamp: Date.now() };

    return downloadCount;
  } catch (err) {
    // R2 Analytics 查询失败时静默降级，不影响主流程
    console.warn("[R2 Analytics] 查询失败:", String(err));
    return r2Cache?.count ?? 0;
  }
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
    // 并行发起：GitHub Release 拉取 + R2 Analytics 查询
    const r2CountPromise = fetchR2DownloadCount();

    // 拉取全部 release（自动分页），用于计算总下载量
    const allReleases: GitHubRelease[] = [];
    let url: string | null =
      `https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=100`;

    while (url) {
      const res: Response = await fetch(url, {
        headers: {
          Authorization: `Bearer ${GITHUB_TOKEN}`,
          Accept: "application/vnd.github+json",
          "X-GitHub-Api-Version": "2022-11-28",
        },
      });

      if (!res.ok) {
        const text = await res.text();
        return new Response(
          JSON.stringify({
            error: `GitHub API error: ${res.status}`,
            detail: text,
          }),
          { status: 502, headers: { "Content-Type": "application/json" } },
        );
      }

      const page: GitHubRelease[] = await res.json();
      allReleases.push(...page);

      const link: string | null = res.headers.get("Link");
      const next: RegExpMatchArray | null =
        link?.match(/<([^>]+)>;\s*rel="next"/) ?? null;
      url = next ? next[1] : null;
    }

    const releases = allReleases;

    const latest = releases.find((r) => !r.draft && !r.prerelease);

    if (!latest) {
      return new Response(
        JSON.stringify({ error: "No published release found" }),
        { status: 404, headers: { "Content-Type": "application/json" } },
      );
    }

    const version = latest.tag_name.replace(/^v/, "");

    // 匹配资产文件（兼容旧命名：-windows-setup.exe / 新命名：-windows-x64-setup.exe）
    const setupAsset = latest.assets.find(
      (a) =>
        a.name.endsWith("-windows-x64-setup.exe") ||
        a.name.endsWith("-windows-setup.exe"),
    );
    const portableAsset = latest.assets.find(
      (a) =>
        a.name.endsWith("-windows-x64-portable.zip") ||
        a.name.endsWith("-windows-portable.zip"),
    );
    // ARM64 资产（仅新版 Release 包含）
    const setupArm64Asset = latest.assets.find((a) =>
      a.name.endsWith("-windows-arm64-setup.exe"),
    );
    const portableArm64Asset = latest.assets.find((a) =>
      a.name.endsWith("-windows-arm64-portable.zip"),
    );
    const extensionAsset = latest.assets.find(
      (a) =>
        a.name.endsWith("-chrome.zip") || a.name.endsWith("-extension.zip"),
    );
    const firefoxExtensionAsset = latest.assets.find((a) =>
      a.name.endsWith("-firefox.xpi"),
    );
    // Linux 资产
    const linuxAppImageAsset = latest.assets.find((a) =>
      a.name.endsWith("-linux-x64.AppImage"),
    );
    const linuxDebAsset = latest.assets.find((a) =>
      a.name.endsWith("-linux-x64.deb"),
    );
    const linuxArchAsset = latest.assets.find((a) =>
      a.name.endsWith("-linux-x64.pkg.tar.zst"),
    );
    const linuxTarballAsset = latest.assets.find((a) =>
      a.name.endsWith("-linux-x64.tar.gz"),
    );

    const formatAsset = (asset: GitHubAsset | undefined) => {
      if (!asset) return null;
      return {
        name: asset.name,
        size: asset.size,
        // 使用我们自己的代理下载端点，避免前端直接访问 GitHub
        download_url: `/api/download/${asset.name}`,
      };
    };

    // ── 下载量计算 ──
    // GitHub 下载量：累计所有 release 的 asset download_count
    // 当下载走 GitHub CDN 时，GitHub 会记录 download_count
    let githubDownloads = 0;
    for (const release of releases) {
      for (const asset of release.assets) {
        githubDownloads += asset.download_count;
      }
    }

    // R2 下载量：通过 CF GraphQL Analytics API 获取 GetObject 次数
    // 当下载走 R2 路径时（302 → R2 CDN），GitHub download_count 不增加，
    // 需要从 R2 Analytics 补全这部分。两者不重复：走 R2 则不走 GitHub，反之亦然。
    const r2Downloads = await r2CountPromise;

    const totalDownloads = githubDownloads + r2Downloads;

    const data = {
      version,
      tag: latest.tag_name,
      published_at: latest.published_at,
      total_downloads: totalDownloads,
      assets: {
        setup: formatAsset(setupAsset),
        portable: formatAsset(portableAsset),
        setup_arm64: formatAsset(setupArm64Asset),
        portable_arm64: formatAsset(portableArm64Asset),
        extension: formatAsset(extensionAsset),
        firefox_extension: formatAsset(firefoxExtensionAsset),
        linux_appimage: formatAsset(linuxAppImageAsset),
        linux_deb: formatAsset(linuxDebAsset),
        linux_arch: formatAsset(linuxArchAsset),
        linux_tarball: formatAsset(linuxTarballAsset),
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
      JSON.stringify({
        error: "Failed to fetch release info",
        detail: String(err),
      }),
      { status: 500, headers: { "Content-Type": "application/json" } },
    );
  }
};
