/**
 * FluxDown Background Service Worker
 *
 * 职责：
 * 1. 拦截浏览器下载事件，转发给 FluxDown 桌面应用
 * 2. 注册右键菜单（发送链接到 FluxDown）
 * 3. 管理与 Native Host 的通信
 * 4. 响应 popup 的消息
 * 5. 维护拦截统计数据
 */

import { sendDownloadRequest, checkFluxDownAvailable } from '@/utils/native-messaging';
import type { DownloadRequest } from '@/utils/native-messaging';
import { loadSettings, shouldIntercept } from '@/utils/settings';
import type { DownloadItemInfo } from '@/utils/settings';

// ===== 统计相关 =====
interface DailyStats {
  intercepted: number;
  sent: number;
  failed: number;
  date: string;
}

async function getTodayStats(): Promise<DailyStats> {
  const today = new Date().toDateString();
  const result = await chrome.storage.local.get('stats');
  const stats: DailyStats = result.stats || { intercepted: 0, sent: 0, failed: 0, date: '' };

  // 跨天自动重置
  if (stats.date !== today) {
    const resetStats: DailyStats = { intercepted: 0, sent: 0, failed: 0, date: today };
    await chrome.storage.local.set({ stats: resetStats });
    return resetStats;
  }

  return stats;
}

async function incrementStat(field: 'intercepted' | 'sent' | 'failed') {
  const stats = await getTodayStats();
  stats[field]++;
  await chrome.storage.local.set({ stats });
}

export default defineBackground(() => {
  console.log('[FluxDown] Background service worker started');

  // ===== 右键菜单 =====
  chrome.runtime.onInstalled.addListener(() => {
    chrome.contextMenus.create({
      id: 'fluxdown-download-link',
      title: '使用 FluxDown 下载链接',
      contexts: ['link'],
    });

    chrome.contextMenus.create({
      id: 'fluxdown-download-media',
      title: '使用 FluxDown 下载媒体',
      contexts: ['image', 'video', 'audio'],
    });

    chrome.contextMenus.create({
      id: 'fluxdown-download-page',
      title: '使用 FluxDown 下载此页面所有链接',
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
          notify('功能开发中', '批量下载页面链接功能即将推出');
        }
        return;
    }

    if (url) {
      await sendToFluxDown(url, tab?.url);
    }
  });

  // ===== 下载拦截 =====
  chrome.downloads.onCreated.addListener(async (downloadItem) => {
    const settings = await loadSettings();

    if (!settings.enabled) return;

    const url = downloadItem.url;
    const fileSize = downloadItem.fileSize > 0 ? downloadItem.fileSize : undefined;

    // 跳过 blob 和 data URL
    if (url.startsWith('blob:') || url.startsWith('data:')) return;

    // 构建下载项信息，供综合判断
    const itemInfo: DownloadItemInfo = {
      url,
      fileSize,
      mime: downloadItem.mime || undefined,
      filename: downloadItem.filename || undefined,
    };

    // 判断是否需要拦截
    if (!shouldIntercept(itemInfo, settings)) return;

    console.log('[FluxDown] Intercepting download:', {
      url,
      mime: downloadItem.mime,
      filename: downloadItem.filename,
      fileSize,
      mode: settings.interceptMode,
    });

    // 统计：拦截
    await incrementStat('intercepted');

    // 取消浏览器的下载
    try {
      await chrome.downloads.cancel(downloadItem.id);
      chrome.downloads.erase({ id: downloadItem.id });
    } catch (e) {
      console.warn('[FluxDown] Failed to cancel download:', e);
    }

    // 发送到 FluxDown
    await sendToFluxDown(
      url,
      downloadItem.referrer,
      downloadItem.filename,
      fileSize,
      downloadItem.mime,
    );
  });

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
    const settings = await loadSettings();

    const request: DownloadRequest = {
      url,
      filename: filename || extractFilename(url),
      referrer: referrer || '',
      fileSize,
      mimeType,
    };

    console.log('[FluxDown] Sending to FluxDown app:', request);

    const response = await sendDownloadRequest(request);

    if (response.success) {
      // 统计：发送成功
      await incrementStat('sent');

      if (settings.showNotification) {
        notify('下载已发送', `${request.filename || url} 已发送到 FluxDown`);
      }
    } else {
      // 统计：失败
      await incrementStat('failed');

      notify('发送失败', `无法连接到 FluxDown 应用: ${response.message}`);
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
  function extractFilename(url: string): string {
    try {
      const pathname = new URL(url).pathname;
      const segments = pathname.split('/');
      const lastSegment = segments[segments.length - 1];
      return decodeURIComponent(lastSegment) || 'download';
    } catch {
      return 'download';
    }
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
