export interface DownloadAsset {
  name: string;
  size: number;
  browser_download_url: string;
}

export interface DownloadRelease {
  tag_name: string;
  draft: boolean;
  prerelease: boolean;
  assets: DownloadAsset[];
}

export type DownloadSource = "auto" | "github" | "oss" | "legacy";

// 未知 source 延续旧客户端行为，不改变既有链接语义。
export function downloadSource(source: string | null): DownloadSource {
  return source === "auto" || source === "github" || source === "oss" ? source : "legacy";
}

export function latestWithAsset(
  releases: DownloadRelease[],
  filename: string,
): DownloadRelease | null {
  return releases.find((r) => !r.draft && !r.prerelease &&
    r.assets.some((a) => a.name === filename)) ?? null;
}

export function isGitHubDownloadUrl(value: string, repository: string): boolean {
  try {
    const url = new URL(value);
    const parts = url.pathname.split("/");
    // GitHub owner/repo 不区分大小写；tag、文件名及其原始 URL 不做归一化。
    return url.protocol === "https:" && url.hostname === "github.com" &&
      !url.port && !url.username && !url.password &&
      parts.length === 7 && parts[1] !== "" && parts[2] !== "" &&
      `${parts[1]}/${parts[2]}`.toLowerCase() === repository.toLowerCase() &&
      parts[3] === "releases" && parts[4] === "download" &&
      parts[5] !== "" && parts[6] !== "";
  } catch {
    return false;
  }
}

export function matchesAssetSize(response: Response, expected: number): boolean {
  const length = response.headers.get("content-length");
  return response.ok && Number.isSafeInteger(expected) && expected >= 0 &&
    length !== null && /^\d+$/.test(length) && Number(length) === expected;
}

export class DownloadLookupError extends Error {
  constructor(message: string, readonly transient: boolean) {
    super(message);
  }
}

export function transientGitHubStatus(status: number, headers: Headers): boolean {
  return status === 429 || (status >= 500 && status <= 599) ||
    (status === 403 && (headers.get("x-ratelimit-remaining") === "0" ||
      headers.has("retry-after")));
}

/** 本可走镜像却被成本闸挡回 GitHub 的原因（回 `X-Download-Limit`，便于在访问日志里统计）。 */
export type MirrorLimit = "ip-quota" | "budget" | "asset";

export function downloadRedirect(
  location: string,
  source: "github" | "cdn" | "pan123",
  stale = false,
  limit: MirrorLimit | null = null,
): Response {
  return new Response(null, {
    status: 302,
    headers: {
      Location: location,
      "Cache-Control": "private, no-cache",
      "X-Download-Source": source,
      ...(stale ? { "X-Download-Metadata": "stale", "Cache-Control": "private, no-store" } : {}),
      ...(limit ? { "X-Download-Limit": limit } : {}),
    },
  });
}

export function ossUnavailable(): Response {
  return new Response(JSON.stringify({
    error: "OSS download unavailable",
    detail: "The backup asset could not be verified. Retry later or choose GitHub.",
  }), {
    status: 503,
    headers: { "Content-Type": "application/json", "Cache-Control": "no-store", "Retry-After": "60" },
  });
}
