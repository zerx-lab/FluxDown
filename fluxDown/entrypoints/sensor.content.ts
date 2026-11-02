/**
 * Sensor（ISOLATED world，document_start，allFrames）
 *
 * 职责：
 * 1. 与 MAIN world 钩子（page/hooks.ts）桥接：下发 PageConfig，接收资源 / 清单 / body / preempt / pageUrl 上报。
 * 2. DOM 扫描 + MutationObserver + 声明式 dom 规则（迁自旧 resource-detector.content.ts）。
 * 3. 本帧媒体元素跟踪（page/media-tracker.ts）与右键目标记录。
 * 4. 响应 background 的 getPageCharset / collectLinks / pageConfigChanged。
 * 5. 本帧的视频浮钮（ui/features/FloatButton.tsx）。
 */
import { browser } from "wxt/browser";
import { defineContentScript } from "wxt/utils/define-content-script";
import type { BrowserDownloadNotice, ContentMessage, HooksInfo, ProbeReply } from "@/background/protocol";
import {
  EV_CONFIG,
  EV_MEDIA,
  EV_SHADOW,
  EV_REPORT,
  decodeDetail,
  encodeDetail,
  type MediaDetail,
  type PageConfig,
  type ReportDetail,
} from "@/page/bridge";
import {
  SCAN_SELECTOR,
  checkElement,
  collectLinks,
  isMediaishElement,
  runDomRule,
  scanPageResources,
  type DomRuleHit,
  type ScanContext,
} from "@/page/dom-scan";
import { MediaTracker } from "@/page/media-tracker";
import { compileBundled, createClassifier } from "@/rules/engine";
import { createFloatButton, type FloatButtonController } from "@/ui/features/FloatButton";
import { createDownloadNotice } from "@/ui/features/DownloadNotice";
import { normalizeDashManifest, parseDashManifestText } from "@/utils/dash-manifest";
import { isHttpUrl } from "@/utils/media-sniff";
import { installErrorCapture } from "@/utils/error-log";
import { sendAction } from "@/utils/messaging";
import { isPageChange } from "@/utils/page-nav";
import { normalizeHlsPlaylist } from "@/utils/hls-playlist";
import type { ResourceMessagePayload } from "@/utils/resource-types";
import { normalizeSniffMaxResources } from "@/utils/sniff-limit";

const REPORT_BATCH_MS = 200;
const MUTATION_FLUSH_DELAY_MS = 200;
const DOM_RULE_THROTTLE_MS = 1000;
/** 等钩子握手应答的时间；超时视为协议 0 的旧钩子（不应答但事件照常）。 */
const HOOKS_HELLO_TIMEOUT_MS = 1000;

const MUTATION_OBSERVER_OPTIONS: MutationObserverInit = {
  childList: true,
  subtree: true,
  attributes: true,
  attributeFilter: ["src", "href", "data"],
};

/** 本帧是否为顶层帧（跨域访问 window.top 不抛错，但保守起见包 try）。 */
function isTopFrame(): boolean {
  try {
    return window === window.top;
  } catch {
    return false;
  }
}

/**
 * 用于规则匹配的「页面 URL」：顶层帧用自身；子帧优先取祖先链最顶层 origin，
 * 退化为 referrer，最后才是自身。资源自带的 pageUrl / referrer 仍用 location.href。
 */
function matchPageUrl(top: boolean): string {
  if (top) return location.href;
  const origins = location.ancestorOrigins;
  if (origins && origins.length > 0) return `${origins[origins.length - 1]}/`;
  return document.referrer || location.href;
}

function swallow(p: Promise<unknown>): void {
  p.catch(() => {
    // 扩展可能已失效
  });
}

export default defineContentScript({
  matches: ["<all_urls>"],
  runAt: "document_start",
  allFrames: true,
  // 浮钮 / 下载提示样式构建期内联进脚本（ui/content-css.ts），只进 Shadow DOM，不得注入页面
  cssInjectionMode: "manual",
  matchAboutBlank: true,

  async main(ctx) {
    const top = isTopFrame();
    // 同一扩展实例的内容脚本共享本帧的 ISOLATED 全局。补注入（background/probe.ts）是 allFrames 的，
    // 会命中已有存活 sensor 的子帧（扩展更新后新建的 iframe 已由声明式注入）；WXT 只在顶层帧淘汰旧实例，
    // 子帧里由这里保证只有一个存活的 sensor。旧扩展实例留下的探针在其上下文失效后返回 false。
    const shared = globalThis as { __fluxdownSensorAlive?: () => boolean };
    if (!top && shared.__fluxdownSensorAlive?.() === true) return;
    shared.__fluxdownSensorAlive = () => !ctx.isInvalid;
    installErrorCapture("content");
    // 收到 PageConfig 前用随包规则；之后换成用户实际生效的 classifier。
    let classify = compileBundled().classify;

    let config: PageConfig | null = null;
    const sniffImages = (): boolean => config?.sniffImages === true;
    /** 本 sensor 判定的 SPA 换页次数；MAIN 据此（同一 sid 下增大）换代。 */
    let navCount = 0;

    const scanCtx: ScanContext = {
      classify: (url) => classify(url),
      get sniffImages() {
        return sniffImages();
      },
      keyOf: (el) => tracker.keyOf(el),
    };

    const sniffActive = (): boolean => config?.sniff !== false;
    /** 单页 DOM 上报上限（#288）；收到配置前 / 旧 background 未下发时用默认值。 */
    const resourceLimit = (): number => normalizeSniffMaxResources(config?.sniffMaxResources);

    // ===== 媒体元素跟踪 =====
    const tracker = new MediaTracker({
      send: (payload) => {
        sendAction({ action: "mediaElementState", sid: tracker.sid, ...payload })
          .then((res) => {
            // background 不认识本 sid（SW 重启丢了登记表）：补一次全量
            if (res?.resync) tracker.resendAll();
          })
          .catch(() => {
            // 扩展可能已失效
          });
      },
    });

    // ===== 钩子握手 =====
    let hooks: HooksInfo | null = null;
    let hooksReported = false;
    let hooksTimer: number | undefined;

    function reportHooks(): void {
      window.clearTimeout(hooksTimer);
      hooksTimer = undefined;
      hooksReported = true;
      swallow(sendAction({ action: "sensorStatus", sid: tracker.sid, hooks }));
    }

    // ===== 资源上报（合批 + 单页上限） =====
    const reportedUrls = new Set<string>();
    let reportedCount = 0;
    /** 上限随配置变化：调高后 applyConfig → refreshSniffState 重新扫描并接上观察。 */
    const capReached = (): boolean => reportedCount >= resourceLimit();
    let queue: ResourceMessagePayload[] = [];
    let flushTimer: number | undefined;

    function flushReports(): void {
      flushTimer = undefined;
      if (queue.length === 0) return;
      const resources = queue;
      queue = [];
      swallow(sendAction({ action: "sensorReport", resources }));
    }

    function reportResources(list: ResourceMessagePayload[]): void {
      if (!sniffActive()) return;
      for (const r of list) {
        // 上限只计 DOM 来源；fetch/XHR 嗅探到的媒体不应被首屏 a[href] 挤掉
        const fromHook = r.detectedBy === "fetch-intercept" || r.detectedBy === "xhr-intercept";
        if (!fromHook && capReached()) continue;
        if (reportedUrls.has(r.url)) continue;
        reportedUrls.add(r.url);
        // 整体清空防 SPA 长时间运行内存增长；background 有基于 dedupeKey 的去重兜底。
        // 阈值须高于上限，否则清空后重扫到的同一批链接会被重复计数、提前触顶。
        if (reportedUrls.size > Math.max(500, resourceLimit() * 2)) reportedUrls.clear();
        r.pageUrl ||= location.href;
        queue.push(r);
        if (!fromHook) reportedCount++;
      }
      if (queue.length > 0 && flushTimer === undefined) {
        flushTimer = window.setTimeout(flushReports, REPORT_BATCH_MS);
      }
      if (capReached()) domSniff.disconnect();
    }

    // ===== DOM 嗅探（扫描 + MutationObserver + dom 规则 + 内嵌 DASH） =====
    const domSniff = (() => {
      const pendingAdded = new Set<Element>();
      const pendingAttr = new Set<Element>();
      const scannedScripts = new WeakSet<HTMLScriptElement>();
      const seenDomHits = new Set<string>();
      let observing = false;
      let handle: number | undefined;
      let idle = false;
      let inlineDashSignature = "";
      let inlineDashTimer: number | undefined;
      let domRuleTimer: number | undefined;
      let domRuleLast = 0;

      const observer = new MutationObserver((mutations) => {
        if (capReached()) return;
        for (const m of mutations) {
          for (const node of m.addedNodes) if (node.nodeType === 1) pendingAdded.add(node as Element);
          if (m.type === "attributes" && m.target.nodeType === 1 && isMediaishElement(m.target as Element)) {
            pendingAttr.add(m.target as Element);
          }
        }
        if (pendingAdded.size > 0 || pendingAttr.size > 0) scheduleFlush();
      });

      function scheduleFlush(): void {
        if (handle !== undefined) return;
        if (typeof requestIdleCallback === "function") {
          idle = true;
          handle = requestIdleCallback(flush, { timeout: MUTATION_FLUSH_DELAY_MS });
        } else {
          idle = false;
          handle = window.setTimeout(flush, MUTATION_FLUSH_DELAY_MS);
        }
      }

      function cancelFlush(): void {
        if (handle === undefined) return;
        if (idle && typeof cancelIdleCallback === "function") cancelIdleCallback(handle);
        else if (!idle) window.clearTimeout(handle);
        handle = undefined;
      }

      function flush(): void {
        handle = undefined;
        if (capReached() || !sniffActive()) {
          pendingAdded.clear();
          pendingAttr.clear();
          return;
        }
        const found: ResourceMessagePayload[] = [];
        let scriptAdded = false;
        for (const node of pendingAdded) {
          if (node.tagName === "SCRIPT") scriptAdded = true;
          found.push(...checkElement(node, "mutation-observer", scanCtx));
          for (const child of node.querySelectorAll(SCAN_SELECTOR)) {
            found.push(...checkElement(child, "mutation-observer", scanCtx));
          }
        }
        pendingAdded.clear();
        for (const target of pendingAttr) found.push(...checkElement(target, "mutation-observer", scanCtx));
        pendingAttr.clear();
        if (scriptAdded) scheduleInlineDash();
        if (found.length > 0) reportResources(found);
        scheduleDomRules();
      }

      /** 补扫内嵌 JSON 状态中的 DASH：赶不上请求时仍可得到候选。 */
      function scanInlineDash(): void {
        if (!sniffActive()) return;
        for (const script of document.scripts) {
          if (scannedScripts.has(script)) continue;
          scannedScripts.add(script);
          const text = script.textContent || "";
          if (!text) continue;
          const manifest = parseDashManifestText(text, location.href);
          if (!manifest || manifest.video.length === 0) continue;
          const signature = [...manifest.video, ...manifest.audio]
            .map((t) => `${t.url}|${t.bandwidth ?? 0}|${t.codecs ?? ""}`)
            .join("\n");
          if (signature === inlineDashSignature) continue;
          inlineDashSignature = signature;
          swallow(
            sendAction({
              action: "manifestObserved",
              manifest,
              manifestUrl: `${location.href}#fluxdown-inline-dash`,
              pageUrl: location.href,
            }),
          );
          return;
        }
      }

      function scheduleInlineDash(): void {
        if (!sniffActive() || inlineDashTimer !== undefined) return;
        inlineDashTimer = window.setTimeout(() => {
          inlineDashTimer = undefined;
          scanInlineDash();
        }, 0);
      }

      /** 声明式 dom 规则：命中以 domObserved 交给 background 按 emit 模板渲染。 */
      function runDomRules(): void {
        domRuleTimer = undefined;
        domRuleLast = Date.now();
        if (!config || !sniffActive() || config.dom.length === 0) return;
        const fresh: DomRuleHit[] = [];
        for (const rule of config.dom) {
          for (const hit of runDomRule(rule, scanCtx.keyOf)) {
            // 元素晚于首次扫描才被跟踪时，带上元素归属再报一次（background 合并补齐 elementKey）。
            const key = `${hit.ruleId}\0${hit.value}\0${hit.elementKey ?? ""}`;
            if (seenDomHits.has(key)) continue;
            seenDomHits.add(key);
            fresh.push(hit);
          }
        }
        if (seenDomHits.size > 2000) seenDomHits.clear();
        if (fresh.length > 0) {
          swallow(sendAction({ action: "domObserved", pageUrl: matchPageUrl(top), hits: fresh }));
        }
      }

      function scheduleDomRules(): void {
        if (domRuleTimer !== undefined || !config || config.dom.length === 0) return;
        const wait = Math.max(0, DOM_RULE_THROTTLE_MS - (Date.now() - domRuleLast));
        domRuleTimer = window.setTimeout(runDomRules, wait);
      }

      function connect(): void {
        if (observing || capReached()) return;
        observing = true;
        observer.observe(document.documentElement, MUTATION_OBSERVER_OPTIONS);
      }

      return {
        /** 嗅探开启后的首次扫描 + 连接观察；重复调用只补做缺失的部分。 */
        start(): void {
          if (!sniffActive()) return;
          const initial = scanPageResources(scanCtx);
          if (initial.length > 0) reportResources(initial);
          scanInlineDash();
          runDomRules();
          connect();
        },
        stop(): void {
          observer.disconnect();
          observing = false;
          cancelFlush();
          pendingAdded.clear();
          pendingAttr.clear();
        },
        disconnect(): void {
          observer.disconnect();
          observing = false;
        },
        /** SPA 路由切换：重置单页计数，必要时重新连接观察（新页面重新计数）。 */
        resetForNavigation(): void {
          reportedCount = 0;
          seenDomHits.clear();
          inlineDashSignature = "";
          reportedUrls.clear();
          queue = [];
          window.clearTimeout(flushTimer);
          flushTimer = undefined;
          if (sniffActive()) {
            connect();
            scheduleDomRules();
          }
        },
        rerunDomRules(): void {
          scheduleDomRules();
        },
      };
    })();

    // DOM 就绪后才开始 DOM 扫描（document_start 时 DOM 尚空）
    let domReady = document.readyState !== "loading";
    function refreshSniffState(): void {
      if (!domReady) return;
      if (sniffActive()) domSniff.start();
      else domSniff.stop();
    }
    if (!domReady) {
      document.addEventListener(
        "DOMContentLoaded",
        () => {
          domReady = true;
          refreshSniffState();
        },
        { once: true },
      );
    }

    // ===== PageConfig：拉取并下发给 MAIN =====
    let float: FloatButtonController | null = null;
    // 浏览器下载提示卡（仅顶层帧会收到消息，首次提示才挂载）
    const downloadNotice = createDownloadNotice(ctx);

    function applyConfig(next: PageConfig): void {
      config = next;
      // 设置改为关闭时本帧可能还报着「按住」：立即撤销，别让后台继续放行
      if (bypassModifier() === null) reportBypassKey(false);
      if (Array.isArray(next.classify)) {
        try {
          classify = createClassifier(next.classify);
        } catch {
          /* 形状不对：沿用当前 classifier */
        }
      }
      try {
        document.dispatchEvent(
          new CustomEvent(EV_CONFIG, { detail: encodeDetail({ ...next, sid: tracker.sid, nav: navCount }) }),
        );
      } catch {
        /* 派发失败无妨 */
      }
      // 首次下发即握手：协议 ≥ 1 的钩子同步应答（见 onReport 的 "hooks"），否则超时按旧钩子上报
      if (!hooksReported && hooksTimer === undefined) {
        hooksTimer = window.setTimeout(reportHooks, HOOKS_HELLO_TIMEOUT_MS);
      }
      const trackingNeeded = next.sniff || next.floatButton || next.pageDot;
      if (trackingNeeded) tracker.start();
      else tracker.stop();
      tracker.setHints(next.titles ?? [], next.floatExclude ?? []);
      refreshSniffState();
      domSniff.rerunDomRules();
      float?.setConfig(next);
    }

    async function loadConfig(retry = true): Promise<void> {
      try {
        const next = (await sendAction({ action: "getPageConfig", pageUrl: matchPageUrl(top), sid: tracker.sid })) as
          | PageConfig
          | undefined;
        // background 出错时返回 { error }：形状不对不应用，避免后续 config.dom 访问抛错
        if (!next || typeof next.sniff !== "boolean" || !Array.isArray(next.dom) || !Array.isArray(next.bodyWatch)) {
          if (retry) window.setTimeout(() => void loadConfig(false), 1000);
          return;
        }
        applyConfig(next);
      } catch {
        // 扩展刚启动 / service worker 唤醒中：重试一次
        if (retry) window.setTimeout(() => void loadConfig(false), 1000);
      }
    }

    // ===== 桥接：MAIN → ISOLATED =====
    function onReport(event: Event): void {
      const detail = decodeDetail((event as CustomEvent).detail) as ReportDetail | null;
      if (!detail || typeof detail !== "object") return;
      switch (detail.t) {
        case "resource": {
          if (!isHttpUrl(detail.url)) return;
          reportResources([
            {
              url: detail.url,
              type: classify(detail.url, detail.mime),
              mimeType: detail.mime,
              size: detail.size,
              detectedBy: detail.via === "xhr" ? "xhr-intercept" : detail.via === "json" ? "api-scan" : "fetch-intercept",
            },
          ]);
          return;
        }
        case "manifest": {
          const manifest = normalizeDashManifest(detail.manifest);
          if (!manifest) return;
          swallow(
            sendAction({
              action: "manifestObserved",
              manifest,
              manifestUrl: detail.manifestUrl || "",
              pageUrl: location.href,
            }),
          );
          return;
        }
        case "body":
          if (typeof detail.text !== "string" || !Array.isArray(detail.ruleIds)) return;
          swallow(
            sendAction({
              action: "bodyObserved",
              url: detail.url,
              pageUrl: matchPageUrl(top),
              ruleIds: detail.ruleIds,
              text: detail.text,
            }),
          );
          return;
        case "blob":
          if (typeof detail.blobUrl === "string" && detail.blobUrl.startsWith("blob:") && isHttpUrl(detail.url)) {
            tracker.setBlobSource(detail.blobUrl, detail.url);
          }
          return;
        case "swScope":
          if (typeof detail.scope === "string" && isHttpUrl(detail.scope)) {
            swallow(sendAction({ action: "serviceWorkerScope", scope: detail.scope }));
          }
          return;
        case "hooks":
          if (typeof detail.protocol !== "number" || typeof detail.hid !== "string") return;
          hooks = { protocol: detail.protocol, hid: detail.hid, late: detail.late === true };
          reportHooks();
          return;
        case "hls": {
          const playlist = normalizeHlsPlaylist(detail.playlist);
          if (!isHttpUrl(detail.url) || !playlist) return;
          swallow(sendAction({ action: "hlsObserved", url: detail.url, playlist, pageUrl: location.href }));
          return;
        }
        case "preempt":
          if (detail.url) swallow(sendAction({ action: "preemptDownload", url: detail.url }));
          return;
        case "pageUrl": {
          const kind = detail.kind === "push" || detail.kind === "replace" ? detail.kind : "pop";
          // replaceState 只改 query（如补追踪参数）不是换页：保留待发队列与已上报状态，
          // 否则本页刚嗅探到的资源 / 内嵌清单会随 background 清空而丢失且不再补报。
          if (isPageChange(kind, typeof detail.from === "string" ? detail.from : "", location.href)) {
            // 旧页面的待发队列属于上一个 URL：丢弃，避免 background 因旧 pageUrl 把 tab 状态改回去
            navCount++;
            domSniff.resetForNavigation();
            tracker.resetForNavigation();
          }
          if (top) swallow(sendAction({ action: "pageUrlChanged", pageUrl: location.href, kind }));
          void loadConfig();
          return;
        }
      }
    }
    document.addEventListener(EV_REPORT, onReport);

    function onMedia(event: Event): void {
      const detail = decodeDetail((event as CustomEvent).detail) as MediaDetail | null;
      if (!detail || !Array.isArray(detail.sourceBuffers)) return;
      const target = detail.elementless ? null : event.composedPath()[0];
      tracker.applyBinding(target, detail);
    }
    // 事件 bubbles:false，只能在捕获阶段于 document 上接收
    document.addEventListener(EV_MEDIA, onMedia, true);

    // 页面给已连接的元素挂了 shadow root（MAIN 的 attachShadow 钩子派发，composed 冒泡到 document）
    function onShadow(event: Event): void {
      const host = event.composedPath()[0];
      if (host instanceof Element) tracker.observeShadowHost(host);
    }
    document.addEventListener(EV_SHADOW, onShadow, true);

    // 右键目标：video/audio 菜单在 srcUrl 不是 http(s) 时靠它回溯元素（#407）
    function onContextMenu(event: MouseEvent): void {
      swallow(sendAction({ action: "contextTarget", elementKey: tracker.fromEvent(event)?.key ?? null }));
    }
    document.addEventListener("contextmenu", onContextMenu, true);

    // ===== background → content =====
    const onMessage = (
      message: unknown,
      _sender: unknown,
      sendResponse: (response: unknown) => void,
    ): true | undefined => {
      switch ((message as ContentMessage | undefined)?.action) {
        case "pageConfigChanged":
          void loadConfig();
          return undefined;
        case "getPageCharset":
          // 不受嗅探 / 拦截开关门控；只有顶层帧应答（文件名解码先验）
          if (!top) return undefined;
          sendResponse({ charset: document.characterSet });
          return true;
        case "collectLinks":
          sendResponse({ links: collectLinks() });
          return true;
        case "probe": {
          // background 不认识本帧的 sensor（SW 重启丢了登记 / 补注入前的探测）：应答身份并全量重发
          const reply: ProbeReply = { sid: tracker.sid, hooks };
          sendResponse(reply);
          if (!config) void loadConfig();
          tracker.resendAll();
          return true;
        }
        case "browserDownload": {
          // 只有顶层帧显示；background 也只发 frameId 0
          if (!top) return undefined;
          const notice = message as BrowserDownloadNotice;
          downloadNotice
            .handle(notice)
            .then((ok) => sendResponse({ ok }))
            .catch(() => sendResponse({ ok: false }));
          return true;
        }
        default:
          return undefined;
      }
    };
    browser.runtime.onMessage.addListener(onMessage);

    // ===== 跳过接管修饰键（IDM 式 Alt+点击）=====
    // 只上报状态变化；后台全局记一份并在请求发起时快照成放行令牌（background/breaker.ts）。
    // 失焦 / 隐藏视为松开（keyup 会丢在别的窗口里）；pointerdown 按事件自带的修饰键位校正，
    // 覆盖「在别的帧按下、焦点移进本帧后点击」这类本帧收不到 keydown 的情况。
    let bypassKeyHeld = false;

    function bypassModifier(): "Alt" | "Shift" | null {
      if (config?.bypassKey === "alt") return "Alt";
      if (config?.bypassKey === "shift") return "Shift";
      return null;
    }

    function reportBypassKey(held: boolean): void {
      if (held === bypassKeyHeld) return;
      bypassKeyHeld = held;
      swallow(sendAction({ action: "bypassKey", held }));
    }

    function onBypassKey(e: KeyboardEvent): void {
      if (e.repeat || e.key !== bypassModifier()) return;
      reportBypassKey(e.type === "keydown");
    }

    function onBypassPointer(e: PointerEvent): void {
      const key = bypassModifier();
      if (key === null) return;
      const held = key === "Alt" ? e.altKey : e.shiftKey;
      if (!held) {
        reportBypassKey(false);
        return;
      }
      // 本帧以为已按住时也重发：别的帧失焦已让后台撤销，本地去重会吞掉这次点击
      bypassKeyHeld = true;
      swallow(sendAction({ action: "bypassKey", held: true }));
      // 左 / 中键的真实按下：把放行意图绑定到这次点击（链接 href 立即领令牌，本 tab 随后一小段时间的
      // 请求也领），不依赖请求发起时键是否还按着——页面 JS 常先 await 再程序化跳转。
      // 在 pointerdown 而非 click 上报：最早的信号，且有些站点在 mousedown 上就发起下载。
      if (!e.isTrusted || e.button > 1) return;
      const origin = e.composedPath()[0];
      const link = origin instanceof Element ? origin.closest("a[href]") : null;
      const href = link instanceof HTMLAnchorElement ? link.href : undefined;
      swallow(sendAction({ action: "bypassClick", href }));
    }

    function onBypassRelease(): void {
      reportBypassKey(false);
    }

    // ===== 链接按下预热 =====
    // 浏览器要等对方响应头才把点击变成下载（标签页转圈），之后才轮到拦截层投递；按下时就请
    // background 判断并预热 NMH，让 App 冷启动与这段等待并行。pointerdown 是最早的点击信号。
    function onLinkPress(e: PointerEvent): void {
      if (config?.warmOnLinkPress !== true || !e.isTrusted || e.button > 1) return;
      const key = bypassModifier();
      if ((key === "Alt" && e.altKey) || (key === "Shift" && e.shiftKey)) return;
      const origin = e.composedPath()[0];
      const link = origin instanceof Element ? origin.closest("a[href]") : null;
      if (!(link instanceof HTMLAnchorElement)) return;
      const href = link.href;
      if (!href.startsWith("http:") && !href.startsWith("https:")) return;
      swallow(sendAction({ action: "linkPress", href, download: link.hasAttribute("download") }));
    }

    function onVisibility(): void {
      if (document.visibilityState === "hidden") reportBypassKey(false);
    }

    window.addEventListener("keydown", onBypassKey, true);
    window.addEventListener("keyup", onBypassKey, true);
    window.addEventListener("pointerdown", onBypassPointer, true);
    window.addEventListener("pointerdown", onLinkPress, true);
    window.addEventListener("blur", onBypassRelease);
    document.addEventListener("visibilitychange", onVisibility);

    // ===== 磁力链接点击接管 =====
    // 阻止浏览器弹出 OS 应用选择框，改由 FluxDown 接管；捕获阶段早于页面自身处理器。
    // interceptMagnet 关闭或按住跳过接管修饰键时放行，交还系统默认磁力处理程序。
    function onClick(e: MouseEvent): void {
      if (config?.interceptMagnet !== true) return;
      const key = bypassModifier();
      if ((key === "Alt" && e.altKey) || (key === "Shift" && e.shiftKey)) return;
      const target = e.target;
      if (!(target instanceof Element)) return;
      const link = target.closest<HTMLAnchorElement>("a[href]");
      const href = link?.href;
      if (!href || !href.toLowerCase().startsWith("magnet:")) return;
      e.preventDefault();
      e.stopPropagation();
      sendAction({ action: "downloadUrls", urls: [href], referrer: location.href })
        .then((res) => {
          // 未被 FluxDown 接管（被拒绝 / 后台无响应）时交还系统处理程序，避免点击变死链
          if (!res || res.success === false) location.href = href;
        })
        .catch(() => {
          location.href = href;
        });
    }
    document.addEventListener("click", onClick, true);

    // bfcache 恢复：内容脚本与页面状态原样回来，但 background 可能已按导航清空了本页投影
    function onPageShow(event: PageTransitionEvent): void {
      if (!event.persisted) return;
      void loadConfig();
      tracker.resendAll();
    }
    window.addEventListener("pageshow", onPageShow);

    ctx.onInvalidated(() => {
      document.removeEventListener(EV_REPORT, onReport);
      document.removeEventListener(EV_MEDIA, onMedia, true);
      document.removeEventListener(EV_SHADOW, onShadow, true);
      document.removeEventListener("contextmenu", onContextMenu, true);
      document.removeEventListener("click", onClick, true);
      window.removeEventListener("keydown", onBypassKey, true);
      window.removeEventListener("keyup", onBypassKey, true);
      window.removeEventListener("pointerdown", onBypassPointer, true);
      window.removeEventListener("pointerdown", onLinkPress, true);
      window.removeEventListener("blur", onBypassRelease);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("pageshow", onPageShow);
      browser.runtime.onMessage.removeListener(onMessage);
      domSniff.stop();
      tracker.stop();
      downloadNotice.destroy();
      float?.destroy();
      window.clearTimeout(flushTimer);
      window.clearTimeout(hooksTimer);
    });

    // ===== 启动序列 =====
    // MAIN world 钩子由清单声明式注入（page-hooks.content.ts，document_start），先于页面脚本运行；
    // 本脚本无需注入或等待它，配置下发后经 hooks 握手确认。
    // 1. 浮钮
    float = createFloatButton({ ctx, tracker, pageUrl: () => matchPageUrl(top) });

    // 2. 拉取配置（之后 DOM 扫描 / 跟踪 / 浮钮才真正启用）
    await loadConfig();
  },
});
