/**
 * FluxDown Background Service Worker
 *
 * 职责：
 * 1. 拦截浏览器下载事件，转发给 FluxDown 桌面应用
 * 2. 注册右键菜单（发送链接到 FluxDown）
 * 3. 管理与 Native Host 的通信
 * 4. 响应 popup 的消息
 * 5. 维护拦截统计数据
 * 6. 多语言支持
 *
 * === 下载拦截三层防线 ===
 *
 * 第一层（HTTP 响应感知）: webRequest.onHeadersReceived
 *   - 监听所有请求的响应头
 *   - 当响应包含 Content-Disposition: attachment 或 下载类 Content-Type 时，
 *     将该 URL 标记为"已知下载"，缓存 Content-Type / Content-Length / 文件名等
 *   - 为后续 onCreated 兜底提供可靠的元数据来源
 *
 * 第二层（主拦截）: downloads.onDeterminingFilename
 *   - 浏览器弹出「另存为」之前触发，suggest({ cancel: true }) 可取消下载
 *   - 最优先、最干净的拦截方式
 *   - 但对 JS location.href / meta refresh 触发的"导航转下载"存在 MV3 时序问题
 *
 * 第三层（兜底拦截）: downloads.onCreated + onChanged
 *   - onCreated 始终可靠触发，配合 onChanged 等待元数据就绪后再判断
 *   - 如果 onDeterminingFilename 在限定时间内未处理，由此层接管
 *   - 利用第一层缓存的 HTTP 响应信息来补全 downloadItem 中缺失的元数据
 */

import { sendDownloadRequest, sendBatchDownloadRequest, checkFluxDownAvailable } from '@/utils/native-messaging';
import type { DownloadRequest, BatchDownloadItem } from '@/utils/native-messaging';
import { loadSettings, shouldIntercept } from '@/utils/settings';
import type { DownloadItemInfo } from '@/utils/settings';
import { initI18n, t } from '@/utils/i18n';
import { isSniffableContentType, classifyResource, extractFilenameFromUrl } from '@/utils/resource-types';
import type { ResourceMessagePayload } from '@/utils/resource-types';
import {
  addResources,
  addSniffedResource,
  getResourcesForTab,
  getResourceCountForTab,
  clearResourcesForTab,
  updateBadgeForTab,
  initTabLifecycleListeners,
} from '@/utils/resource-store';

// ===== 统计相关 =====
interface DailyStats {
  sent: number;
  failed: number;
  date: string;
}

async function getTodayStats(): Promise<DailyStats> {
  const today = new Date().toDateString();
  const result = await chrome.storage.local.get('stats') ?? {};
  const stats: DailyStats = result.stats || { sent: 0, failed: 0, date: '' };

  // 跨天自动重置
  if (stats.date !== today) {
    const resetStats: DailyStats = { sent: 0, failed: 0, date: today };
    await chrome.storage.local.set({ stats: resetStats });
    return resetStats;
  }

  return stats;
}

async function incrementStat(field: 'sent' | 'failed') {
  const stats = await getTodayStats();
  stats[field]++;
  await chrome.storage.local.set({ stats });
}

export default defineBackground(() => {
  console.log('[FluxDown] Background service worker started');

  // 初始化 i18n
  initI18n().then(() => {
    console.log('[FluxDown] i18n initialized');
  });

  // 初始化 tab 生命周期监听器（自动清理关闭/导航的 tab 资源）
  initTabLifecycleListeners();

  // ==========================================
  // 第一层：HTTP 响应感知（webRequest 缓存）
  // ==========================================

  // 请求头缓存（Cookie / Authorization）
  const requestHeaderCache = new Map<string, { cookies: string; headers: Record<string, string>; ts: number }>();

  // 响应头缓存 —— 当 HTTP 响应指示"这是一个下载"时，缓存其元数据
  // 这是第三层兜底拦截的关键数据来源
  interface ResponseDownloadInfo {
    url: string;
    contentType: string;         // Content-Type
    contentLength: number;       // Content-Length（-1 = 未知）
    dispositionFilename: string; // 从 Content-Disposition 解析出的文件名
    ts: number;
  }
  const responseDownloadCache = new Map<string, ResponseDownloadInfo>();

  // Chrome MV3 需要 'extraHeaders' 才能看到 Cookie 等敏感头，Firefox 不需要也不识别此选项
  const sendHeadersOpts: string[] = ['requestHeaders'];
  try {
    // 先尝试带 extraHeaders（Chrome MV3），失败则降级（Firefox）
    chrome.webRequest.onSendHeaders.addListener(
      onSendHeadersHandler,
      { urls: ['<all_urls>'] },
      [...sendHeadersOpts, 'extraHeaders'] as any,
    );
    console.log('[FluxDown] webRequest.onSendHeaders listener registered (with extraHeaders)');
  } catch {
    try {
      chrome.webRequest.onSendHeaders.addListener(
        onSendHeadersHandler,
        { urls: ['<all_urls>'] },
        sendHeadersOpts,
      );
      console.log('[FluxDown] webRequest.onSendHeaders listener registered (without extraHeaders)');
    } catch (e) {
      console.warn('[FluxDown] Failed to register webRequest.onSendHeaders listener:', e);
    }
  }

  function onSendHeadersHandler(details: chrome.webRequest.WebRequestHeadersDetails) {
    if (!details.requestHeaders) return;
    const headers: Record<string, string> = {};
    let cookies = '';
    for (const h of details.requestHeaders) {
      if (h.name && h.value) {
        headers[h.name] = h.value;
        if (h.name.toLowerCase() === 'cookie') {
          cookies = h.value;
        }
      }
    }
    requestHeaderCache.set(details.url, { cookies, headers, ts: Date.now() });

    // 清理 60 秒前的缓存条目 + 强制大小上限（防止高流量页面短时间内积累过多条目）
    const now = Date.now();
    for (const [cachedUrl, entry] of requestHeaderCache) {
      if (now - entry.ts > 60_000) {
        requestHeaderCache.delete(cachedUrl);
      }
    }
    if (requestHeaderCache.size > 1000) {
      const excess = requestHeaderCache.size - 800;
      let deleted = 0;
      for (const key of requestHeaderCache.keys()) {
        if (deleted >= excess) break;
        requestHeaderCache.delete(key);
        deleted++;
      }
    }
  }

  // === 响应头监听：检测"导航转下载"场景 ===
  // 当浏览器主框架导航的响应带有 Content-Disposition: attachment 或
  // 下载类 Content-Type 时，说明这是一个"导航转下载"的请求。
  // 缓存其信息，供 onCreated 兜底拦截使用。
  try {
    chrome.webRequest.onHeadersReceived.addListener(
      (details) => {
        // 只关注主框架导航（sub_frame、xhr 等交给正常 download 流程处理）
        if (details.type !== 'main_frame') return;
        if (!details.responseHeaders) return;

        let contentType = '';
        let contentLength = -1;
        let contentDisposition = '';

        for (const h of details.responseHeaders) {
          const name = h.name.toLowerCase();
          if (name === 'content-type' && h.value) {
            contentType = h.value.split(';')[0].trim().toLowerCase();
          } else if (name === 'content-length' && h.value) {
            const parsed = parseInt(h.value, 10);
            if (!isNaN(parsed)) contentLength = parsed;
          } else if (name === 'content-disposition' && h.value) {
            contentDisposition = h.value;
          }
        }

        // 判断该响应是否会触发下载
        const isAttachment = contentDisposition.toLowerCase().startsWith('attachment');
        const isDownloadMime = isDownloadContentType(contentType);

        if (!isAttachment && !isDownloadMime) return;

        // 从 Content-Disposition 提取文件名
        const dispositionFilename = parseContentDispositionFilename(contentDisposition);

        const info: ResponseDownloadInfo = {
          url: details.url,
          contentType,
          contentLength,
          dispositionFilename,
          ts: Date.now(),
        };

        responseDownloadCache.set(details.url, info);
        console.log('[FluxDown] Detected download-triggering response (onHeadersReceived):', info);

        // 60 秒后自动清理
        setTimeout(() => responseDownloadCache.delete(details.url), 60_000);

        // 同时将 main_frame 下载资源加入嗅探面板
        // （资源嗅探层只监听 media/xhr/object/other，main_frame 会绕过它）
        if (details.tabId >= 0) {
          const added = addSniffedResource(
            details.tabId,
            details.url,
            contentType,
            contentLength,
            dispositionFilename,
            isAttachment,
          );
          if (added > 0) {
            updateBadgeForTab(details.tabId);
            notifyContentScript(details.tabId);
          }
        }
      },
      { urls: ['<all_urls>'] },
      ['responseHeaders'],
    );
    console.log('[FluxDown] webRequest.onHeadersReceived listener registered');
  } catch (e) {
    console.warn('[FluxDown] Failed to register webRequest.onHeadersReceived listener:', e);
  }

  // ==========================================
  // 资源嗅探层：监听所有 media / XHR 类型请求的响应头
  // 检测可下载的媒体资源，加入资源列表供 UI 展示
  // ==========================================
  try {
    chrome.webRequest.onHeadersReceived.addListener(
      (details) => {
        // 跳过无效或非 tab 请求
        if (details.tabId < 0 || !details.responseHeaders) return;

        // 跳过非成功响应（重定向、客户端/服务器错误）
        if (details.statusCode < 200 || details.statusCode >= 400) return;

        let contentType = '';
        let contentLength = -1;
        let contentDisposition = '';

        for (const h of details.responseHeaders) {
          const name = h.name.toLowerCase();
          if (name === 'content-type' && h.value) {
            contentType = h.value.split(';')[0].trim().toLowerCase();
          } else if (name === 'content-length' && h.value) {
            const parsed = parseInt(h.value, 10);
            if (!isNaN(parsed)) contentLength = parsed;
          } else if (name === 'content-disposition' && h.value) {
            contentDisposition = h.value;
          }
        }

        // 判断是否是有价值的资源
        const isSniffable = isSniffableContentType(contentType);
        const isAttachment = contentDisposition.toLowerCase().startsWith('attachment');

        if (!isSniffable && !isAttachment) return;

        // 提取文件名
        let filename = '';
        if (contentDisposition) {
          filename = parseContentDispositionFilename(contentDisposition);
        }
        if (!filename) {
          filename = extractFilenameFromUrl(details.url);
        }

        // 添加到资源存储（传递 isAttachment 标记用于可信度计算）
        const added = addSniffedResource(
          details.tabId,
          details.url,
          contentType,
          contentLength,
          filename,
          isAttachment,
        );

        if (added > 0) {
          // 更新 Badge
          updateBadgeForTab(details.tabId);
          // 推送给 Content Script UI
          notifyContentScript(details.tabId);
        }
      },
      { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'object', 'other', 'sub_frame'] },
      ['responseHeaders'],
    );
    console.log('[FluxDown] Resource sniffer (onHeadersReceived for media) registered');
  } catch (e) {
    console.warn('[FluxDown] Failed to register resource sniffer:', e);
  }

  /**
   * 向指定 tab 的 Content Script 推送最新资源列表
   */
  async function notifyContentScript(tabId: number): Promise<void> {
    const resources = getResourcesForTab(tabId);
    try {
      await chrome.tabs.sendMessage(tabId, {
        action: 'resourcesUpdated',
        resources,
      });
    } catch {
      // Content script 可能还未注入
    }
  }

  // ==========================================
  // 第二层 + 第三层：下载事件拦截
  // ==========================================

  const downloadItemCache = new Map<number, chrome.downloads.DownloadItem>();
  const handledDownloads = new Map<number, 'primary' | 'fallback'>();
  // Alt+Click 绕过令牌：URL → 过期时间戳，15 秒内有效
  const bypassTokens = new Map<string, number>();

  // Firefox 不支持 onDeterminingFilename，兜底层是唯一拦截路径，
  // 需要更长等待让浏览器填充 downloadItem 元数据
  const hasDeterminingFilename = !!chrome.downloads.onDeterminingFilename;

  // === 第三层：onCreated 兜底 + onChanged 元数据补全 ===
  chrome.downloads.onCreated.addListener((downloadItem) => {
    const downloadId = downloadItem.id;
    const url = downloadItem.url;

    console.log('[FluxDown] onCreated:', { id: downloadId, url, mime: downloadItem.mime, filename: downloadItem.filename });

    // 缓存 downloadItem 信息，onDeterminingFilename 会用到
    downloadItemCache.set(downloadId, downloadItem);

    // 跳过 blob 和 data URL
    if (url.startsWith('blob:') || url.startsWith('data:')) return;

    // 启动兜底计时器
    // 给 onDeterminingFilename 一个处理窗口，超时后由 onCreated 兜底
    //
    // 关键点：不使用固定的"猜测"超时，而是利用 onChanged 获取完整元数据后再判断。
    // 这里只是一个启动延迟，等 onDeterminingFilename 有机会先处理。
    // 如果 onDeterminingFilename 已处理，兜底逻辑会跳过。
    //
    // 注意：我们注册一个 onChanged 监听器，一旦 downloadItem 的 filename 或 mime
    // 字段被填充（说明浏览器已解析完响应头），就可以做出更准确的判断。
    startFallbackInterception(downloadId, downloadItem);

    // 30 秒后全面清理
    setTimeout(() => {
      downloadItemCache.delete(downloadId);
      handledDownloads.delete(downloadId);
    }, 30_000);
  });

  /**
   * 兜底拦截入口。策略：
   *
   * 1. 立即检查 responseDownloadCache（第一层的 HTTP 响应缓存），
   *    如果命中说明这是已确认的"导航转下载"，可以直接拦截，不必等待。
   *
   * 2. 如果缓存未命中，等待 150ms（给 onDeterminingFilename 机会先处理），
   *    然后用 chrome.downloads.search() 查询最新的 downloadItem 元数据，
   *    获取浏览器已解析的 filename / mime / fileSize，再做判断。
   */
  async function startFallbackInterception(downloadId: number, originalItem: chrome.downloads.DownloadItem) {
    const url = originalItem.url;

    console.log('[FluxDown] startFallbackInterception:', { id: downloadId, url, cacheHit: responseDownloadCache.has(url), cacheKeys: [...responseDownloadCache.keys()] });

    // === 路径 A：检查 HTTP 响应缓存（即时判断，不等待） ===
    const responseCached = responseDownloadCache.get(url);
    if (responseCached) {
      // 有 onDeterminingFilename 时给它一个短窗口先处理（suggest cancel 更干净），
      // 没有时（Firefox）直接拦截，减少用户看到浏览器下载 UI 闪烁的概率
      await sleep(hasDeterminingFilename ? 50 : 10);
      if (handledDownloads.has(downloadId)) return;

      console.log('[FluxDown] Fallback (path A - response cache hit):', {
        id: downloadId,
        url,
        contentType: responseCached.contentType,
        contentLength: responseCached.contentLength,
        dispositionFilename: responseCached.dispositionFilename,
      });

      const settings = await loadSettings();
      if (!settings.enabled) return;

      // 用响应头缓存的信息构造 DownloadItemInfo
      const itemInfo: DownloadItemInfo = {
        url,
        fileSize: responseCached.contentLength > 0 ? responseCached.contentLength : undefined,
        mime: responseCached.contentType || undefined,
        filename: responseCached.dispositionFilename || originalItem.filename || undefined,
      };

      // 检查 Alt+Click 绕过令牌（路径 A）
      const bypassA = bypassTokens.get(url);
      if (bypassA && bypassA > Date.now()) {
        bypassTokens.delete(url);
        return;
      }

      const interceptA = shouldIntercept(itemInfo, settings);
      console.log('[FluxDown] Path A shouldIntercept:', interceptA, itemInfo);
      if (!interceptA) return;

      await executeFallbackIntercept(downloadId, url, originalItem.referrer, itemInfo);
      responseDownloadCache.delete(url);
      return;
    }

    // === 路径 B：响应缓存未命中 — 等待后查询最新元数据 ===
    // 有 onDeterminingFilename 时等 150ms 让它优先处理；
    // Firefox 无此 API，兜底是唯一路径，等 300ms 让浏览器充分填充元数据。
    // 注意：Firefox 中 onCreated 先于 onHeadersReceived 触发，等待后需再次检查缓存。
    await sleep(hasDeterminingFilename ? 150 : 300);
    if (handledDownloads.has(downloadId)) {
      console.log('[FluxDown] Path B: already handled after sleep, skip');
      return;
    }

    // Firefox 时序补偿：onCreated 先于 onHeadersReceived 触发，sleep 期间缓存可能已被填充
    const responseCachedLate = responseDownloadCache.get(url);
    if (responseCachedLate) {
      console.log('[FluxDown] Path B: late cache hit for', url);
      const settings = await loadSettings();
      if (!settings.enabled) return;

      const itemInfo: DownloadItemInfo = {
        url,
        fileSize: responseCachedLate.contentLength > 0 ? responseCachedLate.contentLength : undefined,
        mime: responseCachedLate.contentType || undefined,
        filename: responseCachedLate.dispositionFilename || originalItem.filename || undefined,
      };

      const bypassLate = bypassTokens.get(url);
      if (bypassLate && bypassLate > Date.now()) {
        bypassTokens.delete(url);
        return;
      }

      const interceptLate = shouldIntercept(itemInfo, settings);
      console.log('[FluxDown] Path B late shouldIntercept:', interceptLate, itemInfo);
      if (!interceptLate) return;
      if (handledDownloads.has(downloadId)) return;

      await executeFallbackIntercept(downloadId, url, originalItem.referrer, itemInfo);
      responseDownloadCache.delete(url);
      return;
    }

    // 用 chrome.downloads.search 查询最新状态（此时浏览器可能已解析了响应头）
    let freshItems: chrome.downloads.DownloadItem[];
    try {
      freshItems = (await chrome.downloads.search({ id: downloadId })) ?? [];
    } catch (e) {
      console.warn('[FluxDown] Path B: downloads.search threw:', e);
      return;
    }

    if (freshItems.length === 0) {
      console.warn('[FluxDown] Path B: freshItems empty for id:', downloadId, '(download erased or search failed)');
      // 兜底：使用 originalItem 数据直接判断（Firefox search 可能返回空）
      const settingsFallback = await loadSettings();
      if (!settingsFallback.enabled) return;
      const itemInfoFallback: DownloadItemInfo = {
        url,
        fileSize: originalItem.fileSize > 0 ? originalItem.fileSize : undefined,
        mime: originalItem.mime || undefined,
        filename: originalItem.filename || undefined,
      };
      const bypassFallback = bypassTokens.get(url);
      if (bypassFallback && bypassFallback > Date.now()) { bypassTokens.delete(url); return; }
      const interceptFallback = shouldIntercept(itemInfoFallback, settingsFallback);
      console.log('[FluxDown] Path B fallback shouldIntercept:', interceptFallback, itemInfoFallback);
      if (!interceptFallback) return;
      if (handledDownloads.has(downloadId)) return;
      await executeFallbackIntercept(downloadId, url, originalItem.referrer, itemInfoFallback);
      return;
    }

    if (handledDownloads.has(downloadId)) {
      console.log('[FluxDown] Path B: already handled after search, skip');
      return;
    }

    const freshItem = freshItems[0];
    console.log('[FluxDown] Path B freshItem:', { state: freshItem.state, mime: freshItem.mime, filename: freshItem.filename, fileSize: freshItem.fileSize });

    // 如果下载已经完成或被取消了，不处理
    if (freshItem.state === 'complete' || (freshItem as any).state === 'interrupted') {
      console.log('[FluxDown] Path B: download already complete/interrupted, skip');
      return;
    }

    const settings = await loadSettings();
    if (!settings.enabled) return;

    const mime = freshItem.mime || originalItem.mime || undefined;
    const fileSize = (freshItem.fileSize > 0 ? freshItem.fileSize : undefined)
      ?? (originalItem.fileSize > 0 ? originalItem.fileSize : undefined);
    const filename = freshItem.filename || originalItem.filename || undefined;

    const itemInfo: DownloadItemInfo = {
      url: freshItem.url || url,
      fileSize,
      mime,
      filename,
    };

    // 检查 Alt+Click 绕过令牌（路径 B）
    const bypassB = bypassTokens.get(url);
    if (bypassB && bypassB > Date.now()) {
      bypassTokens.delete(url);
      return;
    }

    const interceptB = shouldIntercept(itemInfo, settings);
    console.log('[FluxDown] Path B shouldIntercept:', interceptB, itemInfo);
    if (!interceptB) return;

    // 最后一次检查——避免和 onDeterminingFilename 竞态
    if (handledDownloads.has(downloadId)) return;

    console.log('[FluxDown] Fallback (path B - search query):', {
      id: downloadId,
      url: itemInfo.url,
      mime,
      filename,
      fileSize,
    });

    await executeFallbackIntercept(downloadId, itemInfo.url, freshItem.referrer || originalItem.referrer, itemInfo);
  }

  /**
   * 执行兜底拦截：cancel + erase + 发送到 FluxDown
   */
  async function executeFallbackIntercept(
    downloadId: number,
    url: string,
    referrer: string | undefined,
    itemInfo: DownloadItemInfo,
  ) {
    // 标记为 fallback 已处理
    handledDownloads.set(downloadId, 'fallback');

    // cancel + erase（替代 suggest({ cancel: true })）
    try {
      await chrome.downloads.cancel(downloadId);
    } catch (e) {
      console.warn('[FluxDown] Fallback: failed to cancel download:', e);
    }
    try {
      chrome.downloads.erase({ id: downloadId });
    } catch (e) {
      console.warn('[FluxDown] Fallback: failed to erase download:', e);
    }

    // 发送到 FluxDown
    const cleanFilename = extractCleanFilename(itemInfo.filename, url);
    await sendToFluxDown(url, referrer, cleanFilename, itemInfo.fileSize, itemInfo.mime);
  }

  // === 第二层：onDeterminingFilename（主拦截） ===
  // 在浏览器弹出「另存为」对话框之前触发，
  // suggest({ cancel: true }) 可以在不弹出任何浏览器下载 UI 的情况下直接取消下载。
  // Firefox 不支持此 API，完全依赖第三层兜底拦截
  if (chrome.downloads.onDeterminingFilename) chrome.downloads.onDeterminingFilename.addListener(
    (downloadItem, suggest) => {
      const url = downloadItem.url;

      // 跳过 blob 和 data URL
      if (url.startsWith('blob:') || url.startsWith('data:')) {
        suggest({ filename: downloadItem.filename });
        return;
      }

      // 如果已被兜底层处理，直接取消（不重复发送）
      if (handledDownloads.get(downloadItem.id) === 'fallback') {
        console.log('[FluxDown] onDeterminingFilename: already handled by fallback, cancelling:', downloadItem.id);
        suggest({ cancel: true });
        return;
      }

      // 异步判断
      (async () => {
        try {
          // 再次检查兜底状态（await 期间可能被兜底层抢先处理了）
          if (handledDownloads.get(downloadItem.id) === 'fallback') {
            suggest({ cancel: true });
            return;
          }

          const settings = await loadSettings();
          if (!settings.enabled) {
            suggest({ filename: downloadItem.filename });
            return;
          }

          // 检查 Alt+Click 绕过令牌
          const bypass = bypassTokens.get(url);
          if (bypass && bypass > Date.now()) {
            bypassTokens.delete(url);
            // 必须标记为已处理，否则兜底层（onCreated）会在令牌消费后再次拦截，导致双重下载
            handledDownloads.set(downloadItem.id, 'primary');
            suggest({ filename: downloadItem.filename });
            return;
          }

          // 合并 onCreated 缓存的额外信息
          const cached = downloadItemCache.get(downloadItem.id);
          const mime = downloadItem.mime || cached?.mime || undefined;
          const fileSize = (downloadItem.fileSize > 0 ? downloadItem.fileSize : undefined)
            ?? (cached && cached.fileSize > 0 ? cached.fileSize : undefined);
          const referrer = cached?.referrer || undefined;

          const itemInfo: DownloadItemInfo = {
            url,
            fileSize,
            mime,
            filename: downloadItem.filename || undefined,
          };

          if (!shouldIntercept(itemInfo, settings)) {
            suggest({ filename: downloadItem.filename });
            return;
          }

          // 最后一次竞态检查
          if (handledDownloads.has(downloadItem.id)) {
            suggest({ cancel: true });
            return;
          }

          console.log('[FluxDown] Intercepting download (onDeterminingFilename):', {
            url,
            mime,
            filename: downloadItem.filename,
            fileSize,
            mode: settings.interceptMode,
          });

          // 标记为主拦截已处理
          handledDownloads.set(downloadItem.id, 'primary');

          // 取消浏览器下载
          suggest({ cancel: true });

          // 清理下载记录
          try {
            chrome.downloads.erase({ id: downloadItem.id });
          } catch (e) {
            console.warn('[FluxDown] Failed to erase download:', e);
          }

          // 发送到 FluxDown
          const cleanFilename = extractCleanFilename(downloadItem.filename, url);
          await sendToFluxDown(url, referrer, cleanFilename, fileSize, mime);
        } catch (e) {
          console.error('[FluxDown] Error in onDeterminingFilename handler:', e);
          // 出错时放行下载，不阻塞用户
          suggest({ filename: downloadItem.filename });
        } finally {
          downloadItemCache.delete(downloadItem.id);
        }
      })();

      // 返回 true 表示 suggest 将被异步调用
      return true;
    },
  );

  // ===== 消息处理（Popup + Content Script） =====
  //
  // 直接返回 Promise，而非 sendResponse + return true。
  //
  // 原因：Firefox MV2 中 "return true + 异步 sendResponse" 模式不可靠——
  // sendResponse 被调用后响应值经常被丢弃，popup 收到 undefined。
  // 返回 Promise 是 Firefox 原生支持的正确方式，Chrome 99+（含 MV3）同样支持。
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  chrome.runtime.onMessage.addListener((message, sender, _sendResponse) => {
    return handleMessage(message, sender).catch((_err) => ({ error: String(_err) })) as any;
  });

  // ===== 下载此页面所有链接 =====
  // ===== 核心：发送下载请求到 FluxDown App =====
  async function sendToFluxDown(
    url: string,
    referrer?: string,
    filename?: string,
    fileSize?: number,
    mimeType?: string,
  ) {
    // === 提取认证信息（Cookie / Authorization 等） ===
    // 策略 1：从 webRequest 缓存获取（最可靠 — 浏览器真正发出的请求头）
    let cookieString = '';
    const cached = requestHeaderCache.get(url);
    if (cached) {
      cookieString = cached.cookies;
      console.log('[FluxDown] Cookies from webRequest cache:', cookieString.length, 'chars');
      requestHeaderCache.delete(url); // 使用后清理
    }

    // 策略 2：通过 chrome.cookies API 提取（兜底）
    if (!cookieString) {
      try {
        const cookies = await chrome.cookies.getAll({ url });
        cookieString = cookies.map((c) => `${c.name}=${c.value}`).join('; ');
        console.log('[FluxDown] Cookies from cookies API:', cookies.length, 'cookies,', cookieString.length, 'chars');
      } catch (e) {
        console.warn('[FluxDown] Failed to extract cookies via API:', e);
      }
    }

    if (!cookieString) {
      console.log('[FluxDown] No cookies available for URL:', url);
    }

    const request: DownloadRequest = {
      url,
      filename: filename || '',
      referrer: referrer || '',
      cookies: cookieString,
      fileSize,
      mimeType,
    };

    console.log('[FluxDown] Sending to FluxDown app:', request);

    const response = await sendDownloadRequest(request);

    if (response.success) {
      // 统计：接管成功
      await incrementStat('sent');
    } else {
      // 统计：接管失败
      await incrementStat('failed');

      notify(
        t('notify.sendFailed'),
        t('notify.connectionFailed', { message: response.message }),
      );
    }
  }

  // ===== 统一消息处理（Popup + Content Script） =====
  async function handleMessage(message: any, sender: chrome.runtime.MessageSender): Promise<any> {
    switch (message.action) {
      // --- Popup 消息 ---
      case 'getStatus': {
        const available = await checkFluxDownAvailable();
        const settings = await loadSettings();
        return { connected: available, settings };
      }

      case 'toggleEnabled': {
        const currentSettings = await loadSettings();
        const newEnabled = !currentSettings.enabled;
        await chrome.storage.sync.set({
          settings: { ...currentSettings, enabled: newEnabled },
        });
        updateIcon(newEnabled);
        return { enabled: newEnabled };
      }

      case 'updateSettings': {
        const currentSettings = await loadSettings();
        const merged = { ...currentSettings, ...message.settings };
        await chrome.storage.sync.set({ settings: merged });
        return { success: true, settings: merged };
      }

      case 'checkConnection': {
        const isAvailable = await checkFluxDownAvailable();
        return { connected: isAvailable };
      }

      // --- Alt+Click 绕过令牌写入 ---
      case 'addBypassToken': {
        const bypassUrl = message.url as string;
        if (bypassUrl) {
          bypassTokens.set(bypassUrl, Date.now() + 15_000);
        }
        return { success: true };
      }

      // --- Content Script: 资源检测上报 ---
      case 'resourceDetected': {
        const tabId = sender.tab?.id;
        if (!tabId || tabId < 0) return { success: false };

        const pageUrl = sender.tab?.url || sender.url || '';
        const payloads: ResourceMessagePayload[] = message.resources || [];

        if (payloads.length === 0) return { success: true, added: 0 };

        const added = addResources(tabId, pageUrl, payloads);
        if (added > 0) {
          await updateBadgeForTab(tabId);
          await notifyContentScript(tabId);
        }
        return { success: true, added };
      }

      // --- Content Script UI: 请求当前 tab 的资源列表 ---
      case 'getResources': {
        const tabId = sender.tab?.id;
        if (!tabId || tabId < 0) return { resources: [] };
        return { resources: getResourcesForTab(tabId) };
      }

      // --- Content Script UI / Popup: 触发单个资源下载 ---
      case 'downloadResource': {
        const url = message.url as string;
        if (!url) return { success: false, message: 'No URL' };
        await sendToFluxDown(
          url,
          message.referrer,
          message.filename,
          message.fileSize,
          message.mimeType,
        );
        return { success: true };
      }

      // --- Content Script UI: 批量下载多个资源 ---
      // 将所有选中资源的 URL 合并为一个请求发送给桌面应用，
      // 由 Flutter 端的快速下载对话框按换行符拆分后批量创建任务。
      // 不应循环发送多次请求，而是一次性添加全部。
      case 'batchDownload': {
        const items = message.items as Array<{
          url: string;
          referrer?: string;
          filename?: string;
          fileSize?: number;
          mimeType?: string;
        }>;
        if (!Array.isArray(items) || items.length === 0) {
          return { success: false, message: 'No items' };
        }

        // 为每个 URL 提取 cookies，构建 BatchDownloadItem 列表
        const batchItems: BatchDownloadItem[] = [];
        for (const item of items) {
          let cookieString = '';
          const cached = requestHeaderCache.get(item.url);
          if (cached) {
            cookieString = cached.cookies;
            requestHeaderCache.delete(item.url);
          }
          if (!cookieString) {
            try {
              const cookies = await chrome.cookies.getAll({ url: item.url });
              cookieString = cookies.map((c) => `${c.name}=${c.value}`).join('; ');
            } catch { /* ignore */ }
          }
          batchItems.push({
            url: item.url,
            referrer: item.referrer || '',
            filename: item.filename,
            cookies: cookieString,
            fileSize: item.fileSize,
            mimeType: item.mimeType,
          });
        }

        // 单次 HTTP POST 发送所有 URL（用换行符连接）
        const response = await sendBatchDownloadRequest(batchItems);
        if (response.success) {
          await incrementStat('sent');
        } else {
          await incrementStat('failed');
        }
        return { success: response.success, sent: items.length };
      }

      // --- Popup: 切换资源面板显示（发消息给当前活跃 tab 的 Content Script） ---
      case 'toggleResourcePanel': {
        try {
          const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
          if (activeTab?.id) {
            await chrome.tabs.sendMessage(activeTab.id, { action: 'toggleResourcePanel' });
          }
        } catch {
          // tab 可能未注入 content script
        }
        return { success: true };
      }

      default:
        return { error: 'Unknown action' };
    }
  }

  // ===== 工具函数 =====

  function sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  /**
   * 判断 Content-Type 是否为"下载类型"（即浏览器会将导航转为下载的类型）
   */
  function isDownloadContentType(contentType: string): boolean {
    const ct = contentType.toLowerCase();
    const downloadTypes = [
      'application/octet-stream',
      'application/x-download',
      'application/force-download',
      'application/zip',
      'application/x-rar-compressed',
      'application/x-7z-compressed',
      'application/gzip',
      'application/x-tar',
      'application/x-bzip2',
      'application/x-xz',
      'application/x-msdownload',
      'application/x-msi',
      'application/x-apple-diskimage',
      'application/vnd.debian.binary-package',
      'application/x-iso9660-image',
      'application/x-raw-disk-image',
      'application/pdf',
      'application/vnd.android.package-archive',
      'application/x-bittorrent',
    ];
    // 精确匹配 + 前缀匹配
    if (downloadTypes.includes(ct)) return true;
    if (ct.startsWith('video/') || ct.startsWith('audio/')) return true;
    if (ct.startsWith('application/vnd.openxmlformats-officedocument')) return true;
    if (ct.startsWith('application/vnd.ms-')) return true;
    return false;
  }

  /**
   * 从 Content-Disposition 头解析文件名
   *
   * 支持格式：
   * - Content-Disposition: attachment; filename="report.pdf"
   * - Content-Disposition: attachment; filename=report.pdf
   * - Content-Disposition: attachment; filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf
   */
  function parseContentDispositionFilename(disposition: string): string {
    if (!disposition) return '';

    // 优先尝试 filename*（RFC 5987 编码）
    const starMatch = disposition.match(/filename\*\s*=\s*(?:UTF-8|utf-8)'[^']*'(.+?)(?:;|$)/i);
    if (starMatch) {
      try {
        return decodeURIComponent(starMatch[1].trim());
      } catch {
        // fallthrough
      }
    }

    // 再尝试 filename="..."（带引号）
    const quotedMatch = disposition.match(/filename\s*=\s*"(.+?)"/i);
    if (quotedMatch) {
      return quotedMatch[1];
    }

    // 最后尝试 filename=...（无引号）
    const plainMatch = disposition.match(/filename\s*=\s*([^\s;]+)/i);
    if (plainMatch) {
      return plainMatch[1];
    }

    return '';
  }

  /**
   * 从浏览器的 downloadItem.filename（本地保存路径）和 URL 中提取有意义的文件名。
   *
   * 策略：
   * 1. 如果浏览器给出的 filename 有合法扩展名 → 使用它（浏览器已解析了 Content-Disposition）
   * 2. 否则尝试从 URL 路径提取
   * 3. 如果都无法获得有意义的文件名 → 返回空字符串，交给 Rust 引擎通过 HTTP 探测获取
   */
  function extractCleanFilename(browserFilename: string | undefined, url: string): string {
    // 从浏览器的本地路径中提取纯文件名
    if (browserFilename) {
      // downloadItem.filename 是完整路径，如 "C:\Users\xxx\Downloads\report.pdf"
      // 或 "/home/user/Downloads/report.pdf"
      const basename = browserFilename.split(/[/\\]/).pop() || '';
      if (basename && looksLikeRealFilename(basename)) {
        return basename;
      }
    }

    // 从 URL 路径提取
    try {
      const pathname = new URL(url).pathname;
      const segments = pathname.split('/');
      const lastSegment = decodeURIComponent(segments[segments.length - 1] || '');
      if (lastSegment && looksLikeRealFilename(lastSegment)) {
        return lastSegment;
      }
    } catch {
      // ignore
    }

    // 无法确定有意义的文件名，返回空字符串
    // Rust 端会通过 HTTP HEAD/GET 探测 Content-Disposition 获取真实文件名
    return '';
  }

  /**
   * 判断一个文件名是否看起来像真实的文件名（而非 CDN hash / UUID / 无意义路径段）
   *
   * 真实文件名特征：有常见扩展名，如 "report.pdf", "video.mp4"
   * 非真实文件名：纯 hash "a1b2c3d4e5f6", UUID "550e8400-e29b-41d4-a716-446655440000",
   *               无扩展名 "download", 单字母段 "f", 短 ID "j5g6z92sied"
   */
  function looksLikeRealFilename(name: string): boolean {
    // 必须包含扩展名（至少一个点，且点后有 1-10 个字母/数字）
    const extMatch = name.match(/\.([a-zA-Z0-9]{1,10})$/);
    if (!extMatch) return false;

    // 排除看起来像网页路径的扩展名
    const webExts = ['html', 'htm', 'php', 'asp', 'aspx', 'jsp', 'cgi'];
    if (webExts.includes(extMatch[1].toLowerCase())) return false;

    return true;
  }

  function notify(title: string, message: string) {
    chrome.notifications.create({
      type: 'basic',
      iconUrl: '/icon/128.png',
      title: `FluxDown - ${title}`,
      message,
    });
  }

  function updateIcon(enabled: boolean) {
    const suffix = enabled ? '' : '-disabled';
    const iconPath = {
      16: `/icon/16${suffix}.png`,
      32: `/icon/32${suffix}.png`,
      48: `/icon/48${suffix}.png`,
      128: `/icon/128${suffix}.png`,
    };
    // chrome.action (MV3) / chrome.browserAction (Firefox MV2)
    const api = chrome.action ?? (chrome as any).browserAction;
    if (api) api.setIcon({ path: iconPath });
  }

  // 启动时检查连接状态
  loadSettings().then((s) => updateIcon(s.enabled));
});
