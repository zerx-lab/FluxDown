// Hand-written typings for the FluxDown plugin runtime: the `flux` global injected by
// FLUX_PRELUDE (every entry script) and HOST_PRELUDE (the resident `main` script), plus the
// entry-point shapes the engine calls (resolve / hooks / authenticate / subscribe / main).
// Mirrors native/engine/src/plugin/{quickjs.rs,quickjs_host.rs,runtime.rs}.
//
// Importing anything from "fluxdown-plugin-sdk" pulls these globals in.

import type { ViewChild, ViewNode } from "./view";

// ---------------------------------------------------------------------------
// flux.fetch / auth / storage / fs / task
// ---------------------------------------------------------------------------

export interface FluxFetchOptions {
  /** Default `GET`. */
  method?: string;
  url: string;
  headers?: Record<string, string>;
  /** Text body. */
  body?: string;
  /** Explicit credential reference; by default the host derives one from plugin id + site. */
  authRef?: string;
}

export interface FluxFetchResponse {
  status: number;
  headers: Record<string, string>;
  /** Text body (binary is unsupported). */
  body: string;
  /** The body was cut at the host's size limit. */
  truncated: boolean;
}

/** `flux.auth` — only present when `permissions` contains `auth`. */
export interface FluxAuth {
  get(authRef: string): Promise<unknown>;
  /** Persists the credential profile; resolves to its `authRef`. */
  save(profile: Record<string, unknown>): Promise<string>;
  /** Resolves to `true` once removed. */
  remove(authRef: string): Promise<true>;
}

export interface FluxStorage {
  /** `null` when the key is unset. */
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
}

/** Per-plugin scratch workspace (same root as the ffmpeg/yt-dlp cwd). Always available. */
export interface FluxFs {
  writeFile(name: string, content: string): Promise<void>;
  readFile(name: string): Promise<string | null>;
  remove(name: string): Promise<void>;
  list(): Promise<string[]>;
}

export interface FluxLogger {
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
}

export interface FluxTask {
  /** Synchronous; only effective inside `onError` (ignored with a warning elsewhere). */
  requestRetry(opts?: { delayMs?: number }): void;
  recordArtifact(name: string): Promise<void>;
}

export interface FluxInfo {
  identity: string;
  version: string;
  appVersion: string;
}

export interface ToolSpec {
  /** Arguments passed straight to the binary (no program name). */
  args: string[];
  /** Sub-directory of the workspace used as cwd (safe relative path). */
  subdir?: string;
  /** Per-call timeout; clamped by the host. */
  timeoutMs?: number;
}

export interface ToolOutcome {
  code: number;
  stdout: string;
  stderr: string;
  timedOut: boolean;
  truncatedStdout: boolean;
  truncatedStderr: boolean;
}

export interface ToolAvailability {
  available: boolean;
  version: string;
  /** `manual` | `managed` | `system` | `none`. */
  source: string;
}

/** `flux.ffmpeg` — only present when `permissions` contains `ffmpeg`. */
export interface FluxFfmpeg {
  available(): Promise<ToolAvailability>;
  run(spec: ToolSpec): Promise<ToolOutcome>;
}
/** `flux.ffprobe` — present together with `flux.ffmpeg`. */
export interface FluxFfprobe {
  run(spec: ToolSpec): Promise<ToolOutcome>;
}
/** `flux.ytdlp` — only present when `permissions` contains `ytdlp`. */
export interface FluxYtdlp {
  available(): Promise<ToolAvailability>;
  run(spec: ToolSpec): Promise<ToolOutcome>;
}

// ---------------------------------------------------------------------------
// HOST_PRELUDE (resident `main` script)
// ---------------------------------------------------------------------------

export type ToastLevel = "info" | "success" | "warning" | "error";

/** Delivered to a view's `event` handler. */
export interface ViewEvent {
  /** Id of the node that fired, e.g. `"root/save"` (derived from `key`s / indices). */
  node: string;
  type: "press" | "change" | "submit" | "rowAction" | "select";
  /** `change` / `submit` payload. */
  value?: unknown;
  /** Row / item key for `rowAction` and `select`. */
  key?: string;
  /** Row action id for `rowAction`. */
  action?: string;
}

/** Navigation target: a built-in page, or an `activity` view of this plugin. */
export type NavigateTarget = "downloads" | "rss" | "webhooks" | { view: string };

export interface ViewContext {
  /** Unique id of this open view session. */
  session: string;
  /** Props given when the view was opened (`{}` by default). */
  props: Record<string, unknown>;
  /** Task the view is attached to (taskDetail views); `null` otherwise. */
  taskId: string | null;
  locale: string;
  /** Requests a re-render (throttled to one render per 100 ms per session). */
  refresh(): void;
  /**
   * Closes the container hosting this view (login dialog, settings dialog, standalone
   * window) and ends the session. For views embedded in a page (activity page, task
   * detail tab) there is no container: the view just shows as "ended" with a reopen button.
   * Idempotent. May be called inside `init` (for example when already logged in): the client
   * shows the view and closes it right away.
   */
  close(): void;
  /**
   * Brings the main window to the front and switches to the target page. Rejects on an
   * unknown target or a `{ view }` that is not an `activity` view of this plugin. Ignored by
   * the client if the target page does not exist. Only the client that opened this view is
   * affected. Rejects when called after `close()` (navigate first, then close). May be called
   * inside `init`.
   */
  navigate(target: NavigateTarget): Promise<void>;
}

type Awaitable<T> = T | Promise<T>;

export interface ViewDefinition<S = Record<string, never>> {
  /** Builds the initial state (default `{}`). */
  init?(ctx: ViewContext): Awaitable<S>;
  /** Must be pure with respect to `state`; returns the node tree. */
  render(state: S, ctx: ViewContext): Awaitable<ViewNode>;
  /**
   * Handles a UI event. Mutate `state` in place (return nothing), or return a value
   * to replace the state. A render follows automatically.
   */
  event?(state: S, ev: ViewEvent, ctx: ViewContext): Awaitable<S | void>;
  /** Called when the session closes. */
  dispose?(state: S): Awaitable<void>;
}

/** Event passed to `flux.events.on` handlers (`PluginEvent`, camelCase). */
export type FluxEvent =
  | { event: "start"; taskId: string; url: string }
  | { event: "error"; taskId: string; url: string; message: string }
  | {
      event: "done";
      taskId: string;
      url: string;
      filePath: string;
      audioPath: string | null;
      muxed: boolean;
    }
  | { event: "metaProbed"; taskId: string; url: string; fileName: string; totalBytes: number }
  | {
      event: "cancel";
      taskId: string;
      url: string;
      reason: "user_cancelled" | "variant_selection_cancelled";
    };

export type FluxEventName = "onStart" | "onError" | "onDone" | "onMetaProbed" | "onCancel";

export interface FluxUi {
  /** Registers a portable view; `id` must match a `contributes.views[].id`. */
  view<S = Record<string, never>>(id: string, def: ViewDefinition<S>): void;
  /** Builds a node. Same shape as the JSX runtime output. */
  h(kind: string, props?: Record<string, unknown> | null, ...children: ViewChild[]): ViewNode;
  /** Fragment marker for `h`; children are spliced into the parent. */
  Fragment: (props?: { children?: ViewChild } | null) => ViewNode;
  toast(level: ToastLevel, text: string): void;
}

export interface FluxCommands {
  /** Registers the handler for a `contributes.commands[].id`. */
  register(id: string, handler: (ctx: { taskIds: string[] }) => Awaitable<void>): void;
}

/** Context passed as the handler's second argument. */
export type FluxRpcContext =
  | { readonly caller: "desktop" }
  | {
      readonly caller: "page";
      /** URL of the page whose script made the call; comes from the web page, treat as untrusted. */
      readonly pageUrl: string;
    };

export interface FluxRpcHandleOptions {
  /**
   * Also expose the handler to browser page scripts (`fluxdown.invoke(name, args)` in
   * `contributes.pageScripts`). Requires the `pageScripts` permission; default `false`
   * (desktop views only). Page-callable handlers must treat `args` and `ctx.pageUrl` as untrusted.
   */
  page?: boolean;
}

export interface FluxRpc {
  /**
   * Exposes `name` to the plugin's own desktop views via `fluxdown.invoke(name, args)`; with
   * `{ page: true }` also to the plugin's page scripts. Old one-argument handlers keep working.
   */
  handle(
    name: string,
    handler: (args: unknown, ctx: FluxRpcContext) => Awaitable<unknown>,
    options?: FluxRpcHandleOptions,
  ): void;
}

export interface FluxEvents {
  /** `name` must be listed in `main.events`. Resolved handlers trigger a re-render of open views. */
  on(name: FluxEventName, handler: (event: FluxEvent) => Awaitable<void>): void;
}

export interface FluxTimers {
  /** At most 64 live timers per plugin. */
  setTimeout(fn: () => void, ms: number): number;
  /**
   * Intervals are clamped to ≥1000 ms while a view session is open and ≥60000 ms otherwise.
   */
  setInterval(fn: () => void, ms: number): number;
  clear(id: number): void;
}

/** Daemon task wire shape (`daemon.task.*`), camelCase; see the protocol docs for fields. */
export type TaskDto = Record<string, unknown>;

/** `flux.tasks` — only present when `permissions` contains `tasks` (`main` script only). */
export interface FluxTasks {
  /**
   * Without arguments resolves to every task, but rejects when the library holds more than 5000
   * tasks. Pass `{ cursor, limit }` to page in ascending `taskId` order (`cursor` = the last
   * `taskId` of the previous page; a page shorter than `limit` is the last one; `limit` ≤ 5000).
   */
  list(params?: { cursor?: string; limit?: number }): Promise<TaskDto[]>;
  get(id: string): Promise<TaskDto>;
  /** Rate-limited to 20 per minute. Resolves to the new (or, with URL dedup, existing) task id. */
  create(params: CreateTaskParams): Promise<{ taskId: string }>;
  pause(id: string): Promise<{ ok: true }>;
  resume(id: string): Promise<{ ok: true }>;
  remove(id: string, deleteFiles?: boolean): Promise<{ ok: true }>;
}

/** `daemon.task.create` request body (`CreateTaskRequest`); every field except `url` is optional. */
export interface CreateTaskRequest {
  url: string;
  /** Empty = inferred from the URL / Content-Disposition. */
  fileName?: string;
  /** Empty = the global default save directory. */
  saveDir?: string;
  /** 0 = chosen automatically. */
  segments?: number;
  cookies?: string;
  referrer?: string;
  proxyUrl?: string;
  userAgent?: string;
  queueId?: string;
  /** `algo=hexhash`. */
  checksum?: string;
  ignoreTlsErrors?: boolean;
  headers?: Record<string, string>;
  /** Base64 torrent bytes; `url` may then be empty. */
  torrentB64?: string;
  method?: string;
  audioUrl?: string;
  /** `true` = create paused. */
  startPaused?: boolean;
  httpUser?: string;
  httpPassword?: string;
  saveSiteAuth?: boolean;
}

/** `daemon.task.create` parameters (`DaemonCreateTaskParams`). */
export interface CreateTaskParams {
  request: CreateTaskRequest;
  unattended?: boolean;
  /** Known file size in bytes (`> 0` takes effect). */
  hintFileSize?: number;
  /** Original file name used to complete a missing extension of `request.fileName`. */
  nameHint?: string;
}

export interface FluxSettingsHost {
  /** Writes plugin settings (values are strings, validated against `manifest.settings`). */
  update(entries: Record<string, string>): Promise<void>;
}

/** Typed settings values; `null` = unset string/number field without a default. */
export type FluxSettingsValues = Record<string, string | number | boolean | null>;

/** The `flux` facade shared by every entry script (resolve / hooks / auth / subscribe / main). */
export interface Flux {
  fetch(opts: FluxFetchOptions): Promise<FluxFetchResponse>;
  auth?: FluxAuth;
  storage: FluxStorage;
  fs: FluxFs;
  /** Typed settings values, keyed by `manifest.settings[].key`. */
  settings: FluxSettingsValues;
  info: FluxInfo;
  logger: FluxLogger;
  task: FluxTask;
  ffmpeg?: FluxFfmpeg;
  ffprobe?: FluxFfprobe;
  ytdlp?: FluxYtdlp;
}

/** The `flux` facade of the resident `main` script (received by `defineMain`). */
export interface MainFlux extends Omit<Flux, "settings"> {
  /** Values as in `Flux.settings`, plus the non-enumerable `update` method. */
  settings: FluxSettingsValues & FluxSettingsHost;
  ui: FluxUi;
  commands: FluxCommands;
  rpc: FluxRpc;
  events: FluxEvents;
  timers: FluxTimers;
  /** Only present when `permissions` contains `tasks`. */
  tasks?: FluxTasks;
}

// ---------------------------------------------------------------------------
// Entry-point contexts (what the engine passes to your globals)
// ---------------------------------------------------------------------------

/** `resolve(ctx)` argument. */
export interface ResolveContext {
  taskId: string;
  /** The original task URL. */
  url: string;
  /** Default credential reference for this site (empty unless the `auth` permission is granted). */
  authRef: string;
  cookies: string;
  referrer: string;
  userAgent: string;
  extraHeaders: Record<string, string>;
  /** Non-empty on the second stage of a manifest resolve (`<itemId>` or `<itemId>@<variantId>`). */
  resolverItem: string;
}

export interface ResolveVariant {
  label: string;
  url: string;
  audioUrl?: string;
  fileName?: string;
  totalBytes?: number;
  bandwidth?: number;
  width?: number;
  height?: number;
  container?: string;
}

export interface ManifestVariant {
  id: string;
  label: string;
  size?: number;
}

export interface ManifestItem {
  id: string;
  name: string;
  path?: string;
  size?: number;
  kind?: "" | "file";
  variants?: ManifestVariant[];
}

export interface ResolveResult {
  url?: string;
  audioUrl?: string;
  fileName?: string;
  totalBytes?: number;
  extraHeaders?: Record<string, string>;
  ephemeral?: boolean;
  rangeSupported?: boolean;
  variants?: ResolveVariant[];
  defaultVariantIndex?: number;
  /** Request overlay applied to this task's every run (same shape as the `prepare` result). */
  request?: RequestPatch;
  /** Multi-file listing; mutually exclusive with `url` / `variants` / `audioUrl`. */
  manifest?: { name?: string; items: ManifestItem[] };
}

/** `authenticate(ctx)` argument. */
export interface AuthContext {
  action: "begin" | "poll" | "cancel" | "logout" | "status";
  site: string;
  authRef: string;
  sessionId: string;
  input: string;
}

export interface AuthResult {
  status: "pending" | "success" | "error";
  sessionId?: string;
  /** QR content, data URL, or any challenge to show the user. */
  challenge?: string;
  /** How to render `challenge`, e.g. `"text"`. */
  challengeType?: string;
  message?: string;
  authRef?: string;
}

/** `subscribe(ctx)` argument. */
export interface SubscribeContext {
  providerId: string;
  sourceId: string;
  url: string;
  /** Opaque provider config JSON string. */
  providerConfig: string;
  cookies: string;
  userAgent: string;
}

export interface SubscribeItem {
  /** Required, non-empty, ≤2048 bytes; the dedup key. */
  guid: string;
  /** ≤1024 bytes. */
  title?: string;
  /** At least one of `link` / `enclosureUrl` is required (http/https/ftp/magnet/ed2k, ≤8KB). */
  link?: string;
  enclosureUrl?: string;
  /** Second-stage resolver id (≤2048 bytes); requires `link`. */
  resolverItem?: string;
  /** Bytes; must not be negative. */
  enclosureLength?: number;
  /** Unix timestamp (seconds). */
  pubDate?: number;
}

export interface SubscribeResult {
  title?: string;
  link?: string;
  items: SubscribeItem[];
}

export type ResolveFn = (ctx: ResolveContext) => Awaitable<ResolveResult | null | undefined>;
export type AuthFn = (ctx: AuthContext) => Awaitable<AuthResult>;
/** Returns the normalized subscription payload (at most 1000 items). */
export type SubscribeFn = (ctx: SubscribeContext) => Awaitable<SubscribeResult>;

// ---- Interceptor plane (manifest `interceptors`; entry `globalThis.intercept(ctx)`) ----

export type InterceptStage =
  | "prepare"
  | "probed"
  | "refresh"
  | "selectVariant"
  | "selectPeriod"
  | "segments"
  | "liveSegments"
  | "finalize"
  | "failed";

/** Same shape as the protocol `RequestBody`. */
export type CapturedBody =
  | { kind: "formData"; fields: Record<string, string[]> }
  | { kind: "urlencoded"; raw: string }
  | { kind: "raw"; bytesB64: string; contentType?: string | null };

export interface OriginHeaders {
  origin: string;
  headers: Record<string, string>;
}

export interface InterceptTask {
  id: string;
  /** Request URL of this run (after resolver rewrite). */
  url: string;
  /** Original persisted URL of the task. */
  sourceUrl: string;
  fileName: string;
  saveDir: string;
  protocol: "http" | "ftp" | "hls" | "dash";
  /** Audio/video track-pair task (has an audio URL). */
  trackPair: boolean;
  isResume: boolean;
}

export interface InterceptRequest {
  url: string;
  method: string;
  cookies: string;
  referrer: string;
  /** Effective UA: the one in request headers, else the task/queue/global resolved UA. */
  userAgent: string;
  headers: Record<string, string>;
  body: CapturedBody | null;
  originHeaders: OriginHeaders[];
  /** Read-only. */
  proxyUrl: string;
  /** Read-only. */
  ignoreTlsErrors: boolean;
}

/** 0 = automatic / unlimited. */
export interface InterceptConnections {
  segments: number;
  maxConnections: number;
  autoMaxConnections: number;
}

export interface SegmentItem {
  /** HLS: absolute media sequence; DASH: 0-based index in the track's media segment list (init excluded). */
  id: number;
  uri: string;
  duration: number | null;
  byteRange: { offset: number; length: number } | null;
  discontinuity: boolean;
  title: string | null;
  /** RFC 3339. */
  programDateTime: string | null;
  dateRange: {
    id: string;
    class: string | null;
    startDate: string;
    endDate: string | null;
    duration: number | null;
    plannedDuration: number | null;
    endOnNext: boolean;
    attributes: Record<string, string>;
  } | null;
  /** Raw tags the engine does not interpret, e.g. `"#EXT-X-CUE-OUT:30"`. */
  tags: string[];
  key: { method: "AES-128"; uri: string; iv: string | null } | null;
  map: { uri: string; byteRange: { offset: number; length: number } | null } | null;
  /** DASH SegmentTemplate only. */
  number: number | null;
  time: number | null;
}

export interface InterceptVariant {
  index: number;
  bandwidth: number;
  width: number | null;
  height: number | null;
  uri: string;
  id: string | null;
  codecs: string | null;
}

export interface InterceptPeriod {
  index: number;
  id: string | null;
  startSeconds: number | null;
  durationSeconds: number | null;
  hasVideo: boolean;
  hasAudio: boolean;
}

export type InterceptContext =
  | { stage: "prepare"; task: InterceptTask; request: InterceptRequest; connections: InterceptConnections }
  | {
      stage: "probed";
      task: InterceptTask;
      request: InterceptRequest;
      connections: InterceptConnections;
      probe: {
        totalBytes: number;
        supportsRange: boolean;
        fileName: string;
        contentType: string;
        etag: string;
        lastModified: string;
      };
    }
  | { stage: "selectVariant"; task: InterceptTask; format: "hls" | "dash"; variants: InterceptVariant[] }
  | {
      stage: "selectPeriod";
      task: InterceptTask;
      /** MPD text; empty when over 4 MiB (see `manifestTextOmitted`). */
      manifestText: string;
      manifestTextOmitted: boolean;
      periods: InterceptPeriod[];
    }
  | {
      stage: "refresh";
      task: InterceptTask;
      /** Identity of the rejected request (`url` is the actual request address). */
      request: InterceptRequest;
      /** Rejecting HTTP status (401 / 403 / 410). */
      status: number;
      /** Successful refreshes already done in this run. */
      refreshCount: number;
    }
  | {
      stage: "segments" | "liveSegments";
      task: InterceptTask;
      format: "hls" | "dash";
      track: "video" | "audio";
      playlistUrl: string;
      manifestText: string;
      manifestTextOmitted: boolean;
      mediaSequence: number | null;
      targetDuration: number | null;
      periodIndex: number | null;
      representationId: string | null;
      segments: SegmentItem[];
    }
  | {
      stage: "finalize";
      task: InterceptTask;
      filePath: string;
      fileName: string;
      saveDir: string;
      totalBytes: number;
      audioPath: string | null;
    }
  | {
      stage: "failed";
      task: InterceptTask;
      errorMessage: string;
      httpStatus: number | null;
      autoRetryCount: number;
    };

/** `prepare` result; also the shape of a resolver result's `request` field. */
export interface RequestPatch {
  url?: string;
  method?: string;
  body?: CapturedBody;
  clearBody?: boolean;
  cookies?: string;
  referrer?: string;
  userAgent?: string;
  setHeaders?: Record<string, string>;
  removeHeaders?: string[];
  originHeaders?: OriginHeaders[];
  segments?: number;
  maxConnections?: number;
  /**
   * HTTP only; must be >= 65536 (64 KiB). Bytes. Caps the `Range` span of every data request
   * of this task: each segment worker requests its range in windows of at most this size, and
   * single-stream / fallback downloads send only bounded `Range` requests of at most this
   * size (never a plain GET or an open-ended `bytes=N-`). Use it for direct links that reject
   * large ranges, e.g. a cloud-drive CDN that answers 403 above ~5 MiB per request. Needs a
   * known `totalBytes` and a server that honours `Range`; the metadata probe requests are
   * unaffected. Not allowed in `refresh` results.
   */
  maxRangeBytes?: number;
  /**
   * HTTP only, same meaning as in a resolver result: skip the metadata probe for this run
   * (use for one-time credentials a probe would burn).
   */
  ephemeral?: boolean;
  /** HTTP only; requires `ephemeral: true`; must be > 0. Size hint used instead of probing. */
  totalBytes?: number;
  /** HTTP only; requires `ephemeral: true`. Whether the server is known to support ranges. */
  rangeSupported?: boolean;
  fileName?: string;
}
/**
 * `refresh` result: the credential subset of `RequestPatch`. `url` is only allowed for HTTP
 * tasks (same host, same protocol class).
 */
export type RefreshPatch = Pick<
  RequestPatch,
  "url" | "cookies" | "referrer" | "userAgent" | "setHeaders" | "removeHeaders" | "originHeaders"
>;
export interface ProbedResult {
  segments?: number;
  maxConnections?: number;
  fail?: string;
}
export interface SelectResult {
  select?: number;
}
export interface SegmentsResult {
  drop?: number[];
  /** Replace segment URIs (e.g. per-segment signed addresses); relative values resolve against `playlistUrl`. */
  rewrite?: { id: number; uri: string }[];
}
export interface FinalizeResult {
  fail?: string;
  replaceWith?: string;
}
export interface FailedResult {
  retry?: { delayMs?: number };
}

type InterceptCtx<S extends InterceptStage> = Extract<InterceptContext, { stage: S }>;

export interface InterceptHandlers {
  prepare?(ctx: InterceptCtx<"prepare">): Awaitable<RequestPatch | null | undefined>;
  probed?(ctx: InterceptCtx<"probed">): Awaitable<ProbedResult | null | undefined>;
  refresh?(ctx: InterceptCtx<"refresh">): Awaitable<RefreshPatch | null | undefined>;
  selectVariant?(ctx: InterceptCtx<"selectVariant">): Awaitable<SelectResult | null | undefined>;
  selectPeriod?(ctx: InterceptCtx<"selectPeriod">): Awaitable<SelectResult | null | undefined>;
  segments?(ctx: InterceptCtx<"segments">): Awaitable<SegmentsResult | null | undefined>;
  liveSegments?(ctx: InterceptCtx<"liveSegments">): Awaitable<SegmentsResult | null | undefined>;
  finalize?(ctx: InterceptCtx<"finalize">): Awaitable<FinalizeResult | null | undefined>;
  failed?(ctx: InterceptCtx<"failed">): Awaitable<FailedResult | null | undefined>;
}

export interface HookHandlers {
  onStart?(ev: Extract<FluxEvent, { event: "start" }>): Awaitable<void>;
  onError?(ev: Extract<FluxEvent, { event: "error" }>): Awaitable<void>;
  onDone?(ev: Extract<FluxEvent, { event: "done" }>): Awaitable<void>;
  onMetaProbed?(ev: Extract<FluxEvent, { event: "metaProbed" }>): Awaitable<void>;
  onCancel?(ev: Extract<FluxEvent, { event: "cancel" }>): Awaitable<void>;
}

declare global {
  /** The plugin runtime facade. */
  const flux: Flux;
  /**
   * Console routed to the plugin log. Declared as a plain variable: do not enable the DOM
   * lib in plugin projects (the QuickJS host has no DOM).
   */
  var console: {
    log(...args: unknown[]): void;
    info(...args: unknown[]): void;
    warn(...args: unknown[]): void;
    error(...args: unknown[]): void;
    debug(...args: unknown[]): void;
  };
}
