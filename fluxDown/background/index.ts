/**
 * FluxDown Background 入口（Chrome：Service Worker；Firefox：非持久 event page）。
 *
 * 职责：拦截浏览器下载并转发给桌面应用（见 intercept.ts 的三层防线）、被动嗅探
 * （sniff.ts + sensor 上报 + 规则引擎）、右键菜单、与桌面端通信、统计、多语言。
 *
 * `startBackground` 同步注册全部 listener（两种后台都要求事件监听器在首次执行时同步注册，
 * 休眠后由事件唤醒重新执行本入口；跨休眠的状态落 storage.session，见各模块）。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { initI18n, t } from "@/utils/i18n";
import { logWarn } from "@/utils/error-log";
import { hasCommandsApi } from "@/utils/platform";
import { installBackgroundErrorLog } from "@/utils/error-log-background";
import { loadSettings, persistSettings } from "@/utils/settings";
import type { ContentMessage } from "./protocol";
import { installBypassKey, restoreBreakerState } from "./breaker";
import { installBrowserDownloadNotices } from "./browser-downloads";
import { installCaptureTracker } from "./capture-tracker";
import {
  installIntercept,
  setInterceptListenersActive,
  syncDownloadShelfState,
} from "./intercept";
import { installLinkCapture } from "./link-capture";
import { createContextMenus, installMenus } from "./menus";
import { downloadPlayingMedia } from "./downloads";
import { installMessages } from "./messages";
import { installPageScripts } from "./page-scripts";
import { notify } from "./notify";
import { installPorts } from "./ports";
import { installProbe } from "./probe";
import { installRequestCache } from "./request-cache";
import { installRulesStore, onEngineChanged } from "./rules-store";
import {
  getCachedSettings,
  installSettingsCacheListener,
  onSettingsChanged,
} from "./settings-cache";
import { installSniff } from "./sniff";
import { loadServiceWorkerScopes } from "./sw-scopes";
import { allFrames, installTabState, setSniffLimit } from "./tab-state";
import { installTaskListeners } from "./tasks";
import { updateIcon } from "./toolbar";

const LEGACY_PENDING_QUEUE_KEY = "pendingDownloadQueue";

/** 设置变化 / 启动预热：图标、下载栏隐藏、拦截监听的注册状态、每 tab 资源上限。 */
function applyInterceptState(): void {
  getCachedSettings()
    .then((s) => {
      updateIcon(s.enabled);
      // 同类产品（IDM/Motrix/FDM）共同使用的策略：拦截启用时隐藏下载 UI。
      syncDownloadShelfState(s.enabled);
      setInterceptListenersActive(s.enabled);
      setSniffLimit(s.sniffMaxResources);
    })
    .catch((e) => {
      logWarn("[FluxDown] Settings cache warmup failed (non-fatal):", e);
    });
}

/** 规则 / 设置变化后，通知所有已登记帧重新拉取 PageConfig。 */
function broadcastPageConfigChanged(): void {
  const message: ContentMessage = { action: "pageConfigChanged" };
  for (const [tabId, frames] of allFrames()) {
    for (const frameId of frames) {
      browser.tabs.sendMessage(tabId, message, { frameId }).catch(() => {
        /* 该帧已不存在 */
      });
    }
  }
}

export function startBackground(): void {
  // 第一步：之后 background 的错误 / 未捕获异常都记入扩展错误日志，不进浏览器的扩展错误列表。
  installBackgroundErrorLog();
  console.log("[FluxDown] Background service worker started");

  installSettingsCacheListener();
  installRequestCache();
  installBypassKey();
  installTabState();
  installProbe();
  installRulesStore();
  installSniff();
  installIntercept();
  installMenus();
  installLinkCapture();
  installTaskListeners();
  installBrowserDownloadNotices();
  installCaptureTracker();
  installMessages();
  // 页面脚本管理器：onUserScriptMessage / onInstalled 等监听器必须在顶层同步注册。
  installPageScripts();
  installPorts();
  restoreBreakerState();
  // 拦截同步路径需要 SW 作用域登记表就绪，启动即恢复
  void loadServiceWorkerScopes();

  onSettingsChanged(applyInterceptState);
  onSettingsChanged(broadcastPageConfigChanged);
  onEngineChanged(broadcastPageConfigChanged);
  applyInterceptState();

  // 快捷键切换拦截开关（替代原来的 Alt+Click 绕过机制）；移动端浏览器没有 commands API。
  const handleCommand = async (command: string, tab?: Browser.tabs.Tab): Promise<void> => {
    if (command === "open-side-panel") {
      // Chromium 专用（Firefox 走内置 _execute_sidebar_action）。快捷键算用户手势，
      // 但 sidePanel.open 必须在任何 await 之前同步调用。
      const windowId = tab?.windowId;
      if (windowId !== undefined && typeof browser.sidePanel?.open === "function") {
        browser.sidePanel
          .open({ windowId })
          .catch((e) => logWarn("[FluxDown] open side panel failed:", e));
      }
      return;
    }
    if (command === "download-playing-media") {
      await downloadPlayingMedia();
      return;
    }
    if (command !== "toggle-intercept") return;
    const settings = await loadSettings();
    const enabled = !settings.enabled;
    await persistSettings({ ...settings, enabled }, settings);
    updateIcon(enabled);
    syncDownloadShelfState(enabled);
    notify(
      t("shortcut.toggleTitle"),
      enabled ? t("shortcut.interceptOn") : t("shortcut.interceptOff"),
    );
  };
  if (hasCommandsApi()) browser.commands.onCommand.addListener(handleCommand);

  // 清理已删除功能（发送失败重队列）的遗留数据。
  browser.storage.local.remove(LEGACY_PENDING_QUEUE_KEY).catch(() => {});

  // i18n 就绪后创建右键菜单（菜单标题需要翻译）。
  initI18n()
    .then(() => createContextMenus())
    .catch((e) => {
      logWarn("[FluxDown] i18n init failed (non-fatal):", e);
    });
}
