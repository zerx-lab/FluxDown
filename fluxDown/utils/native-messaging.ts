/**
 * HTTP 通信模块
 * 负责与 FluxDown 桌面应用通过本地 HTTP 服务器通信
 *
 * FluxDown 桌面应用在 localhost:19527 启动 HTTP 服务器，
 * 浏览器扩展直接通过 fetch() 发送请求，无需 Native Messaging。
 *
 * 使用 localhost 而非 127.0.0.1：Firefox 遵循 W3C Secure Context 规范，
 * 只将 localhost 主机名视为可信回环地址，127.0.0.1 IP 不在其中，
 * 导致从 moz-extension:// 安全上下文向 127.0.0.1 发起的 HTTP 请求
 * 被 Firefox 当作混合内容阻断。Chrome 对此更宽松，故两者均正常。
 *
 * 当应用未运行时，通过 fluxdown:// 协议唤起应用后重试 HTTP。
 */

const FLUXDOWN_BASE_URL = 'http://localhost:19527';

const RETRY_DELAYS = [1500, 2000, 3000];

export interface DownloadRequest {
  url: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string>;
  fileSize?: number;
  mimeType?: string;
}

export interface ApiResponse {
  success: boolean;
  message?: string;
  taskId?: string;
}

export interface BatchDownloadItem {
  url: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  fileSize?: number;
  mimeType?: string;
}

async function launchViaProtocol(): Promise<void> {
  try {
    const tabs = await chrome.tabs.query({ active: true, currentWindow: true });
    const tab = tabs[0];
    const tabUrl = tab?.url ?? '';
    const canInject =
      tab?.id != null &&
      tabUrl !== '' &&
      !tabUrl.startsWith('chrome://') &&
      !tabUrl.startsWith('chrome-extension://') &&
      !tabUrl.startsWith('edge://') &&
      !tabUrl.startsWith('about:') &&
      !tabUrl.startsWith('moz-extension://');

    if (canInject && tab.id != null) {
      const injectFn = () => {
        const iframe = document.createElement('iframe');
        iframe.style.display = 'none';
        iframe.src = 'fluxdown://wake';
        document.body.appendChild(iframe);
        setTimeout(() => iframe.remove(), 3000);
      };

      if (chrome.scripting?.executeScript) {
        // Chrome MV3 / Firefox MV3
        await chrome.scripting.executeScript({
          target: { tabId: tab.id },
          func: injectFn,
        });
      } else {
        // Firefox MV2 fallback
        const code = `(${injectFn.toString()})()`;
        await new Promise<void>((resolve) => {
          (chrome as any).tabs.executeScript(tab.id, { code }, () => resolve());
        });
      }
      return;
    }
  } catch {
    // iframe injection failed — fall through to tabs.create
  }

  try {
    const newTab = await chrome.tabs.create({ url: 'fluxdown://wake', active: false });
    if (newTab.id != null) {
      setTimeout(() => {
        chrome.tabs.remove(newTab.id!).catch(() => {});
      }, 2000);
    }
  } catch {
    // both methods failed
  }
}

async function httpPost(body: string): Promise<Response> {
  return fetch(`${FLUXDOWN_BASE_URL}/download`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body,
    signal: AbortSignal.timeout(3000),
  });
}

async function sendWithAutoLaunch(body: string): Promise<ApiResponse> {
  try {
    const response = await httpPost(body);
    return (await response.json()) as ApiResponse;
  } catch {
    // HTTP 失败 — 应用可能未运行
  }

  await launchViaProtocol();

  for (const delay of RETRY_DELAYS) {
    await new Promise((resolve) => setTimeout(resolve, delay));
    try {
      const response = await httpPost(body);
      return (await response.json()) as ApiResponse;
    } catch {
      // 继续重试
    }
  }

  return { success: false, message: 'FluxDown app not running' };
}

export async function sendDownloadRequest(request: DownloadRequest): Promise<ApiResponse> {
  return sendWithAutoLaunch(JSON.stringify(request));
}

export async function sendBatchDownloadRequest(items: BatchDownloadItem[]): Promise<ApiResponse> {
  if (items.length === 0) {
    return { success: false, message: 'No items' };
  }

  const joinedUrl = items.map((item) => item.url).join('\n');
  const cookies = items.find((item) => item.cookies)?.cookies || '';

  const request: DownloadRequest = {
    url: joinedUrl,
    filename: '',
    referrer: items[0]?.referrer || '',
    cookies,
  };

  return sendWithAutoLaunch(JSON.stringify(request));
}

export async function checkFluxDownAvailable(): Promise<boolean> {
  try {
    const response = await fetch(`${FLUXDOWN_BASE_URL}/ping`, {
      method: 'GET',
      signal: AbortSignal.timeout(3000),
    });
    const data = (await response.json()) as ApiResponse;
    return data.success === true;
  } catch {
    return false;
  }
}
