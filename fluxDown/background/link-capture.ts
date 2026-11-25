/**
 * 右键链接浏览器捕获的单例（见 utils/link-capture.ts）：菜单发起，拦截层认领。
 */
import { browser } from "wxt/browser";
import { LinkCaptureRegistry } from "@/utils/link-capture";
import { hasDownloadsApi } from "@/utils/platform";
import type { DispatchTarget } from "@/utils/download-dispatch";

export const linkCapture = new LinkCaptureRegistry<DispatchTarget>({
  download: (url) => browser.downloads.download({ url }),
  cancel: (downloadId) => browser.downloads.cancel(downloadId),
  erase: (downloadId) => browser.downloads.erase({ id: downloadId }),
});

/**
 * 认领前中断的浏览器下载交还直接发送；MV3 要求监听器在 SW 首次执行时同步注册。
 * 没有 downloads API（Firefox for Android）时不走浏览器捕获，无需监听。
 */
export function installLinkCapture(): void {
  if (!hasDownloadsApi()) return;
  browser.downloads.onChanged.addListener((delta) => {
    if (delta.state?.current !== "interrupted") return;
    browser.downloads
      .search({ id: delta.id })
      .then(([item]) => {
        if (item) linkCapture.interrupted(item);
      })
      .catch((error: unknown) => {
        console.debug("[FluxDown] link capture: failed to read interrupted download:", delta.id, error);
      });
  });
}
