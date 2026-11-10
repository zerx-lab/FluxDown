/**
 * Release 下载入口（仅 302，不代理文件流量）。
 * 下载源优先级：123 云盘直链（仅应用内更新）→ dl-cn 镜像（中国大陆访客）→ GitHub。
 * 两个镜像都不能回退 GitHub，302 前必须确认镜像有该资产且大小与 GitHub 资产一致。
 * 镜像只发给像正常客户端的请求（24 小时内读过发布清单、非脚本 User-Agent，见 download-gate.ts）；
 * 浏览器等非更新器请求还须带验证码通行证 Cookie（ESA AI 验证码换取，见 download-pass.ts / pass.ts）。
 * 大陆按边缘（阿里云 ESA）判定的访客国家（X-Client-Country = CN，见 client-ip.ts）判定。
 * 123 另外要求（不分地区）：≥ 0.6.0 更新器 User-Agent（isPan123Updater）、资产在当前更新清单里
 * （isUpdateAsset）、该 IP 未超签发配额（hasPan123Quota）；官网浏览器下载不走 123。
 * source=github：仅 GitHub；source=oss：大陆准入访客在镜像都不可用时返回 503，其余同 auto。
 */
import type { APIRoute } from "astro";
import { DOWNLOAD_PASS_SECRET, GITHUB_TOKEN, GITHUB_REPO } from "astro:env/server";
import { getCached, setCached } from "@/lib/api-cache";
import { getClientCountry, getClientIp } from "@/lib/client-ip";
import { readDownloadPass, verifyDownloadPass } from "@/lib/download-pass";
import { hasPan123Quota, isPan123Updater, isUpdateAsset, mayUseMirror, recordPan123Link } from "@/lib/download-gate";
import { cdnConfigured, cdnDownloadUrl, ossConfigured, presignOssHeadUrl, releaseObjectKey } from "@/lib/oss";
import { DownloadLookupCache } from "@/lib/download-cache";
import { createOssProbe } from "@/lib/download-probe";
import { pan123Configured, pan123DownloadUrl } from "@/lib/pan123";
import {
  DownloadLookupError,
  downloadRedirect,
  downloadSource,
  latestWithAsset,
  isGitHubDownloadUrl,
  ossUnavailable,
  transientGitHubStatus,
  type DownloadRelease,
} from "@/lib/download-policy";

export const prerender = false;

const GITHUB_HEADERS: Record<string, string> = {
  Accept: "application/vnd.github+json",
  "X-GitHub-Api-Version": "2022-11-28",
  ...(GITHUB_TOKEN ? { Authorization: `Bearer ${GITHUB_TOKEN}` } : {}),
};
const ossHasAsset = createOssProbe((key) => presignOssHeadUrl(key, 60));

function releaseCache(): DownloadLookupCache<DownloadRelease[]> {
  const key = "download:lookup-cache:v2";
  let cache = getCached<DownloadLookupCache<DownloadRelease[]>>(key, Infinity);
  if (!cache) {
    cache = new DownloadLookupCache<DownloadRelease[]>();
    setCached(key, cache);
  }
  return cache;
}

async function loadReleases(tag: string): Promise<DownloadRelease[]> {
  let response: Response;
  try {
    response = await fetch(
      `https://api.github.com/repos/${GITHUB_REPO}/releases${
        tag ? `/tags/${encodeURIComponent(tag)}` : "?per_page=30"
      }`,
      { headers: GITHUB_HEADERS, signal: AbortSignal.timeout(8000) },
    );
  } catch {
    throw new DownloadLookupError("GitHub API network failure or timeout", true);
  }
  if (tag && response.status === 404) return [];
  if (!response.ok) {
    throw new DownloadLookupError(
      `GitHub API error ${response.status}`,
      transientGitHubStatus(response.status, response.headers),
    );
  }
  let body: string;
  try {
    // fetch 在响应头到达时就完成；同一 8s signal 也覆盖后续读体。
    body = await response.text();
  } catch {
    throw new DownloadLookupError("GitHub API response body interrupted or timed out", true);
  }
  // 读体与解析分开：完整响应中的 JSON/结构异常不能触发旧数据兜底。
  const data: unknown = JSON.parse(body);
  const releases = tag ? [data] : data;
  if (!Array.isArray(releases) || !releases.every(isRelease)) {
    throw new DownloadLookupError("Invalid GitHub release metadata", false);
  }
  if (tag && releases[0]?.tag_name !== tag) {
    throw new DownloadLookupError("GitHub release tag mismatch", false);
  }
  return releases;
}

function isRelease(value: unknown): value is DownloadRelease {
  if (!value || typeof value !== "object") return false;
  const r = value as DownloadRelease;
  return typeof r.tag_name === "string" && typeof r.draft === "boolean" &&
    typeof r.prerelease === "boolean" && Array.isArray(r.assets) &&
    r.assets.every((a) => a && typeof a.name === "string" &&
      Number.isSafeInteger(a.size) && a.size >= 0 &&
      typeof a.browser_download_url === "string" &&
      isGitHubDownloadUrl(a.browser_download_url, GITHUB_REPO));
}

function jsonError(status: number, error: string): Response {
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { "Content-Type": "application/json", "Cache-Control": "no-store" },
  });
}

export const GET: APIRoute = async ({ params, url, request, clientAddress }) => {
  const { filename } = params;
  if (!filename) return jsonError(400, "Missing filename");
  const tag = url.searchParams.get("tag")?.trim() || "";
  const source = downloadSource(url.searchParams.get("source"));
  const ip = getClientIp(request, clientAddress);
  const userAgent = request.headers.get("user-agent");
  const hasPass = !DOWNLOAD_PASS_SECRET || verifyDownloadPass(DOWNLOAD_PASS_SECRET, readDownloadPass(request), ip);
  const admitted = source !== "github" && mayUseMirror(ip, userAgent, hasPass);
  const wantsMirror = admitted && getClientCountry(request) === "CN";
  const wantsPan123 = admitted && isPan123Updater(userAgent);

  try {
    const { value: releases, stale } = await releaseCache().lookup(
      tag ? `tag:${tag}` : "releases", () => loadReleases(tag),
    );
    const release = tag ? releases[0] : latestWithAsset(releases, filename);
    if (!release) {
      return jsonError(404, tag ? `Release "${tag}" not found` :
        `No published release contains asset "${filename}"`);
    }
    // 显式 tag 允许预发布，任何来源都禁止草稿。
    if (release.draft) return jsonError(403, `Release "${tag}" is a draft`);
    const asset = release.assets.find((a) => a.name === filename);
    if (!asset) {
      return jsonError(404, `Asset "${filename}" not found in release "${release.tag_name}"`);
    }

    // 只有 GitHub 新鲜元数据确认的 tag + asset 才有资格获得镜像链接；探测结果按资产缓存。
    // 过期缓存只允许 GitHub 公共 URL：撤回/转草稿后由 GitHub 自身拒绝访问。
    if (!stale && (wantsPan123 || wantsMirror)) {
      const key = releaseObjectKey(release.tag_name, asset.name);
      if (wantsPan123 && pan123Configured && isUpdateAsset(release.tag_name, asset.name) && hasPan123Quota(ip)) {
        const link = await pan123DownloadUrl(key, asset.size);
        if (link) {
          recordPan123Link(ip);
          return downloadRedirect(link, "pan123");
        }
      }
      if (wantsMirror && cdnConfigured && ossConfigured && await ossHasAsset(key, asset.size)) {
        return downloadRedirect(cdnDownloadUrl(key), "cdn");
      }
    }
    if (source === "oss" && wantsMirror) return ossUnavailable();
    return downloadRedirect(asset.browser_download_url, "github", stale);
  } catch (error) {
    if (source === "oss" && wantsMirror) return ossUnavailable();
    if (error instanceof DownloadLookupError && error.transient) {
      return jsonError(503, error.message);
    }
    return jsonError(500, "Download metadata lookup failed");
  }
};
