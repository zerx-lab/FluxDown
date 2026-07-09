/**
 * Main World 脚本 — Fetch / XHR / Blob 拦截器
 *
 * 运行在页面的 Main World 中（与页面 JS 共享全局作用域），
 * 通过 monkey-patch 拦截页面的网络请求，捕获 HLS/DASH 清单等流媒体资源。
 *
 * 与 Content Script（Isolated World）通过 CustomEvent 通信。
 */

// cat-catch(GPL-3.0) 嗅探思路的复用：JSON 内嵌媒体 URL 深扫见 utils/media-sniff.ts 归属说明。
// 本文件新增的响应体流式部分读取为 FluxDown 自研，非 cat-catch 代码移植。
import {
  isHttpUrl,
  isSkippableCt,
  sniffManifestMagic,
  looksLikeJson,
  scanForMediaUrls,
} from "@/utils/media-sniff";
import { parseDashJson } from "@/utils/dash-manifest";
export default defineUnlistedScript(() => {
  // 防止重复注入
  if ((window as any).__fluxdown_interceptor__) return;
  (window as any).__fluxdown_interceptor__ = true;

  const FLUXDOWN_EVENT = "fluxdown-resource-detected";
  // 拦到标准 DASH JSON manifest 时派发：权威清晰度 + 视频/音频轨 URL 列表，
  // 供 Isolated World 转发给 background 存入 tab 级 manifest store。
  const FLUXDOWN_DASH_EVENT = "fluxdown-dash-manifest";

  /** 已通知过的 URL 集合（防止重复通知） */
  const notifiedUrls = new Set<string>();

  // ===== 流媒体 URL 检测 =====

  function isStreamingUrl(url: string): boolean {
    const lower = url.toLowerCase();
    return (
      lower.includes(".m3u8") ||
      lower.includes(".mpd") ||
      lower.includes("/manifest") ||
      lower.includes("/playlist")
    );
  }

  function isMediaContentType(ct: string): boolean {
    const lower = ct.toLowerCase();
    return (
      lower.startsWith("video/") ||
      lower.startsWith("audio/") ||
      lower === "application/vnd.apple.mpegurl" ||
      lower === "application/x-mpegurl" ||
      lower === "application/mpegurl" ||
      lower === "application/octet-stream-m3u8" ||
      lower === "application/dash+xml"
    );
  }

  /**
   * 检测是否为可下载资源的 Content-Type（覆盖文档/压缩包/安装包等）。
   * 用于 fetch/XHR 响应拦截，使扩展能捕获页面 JS 通过 AJAX 加载的 PDF 等资源。
   */
  function isDownloadableContentType(ct: string): boolean {
    if (isMediaContentType(ct)) return true;
    const lower = ct.toLowerCase();
    // 文档类型
    if (lower === "application/pdf") return true;
    if (lower === "application/msword") return true;
    if (lower.startsWith("application/vnd.openxmlformats-officedocument"))
      return true;
    if (lower.startsWith("application/vnd.ms-")) return true;
    if (lower === "application/epub+zip") return true;
    if (lower === "text/csv") return true;
    // 通用二进制/强制下载
    if (lower === "application/octet-stream") return true;
    if (lower === "application/x-download") return true;
    if (lower === "application/force-download") return true;
    // 压缩包
    if (lower === "application/zip") return true;
    if (lower === "application/x-rar-compressed") return true;
    if (lower === "application/x-7z-compressed") return true;
    if (lower === "application/gzip") return true;
    if (lower === "application/x-tar") return true;
    if (lower === "application/x-bzip2") return true;
    if (lower === "application/x-xz") return true;
    if (lower === "application/zstd") return true;
    // 安装包/镜像
    if (lower === "application/x-msdownload") return true;
    if (lower === "application/x-msi") return true;
    if (lower === "application/x-apple-diskimage") return true;
    if (lower === "application/vnd.debian.binary-package") return true;
    if (lower === "application/vnd.android.package-archive") return true;
    if (lower === "application/x-iso9660-image") return true;
    // 种子
    if (lower === "application/x-bittorrent") return true;
    return false;
  }

  function classifyStreamUrl(url: string): string {
    const lower = url.toLowerCase();
    if (lower.includes(".m3u8")) return "hls-manifest";
    if (lower.includes(".mpd")) return "dash-manifest";
    return "stream-unknown";
  }

  /**
   * 通知 Content Script（Isolated World）
   */
  function notify(
    type: string,
    url: string,
    contentType?: string,
    size?: number,
  ): void {
    // 去重
    const key = `${type}:${url}`;
    if (notifiedUrls.has(key)) return;
    notifiedUrls.add(key);

    // 防止集合无限增长：整体清空而非删除前半部分
    // 下游 Content Script (reportedUrls) 和 resource-store 仍有去重兜底，
    // 最坏情况是短暂的重复通知被下游过滤掉
    if (notifiedUrls.size > 500) {
      notifiedUrls.clear();
    }

    document.dispatchEvent(
      new CustomEvent(FLUXDOWN_EVENT, {
        detail: { type, url, contentType, size },
      }),
    );
  }

  // ===== 响应体清单 / JSON 深扫（cat-catch 思路增强；纯函数见 utils/media-sniff.ts）=====
  // 门槛：仅对「非媒体 CT + 非流 URL + http + 非可跳过 CT + 体积可控」的响应读体，
  // 避免大文件 / 流式响应被 clone-read 强制整体缓冲拖爆内存。
  const MAX_SNIFF_CL = 2 * 1024 * 1024; // Content-Length 门槛：>2MB 不读
  const MAX_SNIFF_READ = 512 * 1024; // 累积读上限：清单 / JSON API 均远小于此

  /** JSON.parse 后的对象若命中标准 DASH 结构，派发权威轨对事件（结构驱动，无站点特判）。 */
  function tryDashManifest(obj: unknown, url: string): void {
    try {
      const manifest = parseDashJson(obj, url);
      if (manifest) {
        document.dispatchEvent(
          new CustomEvent(FLUXDOWN_DASH_EVENT, {
            detail: { manifest, pageUrl: window.location.href },
          }),
        );
      }
    } catch {
      // 深扫异常绝不冒泡
    }
  }

  /** 对已取到的响应体文本做清单前缀判定 + JSON 内嵌媒体 URL 深扫，命中即上报。 */
  function sniffTextForMedia(
    text: string,
    url: string,
    ct: string,
    type: "fetch-detected" | "xhr-detected",
  ): void {
    try {
      if (!text) return;
      const magic = sniffManifestMagic(text.slice(0, 512));
      if (magic) {
        notify(type, url, magic);
        return;
      }
      if (text.length <= MAX_SNIFF_CL && looksLikeJson(ct, text)) {
        // 主世界原生 JSON.parse（未打补丁）；深扫结果按内层绝对 URL 上报
        const obj = JSON.parse(text);
        tryDashManifest(obj, url);
        for (const mediaUrl of scanForMediaUrls(obj, url)) {
          notify(type, mediaUrl);
        }
      }
    } catch {
      // 嗅探异常绝不冒泡
    }
  }

  /** 有界累积读取响应体（clone），最多 8 次 read 或 512KB，随后判定；fire-and-forget。 */
  async function readBodyAndSniff(
    response: Response,
    url: string,
    ct: string,
  ): Promise<void> {
    const body = response.body;
    if (!body) return;
    const reader = body.getReader();
    const chunks: Uint8Array[] = [];
    let total = 0;
    try {
      for (let i = 0; i < 8 && total < MAX_SNIFF_READ; i++) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value) {
          chunks.push(value);
          total += value.length;
        }
      }
    } finally {
      try {
        await reader.cancel();
      } catch {
        /* 取消失败无妨 */
      }
    }
    const buf = new Uint8Array(total);
    let off = 0;
    for (const c of chunks) {
      buf.set(c, off);
      off += c.length;
    }
    const text = new TextDecoder().decode(buf);
    sniffTextForMedia(text, url, ct, "fetch-detected");
  }

  // ===== 拦截 Fetch API =====

  const originalFetch = window.fetch;
  window.fetch = function (...args: Parameters<typeof fetch>) {
    let url = "";
    try {
      if (typeof args[0] === "string") {
        url = args[0];
      } else if (args[0] instanceof Request) {
        url = args[0].url;
      } else if (args[0] instanceof URL) {
        url = args[0].href;
      }
    } catch {
      // ignore
    }

    // 请求阶段：检测流媒体 URL
    try {
      if (url && isStreamingUrl(url)) {
        notify("fetch-detected", url, classifyStreamUrl(url));
      }
    } catch {
      // 嗅探绝不打断页面请求（#329 #337）
    }

    // 调用原始 fetch，检查响应
    return originalFetch.apply(this, args).then((response) => {
      try {
        const ct = response.headers.get("content-type") || "";
        const cl = response.headers.get("content-length");
        const finalUrl = response.url || url;

        if (ct && isDownloadableContentType(ct)) {
          notify(
            "fetch-detected",
            finalUrl,
            ct,
            cl ? parseInt(cl, 10) : undefined,
          );
        }
        // 检查响应 URL 是否是流媒体（可能是重定向后的 URL）
        if (finalUrl && finalUrl !== url && isStreamingUrl(finalUrl)) {
          notify("fetch-detected", finalUrl, ct || classifyStreamUrl(finalUrl));
        }

        // 响应体清单 / JSON 深扫：覆盖「通用 CT + 非 .m3u8 URL」的清单与 JSON 内嵌媒体 URL
        if (
          !isMediaContentType(ct) &&
          !isStreamingUrl(finalUrl) &&
          isHttpUrl(finalUrl) &&
          !isSkippableCt(ct) &&
          (!cl || parseInt(cl, 10) <= MAX_SNIFF_CL)
        ) {
          readBodyAndSniff(response.clone(), finalUrl, ct).catch(() => {});
        }

        // 拦截一次性 CDN 下载 URL（如蓝奏云 /ajaxm.php）
        if (url && isOneTimeDownloadApi(url)) {
          const cloned = response.clone();
          cloned
            .json()
            .then((json) => {
              notifyPreemptDownload(json, url);
            })
            .catch(() => {});
        }
      } catch {
        // 不干扰原始响应
      }
      return response;
    });
  };

  // ===== 拦截 XMLHttpRequest =====

  const originalOpen = XMLHttpRequest.prototype.open;
  const originalSend = XMLHttpRequest.prototype.send;

  XMLHttpRequest.prototype.open = function (
    method: string,
    url: string | URL,
    ...rest: any[]
  ) {
    // 嗅探绝不可干扰页面请求（#329 #337）：取 URL 与通知整体包 try/catch。
    // 旧实现对非 string/非 URL 的 url 直接取 url.href，页面以 undefined 等
    // 调用 xhr.open 时抛 "Cannot read properties of undefined (reading 'href')"，
    // 异常冒泡使原始 open 无法执行 → 打断页面 XHR（openlist 播放、微博评论）。
    try {
      let urlStr = "";
      if (typeof url === "string") urlStr = url;
      else if (url && typeof (url as any).href === "string")
        urlStr = (url as any).href;
      (this as any).__fluxdown_url = urlStr;
      if (urlStr && isStreamingUrl(urlStr)) {
        notify("xhr-detected", urlStr, classifyStreamUrl(urlStr));
      }
    } catch {
      // ignore — 嗅探异常绝不能打断页面请求
    }
    // 始终用原始 url 透传委托，保持页面语义
    return originalOpen.apply(this, [method, url, ...rest] as any);
  };

  XMLHttpRequest.prototype.send = function (...args: any[]) {
    this.addEventListener("load", function () {
      try {
        const url = (this as any).__fluxdown_url as string | undefined;
        if (!url) return;

        const ct = this.getResponseHeader("content-type") || "";
        const cl = this.getResponseHeader("content-length");
        const responseUrl = this.responseURL || url;

        if (ct && isDownloadableContentType(ct)) {
          notify(
            "xhr-detected",
            responseUrl,
            ct,
            cl ? parseInt(cl, 10) : undefined,
          );
        }
        if (responseUrl !== url && isStreamingUrl(responseUrl)) {
          notify(
            "xhr-detected",
            responseUrl,
            ct || classifyStreamUrl(responseUrl),
          );
        }

        // 响应体清单 / JSON 深扫（XHR）
        if (
          !isMediaContentType(ct) &&
          !isStreamingUrl(responseUrl) &&
          isHttpUrl(responseUrl) &&
          !isSkippableCt(ct)
        ) {
          const rt = this.responseType;
          if (rt === "" || rt === "text") {
            sniffTextForMedia(this.responseText, responseUrl, ct, "xhr-detected");
          } else if (
            rt === "json" &&
            this.response &&
            typeof this.response === "object"
          ) {
            tryDashManifest(this.response, responseUrl);
            for (const mediaUrl of scanForMediaUrls(this.response, responseUrl)) {
              notify("xhr-detected", mediaUrl);
            }
          } else if (rt === "arraybuffer" && this.response instanceof ArrayBuffer) {
            const prefix = new Uint8Array(this.response).subarray(0, 512);
            const magic = sniffManifestMagic(new TextDecoder().decode(prefix));
            if (magic) notify("xhr-detected", responseUrl, magic);
          }
        }

        // 拦截一次性 CDN 下载 URL（如蓝奏云 /ajaxm.php）
        if (isOneTimeDownloadApi(url)) {
          try {
            const json = JSON.parse(this.responseText);
            notifyPreemptDownload(json, url);
          } catch {
            /* JSON 解析失败，忽略 */
          }
        }
      } catch {
        // ignore
      }
    });

    return originalSend.apply(this, args);
  };

  // ===== 一次性 CDN 下载 URL 抢先拦截 =====
  // 针对使用 AJAX 获取一次性签名 URL 再跳转下载的网站（如蓝奏云）。
  // 通过拦截 AJAX 响应，在浏览器发起 CDN GET 之前将 URL 发给 FluxDown，
  // 并由 background 通过 declarativeNetRequest 阻断浏览器的 CDN 请求，
  // 确保 FluxDown 是第一个（也是唯一的）请求方。
  //
  // 目前支持的规则：
  //   - 蓝奏云系列 (/ajaxm.php)：响应 {zt:1, dom:"https://...", url:"/file/?token", inf:"filename"}

  /** 检测 URL 是否为已知的"一次性 CDN 下载 AJAX"端点 */
  function isOneTimeDownloadApi(url: string): boolean {
    return url.includes("/ajaxm.php");
  }

  /**
   * 解析 AJAX 响应 JSON，提取 CDN 下载 URL 并派发预抢占事件。
   * @param json  解析后的响应 JSON
   * @param apiUrl 发出请求的 API URL（用于提取 referrer）
   */
  function notifyPreemptDownload(json: any, apiUrl: string): void {
    if (!json || typeof json !== "object") return;

    // 蓝奏云格式：{zt:1, dom:"https://cdn.example.com", url:"?token", inf:"filename.ext"}
    // 页面 JS 实际拼接：dom + '/file/' + url
    if (
      json.zt === 1 &&
      typeof json.dom === "string" &&
      typeof json.url === "string"
    ) {
      const urlStr = json.url;
      let cdnUrl: string;
      if (urlStr.startsWith("http://") || urlStr.startsWith("https://")) {
        // 完整 URL，直接使用
        cdnUrl = urlStr;
      } else if (urlStr.startsWith("/")) {
        // 以路径开头（如 /file/?token）
        cdnUrl = json.dom + urlStr;
      } else {
        // 仅查询字符串（如 ?token）— 蓝奏云标准格式，路径为 /file/
        cdnUrl = json.dom + "/file/" + urlStr;
      }
      if (!cdnUrl.startsWith("http")) return;

      const key = `preempt:${cdnUrl}`;
      if (notifiedUrls.has(key)) return;
      notifiedUrls.add(key);

      document.dispatchEvent(
        new CustomEvent("fluxdown-preempt-download", {
          detail: {
            url: cdnUrl,
            filename: typeof json.inf === "string" ? json.inf : "",
            referrer: window.location.href,
          },
        }),
      );
    }
  }

  // ===== 拦截 URL.createObjectURL =====

  const originalCreateObjectURL = URL.createObjectURL;
  URL.createObjectURL = function (obj: Blob | MediaSource) {
    const blobUrl = originalCreateObjectURL.call(URL, obj);

    try {
      if (obj instanceof Blob && obj.size > 100 * 1024) {
        // Blob > 100KB，可能是有意义的媒体资源
        notify("blob-detected", blobUrl, obj.type || "", obj.size);
      }
    } catch {
      // ignore
    }

    return blobUrl;
  };

  // ===== 拦截 MediaSource API =====

  try {
    const OrigMediaSource = window.MediaSource;
    if (OrigMediaSource) {
      const origAddSourceBuffer = OrigMediaSource.prototype.addSourceBuffer;
      OrigMediaSource.prototype.addSourceBuffer = function (mimeType: string) {
        try {
          if (
            mimeType &&
            (mimeType.startsWith("video/") || mimeType.startsWith("audio/"))
          ) {
            notify("mse-detected", window.location.href, mimeType);
          }
        } catch {
          /* */
        }
        return origAddSourceBuffer.call(this, mimeType);
      };
    }
  } catch {
    /* MediaSource 不可用 */
  }

  console.log("[FluxDown] Fetch/XHR/Blob/MSE interceptor injected");
});
