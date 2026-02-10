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
 */

import { sendDownloadRequest, checkFluxDownAvailable } from '@/utils/native-messaging';
import type { DownloadRequest } from '@/utils/native-messaging';
import { loadSettings, shouldIntercept } from '@/utils/settings';
import type { DownloadItemInfo } from '@/utils/settings';
import { initI18n, t } from '@/utils/i18n';

// ===== 统计相关 =====
interface DailyStats {
  sent: number;
  failed: number;
  date: string;
}

async function getTodayStats(): Promise<DailyStats> {
  const today = new Date().toDateString();
  const result = await chrome.storage.local.get('stats');
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

  // ===== 请求头缓存 =====
  // 用 webRequest.onSendHeaders 捕获浏览器实际发出的请求头（含 Cookie、Authorization 等）
  // 这比 chrome.cookies API 更可靠，因为它捕获的是浏览器真正发出去的完整头。
  const requestHeaderCache = new Map<string, { cookies: string; headers: Record<string, string>; ts: number }>();

  // Chrome MV3: 需要 'extraHeaders' 才能看到 Cookie / Authorization 等敏感头
  try {
    chrome.webRequest.onSendHeaders.addListener(
      (details) => {
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

        // 清理 60 秒前的缓存条目
        for (const [url, entry] of requestHeaderCache) {
          if (Date.now() - entry.ts > 60_000) {
            requestHeaderCache.delete(url);
          }
        }
      },
      { urls: ['<all_urls>'] },
      ['requestHeaders', 'extraHeaders'],
    );
    console.log('[FluxDown] webRequest.onSendHeaders listener registered');
  } catch (e) {
    console.warn('[FluxDown] Failed to register webRequest listener:', e);
  }

  // ===== 右键菜单 =====
  chrome.runtime.onInstalled.addListener(async () => {
    // 确保 i18n 已初始化
    await initI18n();

    chrome.contextMenus.create({
      id: 'fluxdown-download-link',
      title: t('contextMenu.downloadLink'),
      contexts: ['link'],
    });

    chrome.contextMenus.create({
      id: 'fluxdown-download-media',
      title: t('contextMenu.downloadMedia'),
      contexts: ['image', 'video', 'audio'],
    });

    chrome.contextMenus.create({
      id: 'fluxdown-download-page',
      title: t('contextMenu.downloadPage'),
      contexts: ['page'],
    });

    console.log('[FluxDown] Context menus created');
  });

  // ===== 右键菜单点击处理 =====
  chrome.contextMenus.onClicked.addListener(async (info, tab) => {
    let url: string | undefined;

    switch (info.menuItemId) {
      case 'fluxdown-download-link':
        url = info.linkUrl;
        break;
      case 'fluxdown-download-media':
        url = info.srcUrl;
        break;
      case 'fluxdown-download-page':
        if (tab?.id) {
          // TODO: 实现全部链接提取
          notify(t('notify.featureInDev'), t('notify.batchDownloadComing'));
        }
        return;
    }

    if (url) {
      await sendToFluxDown(url, tab?.url);
    }
  });

  // ===== 下载拦截 =====
  // 缓存 onCreated 中的 downloadItem 信息（mime/fileSize/referrer 等），
  // 供 onDeterminingFilename 使用（后者的参数中信息较少）。
  const downloadItemCache = new Map<number, chrome.downloads.DownloadItem>();

  // 记录已由 onDeterminingFilename 拦截的下载 ID，避免 onCreated 重复处理
  const interceptedIds = new Set<number>();

  chrome.downloads.onCreated.addListener((downloadItem) => {
    // 缓存 downloadItem 信息，onDeterminingFilename 会用到
    downloadItemCache.set(downloadItem.id, downloadItem);

    // 30 秒后自动清理（正常情况下 onDeterminingFilename 会很快触发）
    setTimeout(() => {
      downloadItemCache.delete(downloadItem.id);
      interceptedIds.delete(downloadItem.id);
    }, 30_000);
  });

  // onDeterminingFilename 在浏览器弹出「另存为」对话框 **之前** 触发，
  // 调用 suggest({ cancel: true }) 可以在不弹出任何浏览器下载 UI 的情况下直接取消下载。
  // 这是拦截下载最可靠的时机。
  chrome.downloads.onDeterminingFilename.addListener(
    (downloadItem, suggest) => {
      // 同步部分：先做快速判断，决定是否需要拦截。
      // 注意：此回调必须同步调用 suggest()，或返回 true 表示异步调用 suggest()。
      const url = downloadItem.url;

      // 跳过 blob 和 data URL
      if (url.startsWith('blob:') || url.startsWith('data:')) {
        suggest({ filename: downloadItem.filename });
        return;
      }

      // 返回 true 表示我们会异步调用 suggest()
      // 在异步逻辑中完成拦截判断
      (async () => {
        try {
          const settings = await loadSettings();
          if (!settings.enabled) {
            suggest({ filename: downloadItem.filename });
            return;
          }

          // 合并 onCreated 缓存的额外信息（如 referrer、fileSize、mime）
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

          console.log('[FluxDown] Intercepting download (onDeterminingFilename):', {
            url,
            mime,
            filename: downloadItem.filename,
            fileSize,
            mode: settings.interceptMode,
          });

          // 标记该下载已被拦截
          interceptedIds.add(downloadItem.id);

          // 关键：取消浏览器下载，不会弹出「另存为」对话框
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

  // ===== Popup 消息处理 =====
  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    handlePopupMessage(message).then(sendResponse);
    return true; // 保持消息通道开放（异步响应）
  });

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

  // ===== Popup 消息处理逻辑 =====
  async function handlePopupMessage(message: any): Promise<any> {
    switch (message.action) {
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

      default:
        return { error: 'Unknown action' };
    }
  }

  // ===== 工具函数 =====

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
    chrome.action.setIcon({
      path: {
        16: `/icon/16${suffix}.png`,
        32: `/icon/32${suffix}.png`,
        48: `/icon/48${suffix}.png`,
        128: `/icon/128${suffix}.png`,
      },
    });
  }

  // 启动时检查连接状态
  loadSettings().then((s) => updateIcon(s.enabled));
});
