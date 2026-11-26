/**
 * 右键菜单：即使关闭自动拦截也可以手动发送链接到 FluxDown。
 *
 * - `contextMenuEnabled` 为 false 时清空后不再创建（#445）；
 * - 配置了已验证的远程服务器时，每项下再建「本机 / 远程」两个子项（#262）；
 * - #407：视频/音频的 srcUrl 不是 http(s)（blob / MSE）时，改用 sensor 上报的
 *   右键命中元素，下载其绑定的首个可下载候选；
 * - http(s) 链接与点击下载同一路径：交给浏览器发起，由拦截层在拿到响应头后取消并发送
 *   （文件名 / 大小来自浏览器自己的网络栈）；该路径不可用 / 失败 / 超时时直接发送链接。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { t } from "@/utils/i18n";
import { MANUAL_SERVERS_KEY, loadManualServers } from "@/utils/manual-servers";
import type { DispatchTarget } from "@/utils/download-dispatch";
import { chooseManualTarget } from "./manual-target";
import { downloadCandidate } from "./downloads";
import { canCaptureLinkViaBrowser } from "./intercept";
import { linkCapture } from "./link-capture";
import { notify } from "./notify";
import type { SendChannel } from "./protocol";
import { fallbackAfterSendFailure, sendToFluxDown } from "./sender";
import { getCachedSettings, onSettingsChanged } from "./settings-cache";
import type { FluxDownSettings } from "@/utils/settings";
import { rebuildMenus, type MenuApi, type MenuItemSpec } from "./menu-build";
import { getContextTarget } from "./tab-state";
import { buildTabView } from "./tab-view";
import { allRows, rowsForElement } from "./view-model";
import { logWarn } from "@/utils/error-log";

type MenuBase =
  | "fluxdown-send-link"
  | "fluxdown-send-image"
  | "fluxdown-send-video"
  | "fluxdown-send-page"
  | "fluxdown-send-selection";

interface MenuDef {
  id: MenuBase;
  titleKey:
    | "contextMenu.sendToFluxDown"
    | "contextMenu.sendImageToFluxDown"
    | "contextMenu.sendVideoToFluxDown"
    | "contextMenu.sendPageToFluxDown"
    | "contextMenu.sendSelectionToFluxDown";
  contexts: NonNullable<Browser.contextMenus.CreateProperties["contexts"]>;
}

const MENUS: MenuDef[] = [
  { id: "fluxdown-send-link", titleKey: "contextMenu.sendToFluxDown", contexts: ["link"] },
  { id: "fluxdown-send-image", titleKey: "contextMenu.sendImageToFluxDown", contexts: ["image"] },
  { id: "fluxdown-send-video", titleKey: "contextMenu.sendVideoToFluxDown", contexts: ["video", "audio"] },
  { id: "fluxdown-send-page", titleKey: "contextMenu.sendPageToFluxDown", contexts: ["page"] },
  { id: "fluxdown-send-selection", titleKey: "contextMenu.sendSelectionToFluxDown", contexts: ["selection"] },
];

/**
 * 从选中文本中提取一个可下载的 magnet:/ed2k:/http(s) 纯文本链接（#348）。
 * 仅接受完整且合法的链接格式，避免把选区里夹带的其他文字误判为下载源。
 */
export function extractSelectionDownloadUrl(raw: string | undefined): string | null {
  if (!raw) return null;
  const text = raw.trim();
  if (!text) return null;
  // magnet 链接：xt=urn:<hash-algo>:<hash> 是必需字段
  if (/^magnet:\?xt=urn:[a-zA-Z0-9]+:[a-zA-Z0-9]+/i.test(text)) return text;
  // ed2k 链接：ed2k://|file|<name>|<size>|<32位十六进制 hash>|/
  if (/^ed2k:\/\/\|file\|[^|]+\|\d+\|[a-fA-F0-9]{32}\|\/?$/i.test(text)) return text;
  // 纯文本形式的完整 http(s) 直链
  if (/^https?:\/\/\S+$/i.test(text)) return text;
  return null;
}

/** 串行化菜单重建：初始化与设置变化可能交错，并发的 removeAll/create 会残留菜单或报重复 id。 */
let menuChain: Promise<void> = Promise.resolve();

/** 创建菜单（需在 i18n 初始化后调用以获取正确的翻译文本；设置变化时重建）。 */
export function createContextMenus(): Promise<void> {
  menuChain = menuChain.then(rebuildContextMenus);
  return menuChain;
}

/**
 * 浏览器菜单 API 适配：create 的失败只经回调里的 runtime.lastError 给出（不抛错、不 reject），
 * 不在回调里读取就会被记为「Unchecked runtime.lastError」。
 */
const browserMenus: MenuApi = {
  removeAll: () => browser.contextMenus.removeAll(),
  create(item, done) {
    browser.contextMenus.create(item, () => {
      const error = browser.runtime.lastError;
      done(error ? (error.message ?? String(error)) : null);
    });
  },
};

/** 重建的实际实现；内部吞掉所有错误，保证 menuChain 不会 reject。 */
async function rebuildContextMenus(): Promise<void> {
  if (!browser.contextMenus) {
    logWarn("[FluxDown] contextMenus API not available");
    return;
  }
  try {
    // 先读设置：removeAll 与 create 之间不再隔着存储读取，缩小与菜单恢复交错的窗口。
    const settings = await getCachedSettings();
    const { enabled } = await loadManualServers();
    const errors = await rebuildMenus(browserMenus, menuItems(settings, enabled));
    if (errors.length > 0) logWarn("[FluxDown] Failed to create context menus:", errors.join("; "));
  } catch (e) {
    logWarn("[FluxDown] Failed to create context menus:", e);
  }
}

/** 按设置列出全部菜单项：关闭右键菜单时为空；已验证远程服务器时每项下挂「本机 / 远程」子项。 */
function menuItems(settings: FluxDownSettings, chooseEachTime: boolean): MenuItemSpec[] {
  if (!settings.contextMenuEnabled) return [];
  const items: MenuItemSpec[] = [];
  for (const def of MENUS) {
    items.push({ id: def.id, title: t(def.titleKey), contexts: def.contexts });
    if (settings.remoteVerified && !chooseEachTime) {
      items.push({ id: `${def.id}-local`, parentId: def.id, title: t("menu.sendLocal"), contexts: def.contexts });
      items.push({ id: `${def.id}-remote`, parentId: def.id, title: t("menu.sendRemote"), contexts: def.contexts });
    }
  }
  return items;
}

function parseMenuId(menuItemId: string | number): { base: MenuBase; channel?: SendChannel } | null {
  const id = String(menuItemId);
  for (const def of MENUS) {
    if (id === def.id) return { base: def.id };
    if (id === `${def.id}-local`) return { base: def.id, channel: "local" };
    if (id === `${def.id}-remote`) return { base: def.id, channel: "remote" };
  }
  return null;
}

function notifyVideoUnavailable(): void {
  notify(t("notify.videoSourceUnavailableTitle"), t("notify.videoSourceUnavailableDetail"));
}

/** #407：用右键命中的元素下载其首个可下载候选；没有则返回 false。 */
async function downloadContextElement(tabId: number, channel: DispatchTarget | undefined): Promise<boolean> {
  const elementKey = getContextTarget(tabId);
  if (!elementKey) return false;
  const view = await buildTabView(tabId, { withHandoff: false });
  const row = rowsForElement(view, elementKey).find((r) => r.candidate.downloadable && r.candidate.variants.length > 0);
  if (!row) return false;
  const variantId = row.playingVariantId ?? undefined;
  const result = await downloadCandidate(tabId, row.candidate.id, variantId, channel);
  return result.success;
}

async function handleMenuClick(info: Browser.contextMenus.OnClickData, tab?: Browser.tabs.Tab): Promise<void> {
  const parsed = parseMenuId(info.menuItemId);
  if (!parsed) return; // 非 FluxDown 菜单项，忽略
  const { base } = parsed;
  const channel = await chooseManualTarget(parsed.channel);
  if (channel === null) return;
  const referrer = tab?.url || info.pageUrl || "";

  let downloadUrl: string | undefined;
  switch (base) {
    case "fluxdown-send-link":
      downloadUrl = info.linkUrl;
      break;
    case "fluxdown-send-image":
    case "fluxdown-send-video":
      downloadUrl = info.srcUrl;
      break;
    case "fluxdown-send-page":
      downloadUrl = info.pageUrl;
      break;
    case "fluxdown-send-selection": {
      // #348：选中的磁力/ed2k/http(s) 纯文本链接。
      const selectionUrl = extractSelectionDownloadUrl(info.selectionText);
      if (!selectionUrl) {
        notify(t("notify.selectionInvalidTitle"), t("notify.selectionInvalidDetail"));
        return;
      }
      const result = await sendToFluxDown({ url: selectionUrl, referrer, channel });
      if (!result.success) {
        // magnet:/ed2k: 无法回退浏览器原生下载，只提示失败。
        if (/^https?:/i.test(selectionUrl)) await fallbackAfterSendFailure(selectionUrl, undefined, false, channel);
        else notify(t("notify.sendFailed"), selectionUrl);
      }
      return;
    }
  }

  const isMedia = base === "fluxdown-send-video";
  let downloadable = false;
  if (downloadUrl) {
    try {
      // magnet / ed2k 与选区菜单一致放行（无法回退浏览器下载，失败只提示）。
      downloadable = ["http:", "https:", "ftp:", "magnet:", "ed2k:"].includes(new URL(downloadUrl).protocol);
    } catch {
      downloadable = false;
    }
  }
  if (!downloadUrl || !downloadable) {
    if (!isMedia) return;
    // blob:/MSE 源：用右键命中的元素找绑定候选，找不到才提示。
    if (tab?.id !== undefined && (await downloadContextElement(tab.id, channel))) return;
    notifyVideoUnavailable();
    return;
  }

  // 页面 SW 合成的视频地址（如 Telegram Web 的 /k/stream/…）：引擎重放只会拿到网页。
  if (isMedia && tab?.id !== undefined) {
    const view = await buildTabView(tab.id, { withHandoff: false });
    const synthesized = allRows(view).some(
      (r) => r.candidate.browserOnly && r.candidate.variants.some((v) => v.videoUrl === downloadUrl),
    );
    if (synthesized) {
      notify(t("notify.videoSourceUnavailableTitle"), t("float.browserOnlyHint"));
      return;
    }
  }

  if (
    base === "fluxdown-send-link" &&
    /^https?:/i.test(downloadUrl) &&
    (await canCaptureLinkViaBrowser()) &&
    (await linkCapture.capture({ url: downloadUrl, referrer, channel }))
  ) {
    return;
  }

  const result = await sendToFluxDown({ url: downloadUrl, referrer, channel });
  // 成功提示已收敛到 sendToFluxDown 内部统一发送，此处只处理失败回退。
  if (!result.success) {
    if (/^(https?|ftp):/i.test(downloadUrl)) await fallbackAfterSendFailure(downloadUrl, undefined, false, channel);
    else notify(t("notify.sendFailed"), downloadUrl);
  }
}

/** 同步注册菜单点击监听（MV3 要求监听器在 SW 首次执行时注册）。 */
export function installMenus(): void {
  browser.contextMenus?.onClicked?.addListener((info, tab) => {
    handleMenuClick(info, tab).catch((e) => logWarn("[FluxDown] context menu failed:", e));
  });
  // contextMenuEnabled / remoteVerified 变化后重建。
  onSettingsChanged(() => {
    void createContextMenus();
  });
  browser.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && MANUAL_SERVERS_KEY in changes) void createContextMenus();
  });
}
