/**
 * TabView 端口（C6）+ 徽标：
 * UI 用 `runtime.connect({name:"tab-view"})` 连接后先发 `{subscribe: tabId | "sender"}`；
 * 订阅时与该 tab 视图变化时推送 `{type:"tabView", view}`，同一 tab 150 ms 内合并为一次。
 * 同一次合并后的视图也用来刷新工具栏徽标（#180：徽标数 = 视图可见行数）。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { onCapabilities } from "./host";
import { onEngineChanged } from "./rules-store";
import { onSettingsChanged } from "./settings-cache";
import { scheduleProbeCheck } from "./probe";
import { onTabChanged } from "./tab-state";
import { buildTabView } from "./tab-view";
import { updateBadgeForTab } from "./toolbar";
import { TAB_VIEW_PORT } from "./protocol";
import type { TabViewPush, TabViewSubscribe } from "./protocol";
import { badgeCount } from "./view-model";
import { logWarn } from "@/utils/error-log";
import { MANUAL_SERVERS_KEY } from "@/utils/manual-servers";

const COALESCE_MS = 150;

const subscribers = new Map<number, Set<Browser.runtime.Port>>();
const timers = new Map<number, ReturnType<typeof setTimeout>>();
/** 已被关闭的 tab：不再为它刷新徽标。 */
const closedTabs = new Set<number>();

async function publish(tabId: number): Promise<void> {
  if (closedTabs.has(tabId)) return;
  const ports = subscribers.get(tabId);
  // 没有订阅者时只为徽标刷新：不计算 handoff（会触发 NMH capabilities 探测，可能反复拉起桌面端）。
  const view = await buildTabView(tabId, { withHandoff: ports !== undefined });
  void updateBadgeForTab(tabId, badgeCount(view));
  if (!ports) return;
  const push: TabViewPush = { type: "tabView", view };
  for (const port of ports) {
    try {
      port.postMessage(push);
    } catch {
      ports.delete(port);
    }
  }
}

function schedule(tabId: number): void {
  if (timers.has(tabId)) return;
  timers.set(
    tabId,
    setTimeout(() => {
      timers.delete(tabId);
      publish(tabId).catch((e) => logWarn("[FluxDown] tab view publish failed:", e));
    }, COALESCE_MS),
  );
}

function scheduleAllKnown(): void {
  for (const tabId of subscribers.keys()) schedule(tabId);
}

function isSubscribe(message: unknown): message is TabViewSubscribe {
  if (!message || typeof message !== "object" || !("subscribe" in message)) return false;
  const value = (message as { subscribe: unknown }).subscribe;
  return value === "sender" || typeof value === "number";
}

function unsubscribe(port: Browser.runtime.Port, tabId: number | null): void {
  if (tabId === null) return;
  const ports = subscribers.get(tabId);
  ports?.delete(port);
  if (ports && ports.size === 0) subscribers.delete(tabId);
}

/** 同步注册端口与变化监听；由 startBackground 调用一次。 */
export function installPorts(): void {
  onTabChanged((tabId) => {
    closedTabs.delete(tabId);
    schedule(tabId);
  });
  onEngineChanged(scheduleAllKnown);
  onSettingsChanged(scheduleAllKnown);
  browser.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && MANUAL_SERVERS_KEY in changes) scheduleAllKnown();
  });
  // 视图只限时等待 handoff 探测（tab-view.ts）；探测结果变化后补推，handoff 卡片随之更新。
  let lastCapabilities = "";
  onCapabilities((caps) => {
    const key = JSON.stringify(caps);
    if (key === lastCapabilities) return;
    lastCapabilities = key;
    scheduleAllKnown();
  });
  // tab-state 自己的 onRemoved 负责清状态；这里只停止该 tab 的视图推送。
  browser.tabs.onRemoved.addListener((tabId) => {
    closedTabs.add(tabId);
    clearTimeout(timers.get(tabId));
    timers.delete(tabId);
    subscribers.delete(tabId);
  });

  browser.runtime.onConnect.addListener((port) => {
    if (port.name !== TAB_VIEW_PORT) return;
    let subscribed: number | null = null;
    port.onMessage.addListener((message: unknown) => {
      if (!isSubscribe(message)) return;
      const tabId = message.subscribe === "sender" ? port.sender?.tab?.id : message.subscribe;
      if (tabId === undefined || tabId < 0) return;
      unsubscribe(port, subscribed);
      subscribed = tabId;
      let ports = subscribers.get(tabId);
      if (!ports) {
        ports = new Set();
        subscribers.set(tabId, ports);
      }
      ports.add(port);
      // 打开面板 / 侧边栏是用户正要看这个页面：页面缺少 sensor 时立即开始恢复
      scheduleProbeCheck(tabId, 0);
      // 订阅时立即推送一次，不等合并窗口。
      publish(tabId).catch((e) => logWarn("[FluxDown] tab view publish failed:", e));
    });
    port.onDisconnect.addListener(() => {
      // 页面进入 bfcache / 帧销毁时由浏览器关闭端口，原因在 runtime.lastError；不读会被记为 Unchecked 错误。
      void browser.runtime.lastError;
      unsubscribe(port, subscribed);
    });
  });
}
