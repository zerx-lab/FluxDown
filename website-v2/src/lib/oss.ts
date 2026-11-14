/**
 * 阿里云 OSS 发布资产源。
 *
 * 发布资产由 .github/actions/oss-upload 上传到 `oss://<bucket>/<prefix>/<tag>/<file>`，
 * bucket 保持私有（对象不可公共读）。官网只用 AK/SK 做 HEAD 存在性探测，下载一律经 dl-cn 镜像
 * （`CN_CDN_BASE`，URL 鉴权 A 方式，私有 OSS 回源）；不签发直连 OSS 的下载链接。
 * 签名算法：https://help.aliyun.com/zh/oss/developer-reference/signature-version-1
 */

import { createHash, createHmac, randomBytes } from "node:crypto";
import {
  CN_CDN_AUTH_KEY,
  CN_CDN_BASE,
  OSS_ACCESS_KEY_ID,
  OSS_ACCESS_KEY_SECRET,
  OSS_BUCKET,
  OSS_ENDPOINT,
  OSS_RELEASE_PREFIX,
} from "astro:env/server";
import { ossStorageKey } from "@/lib/oss-storage";

/**
 * 官网签发的下载链接有效期（秒）。镜像侧有效时长须为同一值（ESA 边缘函数变量 `DOWNLOAD_URL_TTL_SEC`，
 * 见 cdn/esa-dl-cn.js）：A 方式的过期时间 = 签发时刻 + 镜像侧有效时长。
 * 分段下载在过期前发起的请求不受影响，过期后客户端重新请求 `/api/download` 即拿到新链接。
 */
export const DOWNLOAD_URL_TTL_SEC = 3 * 3600;

/** 未配置 AK/SK 时无法确认资产存在，整条 OSS / CDN 路径关闭。 */
export const ossConfigured = !!(OSS_ACCESS_KEY_ID && OSS_ACCESS_KEY_SECRET);

/**
 * 发布资产的对象键（无前导斜杠）：`<prefix>/<版本>/<组件>/<file>`。
 * 目录由 release tag 推导：`v0.4.8` → `v0.4.8/app/`（历史目录名；统一 release 起
 * 该目录承载这个 vX.Y.Z release 的全部组件资产），历史组件 release `extension-v…` /
 * `server-v…` / `cli-v…` / `mobile-v…` → 同名组件目录；版本目录保留 `v` 前缀。
 * 同一套规则在 .github/actions/oss-upload 与 .github/scripts/pan123_mirror.sh 的 bash 里各复刻一份
 * （123 云盘用同一对象键），改一处须同步另外两处。
 */
export function releaseObjectKey(tag: string, filename: string): string {
  const m = /^(?:([a-z]+)-)?(v.+)$/.exec(tag);
  const component = m?.[1] ?? "app";
  const version = m?.[2] ?? tag;
  return `${OSS_RELEASE_PREFIX}/${version}/${component}/${filename}`;
}

function encodedPath(key: string): string {
  return `/${key.split("/").map(encodeURIComponent).join("/")}`;
}

const CDN_BASE = (CN_CDN_BASE ?? "").replace(/\/+$/, "");

/** 镜像基址与鉴权主 KEY 都配置时才启用镜像下载。 */
export const cdnConfigured = !!(CDN_BASE && CN_CDN_AUTH_KEY);

/**
 * dl-cn 镜像下载链接（阿里云 URL 鉴权 A 方式：`auth_key=<签发时间>-<rand>-<网段>-<md5>`，
 * md5 串为 `<编码后路径>-<签发时间>-<rand>-<网段>-<密钥>`）。uid 段放访客 IPv4 /24 网段标识
 * （download-gate.ts::mirrorNetworkId），ESA 边缘函数（cdn/esa-dl-cn.js）校验签名后再按访客真实 IP 比对网段，
 * 链接离开签发时的网段即 403。缓存键忽略查询串，签名不影响命中率。`.apk` / `.ipa` 由镜像回源时换成桶内
 * `.bin` 对象。调用前须保证 `cdnConfigured`。https://help.aliyun.com/zh/cdn/user-guide/type-a-signing
 */
export function cdnDownloadUrl(key: string, network: string): string {
  const path = encodedPath(key);
  const timestamp = Math.floor(Date.now() / 1000);
  const rand = randomBytes(16).toString("hex");
  const hash = createHash("md5").update(`${path}-${timestamp}-${rand}-${network}-${CN_CDN_AUTH_KEY}`).digest("hex");
  return `${CDN_BASE}${path}?auth_key=${timestamp}-${rand}-${network}-${hash}`;
}

/**
 * 生成 `ttlSec` 秒内有效的预签名 HEAD URL，用于确认资产存在且大小一致。`key` 为对外对象键，
 * 签名前换成桶内存储键（见 oss-storage.ts）。URL 路径按段编码（OSS 用解码后的资源路径校验签名）。
 * 调用前须保证 `ossConfigured`。
 */
export function presignOssHeadUrl(key: string, ttlSec: number): string {
  const storageKey = ossStorageKey(key);
  const expires = Math.floor(Date.now() / 1000) + ttlSec;
  const stringToSign = `HEAD\n\n\n${expires}\n/${OSS_BUCKET}/${storageKey}`;
  const signature = createHmac("sha1", OSS_ACCESS_KEY_SECRET ?? "")
    .update(stringToSign)
    .digest("base64");
  const query = new URLSearchParams({
    OSSAccessKeyId: OSS_ACCESS_KEY_ID ?? "",
    Expires: String(expires),
    Signature: signature,
  });
  return `https://${OSS_BUCKET}.${OSS_ENDPOINT}${encodedPath(storageKey)}?${query}`;
}
