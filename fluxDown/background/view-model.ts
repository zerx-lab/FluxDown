/**
 * TabView 视图模型。核心 `composeTabView` 是纯函数（可在 bun 下测试）：
 * 资源 + DASH 清单 + 媒体元素 → 候选 → 绑定到元素 → 分组（当前播放 / 其它元素 / 未关联 / 文件）。
 *
 * 取数（tab-state / settings / host）在 `tab-view.ts`，本文件保持零 browser 依赖。
 */
import { urlGlobMatch } from "@/rules/glob";
import type { CompiledRules } from "@/rules/engine";
import type { DashManifest } from "@/utils/dash-manifest";
import { browserOnlyReason } from "@/utils/download-source";
import type { ServiceWorkerScopes } from "@/utils/download-source";
import {
  buildMediaCandidates,
  isMediaCandidateVisible,
} from "@/utils/media-candidates";
import type { DashManifestEntry, MediaCandidate, MediaCandidateVariant } from "@/utils/media-candidates";
import type { HlsPlaylistEntry } from "@/utils/hls-playlist";
import type { DetectedResource, ResourceType } from "@/utils/resource-types";
import type { RemoteMode } from "@/utils/settings";
import { activeElement, bindCandidates } from "./media-binding";
import type { CandidateBinding } from "./media-binding";
import type {
  CandidateRow,
  ElementState,
  HandoffState,
  HostCapabilities,
  ProbeState,
  ResourceRow,
  TabView,
} from "./protocol";

// ===== 音视频轨配对（迁自 utils/resource-types.groupTrackPairs，判轨改为 engine.trackKind） =====

/** 一组「清晰度 + 视频轨 + 可选音频轨」的下载配对。 */
export interface TrackPairGroup {
  /** 清晰度标签：优先取 DetectedResource.quality，否则按 size 降序生成占位档位名 */
  quality: string;
  videoUrl: string;
  /** 缺省 = 该清晰度无独立音频轨，视频轨可单独直下 */
  audioUrl?: string;
  videoRes: DetectedResource;
  audioRes?: DetectedResource;
}

export type TrackKindOf = (resource: DetectedResource) => "video" | "audio" | null;

/**
 * 把一个 tab 内嗅探到的媒体资源（video/audio/stream）按轨道类型 + 清晰度分组为
 * 「视频轨 + 共享音频轨」列表。判轨由 `trackKindOf`（engine.trackKind）给出；
 * 判不出的（null）：只有 1 条时归为视频轨；≥2 条且大小均已知时最小的一条视为音频轨，
 * 其余视频轨；否则全部归为独立视频轨，不猜测音频配对。音频轨取体积最大的一条供各视频档共用。
 */
export function groupTrackPairs(
  resources: DetectedResource[],
  trackKindOf: TrackKindOf,
): TrackPairGroup[] {
  const candidates = resources.filter(
    (r) => r.type === "video" || r.type === "audio" || r.type === "stream",
  );
  if (candidates.length === 0) return [];

  const videoTracks: DetectedResource[] = [];
  const audioTracks: DetectedResource[] = [];
  const unknown: DetectedResource[] = [];
  for (const r of candidates) {
    const kind = trackKindOf(r);
    if (kind === "audio") audioTracks.push(r);
    else if (kind === "video") videoTracks.push(r);
    else unknown.push(r);
  }

  if (unknown.length === 1) {
    videoTracks.push(unknown[0]);
  } else if (unknown.length >= 2) {
    if (unknown.every((r) => r.size > 0)) {
      const sorted = [...unknown].sort((a, b) => b.size - a.size);
      audioTracks.push(sorted[sorted.length - 1]);
      videoTracks.push(...sorted.slice(0, -1));
    } else {
      videoTracks.push(...unknown);
    }
  }

  if (videoTracks.length === 0) return [];

  const sharedAudio =
    audioTracks.length > 0
      ? audioTracks.reduce((best, cur) => (cur.size > best.size ? cur : best))
      : undefined;
  const sortedVideo = [...videoTracks].sort((a, b) => b.size - a.size);
  return sortedVideo.map((v, idx) => ({
    quality: v.quality || `画质${idx + 1}`,
    videoUrl: v.url,
    audioUrl: sharedAudio?.url,
    videoRes: v,
    audioRes: sharedAudio,
  }));
}

// ===== 转交桌面端解析（HandoffState） =====

export interface HandoffInput {
  remoteMode: RemoteMode;
  caps: HostCapabilities;
  pageUrl: string;
}

export function handoffFor({ remoteMode, caps, pageUrl }: HandoffInput): HandoffState {
  if (remoteMode === "always") return { kind: "unavailable", reason: "remote-mode" };
  if (!caps.connected) return { kind: "unavailable", reason: "disconnected" };
  if (caps.protocol === 1) return { kind: "unavailable", reason: "old-app" };
  if (!caps.pluginsSupported) return { kind: "unavailable", reason: "plugins-unsupported" };

  let disabled: HandoffState | null = null;
  for (const resolver of caps.resolvers) {
    if (!resolver.urls.some((pattern) => urlGlobMatch(pattern, pageUrl))) continue;
    if (resolver.enabled && resolver.loaded) {
      return { kind: "match", identity: resolver.identity, name: resolver.name };
    }
    disabled ??= { kind: "disabled", identity: resolver.identity, name: resolver.name };
  }
  return disabled ?? { kind: "no-match", installed: caps.resolvers.length };
}

// ===== 视图模型核心 =====

export interface ViewModelInput {
  tabId: number;
  pageUrl: string;
  pageTitle: string;
  version: number;
  resources: DetectedResource[];
  manifests: DashManifestEntry[];
  hlsPlaylists: HlsPlaylistEntry[];
  /** mirror 规则声明的镜像域名组。 */
  mirrorGroups: string[][];
  elements: ElementState[];
  /** 历史播放会话（tab-state 归档，新的在前）：参与关联，不会成为当前播放元素。 */
  retiredElements: ElementState[];
  engine: Pick<CompiledRules, "dedupeKey" | "trackKind">;
  showLowConfidence: boolean;
  handoff: HandoffState;
  chooseManualTarget: boolean;
  site: TabView["site"];
  /** 探测协调器给出的基础状态（live / recovering / unavailable / unknown）。 */
  probe: ProbeState;
  /** 钩子补注入晚于页面加载的帧。 */
  lateFrames: ReadonlySet<number>;
  /** 页面上报的 Service Worker 作用域：作用域内、没有网络证据的地址只能由浏览器取得。 */
  serviceWorkerScopes: ServiceWorkerScopes;
  /** 该 URL 近期是否真的发往过网络（webRequest 请求头缓存）。 */
  sawNetworkRequest: (url: string) => boolean;
}

const MEDIA_TYPES: ReadonlySet<ResourceType> = new Set(["video", "stream", "audio"]);

function rowCount(candidate: MediaCandidate): number {
  return Math.max(candidate.variants.length, 1);
}

/** 元素同时绑定 video 与 audio 两类 SourceBuffer。 */
function hasBothKinds(element: ElementState): boolean {
  const kinds = new Set(element.bindings.map((b) => b.kind));
  return kinds.has("video") && kinds.has("audio");
}

/** 直链播放（currentSrc）而候选里没有对应项时，为元素合成一个单 variant 候选。 */
function synthesizeCurrentSrc(
  element: ElementState,
  existing: MediaCandidate[],
  key: (u: string) => string,
  pageTitle: string,
  pageUrl: string,
): MediaCandidate | null {
  if (!element.currentSrc) return null;
  const k = key(element.currentSrc);
  const covered = existing.some((c) =>
    c.variants.some((v) => key(v.videoUrl) === k || (v.audioUrl && key(v.audioUrl) === k)),
  );
  if (covered) return null;
  const id = `element:${element.key}`;
  return {
    id,
    title: pageTitle || "Video",
    type: "video",
    source: "direct",
    pageUrl,
    variants: [
      {
        id: `${id}:0`,
        label: element.videoHeight > 0 ? `${element.videoHeight}p` : "original",
        videoUrl: element.currentSrc,
      },
    ],
    rawResourceIds: [],
    fragmentCount: 0,
    downloadable: true,
  };
}

/**
 * 同时向音频与视频 SourceBuffer 追加了完整文件直链的元素（音视频分离播放）：把两条轨配成一个候选。
 *
 * 轨道类型以「数据被追加进哪个 SourceBuffer」为准——这是播放器自己的判断，比 MIME（分离的音轨
 * 常被服务端标成 video/mp4）、URL 关键字或文件大小都可靠。
 */
function synthesizePair(
  element: ElementState,
  resources: DetectedResource[],
  existing: MediaCandidate[],
  input: ViewModelInput,
  key: (u: string) => string,
): MediaCandidate | null {
  const kindOf = new Map<string, "video" | "audio">();
  for (const binding of element.bindings) {
    if (binding.kind === "unknown") continue;
    for (const url of binding.exact) kindOf.set(key(url), binding.kind);
  }
  // 已经归入清单 / 播放列表候选的分片不是独立轨道，不能再配成「音视频对」；直链候选（音视频
  // 分离的两个完整文件）正是配对的对象。
  const represented = new Set(
    existing.filter((c) => c.source === "dash" || c.source === "hls").flatMap((c) => c.rawResourceIds),
  );
  const used = resources.filter((r) => kindOf.has(key(r.url)) && !represented.has(r.id));
  const groups = groupTrackPairs(used, (r) => kindOf.get(key(r.url)) ?? null).filter((g) => g.audioUrl);
  if (groups.length === 0) return null;

  const id = `pair:${element.key}`;
  const variants: MediaCandidateVariant[] = groups.map((g, i) => ({
    id: `${id}:${i}`,
    label: g.videoRes.quality || (element.videoHeight > 0 ? `${element.videoHeight}p` : g.quality),
    videoUrl: g.videoUrl,
    audioUrl: g.audioUrl,
    mimeType: g.videoRes.mimeType,
    fileSize: g.videoRes.size > 0 ? g.videoRes.size + (g.audioRes && g.audioRes.size > 0 ? g.audioRes.size : 0) : undefined,
    resourceId: g.videoRes.id,
  }));
  return {
    id,
    title: input.pageTitle || "Video",
    type: "video",
    source: "direct",
    pageUrl: input.pageUrl,
    variants,
    rawResourceIds: groups.flatMap((g) => [g.videoRes.id, g.audioRes?.id ?? ""]).filter(Boolean),
    fragmentCount: 0,
    downloadable: true,
  };
}

function isLowConfidence(candidate: MediaCandidate, byId: Map<string, DetectedResource>): boolean {
  const raws = candidate.rawResourceIds
    .map((id) => byId.get(id))
    .filter((r): r is DetectedResource => !!r);
  return raws.length > 0 && raws.every((r) => r.confidence === "low");
}

/** 元素是否完全没有来源证据（无 http 直链、MSE 归因里没有任何 URL）。 */
function hasNoSourceEvidence(element: ElementState): boolean {
  return !element.currentSrc && element.bindings.every((b) => b.exact.length === 0 && b.temporal.length === 0);
}

/**
 * 规则声明的归属：dom 规则命中（或包含）媒体元素时，产出的资源带着该元素的 key——如站点解析插件
 * 把视频页本身声明为播放器的下载源（播放数据来自 MSE 分片，没有可直接下载的 URL 证据）。
 * 只补绑没有 URL 证据的候选，且它的媒体资源只指向一个现存元素。
 */
function bindDeclared(
  candidates: MediaCandidate[],
  elements: ElementState[],
  bindings: Map<string, CandidateBinding>,
  byId: Map<string, DetectedResource>,
): void {
  const present = new Set(elements.map((e) => e.key));
  for (const candidate of candidates) {
    if (bindings.has(candidate.id)) continue;
    const keys = new Set<string>();
    for (const id of candidate.rawResourceIds) {
      const resource = byId.get(id);
      if (resource?.elementKey && MEDIA_TYPES.has(resource.type) && present.has(resource.elementKey)) {
        keys.add(resource.elementKey);
      }
    }
    if (keys.size !== 1) continue;
    const [elementKey] = keys;
    bindings.set(candidate.id, { elementKey, playingVariantId: null, confidence: "declared" });
  }
}

/**
 * 推断关联：播放器在 Worker 里取数据（或用 MSE in Worker）时，页面钩子看不到任何来源证据，
 * 浮钮会因此永远不出现。只有在没有歧义时才推断——整页恰好一个正在播放的元素、它没有任何证据、
 * 且恰好一个尚未关联、可下载、非低可信、非换页前的候选。
 */
function inferBinding(
  candidates: MediaCandidate[],
  elements: ElementState[],
  bindings: Map<string, CandidateBinding>,
  byId: Map<string, DetectedResource>,
): void {
  const playing = elements.filter((e) => e.playing);
  if (playing.length !== 1) return;
  const element = playing[0];
  if (!hasNoSourceEvidence(element)) return;
  if ([...bindings.values()].some((b) => b.elementKey === element.key)) return;
  const eligible = candidates.filter((c) =>
    c.downloadable && !c.provisional && !bindings.has(c.id) && !isLowConfidence(c, byId),
  );
  if (eligible.length !== 1) return;
  bindings.set(eligible[0].id, { elementKey: element.key, playingVariantId: null, confidence: "inferred" });
}

/**
 * 按 SourceBuffer 证据纠正轨道类型：被追加进音频 SourceBuffer（且从未进过视频 SourceBuffer）的
 * 完整文件是音轨，即便服务端把它标成 video/mp4——这是播放器自己的判断。音轨不会再作为一条
 * 「视频」直链单独出现，只在配对候选里出现。
 */
function withSourceBufferKinds(
  resources: DetectedResource[],
  elements: ElementState[],
  key: (u: string) => string,
): DetectedResource[] {
  const audio = new Set<string>();
  const video = new Set<string>();
  for (const element of elements) {
    for (const binding of element.bindings) {
      const into = binding.kind === "audio" ? audio : binding.kind === "video" ? video : null;
      if (into) for (const url of binding.exact) into.add(key(url));
    }
  }
  if (audio.size === 0) return resources;
  return resources.map((r) => {
    if (r.type !== "video") return r;
    const k = key(r.url);
    return audio.has(k) && !video.has(k) ? { ...r, type: "audio" } : r;
  });
}

export function composeTabView(input: ViewModelInput): TabView {
  const { engine, elements } = input;
  const key = (url: string) => engine.dedupeKey(url, input.pageUrl);
  // 真实元素在前：同分时保留真实元素（历史会话只承接真实元素已不再持有的证据）
  const evidenceElements = input.retiredElements.length > 0 ? [...elements, ...input.retiredElements] : elements;
  const resources = withSourceBufferKinds(input.resources, evidenceElements, key);
  const byId = new Map(resources.map((r) => [r.id, r]));

  const built = buildMediaCandidates(resources, {
    fallbackTitle: "Video",
    pageTitle: input.pageTitle,
    pageUrl: input.pageUrl,
    manifests: input.manifests,
    hlsPlaylists: input.hlsPlaylists,
    mirrorGroups: input.mirrorGroups,
    key,
  });
  const candidates = [...built];
  for (const element of evidenceElements) {
    const synthesized = synthesizeCurrentSrc(element, candidates, key, input.pageTitle, input.pageUrl);
    if (synthesized) candidates.push(synthesized);
  }
  // 音视频分离播放的元素合成「音视频对」；被吸收的单轨直链不再单独出现（否则会看到两个
  // 「播放中」的同名候选，各自只有一半内容）。
  for (const element of evidenceElements.filter(hasBothKinds)) {
    const pair = synthesizePair(element, resources, candidates, input, key);
    if (!pair) continue;
    const absorbed = new Set(pair.rawResourceIds);
    for (let i = candidates.length - 1; i >= 0; i--) {
      const c = candidates[i];
      if (c.source === "direct" && c.rawResourceIds.length > 0 && c.rawResourceIds.every((id) => absorbed.has(id))) {
        candidates.splice(i, 1);
      }
    }
    candidates.push(pair);
  }

  const bindings = bindCandidates(candidates, evidenceElements, key);
  const active = activeElement(elements);
  bindDeclared(candidates, elements, bindings, byId);
  inferBinding(candidates, elements, bindings, byId);
  const elementsByKey = new Map(evidenceElements.map((element) => [element.key, element]));
  // webRequest 嗅探到的资源本身就是网络证据（请求头缓存只保留 60 s）。
  const networkSeen = new Set(resources.filter((r) => r.detectedBy === "webRequest").map((r) => key(r.url)));
  const evidenceOf = (url: string) => (networkSeen.has(key(url)) || input.sawNetworkRequest(url) ? "network" : "none");
  // 规则（含桌面端插件下发的 plugin 层）显式声明的资源：地址由规则作者担保可交给 FluxDown
  // （如 YouTube 播放页 URL 交给桌面端插件解析），SW 合成判定只针对没有规则覆盖的站点。
  const ruleDeclared = new Set(resources.filter((r) => (r.ruleIds?.length ?? 0) > 0).map((r) => key(r.url)));
  /**
   * 关联到元素的候选带上元素的事实：元素标题（比页面标题准确，如推荐流 / 弹窗播放器里的单个视频）
   * 与 DRM 状态（EME 加密的内容下载得到的是密文）。
   */
  const withElementFacts = (candidate: MediaCandidate, binding: CandidateBinding | undefined): MediaCandidate => {
    const element = binding ? elementsByKey.get(binding.elementKey) : undefined;
    const title = element?.title ?? "";
    const encrypted = element?.encrypted === true;
    // 每一档都有地址是 SW 合成的：整个候选只能由浏览器取得。
    const browserOnly =
      candidate.variants.length > 0 &&
      candidate.variants.every((v) =>
        [v.videoUrl, v.audioUrl].some(
          (url) =>
            url !== undefined &&
            !ruleDeclared.has(key(url)) &&
            browserOnlyReason([url], input.serviceWorkerScopes, evidenceOf) === "service-worker",
        ),
      );
    if (!title && !encrypted && !browserOnly) return candidate;
    return {
      ...candidate,
      ...(title ? { title } : {}),
      ...(encrypted ? { protected: true } : {}),
      ...(browserOnly ? { browserOnly: true } : {}),
    };
  };
  const rows = (candidate: MediaCandidate, binding: CandidateBinding | undefined): CandidateRow => ({
    candidate: withElementFacts(candidate, binding),
    elementKey: binding?.elementKey ?? null,
    playingVariantId: binding?.playingVariantId ?? null,
    binding: binding?.confidence ?? "none",
    ruleIds: [
      ...new Set(candidate.rawResourceIds.flatMap((id) => byId.get(id)?.ruleIds ?? [])),
    ],
  });

  const playing: CandidateRow[] = [];
  const otherElements: CandidateRow[] = [];
  const unbound: CandidateRow[] = [];
  let hiddenLowConfidence = 0;
  const counts: TabView["counts"] = {};
  const addCount = (type: ResourceType, n: number) => {
    counts[type] = (counts[type] ?? 0) + n;
    counts.all = (counts.all ?? 0) + n;
  };

  // 换页前的清单（预取的下一个视频）只有在关联到播放元素后才是当前页的内容。
  const shown = candidates.filter((candidate) => !candidate.provisional || bindings.has(candidate.id));
  for (const candidate of shown) {
    const synthetic = candidate.id.startsWith("element:") || candidate.id.startsWith("pair:");
    if (!synthetic && !isMediaCandidateVisible(candidate, shown)) continue;
    const binding = bindings.get(candidate.id);
    if (!binding && !input.showLowConfidence && isLowConfidence(candidate, byId)) {
      hiddenLowConfidence++;
      continue;
    }
    const row = rows(candidate, binding);
    if (binding && active && binding.elementKey === active.key) playing.push(row);
    else if (binding) otherElements.push(row);
    else unbound.push(row);
    addCount(candidate.type, rowCount(candidate));
  }

  const represented = new Set(candidates.flatMap((c) => c.rawResourceIds));
  const files: ResourceRow[] = [];
  for (const resource of resources) {
    if (MEDIA_TYPES.has(resource.type) || represented.has(resource.id)) continue;
    if (resource.confidence === "low" && !input.showLowConfidence) {
      hiddenLowConfidence++;
      continue;
    }
    files.push({ resource, ruleIds: resource.ruleIds ?? [] });
    addCount(resource.type, 1);
  }

  // 钩子在页面开始播放之后才补装（扩展安装 / 更新时页面已打开）：当前播放元素的 MediaSource
  // 关联已经错过、拿不到任何来源证据，只有刷新页面才能完整识别。
  const degraded =
    input.probe === "live" &&
    active !== null &&
    active.playing &&
    input.lateFrames.has(active.frameId) &&
    hasNoSourceEvidence(active);
  return {
    tabId: input.tabId,
    pageUrl: input.pageUrl,
    pageTitle: input.pageTitle,
    version: input.version,
    activeElementKey: active?.key ?? null,
    playing,
    otherElements,
    unbound,
    files,
    hiddenLowConfidence,
    counts,
    handoff: input.handoff,
    canChoosePageTarget: input.chooseManualTarget && /^https?:\/\//i.test(input.pageUrl),
    site: input.site,
    probe: degraded ? "degraded" : input.probe,
  };
}

/** 徽标数 = playing / otherElements / unbound 各候选的 max(variants,1) 之和 + files.length。 */
export function badgeCount(view: TabView): number {
  const rowsTotal = [...view.playing, ...view.otherElements, ...view.unbound].reduce(
    (sum, row) => sum + rowCount(row.candidate),
    0,
  );
  return rowsTotal + view.files.length;
}

/** 绑定到某元素的候选行（浮钮 / 右键菜单用）。 */
export function rowsForElement(view: TabView, elementKey: string): CandidateRow[] {
  return [...view.playing, ...view.otherElements].filter((row) => row.elementKey === elementKey);
}

/** 视图里所有候选行。 */
export function allRows(view: TabView): CandidateRow[] {
  return [...view.playing, ...view.otherElements, ...view.unbound];
}
