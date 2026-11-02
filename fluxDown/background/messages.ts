/**
 * runtime.onMessage 路由（C6）。
 *
 * 消息分两族：`type` 族（任务面板 nmh-*、诊断）与 `action` 族（C6 MessageMap）。
 *
 * Firefox：直接返回 Promise。
 * Chromium：Chrome 148 之前 onMessage 监听器不能返回 Promise，必须 "return true + sendResponse"。
 */
import { browser } from "wxt/browser";
import type { Browser } from "wxt/browser";
import { hostOf } from "@/rules/url";
import { installConnectionDiagnostics } from "@/utils/connection-diagnostics-background";
import { ERROR_LOG_TYPES, handleErrorLogMessage } from "@/utils/error-log-background";
import { checkRemoteAvailable, effectiveRemoteMode, warmupNativeHost } from "@/utils/download-dispatch";
import {
  nmhOpenFile,
  nmhRevealFile,
  nmhTaskOp,
  nmhListTasks,
  nmhWarmupNativeHost,
} from "@/utils/native-messaging";
import { parseDashJson } from "@/utils/dash-manifest";
import { normalizeHlsPlaylist } from "@/utils/hls-playlist";
import { buildMediaCandidates } from "@/utils/media-candidates";
import { buildResourceDebugLog } from "@/utils/resource-debug-log";
import { profileFor } from "@/utils/site-profiles";
import { armBypassClick, hasActiveBypass, isAppKnownDown, markAppUp, setBypassKeyHeld } from "./breaker";
import {
  downloadCandidate,
  downloadResources,
  downloadUrls,
  handoffPage,
} from "./downloads";
import { showBrowserDownload } from "./browser-downloads";
import { getCapabilities } from "./host";
import { isInterceptActive, markPreempted } from "./intercept";
import { rememberServiceWorkerScope } from "./sw-scopes";
import { isReplayableUrl, scopeBelongsTo } from "@/utils/download-source";
import { isLikelyDownloadClick } from "@/utils/settings";
import type { PageConfig } from "@/page/bridge";
import type {
  HostStatus,
  MessageAction,
  MessageMap,
  ClassifiedLink,
  PageLink,
  SendChannel,
} from "./protocol";
import { getEngine, getRuleSyncStatus, syncRemotePacks } from "./rules-store";
import {
  approvePageScripts,
  getPageScriptsStatus,
  revokePageScripts,
  viewPageScriptSource,
} from "./page-scripts";
import type { PageScriptsStatus } from "./page-scripts/model";
import { compileWithDraft, engineSettled } from "./rules-store";
import { getRuleHits } from "./rule-hits";
import { isListed, toListItem } from "./rule-view";
import { BUILTIN_RULE_PREVIEW } from "@/utils/rule-preview";
import { getCachedProfiles, getCachedSettings } from "./settings-cache";
import {
  addManifest,
  clearResourcesOnly,
  framesOf,
  addHlsPlaylist,
  getManifests,
  getHlsPlaylists,
  getElements,
  getResources,
  getRetiredElements,
  getTabPageUrl,
  ingestResources,
  isCurrentPage,
  registerSession,
  setContextTarget,
  setSessionHooks,
  syncTabPageUrl,
  unseenEvidence,
  updateElements,
  whenRestored,
} from "./tab-state";
import { scheduleProbeCheck } from "./probe";
import { buildTabView } from "./tab-view";
import { processTasksPollResult } from "./tasks";
import { rowsForElement } from "./view-model";
import type { ResourceMessagePayload } from "@/utils/resource-types";
import type { Emitted, RuleRef } from "@/rules/engine";

type Sender = Browser.runtime.MessageSender;
type Handler<A extends MessageAction> = (
  req: MessageMap[A]["req"],
  sender: Sender,
) => Promise<MessageMap[A]["res"]>;

/** 每条 dom 规则最多处理的命中数。 */
const DOM_HITS_PER_RULE = 200;
const COLLECT_LINKS_TIMEOUT_MS = 3_000;
const MAX_LINKS = 5000;

function senderTabId(sender: Sender): number | undefined {
  const id = sender.tab?.id;
  return id !== undefined && id >= 0 ? id : undefined;
}

/** 消息来自本扩展自己的页面（选项页 / popup / 侧边栏），而不是网页里的内容脚本。 */
function isExtensionPage(sender: Sender): boolean {
  return sender.url?.startsWith(browser.runtime.getURL("/")) === true;
}

/** 非扩展页请求页面脚本管理接口时的空应答（不泄漏任何状态）。 */
function refusedStatus(): PageScriptsStatus {
  return { availability: "ok", blocked: null, outcome: null, lastSyncAt: null, plugins: [], attention: null, pendingCount: 0 };
}

/**
 * content script 以 sender.tab 为准（不信任页面自报）；扩展页（含以 tab 打开的选项页）用显式 tabId，
 * 缺省取活动 tab。
 */
async function resolveTabId(sender: Sender, explicit?: number): Promise<number | undefined> {
  const fromExtensionPage = sender.url?.startsWith(browser.runtime.getURL("/")) === true;
  if (fromExtensionPage && typeof explicit === "number" && explicit >= 0) return explicit;
  const fromSender = senderTabId(sender);
  if (fromSender !== undefined) return fromSender;
  if (typeof explicit === "number" && explicit >= 0) return explicit;
  try {
    const [active] = await browser.tabs.query({ active: true, currentWindow: true });
    return active?.id;
  } catch {
    return undefined;
  }
}

/** 全局嗅探开关与站点配置 sniff 都允许。 */
async function sniffEnabled(tabId: number, fallbackUrl: string): Promise<boolean> {
  const [settings, profiles] = await Promise.all([getCachedSettings(), getCachedProfiles()]);
  if (!settings.resourceSniffing) return false;
  return profileFor(profiles, hostOf(getTabPageUrl(tabId) || fallbackUrl)).profile.sniff;
}

function ingestEmit(
  tabId: number,
  pageUrl: string,
  frameId: number | undefined,
  rule: RuleRef,
  emit: Emitted,
  detectedBy: ResourceMessagePayload["detectedBy"],
  elementKey?: string,
): number {
  if (emit.mode === "preempt") {
    markPreempted(emit.url);
    return 1;
  }
  return ingestResources(
    tabId,
    pageUrl,
    [
      {
        url: emit.url,
        type: emit.type ?? getEngine().classify(emit.url),
        filename: emit.filename,
        quality: emit.quality,
        detectedBy,
        pageUrl,
        elementKey,
      },
    ],
    { frameId, ruleIds: [rule.id] },
  );
}

async function collectFrameLinks(tabId: number, frameId: number): Promise<PageLink[]> {
  try {
    const reply: unknown = await Promise.race([
      browser.tabs.sendMessage(tabId, { action: "collectLinks" }, { frameId }),
      new Promise<undefined>((resolve) => setTimeout(resolve, COLLECT_LINKS_TIMEOUT_MS)),
    ]);
    if (reply && typeof reply === "object" && "links" in reply && Array.isArray(reply.links)) {
      return reply.links as PageLink[];
    }
  } catch {
    /* 该帧没有内容脚本 */
  }
  return [];
}

const handlers: { [A in MessageAction]: Handler<A> } = {
  async getPageConfig(req, sender) {
    const tabId = senderTabId(sender);
    const frameId = sender.frameId ?? 0;
    // 会话登记要落在恢复后的状态上：SW 被本条消息唤醒时快照恢复尚未完成
    const [settings, profiles] = await Promise.all([getCachedSettings(), getCachedProfiles(), whenRestored()]);
    if (tabId !== undefined) {
      if (typeof req.sid === "string" && req.sid) registerSession(tabId, frameId, req.sid);
      if (frameId === 0) syncTabPageUrl(tabId, req.pageUrl, "document");
    }
    const topUrl = (tabId !== undefined && getTabPageUrl(tabId)) || sender.tab?.url || req.pageUrl;
    const profile = profileFor(profiles, hostOf(topUrl)).profile;
    const engine = getEngine();
    // 嵌入播放器的子帧同时受自身页面与顶层页面的 page 规则约束（标题多在顶层，排除规则可能写给子帧）。
    const hints = [engine.pageHints(req.pageUrl), ...(topUrl !== req.pageUrl ? [engine.pageHints(topUrl)] : [])];
    const config: PageConfig = {
      sniff: settings.resourceSniffing && profile.sniff,
      bodyWatch: engine.bodyWatchers(req.pageUrl).map((w) => ({
        ruleId: w.rule.id,
        url: w.url,
        host: w.host,
        pathRegex: w.pathRegex,
      })),
      dom: engine.domRules(req.pageUrl).map((d) => ({
        ruleId: d.rule.id,
        selector: d.selector,
        attr: d.attr,
      })),
      floatButton: settings.showFloatingButton && profile.floatButton,
      floatTrigger: settings.floatButtonTrigger,
      floatPosition: settings.floatButtonPosition,
      titles: hints.flatMap((h) => h.titles),
      floatExclude: hints.flatMap((h) => h.players.map((p) => p.selector)),
      pageDot: settings.showPageDot && profile.pageDot,
      frameIsTop: frameId === 0,
      interceptMagnet: settings.enabled && settings.interceptMagnet && profile.intercept !== "never",
      bypassKey: settings.enabled ? settings.bypassKey : "off",
      warmOnLinkPress: settings.enabled && profile.intercept !== "never",
      sniffImages: settings.sniffImages,
      sniffMaxResources: settings.sniffMaxResources,
      classify: engine.classifyRules(),
    };
    return config;
  },

  async sensorReport(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || req.resources.length === 0) return { added: 0 };
    const frameId = sender.frameId ?? 0;
    // 批量上报有 200 ms 合批延迟：顶层帧里不属于当前嗅探投影的资源来自已离开的旧页面，
    // 直接丢弃；不能再用它去同步 tab 页面 URL（会把刚换页清空的状态又改回旧页并再次清空）。
    // replaceState 改写过的旧 URL 仍属于当前投影（见 syncTabPageUrl）。
    const resources =
      frameId === 0 ? req.resources.filter((r) => isCurrentPage(tabId, r.pageUrl ?? "")) : req.resources;
    if (resources.length === 0) return { added: 0 };
    const pageUrl = resources.find((r) => r.pageUrl)?.pageUrl || sender.tab?.url || "";
    // 旧页面的 content script 可能仍在运行：开关关闭时丢弃。
    if (!(await sniffEnabled(tabId, pageUrl))) return { added: 0 };
    return { added: ingestResources(tabId, pageUrl, resources, { frameId }) };
  },

  async manifestObserved(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || !(await sniffEnabled(tabId, req.pageUrl))) return { ok: false };
    return { ok: addManifest(tabId, req.manifestUrl, req.manifest, req.pageUrl) };
  },

  async hlsObserved(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || !(await sniffEnabled(tabId, req.pageUrl))) return { ok: false };
    const playlist = normalizeHlsPlaylist(req.playlist);
    if (!playlist || !isCurrentPage(tabId, req.pageUrl)) return { ok: false };
    addHlsPlaylist(tabId, req.url, playlist, req.pageUrl);
    return { ok: true };
  },

  async bodyObserved(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || !(await sniffEnabled(tabId, req.pageUrl))) return { emitted: 0 };
    let json: unknown;
    try {
      json = JSON.parse(req.text);
    } catch {
      return { emitted: 0 };
    }
    let emitted = 0;
    const engine = getEngine();
    for (const { rule, emits } of engine.extractFromBody(req.url, req.pageUrl, json)) {
      for (const emit of emits) {
        emitted += ingestEmit(tabId, req.pageUrl, sender.frameId, rule, emit, "fetch-intercept");
      }
    }
    // manifest 规则：站点接口里通用识别认不出的 DASH 结构，按规则给出的路径当清单处理
    for (const node of engine.manifestsFromBody(req.url, req.pageUrl, json)) {
      if (addManifest(tabId, req.url, parseDashJson(node, req.url), req.pageUrl)) emitted++;
    }
    return { emitted };
  },

  async domObserved(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || !(await sniffEnabled(tabId, req.pageUrl))) return { emitted: 0 };
    const engine = getEngine();
    const rules = engine.domRules(req.pageUrl);
    const perRule = new Map<string, number>();
    let emitted = 0;
    for (const hit of req.hits) {
      const count = perRule.get(hit.ruleId) ?? 0;
      if (count >= DOM_HITS_PER_RULE) continue;
      perRule.set(hit.ruleId, count + 1);
      const entry = rules.find((r) => r.rule.id === hit.ruleId);
      if (!entry) continue;
      const emit = engine.renderDomEmit(entry.rule, entry.emit, { value: hit.value, text: hit.text, page: req.pageUrl });
      if (emit) emitted += ingestEmit(tabId, req.pageUrl, sender.frameId, entry.rule, emit, "dom-scan", hit.elementKey);
    }
    return { emitted };
  },

  async preemptDownload(req) {
    if (!req.url || typeof req.url !== "string") return { ok: false };
    // 蓝奏云等网站的 CDN URL 实际是 HTML 中转页：不发送给 FluxDown，仅记录 URL 防重复。
    markPreempted(req.url);
    return { ok: true };
  },

  async serviceWorkerScope(req, sender) {
    // SW 只能注册在同源路径下：以浏览器给出的帧来源为准，跨源上报直接拒绝
    const frameOrigin = sender.origin ?? sender.url ?? "";
    if (typeof req.scope !== "string" || !scopeBelongsTo(req.scope, frameOrigin)) return { ok: false };
    rememberServiceWorkerScope(req.scope);
    return { ok: true };
  },

  async pageUrlChanged(req, sender) {
    const tabId = senderTabId(sender);
    // 只有顶层帧的 pageUrlChanged 才代表整页换址；子帧只做本地重置。
    if (tabId === undefined || !req.pageUrl || (sender.frameId ?? 0) !== 0) return { ok: false };
    syncTabPageUrl(tabId, req.pageUrl, req.kind);
    return { ok: true };
  },

  async mediaElementState(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || typeof req.sid !== "string" || !req.sid) return { ok: false };
    const frameId = sender.frameId ?? 0;
    await whenRestored();
    const resync = updateElements(tabId, frameId, req.sid, req.elements, req.removed, req.full === true);
    // 资源表缺少播放器实际追加过的地址（扩展更新清空了会话存储、播放器已不再请求）：按证据补报
    const missing = unseenEvidence(tabId, req.elements);
    if (missing.length > 0) {
      const pageUrl = getTabPageUrl(tabId) || sender.tab?.url || "";
      if (await sniffEnabled(tabId, pageUrl)) ingestResources(tabId, pageUrl, missing, { frameId });
    }
    return resync ? { ok: true, resync } : { ok: true };
  },

  async sensorStatus(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined || typeof req.sid !== "string" || !req.sid) return { ok: false };
    await whenRestored();
    setSessionHooks(tabId, sender.frameId ?? 0, req.sid, req.hooks ?? null);
    return { ok: true };
  },

  async bypassKey(req) {
    setBypassKeyHeld(req.held === true);
    return { ok: true };
  },

  async bypassClick(req, sender) {
    armBypassClick(senderTabId(sender), typeof req.href === "string" && req.href ? req.href : undefined);
    return { ok: true };
  },

  async linkPress(req, sender) {
    // 浏览器要等对方响应头才把点击变成下载；像下载的点击先预热 NMH，App 冷启动与该等待并行。
    // 只预热不接管：是否接管仍由拦截层按响应头决定。
    const href = typeof req.href === "string" ? req.href : "";
    if (!isReplayableUrl(href) || !isInterceptActive() || isAppKnownDown() || hasActiveBypass(href)) {
      return { ok: true };
    }
    const [settings, profiles] = await Promise.all([getCachedSettings(), getCachedProfiles()]);
    const pageUrl = sender.tab?.url || sender.url;
    if (isLikelyDownloadClick(href, req.download === true, pageUrl, settings, profiles)) {
      warmupNativeHost();
    }
    return { ok: true };
  },

  async reloadTab(req, sender) {
    const tabId = await resolveTabId(sender, req.tabId);
    if (tabId === undefined) return { ok: false };
    await browser.tabs.reload(tabId);
    return { ok: true };
  },

  async contextTarget(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined) return { ok: false };
    setContextTarget(tabId, req.elementKey);
    return { ok: true };
  },

  async getElementCandidates(req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined) return { rows: [] };
    return { rows: rowsForElement(await buildTabView(tabId, { withHandoff: false }), req.elementKey) };
  },

  async getTabView(req, sender) {
    const tabId = await resolveTabId(sender, req.tabId);
    if (tabId === undefined) return null;
    scheduleProbeCheck(tabId, 0);
    return buildTabView(tabId);
  },

  async downloadCandidate(req, sender) {
    const tabId = await resolveTabId(sender, req.tabId);
    if (tabId === undefined) return { success: false, message: "No tab" };
    return downloadCandidate(tabId, req.candidateId, req.variantId, req.channel);
  },

  async downloadResources(req, sender) {
    const tabId = await resolveTabId(sender, req.tabId);
    if (tabId === undefined) return { success: false, message: "No tab" };
    return downloadResources(tabId, req.resourceIds, req.channel);
  },

  async downloadUrls(req, sender) {
    return downloadUrls(req.urls, req.referrer, await resolveTabId(sender, req.tabId), req.channel);
  },

  async handoffPage(req) {
    return handoffPage(req.tabId);
  },

  async clearResources(req, sender) {
    const tabId = await resolveTabId(sender, req.tabId);
    if (tabId === undefined) return { ok: false };
    clearResourcesOnly(tabId);
    return { ok: true };
  },

  async collectLinks(req) {
    const frames = new Set([0, ...framesOf(req.tabId)]);
    const results = await Promise.all([...frames].map((frameId) => collectFrameLinks(req.tabId, frameId)));
    const seen = new Set<string>();
    const engine = getEngine();
    const links: ClassifiedLink[] = [];
    for (const link of results.flat()) {
      if (seen.has(link.url)) continue;
      seen.add(link.url);
      links.push({ ...link, type: engine.classify(link.url) });
      if (links.length >= MAX_LINKS) break;
    }
    return { links };
  },

  async openPanel(_req, sender) {
    const tabId = senderTabId(sender);
    if (tabId === undefined) return { ok: false };
    try {
      await browser.tabs.sendMessage(tabId, { action: "openPanel" }, { frameId: 0 });
      return { ok: true };
    } catch {
      return { ok: false };
    }
  },

  async showBrowserDownload(req, sender) {
    if (typeof req.downloadId !== "number") return { ok: false };
    return { ok: showBrowserDownload(req.downloadId, senderTabId(sender)) };
  },

  async rulesTest(req) {
    const { draft, ...input } = req;
    const engine = draft ? await compileWithDraft(draft) : getEngine();
    const response = engine.classifyResponse(input);
    const allowed = await sniffEnabled(-1, input.pageUrl);
    const ignored = response.decision === "ignore";
    // 与 sniff.ts onSniffResponse 的收录条件一致：未被忽略，且命中资源规则或带 attachment。
    const wouldIngest = allowed && !ignored && (response.decision === "resource" || input.attachment);
    const own = draft ? engine.listRules().find((e) => e.rule.id === draft.id) : undefined;
    return {
      response,
      track: engine.trackKind({ url: input.url, mime: input.mime, pageUrl: input.pageUrl }),
      dedupeKey: engine.dedupeKey(input.url, input.pageUrl),
      fileExtension: engine.fileExtension(input.url, input.pageUrl),
      matches: engine.explainResponse(input),
      sniffAllowed: allowed,
      wouldIngest,
      finalType: !wouldIngest ? null : response.decision === "resource" ? response.type : engine.classify(input.url, input.mime),
      ignoredBy: response.decision === "ignore" ? response.rule : null,
      ...(own ? { draft: { status: own.status, detail: own.detail, code: own.code, param: own.param } } : {}),
    };
  },

  async rulesSyncNow() {
    return syncRemotePacks();
  },

  async getHostStatus() {
    const capabilities = await getCapabilities(true);
    const remoteMode = await effectiveRemoteMode();
    // ping 成功 → 本机 App 已恢复：立即解除熔断，让「接管」与显示的「已连接」一致。
    // 远程 always 模式下 connected 是远程探测结果，不代表本机 App 在线。
    if (capabilities.connected && remoteMode !== "always") markAppUp();
    // fallback：本机不可达时下载改投远程，远程可达同样算已连接（与下载路由一致）。
    const connected =
      capabilities.connected ||
      (remoteMode === "fallback" && (await checkRemoteAvailable().catch(() => false)));
    const status: HostStatus = {
      connected,
      protocol: capabilities.protocol,
      capabilities,
      ruleSync: await getRuleSyncStatus(),
    };
    return status;
  },

  async rulesList() {
    await engineSettled();
    const hits = await getRuleHits();
    return getEngine()
      .listRules()
      .filter(isListed)
      .map((entry) => toListItem(entry, hits.get(entry.rule.id), entry.rule.tier === "builtin" && !BUILTIN_RULE_PREVIEW));
  },

  async ruleGet(req) {
    await engineSettled();
    const entry = getEngine()
      .listRules()
      .find((e) => e.rule.id === req.id);
    if (!entry || !isListed(entry) || (entry.rule.tier === "builtin" && !BUILTIN_RULE_PREVIEW)) return null;
    return entry.rule;
  },

  async rulesSyncStatus() {
    return getRuleSyncStatus();
  },

  async getDebugLog(req) {
    const tabId = req.tabId;
    const view = await buildTabView(tabId, { withHandoff: false });
    const engine = getEngine();
    const key = (url: string) => engine.dedupeKey(url, view.pageUrl);
    const resources = getResources(tabId);
    const manifests = getManifests(tabId);
    const built = buildMediaCandidates(resources, {
      fallbackTitle: "Video",
      pageTitle: view.pageTitle,
      pageUrl: view.pageUrl,
      manifests,
      hlsPlaylists: getHlsPlaylists(tabId),
      mirrorGroups: engine.mirrorGroups(),
      key,
    });
    const rows = [...view.playing, ...view.otherElements, ...view.unbound];
    const builtIds = new Set(built.map((candidate) => candidate.id));
    // 视图模型为直链 / 音视频配对元素合成的候选不在 buildMediaCandidates 里，一并导出。
    const candidates = [...built, ...rows.map((row) => row.candidate).filter((c) => !builtIds.has(c.id))];
    return buildResourceDebugLog({
      source: "sidepanel",
      tabId,
      pageUrl: view.pageUrl,
      pageTitle: view.pageTitle,
      resources,
      manifests,
      hlsPlaylists: getHlsPlaylists(tabId),
      candidates,
      elements: [...getElements(tabId), ...getRetiredElements(tabId)],
      bindings: new Map(
        rows.map((row) => [
          row.candidate.id,
          { elementKey: row.elementKey, binding: row.binding, playingVariantId: row.playingVariantId },
        ]),
      ),
      key,
    });
  },

  // 页面脚本管理：只接受扩展页（选项页 / popup），网页上下文的内容脚本消息一律拒绝。
  async pageScriptsStatus(req, sender) {
    if (!isExtensionPage(sender)) return refusedStatus();
    return getPageScriptsStatus(req.refresh);
  },

  async pageScriptsApprove(req, sender) {
    if (!isExtensionPage(sender)) return { ok: false, message: "forbidden", status: refusedStatus() };
    return approvePageScripts(req.identity);
  },

  async pageScriptsRevoke(req, sender) {
    if (!isExtensionPage(sender)) return { ok: false, message: "forbidden", status: refusedStatus() };
    return revokePageScripts(req.identity);
  },

  async pageScriptsSource(req, sender) {
    if (!isExtensionPage(sender)) return { ok: false, message: "forbidden" };
    return viewPageScriptSource(req.identity, req.id);
  },
};

interface TypedMessage {
  type?: unknown;
  action?: unknown;
  [key: string]: unknown;
}

/** 任务面板（popup ↔ background）：`type` 族消息。返回 undefined 表示不属于本族。 */
async function handleTypeMessage(message: TypedMessage): Promise<unknown> {
  switch (message.type) {
    case "nmh-tasks": {
      const result = await nmhListTasks();
      if (!result.success) return { ok: false, connected: false, tasks: [] };
      await processTasksPollResult(result.tasks);
      return { ok: true, connected: true, tasks: result.tasks };
    }
    case "nmh-task-op": {
      const { op, taskId } = message;
      if (typeof taskId !== "string" || !taskId || (op !== "pause" && op !== "resume" && op !== "remove")) {
        return { ok: false, message: "invalid_request" };
      }
      const result = await nmhTaskOp(op, taskId);
      return { ok: result.success, message: result.message };
    }
    case "nmh-open-file":
    case "nmh-reveal-file": {
      const { taskId } = message;
      if (typeof taskId !== "string" || !taskId) return { ok: false, message: "invalid_request" };
      const result =
        message.type === "nmh-open-file" ? await nmhOpenFile(taskId) : await nmhRevealFile(taskId);
      return { ok: result.success, message: result.message };
    }
    case "nmh-warmup":
      nmhWarmupNativeHost();
      return { ok: true };
    default:
      return undefined;
  }
}

function isMessageAction(action: unknown): action is MessageAction {
  return typeof action === "string" && Object.hasOwn(handlers, action);
}

/** 安装 runtime.onMessage 路由；返回诊断处理器已在内部接入。 */
export function installMessages(): void {
  const handleDiagnostics = installConnectionDiagnostics();

  async function handleMessage(message: TypedMessage, sender: Sender): Promise<unknown> {
    if (message.type === "diagnostics-record" || message.type === "diagnostics-export") {
      return handleDiagnostics({ type: message.type, event: message.event }, sender);
    }
    if (typeof message.type === "string" && ERROR_LOG_TYPES.includes(message.type)) {
      return handleErrorLogMessage(message, sender);
    }
    const typed = await handleTypeMessage(message);
    if (typed !== undefined) return typed;

    if (isMessageAction(message.action)) {
      // 按 action 分发；各 handler 的 req 形状由 MessageMap 约定，发送方经 sendAction 类型化。
      const handler = handlers[message.action] as (req: unknown, s: Sender) => Promise<unknown>;
      return handler(message, sender);
    }
    return { error: "Unknown action" };
  }

  if (import.meta.env.FIREFOX) {
    browser.runtime.onMessage.addListener((message, sender) => {
      const reply = handleMessage(message as TypedMessage, sender).catch((err) => ({
        error: String(err),
      }));
      // 类型声明只允许 boolean 返回；Firefox 运行时接受 Promise。
      return reply as unknown as true;
    });
  } else {
    browser.runtime.onMessage.addListener(
      (message, sender, sendResponse: (response?: unknown) => void) => {
        const typed = message as TypedMessage;
        const respond = (): void => {
          handleMessage(typed, sender).then(sendResponse, (err) =>
            sendResponse({ error: String(err) }),
          );
        };
        // 浮钮「打开资源面板」优先打开浏览器右侧侧边栏。sidePanel.open 只在用户手势的
        // 同步调用栈内有效（内容脚本点击产生的消息会携带手势），所以必须在任何 await 之前调用；
        // 不支持或失败时回退为页内面板（转发给 frame 0）。
        const tabId = senderTabId(sender);
        if (typed.action === "openPanel" && tabId !== undefined && typeof browser.sidePanel?.open === "function") {
          browser.sidePanel.open({ tabId }).then(() => sendResponse({ ok: true }), respond);
          return true;
        }
        respond();
        return true;
      },
    );
  }
}

export type { SendChannel };
