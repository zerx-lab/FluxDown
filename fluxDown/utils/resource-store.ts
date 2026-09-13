/**
 * 资源存储管理模块（v2）
 *
 * 在 Background Service Worker 中维护按 tabId 分组的资源列表。
 * v2 改进：URL 归一化去重、可信度分级、按可信度+时间排序。
 */

import { browser } from "wxt/browser";
import type {
  DetectedResource,
  ResourceMessagePayload,
  ConfidenceLevel,
} from "./resource-types";
import {
  generateResourceId,
  classifyResource,
  extractFilenameFromUrl,
  isWorthShowing,
  computeConfidence,
  isNoiseUrl,
} from "./resource-types";

// ===== 核心存储 =====

const tabResources = new Map<number, Map<string, DetectedResource>>();

// ===== 可信度排序权重 =====

const CONFIDENCE_ORDER: Record<ConfidenceLevel, number> = {
  high: 3,
  medium: 2,
  low: 1,
};

/** 每 tab 资源硬上限：防深扫 / 长会话 SPA 累积把内存与排序成本推高。 */
const MAX_RESOURCES_PER_TAB = 500;

/** 达上限时淘汰一条「最低置信度、最旧」的资源，为新资源腾位（纯防御，不改去重语义）。 */
function evictLowest(resourceMap: Map<string, DetectedResource>): void {
  let victimKey: string | undefined;
  let victimScore = Infinity;
  let victimAt = Infinity;
  for (const [key, r] of resourceMap) {
    const score = CONFIDENCE_ORDER[r.confidence];
    if (score < victimScore || (score === victimScore && r.detectedAt < victimAt)) {
      victimScore = score;
      victimAt = r.detectedAt;
      victimKey = key;
    }
  }
  if (victimKey !== undefined) resourceMap.delete(victimKey);
}

// ===== 公开 API =====

/**
 * 添加检测到的资源（自动去重合并 + 可信度计算）
 * @returns 新增的资源数量
 */
export function addResources(
  tabId: number,
  pageUrl: string,
  payloads: ResourceMessagePayload[],
  cookies?: string,
  headers?: Record<string, string>,
): number {
  let resourceMap = tabResources.get(tabId);
  if (!resourceMap) {
    resourceMap = new Map();
    tabResources.set(tabId, resourceMap);
  }

  let newCount = 0;

  for (const payload of payloads) {
    // 早期过滤：噪音 URL
    if (isNoiseUrl(payload.url)) continue;

    // generateResourceId 内部已做 URL 归一化
    const id = generateResourceId(payload.url);

    const existing = resourceMap.get(id);
    if (existing) {
      mergeResource(existing, payload, cookies, headers);
      continue;
    }

    const type =
      payload.type !== "other"
        ? payload.type
        : classifyResource(payload.url, payload.mimeType);

    const size = payload.size ?? -1;

    const resource: DetectedResource = {
      id,
      url: payload.url,
      filename: payload.filename || extractFilenameFromUrl(payload.url),
      type,
      size,
      mimeType: payload.mimeType,
      quality: payload.quality,
      detectedBy: payload.detectedBy,
      detectedAt: Date.now(),
      tabId,
      pageUrl: payload.pageUrl || pageUrl,
      confidence: computeConfidence(
        type,
        size,
        payload.detectedBy,
        payload.isAttachment,
      ),
      isAttachment: payload.isAttachment,
      cookies: cookies || undefined,
      headers: headers && Object.keys(headers).length > 0 ? headers : undefined,
    };

    if (isWorthShowing(resource)) {
      if (resourceMap.size >= MAX_RESOURCES_PER_TAB) {
        evictLowest(resourceMap);
      }
      resourceMap.set(id, resource);
      newCount++;
    }
  }

  return newCount;
}

/**
 * 通过 webRequest 嗅探到的资源添加到存储
 */
export function addSniffedResource(
  tabId: number,
  url: string,
  contentType: string,
  contentLength: number,
  filename: string,
  isAttachment?: boolean,
  cookies?: string,
  headers?: Record<string, string>,
): number {
  return addResources(
    tabId,
    "",
    [
      {
        url,
        type: classifyResource(url, contentType),
        filename,
        size: contentLength > 0 ? contentLength : undefined,
        mimeType: contentType,
        detectedBy: "webRequest",
        isAttachment,
      },
    ],
    cookies,
    headers,
  );
}

/**
 * 获取指定 tab 的所有资源（按可信度 > 时间排序）
 */
export function getResourcesForTab(tabId: number): DetectedResource[] {
  const resourceMap = tabResources.get(tabId);
  if (!resourceMap) return [];
  return Array.from(resourceMap.values()).sort((a, b) => {
    // 先按可信度降序
    const confDiff =
      CONFIDENCE_ORDER[b.confidence] - CONFIDENCE_ORDER[a.confidence];
    if (confDiff !== 0) return confDiff;
    // 同可信度按时间降序
    return b.detectedAt - a.detectedAt;
  });
}

export function getResourceCountForTab(tabId: number): number {
  return tabResources.get(tabId)?.size ?? 0;
}

export function clearResourcesForTab(tabId: number): void {
  tabResources.delete(tabId);
}

export function clearAllResources(): void {
  tabResources.clear();
}

export function getTabsWithResources(): number[] {
  return Array.from(tabResources.keys());
}

// ===== Badge 更新 =====

export async function updateBadgeForTab(
  tabId: number,
  displayedCount?: number,
): Promise<void> {
  const count = displayedCount ?? getResourceCountForTab(tabId);
  const text = count > 0 ? String(count) : "";

  try {
    await browser.action?.setBadgeText({ text, tabId });
    if (count > 0) {
      await browser.action?.setBadgeBackgroundColor({
        color: "#3B82F6",
        tabId,
      });
    }
  } catch {
    // tab 可能已关闭
  }
}

// ===== 内部辅助 =====

function mergeResource(
  existing: DetectedResource,
  incoming: ResourceMessagePayload,
  cookies?: string,
  headers?: Record<string, string>,
): void {
  // 更精确的类型覆盖 other
  if (existing.type === "other" && incoming.type && incoming.type !== "other") {
    existing.type = incoming.type;
  }
  if (!existing.filename && incoming.filename) {
    existing.filename = incoming.filename;
  }
  if (existing.size <= 0 && incoming.size && incoming.size > 0) {
    existing.size = incoming.size;
  }
  if (!existing.mimeType && incoming.mimeType) {
    existing.mimeType = incoming.mimeType;
  }
  if (!existing.quality && incoming.quality) {
    existing.quality = incoming.quality;
  }
  // attachment 标记只升不降
  if (incoming.isAttachment) {
    existing.isAttachment = true;
  }
  // 合并请求头信息（webRequest 来源的更可靠，覆盖旧值）
  if (cookies && !existing.cookies) {
    existing.cookies = cookies;
  }
  if (headers && Object.keys(headers).length > 0 && !existing.headers) {
    existing.headers = headers;
  }

  // 重新计算可信度（信息更丰富后可能升级）
  const newConf = computeConfidence(
    existing.type,
    existing.size,
    existing.detectedBy,
    existing.isAttachment,
  );
  if (CONFIDENCE_ORDER[newConf] > CONFIDENCE_ORDER[existing.confidence]) {
    existing.confidence = newConf;
  }
}

// ===== Tab 生命周期管理 =====

export function initTabLifecycleListeners(): void {
  browser.tabs.onRemoved.addListener((tabId) => {
    clearResourcesForTab(tabId);
  });

  browser.tabs.onUpdated.addListener((tabId, changeInfo) => {
    if (changeInfo.status === "loading" && changeInfo.url) {
      clearResourcesForTab(tabId);
      updateBadgeForTab(tabId);
    }
  });
}
