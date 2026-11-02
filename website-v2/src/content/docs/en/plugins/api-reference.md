---
title: Plugin API Reference
description: Every entry-point function, the flux.* API, the JavaScript environment, runtime limits and the error messages a plugin can see.
section: plugins
order: 11
---

This page lists everything a plugin script can see: the entry-point functions FluxDown calls, the `flux` object it injects, the JavaScript environment, the runtime limits and the error messages you will meet. It is a reference; for step-by-step walkthroughs see [Your first plugin](/docs/en/plugins/your-first-plugin/), the [TypeScript project](/docs/en/plugins/typescript-project/) tutorial, the [hooks guide](/docs/en/plugins/hooks-guide/), the [auth guide](/docs/en/plugins/auth-guide/), the [subscription guide](/docs/en/plugins/subscription-guide/), the [portable UI guide](/docs/en/plugins/ui-guide/) and the [desktop UI guide](/docs/en/plugins/desktop-ui-guide/).

Conventions:

- All field names crossing the JS boundary are camelCase.
- Entry points are plain global functions. `async` functions and returned Promises are awaited.
- Error messages are produced by the host and are **in Chinese regardless of the UI language**. They are quoted verbatim below so you can search your logs for them; the English text is only an explanation. See [Errors you will see](#errors-you-will-see).
- Limits quoted here are enforced by the host and cannot be raised by a plugin.

## Entry points

| Entry | Called when | Declared by | Script file | Returns |
|---|---|---|---|---|
| [`resolve(ctx)`](#resolvectx) | A matching task starts or resumes (and for pre-resolution in the new-download window). | `resolvers[0]` | `resolvers[0].entry` | Rewrite result, or `null` / `undefined` |
| [`onStart`](#hooks-onstart-ondone-onerror-oncancel-onmetaprobed) | A task starts for the first time. | `hooks.events` | `hooks.entry` | ignored |
| `onMetaProbed` | The metadata probe found a file name or size. | `hooks.events` | `hooks.entry` | ignored |
| `onDone` | A task finished successfully. | `hooks.events` | `hooks.entry` | ignored |
| `onError` | A task entered the failed state. | `hooks.events` | `hooks.entry` | ignored |
| `onCancel` | The user dismissed a selection dialog (plugin choice or variant picker) and the task was paused. | `hooks.events` | `hooks.entry` | ignored |
| [`authenticate(ctx)`](#authenticatectx) | The host drives a login (`begin` / `poll` / `cancel` / `logout` / `status`). | `auth.entry` + permission `auth` | `auth.entry` | [`AuthResult`](#authenticatectx) object |
| [`subscribe(ctx)`](#subscribectx) | A subscription source of your provider is refreshed. | `subscriptions[0]` | `subscriptions[0].entry` | Normalized feed object |
| [`main`](#the-main-script) | The daemon (or a view) activates your resident script. | `main` | `main.entry` | — (registers handlers) |
| [`archive(ctx)`](#archivectx) | A category-routed task has a category directory (a matching rule's directory, or the "Other" fallback); called at start and, if no folder was chosen, at completion. | `contributes.archive` + permission `archive` | The `.js` file named by `contributes.archive` | `{ folderName }`, `null` or `undefined` |

### `archive(ctx)`

```js
import { defineArchive } from "fluxdown-plugin-sdk/runtime";

defineArchive(({ fileName }) => {
  if (/\.(mkv|mp4)$/i.test(fileName)) return { folderName: "Videos by plugin" };
  return null;
});
```

The callback chooses **one subfolder inside the task's category directory** from the final file name. It never changes the file name: the final path is `<categoryDir>/<folderName>/<fileName>`. `ctx` is `{ fileName }`; the function may be sync or async and returns `{ folderName: string }`, or `null` / `undefined` for no subfolder. Each call uses a fresh QuickJS runtime.

**What exists in the archive script.** Only `flux.settings` (typed plugin settings), `flux.logger.info|warn|error` and `console.log|info|warn|error|debug` (the logger and `console` both write to the plugin log). There is no `flux.info`, `flux.storage`, `flux.fetch`, `flux.fs`, `flux.task`, `flux.ffmpeg`, `flux.ytdlp`, `flux.auth` or `flux.rpc` / `flux.ui`, and no file, network, task or process API. Top-level code must not call `flux` or `console`: at install the script is compile-checked in a bare context where neither exists.

**When it is called.** Only for tasks that use category routing, and only when routing picks a category directory: either a matching category rule that has a directory, or the "Other" fallback directory. An explicitly chosen save directory bypasses it.

- It is called before the download starts writing, with the final file name. For HLS and DASH this is the predicted `<stem>.mp4` name.
- If no folder was chosen at start, it is asked once more at completion, with the final file name.
- Once a folder is chosen it is saved with the task and reused for pause/resume, restart and completion. The plugin is not asked again, so a plugin update never splits a task.
- If the category directory changes later (rules edited), the saved folder follows the new category directory. If the rules no longer match at all, the task stays in its saved archive folder.

**Several archive plugins** are called in ascending `identity` order; the first valid `folderName` wins.

**`folderName` rules.** The name is validated, never auto-corrected. An invalid name means that plugin's answer is ignored and the next plugin is tried. A valid name:

- is non-empty and at most 180 UTF-8 bytes;
- is not `.` or `..`;
- contains no `/`, `\`, `:` or control characters;
- is already a clean file name, unchanged by FluxDown's filename sanitizer. That excludes `<>"|?*`, leading or trailing spaces and dots, Windows reserved device names (`CON`, `NUL`, `COM1`, … also with an extension) and invisible formatting characters.

**Failures never fail the download.** A throw, timeout, overload, invalid return type, invalid `folderName`, or a missing required plugin setting (the plugin is skipped) all mean no subfolder: the file stays in the category directory. The reason is written to the task's activity log and to the plugin audit log. Timeouts and memory-limit hits count toward the [circuit breaker](#circuit-breaker).

**Limits.** 5 s per call; memory limit equal to the resolver budget; at most 2 archive calls run concurrently across the whole app; a call waits at most 3 s for a slot, otherwise it is skipped as overloaded. Archive plugins run one after another, so their delays add up. See [Runtime limits](#runtime-limits).

How scripts are run:

- Every call to `resolve`, a hook, `authenticate` or `subscribe` creates a **fresh QuickJS runtime**. The script file is evaluated top to bottom as a classic script, then the entry function is looked up on `globalThis`. No global state survives between calls — use [`flux.storage`](#fluxstorage) or [`flux.fs`](#fluxfs) to keep data.
- Top-level `function` declarations and `globalThis.name = …` both define entry points. `export` / `import` syntax does not work in a script file (the [SDK](/docs/en/plugins/sdk/) bundles to a classic script for you).
- Code at the top level of the file runs on **every** call, before the entry. An exception thrown at the top level fails the call with `脚本编译失败: …`.
- If `resolve`, `authenticate` or `subscribe` is not defined the call fails with an error containing `入口 resolve 未定义` (respectively `authenticate` / `subscribe`). A **hook** that is not defined is silently skipped.
- For `resolve`, `authenticate` and `subscribe` the return value is serialized with `JSON.stringify` and parsed by the host: return a plain object. Do not return an already-stringified JSON string (it would be encoded twice and fail validation). `undefined`, functions and symbols are dropped by `JSON.stringify`.
- Typed `flux.settings` values, `flux.info` and the entry argument are injected before your script runs. The `archive` entry gets only `flux.settings`, `flux.logger`, `console` and its argument (see [`archive(ctx)`](#archivectx)).

### `resolve(ctx)`

Called before protocol dispatch, on **every** start and resume of a task that matches `resolvers[0].match.urls` (resolution is lazy — see the [overview](/docs/en/plugins/overview/)). A resolver whose manifest has `multi: true` is additionally called once by the new-download window to pre-resolve a link (see [Manifests](#manifest-results)).

If a required setting has neither a stored value nor a `default`, the call fails with `缺少必填设置项` **before** your script runs.

`ctx` fields:

| Field | Type | Meaning |
|---|---|---|
| `taskId` | string | Task UUID. |
| `url` | string | The original task URL (never a previously resolved one). |
| `authRef` | string | The default credential reference for `plugin + site`. Only filled when the manifest declares permission `auth` **and** the URL has a derivable http/https site; otherwise `""`. You normally just call `flux.fetch` — the host looks the credential up by this reference automatically. |
| `cookies` | string | Cookie header value attached to the task. `""` when none. |
| `referrer` | string | Referrer attached to the task. `""` when none. |
| `userAgent` | string | Effective User-Agent. |
| `extraHeaders` | object | Extra request headers as string key-values (`{}` when none). |
| `resolverItem` | string | `""` on a normal call. For a selected [manifest](#manifest-results) child: the plugin's own `item.id`, or `<itemId>@<variantId>` when the user explicitly chose one of that item's variants. |

`cookies`, `referrer`, `userAgent` and `extraHeaders` carry whatever was stored on the task when it was created — for example by the browser extension capture or the new-download form. They are empty strings / objects when nothing was supplied.

#### Return value

Return `null` or `undefined` to pass through: FluxDown downloads `ctx.url` unchanged. Otherwise return an object:

| Field | Type | Meaning |
|---|---|---|
| `url` | string | The rewritten direct link. **Must be a valid link** (see the validation table) unless `variants` is non-empty. An empty string is **not** "keep the URL": with no `variants` and no `manifest` it is rejected (`url scheme 不允许: `). To keep the URL while changing something else (for example `fileName`), return `url: ctx.url`. |
| `audioUrl` | string | Separate audio stream link, for DASH-style split audio/video. |
| `fileName` | string | Override the saved file name. Ignored when empty. |
| `totalBytes` | integer | File size in bytes. Used **only when `ephemeral` is `true`**, as the size hint for the skipped probe (an unknown or `0` size becomes "unknown"). Otherwise ignored: FluxDown probes the real size itself. A selected variant's `totalBytes` overrides this field first. |
| `extraHeaders` | object | Headers to send when downloading the resolved link. They replace the headers captured with the task. |
| `request` | object | A [`RequestPatch`](/docs/en/plugins/interceptors-guide/#requestpatch) applied to this run of the task — same rules as the `prepare` stage (same-host `url`, headers, cookies, User-Agent, `segments`, `maxRangeBytes` for links that reject large `Range` spans, …). An invalid patch fails the task with `[插件] <identity>: <message>`. Applied on every run, never stored. If both `extraHeaders` and `request` are returned, `extraHeaders` replaces the header table first and `request` is applied on top. |
| `ephemeral` | boolean | `true` = the link is one-shot / anti-hotlinked: skip the metadata probe (at the cost of weaker resume-integrity checks). Default `false`: probe normally and keep ETag-based resume validation. |
| `rangeSupported` | boolean | `true` = you guarantee the resolved host honours HTTP Range requests (e.g. googlevideo). Combined with `ephemeral`, FluxDown still skips the probe but plans a full multi-segment download right away instead of the conservative single-stream start. Default `false`: without a probe, Range capability is learned from the first response. |
| `variants` | array | Multiple quality/format choices — see [Variants](#variants). |
| `defaultVariantIndex` | integer | Which variant is the default (used on the 60 s picker timeout, do-not-disturb and headless runs). Out-of-range values fall back to `0`. Default `0`. |
| `manifest` | object | A multi-file result — see [Manifests](#manifest-results). Mutually exclusive with `url`, `variants` and `audioUrl`. |

After resolution, FluxDown re-examines the *resolved* URL to pick the protocol engine — a resolver may return an HLS playlist, a magnet link or an FTP URL and the right engine takes over.

Numeric fields (`totalBytes`, `size`, `bandwidth`, `width`, `height`, `defaultVariantIndex`) must be integers; a fractional number makes the whole return value invalid (`插件输出非法: resolve 返回值非法: …`).

#### Output validation

The host validates the result and rejects it as a whole (fail-closed) when any rule is broken:

| Field | Rule |
|---|---|
| `url`, `audioUrl`, variant `url` / `audioUrl` | Scheme must be one of `http`, `https`, `ftp`, `magnet`, `ed2k` (case-insensitive); at most 8 KB. Violations: `url scheme 不允许: <scheme>`, `url 超过 8KB`. An empty `audioUrl` is allowed (ignored). |
| top-level `url` | Validated when non-empty, **and** also when `variants` is empty. It may be `""` only when `variants` has entries. |
| `fileName` (also variant `fileName`, manifest item `name`) | Must not contain `/`, `\`, the sequence `..` anywhere (so `a..b.mp4` is rejected) or any control character. Violation: `file_name 非法: …`. |
| `variants` | At most 50 (`variants 过多: N > 50`). Each `label` non-empty and ≤ 200 characters; each `url` valid. |
| `manifest` | See [Manifests](#manifest-results). |

Error behavior is fail-closed: an exception, timeout, invalid return value, or an uninstalled/disabled plugin all put the task into the error state with the text `[插件] <identity>: <message>`. The original URL is never silently downloaded. (The user can choose "Ignore plugin and retry" in the app to download the original URL.)

#### Variants

When `variants` has more than one entry FluxDown shows a picker dialog and the user's choice collapses into a single link before download. In a headless server or a do-not-disturb download, `defaultVariantIndex` is used silently, exactly like HLS quality selection. If the user closes the picker, the task is paused and `onCancel` fires with `reason: "variant_selection_cancelled"`.

Each entry: `{ label, url, audioUrl?, fileName?, totalBytes?, bandwidth?, width?, height?, container? }`.

| Field | Meaning |
|---|---|
| `label` | Required, shown in the picker. |
| `url` | Required: the variant's main link. |
| `audioUrl`, `fileName`, `totalBytes` | Optional. When present and non-empty they override the top-level value for the chosen variant. |
| `bandwidth`, `width`, `height` | Integers, `0` when unknown. Used only for display / sorting. |
| `container` | Container format such as `"mp4"`; may be empty. |

When a manifest item is resolved in its second stage (`ctx.resolverItem` is non-empty), variants are collapsed silently to the default — no picker appears per child task.

#### Manifest results

A manifest describes several files in one result (a share link, a cloud-drive folder). Shape:

```text
{ name?, items: [{ id, name, path?, size?, kind?, variants?: [{ id, label, size? }] }] }
```

| Field | Rule |
|---|---|
| `items` | 1 to 1000 entries (`manifest items 数量非法`). |
| `name` | Group / folder name. Empty = the host falls back to the parent task's file name. |
| `items[].id` | Plugin-owned selection token. Non-empty, ≤ 200 characters. Passed back as `ctx.resolverItem`. |
| `items[].name` | File name, non-empty, same rule as `fileName`. |
| `items[].path` | Relative sub-directory (`""` = group root). Must be a safe relative path (no absolute path, no `..`), at most **8** levels, and `path + "/" + name` at most **180** characters. |
| `items[].size` | Known size in bytes; missing or negative = unknown. |
| `items[].kind` | `""` or `"file"`. Any other value is rejected. |
| `items[].variants` | Optional per-file formats, at most 50, each with non-empty `id` and `label`. |

Rules:

- `manifest` is **mutually exclusive** with `url`, `variants` and `audioUrl`: returning both fails with `manifest 与 url/variants/audioUrl 互斥`.
- Only a first-stage resolve (`ctx.resolverItem === ""`) may return a manifest. A second-stage call that returns one fails with `二段解析（resolverItem 非空）不可返回 manifest：防递归裂变`. When the user (or the engine's automatic expansion) selects a child, the host calls `resolve` again with `ctx.resolverItem` set and the plugin returns a normal direct-link result.
- One bad item rejects the whole manifest.
- The host does not infer relationships or quality from names.

##### Interactive multi-file manifests

The GPUI new-download window pre-resolves a single local GET link before creating a task (only for resolvers declaring `multi: true`). A non-empty manifest opens file selection in the same window; returning to the form or cancelling the preview creates no task. Confirmation creates a group from the selected files. Group creation errors leave the picker open for retry.

Every manifest item is selected by default. Flat manifests from existing plugins remain flat and keep their original IDs. Per-file quality/format selection appears only when `item.variants` is supplied: leaving **Plugin default** selected passes `item.id`; an explicit choice passes `<itemId>@<variantId>` in `ctx.resolverItem`. The plugin must resolve that token to the chosen resource. These per-file metadata variants are distinct from the top-level direct-link `variants` table above.

Browser capture previews retain the private request context in the agent; only manifest metadata reaches the UI. The capture transaction is consumed only after group creation succeeds. Multi-link submissions, remote dispatch, non-GET/body/audio requests, and unattended downloads keep their existing paths.

A resolver that returns a manifest without declaring `multi` is still expanded automatically by the engine when the task starts, so `multi` is an optimisation for the interactive preview, not a requirement.

### Hooks: `onStart`, `onDone`, `onError`, `onCancel`, `onMetaProbed`

Notification hooks, declared in `hooks.events` and implemented in the `hooks.entry` file. See the [hooks guide](/docs/en/plugins/hooks-guide/) for a walkthrough. All hooks receive `{ event, taskId, url }` plus event-specific fields; `event` is `"start"`, `"error"`, `"done"`, `"metaProbed"` or `"cancel"` respectively.

| Hook | `event` | Extra fields | Fires |
|---|---|---|---|
| `onStart` | `start` | — | When a task starts for the first time. A paused task that is resumed later does not fire it again. |
| `onMetaProbed` | `metaProbed` | `fileName`, `totalBytes` — probe results | After the metadata probe, only if it found a file name or a size. |
| `onDone` | `done` | `filePath` — absolute path of the finished file; `audioPath` — for track-pair tasks (separate video+audio streams) where muxing failed, the absolute path of the standalone audio file (`<stem>.audio.m4a`), `null` for single-file results (including a successful mux); `muxed` — whether a track-pair task was merged into one file, always `false` for non-track-pair tasks | When a task completes successfully. **Not** fired when FluxDown adopted an already existing file instead of downloading. |
| `onError` | `error` | `message` — the task's error text | Every time a task enters the failed state — including failures that FluxDown will retry automatically. |
| `onCancel` | `cancel` | `reason` — see below | When the user closes the "choose a plugin" dialog or the variant picker: the task is paused, not deleted. Mutually exclusive with `onDone` / `onError` for the same task. Use it to release temporary remote resources; it cannot retry. Deleting a task or stopping the daemon does **not** fire it. |

`onCancel` `reason` is always present and is one of `"user_cancelled"` (the "choose a plugin" dialog was closed) or `"variant_selection_cancelled"` (the variant picker was closed).

About `url`:

- For `onStart`, when the task was handled by a resolver, `url` is the **resolved** direct link (the link actually being downloaded). For tasks without a resolver it is the task URL.
- For `onDone`, `onError`, `onCancel` and `onMetaProbed` it is the task's stored URL (the original link for resolver tasks).
- `hooks.match.urls` is applied to that same `url`. With no `match`, a hook receives every task.

Delivery and failure behavior:

- Hooks are fire-and-forget: exceptions and timeouts are logged (`hook onDone 执行失败（已忽略）: …`) and swallowed, and nothing a hook does changes the task — with the two exceptions below.
- If the hook pool is saturated the notification is **dropped** with a warning (`钩子并发已满，本次钩子被丢弃`). Hooks with `ffmpeg` / `ytdlp` permission use a separate pool with a bounded queue; see [Concurrency](#concurrency-and-overload).
- Hook failures never count toward the circuit breaker.

The two exceptions: [`flux.task.requestRetry`](#fluxtaskrequestretryopts) (inside `onError`) and [`flux.task.recordArtifact`](#fluxtaskrecordartifactname) (inside `onDone`).

`onDone` is the only hook with a produced file: `flux.ffmpeg` / `flux.ffprobe` can run there, jailed to the folder of the finished file.

### `intercept(ctx)`

The task interceptor entry point, called only when the manifest declares `interceptors.entry` (and `permissions` includes `"intercept"`). One global function handles every subscribed stage; dispatch on `ctx.stage` (`prepare`, `probed`, `refresh`, `selectVariant`, `selectPeriod`, `segments`, `liveSegments`, `finalize`, `failed`). Return `null` / `undefined` for no change. Contexts, results and the `RequestPatch` rules are in the [interceptors guide](/docs/en/plugins/interceptors-guide/); in TypeScript use `defineInterceptors` from `fluxdown-plugin-sdk/runtime`. Calls share the resolve pool and circuit breaker; `finalize` calls from a plugin with the `ffmpeg` or `ytdlp` permission run under the longer post-processing budget.

### `authenticate(ctx)`

The platform login entry point, called only when the manifest declares `auth.entry` (and `permissions` includes `"auth"`). The host drives it via the `daemon.plugin.auth` RPC with an `action`: `begin` when the user clicks "Log in" in settings, `poll` on subsequent polling as needed, `cancel` when the user cancels, `logout` when the user signs out; `status` lets the host probe the current login state without user interaction. See the [auth guide](/docs/en/plugins/auth-guide/) for complete examples.

`ctx` fields:

| Field | Type | Meaning |
|---|---|---|
| `action` | string | One of `begin` / `poll` / `cancel` / `logout` / `status`. Any other value is rejected by the host before your script runs. |
| `site` | string | The site the user entered (empty string if none was provided). |
| `authRef` | string | The known auth reference. When the host receives an empty `authRef` together with a `site`, it fills in `pluginId::<normalized site>` for you; `poll`/`cancel`/`logout` usually carry back the value `begin`/`status` returned. |
| `sessionId` | string | A plugin-defined session identifier, threaded through from `begin` to `poll`. |
| `input` | string | A value the user typed into an interactive form (e.g. a verification code); the format is up to the plugin. |

Return an object (not a JSON string):

| Field | Type | Meaning |
|---|---|---|
| `status` | string | Required, one of `pending` / `success` / `error` (anything else: `authenticate.status 必须是 pending/success/error`). |
| `sessionId` | string | Echo back or continue the session identifier. |
| `challenge` | string | Optional; QR code content, a data URL, or other challenge data to show the user. |
| `challengeType` | string | Optional; the type of `challenge` (e.g. `"qrcode"`), by agreement between the plugin and the host UI. |
| `message` | string | Optional; status text to show the user. |
| `authRef` | string | Optional; on success this should point at the credential you just saved. When omitted on `success`, the host falls back to the `authRef` of the request. |

While `status` is `pending`, a `poll` reply that omits or returns `null` for `challenge`/`challengeType` keeps the last challenge. A terminal reply clears omitted challenge fields; an explicit empty challenge removes its content.
For `challengeType: "qrcode"`, GPUI and Web render ordinary text (including login URLs) as a locally generated, opaque white QR image; the URL is never fetched as an image. Supported base64 image data URLs use their supplied image instead. If QR encoding fails, the UI falls back to text and keeps a copy button for the full original value. `pending`/`success` messages are informational; only `error` messages use error styling.

On successful login the plugin calls [`flux.auth.save`](#fluxauth) to persist the credential; `authenticate` itself is not responsible for persistence.

Host behavior worth knowing:

- **`logout` is finished by the host.** If the request carries an `authRef`, the host deletes that credential after your script returns, even if the script did not call `flux.auth.remove` (or threw).
- **A disabled plugin can still receive `logout`.** In that case the host does not run your script at all; it deletes the credential directly. All other actions on a disabled plugin fail with `插件 <id> 未启用`.
- Required settings are checked before the script runs (`缺少必填设置项`).
- `authenticate` has its own time budget and concurrency pool and **never counts** toward the circuit breaker. `flux.ytdlp` is **not** available here.

### `subscribe(ctx)`

The subscription provider entry point. It is called when a source whose `providerId` matches `subscriptions[0].providerId` is refreshed; the function name is fixed to `subscribe` and the script file is `subscriptions[0].entry`. See the [subscription guide](/docs/en/plugins/subscription-guide/) for a walkthrough.

`ctx` fields:

| Field | Type | Meaning |
|---|---|---|
| `providerId` | string | Your provider ID. |
| `sourceId` | string | ID of the subscription record. |
| `url` | string | The address stored in the subscription record. |
| `providerConfig` | string | Provider-specific opaque configuration **as a JSON string** (`"{}"` when empty). Parse it yourself with `JSON.parse`. |
| `cookies` | string | Subscription-level Cookie header (may be empty). |
| `userAgent` | string | Resolved User-Agent. |

Return an object:

```text
{ title, link, items: [{ guid, title, link, enclosureUrl, resolverItem, enclosureLength, pubDate }] }
```

| Field | Rule |
|---|---|
| `items` | At most 1000 (`订阅条目数量超过 1000`). |
| `items[].guid` | Required, non-empty, ≤ 2048 bytes. The stable identity used to de-duplicate entries. |
| `items[].title` | ≤ 1024 bytes. |
| `items[].link`, `items[].enclosureUrl` | At least one must be non-empty. Each non-empty value must pass the same URL rule as `resolve` (scheme `http`/`https`/`ftp`/`magnet`/`ed2k`, ≤ 8 KB). |
| `items[].resolverItem` | Optional, ≤ 2048 bytes. When set, the item's `link` is handed to your resolver with this second-stage token, so `link` is then **required**. |
| `items[].enclosureLength` | Integer ≥ 0 (bytes), `0` when unknown. |
| `items[].pubDate` | Integer, Unix time as your provider defines it (`0` when unknown). |

An invalid item is **skipped** and logged, not fatal, so one dirty upstream entry cannot stall the whole source. Only when the list is non-empty and **every** item is invalid does the call fail: `订阅条目全部非法（N 条），首条原因: …`. Returning `null`, a non-object or malformed JSON fails with `订阅返回值非法`.

The time budget defaults to the resolve budget (10 s) and can be set with `subscriptions[0].timeoutMs` (capped at 30 s). `subscribe` shares the resolve concurrency pool and counts toward the circuit breaker. `flux.ytdlp` and, with permission `auth`, `flux.auth` are available.

### The `main` script

`main` is different from every entry above: it is a **resident** script, evaluated **once** when the host starts, and it *registers* handlers instead of exporting entry functions. The registrations live in the JavaScript global state between calls, and every later call (render a view, run a command, handle an event or timer) is dispatched into that same runtime one at a time.

```js
flux.ui.view("dashboard", {
  render: (state, ctx) => flux.ui.h("text", { text: "Hello" }),
});
flux.commands.register("refresh", async ({ taskIds }) => { /* … */ });
flux.rpc.handle("ping", async (args) => ({ pong: args }));
flux.events.on("onDone", async (ev) => flux.ui.toast("success", ev.url));
flux.timers.setInterval(() => flux.logger.info("tick"), 60000);
```

The registration APIs (`flux.ui`, `flux.commands`, `flux.rpc`, `flux.events`, `flux.timers`, `flux.tasks`, `flux.settings.update`) are summarized in [main-only facades](#main-only-facades); the node vocabulary and event payloads are in [Views](/docs/en/plugins/views/), and a tutorial is in the [portable UI guide](/docs/en/plugins/ui-guide/). `main` has its own limits (see [Runtime limits](#runtime-limits)): a timeout or out-of-memory error discards the runtime and all its state.

## Which `flux` APIs exist where

The injected `flux` object is the same for all entries except where this table says otherwise. The `archive` entry is the exception: it runs in a minimal sandbox that only has `flux.settings`, `flux.logger` and `console`; every other row below is absent there (`flux.info`, `flux.storage`, `flux.fetch`, `flux.fs`, `flux.task`, `flux.ffmpeg`, `flux.ffprobe`, `flux.ytdlp`, `flux.auth`, `flux.ui` / `rpc` and the rest, and any file, network, task or process API). "perm" means the manifest `permissions` entry must be declared; without it the property is `undefined` (guard with `if (flux.ytdlp)`).

| API | `resolve` | Hooks (not `onDone`) | `onDone` | `authenticate` | `subscribe` | `main` | `archive` |
|---|---|---|---|---|---|---|---|
| `flux.fetch` | yes | yes | yes | yes | yes | yes | **`undefined`** |
| `flux.storage`, `flux.fs` | yes | yes | yes | yes | yes | yes | **`undefined`** |
| `flux.settings`, `flux.logger`, `console` | yes | yes | yes | yes | yes | yes | yes |
| `flux.info` | yes | yes | yes | yes | yes | yes | **`undefined`** |
| `flux.auth` | perm `auth` | perm `auth` | perm `auth` | always (the entry requires `auth`) | perm `auth` | perm `auth` | **`undefined`** |
| `flux.task.requestRetry` | no effect (warning) | `onError` only | no effect | no effect | no effect | no effect | **`undefined`** |
| `flux.task.recordArtifact` | rejects | rejects | yes | rejects | rejects | rejects | **`undefined`** |
| `flux.ffmpeg`, `flux.ffprobe` | `undefined` | perm `ffmpeg`: object exists, `run` rejects, `ffmpeg.available()` works | perm `ffmpeg`, `run` works | `undefined` | `undefined` | perm `ffmpeg`: object exists, `run` rejects | `undefined` |
| `flux.ytdlp` | perm `ytdlp` | perm `ytdlp` | perm `ytdlp` | **`undefined`** | perm `ytdlp` | perm `ytdlp` | **`undefined`** |
| `flux.ui`, `commands`, `rpc`, `events`, `timers`, `settings.update` | — | — | — | — | — | yes | **`undefined`** |
| `flux.tasks` | — | — | — | — | — | perm `tasks` | **`undefined`** |

`flux.auth.*` and the `authRef` option of `flux.fetch` need `auth` in `permissions`; passing an explicit `authRef` without it makes `flux.fetch` reject (see [Errors](#errors-you-will-see)).

## The `flux` object

### `flux.fetch(opts)` → `Promise<response>`

HTTP client. `opts`:

| Field | Default | Notes |
|---|---|---|
| `method` | `"GET"` | Any valid HTTP method token. |
| `url` | — | Required. |
| `headers` | `{}` | String key-values. A header whose name or value is not valid HTTP is silently dropped. |
| `body` | none | Request body, text only. |
| `authRef` | none | An explicit auth reference (see [`flux.auth`](#fluxauth)). When omitted or empty, and the plugin declares `permissions: ["auth"]`, the host derives a default reference from "plugin ID + request site" and looks it up automatically. A plugin without the `auth` permission never receives any credential injection and must not pass `authRef`. |

`opts` is serialized to JSON. If it cannot be read as the shape above (for example a numeric header value), it is treated as empty and the call fails with a `URL 非法` error.

Resolves to `{ status, headers, body, truncated }`:

- `status` is the numeric HTTP code. Non-2xx statuses **resolve** normally; only network and guard failures reject.
- `headers` is an object with **lowercase** names (`set-cookie`, `content-type`, …). **Duplicate response headers** (most commonly multiple `Set-Cookie`s) are joined into a single string value with a newline (`\n`) rather than dropping every value but the last — split on `\n` when parsing cookies out of a login response. Headers whose value is not valid text are omitted.
- `body` is text: bytes are decoded as UTF-8 with replacement characters (binary responses are not supported).
- `truncated` is `true` when the body hit the size cap.

Guard rails, all enforced host-side:

| Rule | Value |
|---|---|
| Schemes | `http` / `https` only |
| Destinations | Publicly routable addresses only. Loopback, LAN, link-local, CGNAT, documentation/benchmark ranges and cloud-metadata IPs are blocked — checked against the literal URL, at DNS resolution (so DNS rebinding is covered), and again on every redirect hop. When the user has configured a proxy in the app, DNS is resolved by the proxy and only the literal-IP and redirect checks apply. |
| Dev-mode exception | Plugins loaded through "Install from directory (developer mode)" are not subject to the public-address restriction: they can reach `localhost`, LAN hosts, etc. so they can talk to local services during development. They follow the same proxy settings and site rules as other plugins, except that loopback, private LAN ranges and local domains (`.local`, `.lan`, `.home.arpa`, `.internal`) are always reached directly. Normally installed plugins are unchanged. |
| Response body cap | 8 MB, then truncated |
| Per-request timeout | 10 s |
| Concurrent requests | 8, shared across all plugins |
| Max redirects | 30 |
| Proxy | Follows the app's current proxy settings (changes apply to the next request). A request whose URL matches a site proxy rule uses that rule; any other request uses the egress of the call: the task's own proxy (site rules are then ignored), else the rule matching the task's link, else the global mode. In **Auto** mode a call starts direct (or on the proxy that recently worked for that site); if it fails because its requests could not get through the network, the host re-runs the whole call once through each candidate proxy, and the download then uses the same exit as the successful resolve. |

Credentials and redirects: a request that actually received stored credentials (explicit or implicit `authRef`) is sent through a stricter client that **does not follow a redirect to another origin** (different scheme, host or port). You get the `3xx` response back (`status`, `headers.location`) and must decide yourself whether to follow it, so credentials never leak cross-origin. Requests without credentials follow redirects normally.

### `flux.auth`

Available **only** when the manifest declares `permissions: ["auth"]` — otherwise `flux.auth` is `undefined`. Manages host-persisted plugin credentials (cookies / bearer tokens / HTTP Basic / custom headers) for `flux.fetch` to reuse automatically. Credentials do not live in the plugin's own `flux.storage` — the host implements persistence and expiry once, so plugins don't each reinvent it.

- `flux.auth.save(profile)` → `Promise<string>` — write or replace a credential, returning the normalized `authRef`. `profile.site` is required and accepts a full URL or a bare `host[:port]` (a bare host is normalized to `https` by default — registering a site that explicitly allows plaintext requires passing `"http://host"` yourself). When `authRef` is omitted it's generated as `pluginId::normalizedSite`; when provided explicitly it must equal that normalized result or the call rejects. The serialized profile may not exceed **64 KB**.
- `flux.auth.get(authRef)` → `Promise<profile | null>` — read a credential back; rejects if `authRef` doesn't belong to the current plugin.
- `flux.auth.remove(authRef)` → `Promise<true>` — delete a credential; `authRef` must belong to the current plugin (prefixed `pluginId::`). Deleting a missing credential is not an error.

`profile` fields (shared by `save`/`get`):

| Field | Notes |
|---|---|
| `site` | The site key, as above. **Scheme-qualified** — `http` and `https` on the same host are two distinct sites; a credential established over `https` is never auto-reused on an `http` request (this stops a passive man-in-the-middle from harvesting the credential over plaintext). |
| `kind` | `basic` / `cookie` / `bearer` / `headers` / `session` — descriptive only; the host applies the injection rules below regardless of the value. |
| `cookies` | Injected as a `Cookie` header (skipped if the request already has one). |
| `accessToken` | Injected as `Authorization: Bearer <accessToken>` (skipped if `Authorization` is already present). |
| `username` / `password` | When `kind === "basic"`, injected as `Authorization: Basic <base64>` (skipped if `Authorization` is already present). |
| `headers` | Extra headers, written individually (highest priority — overrides same-named headers generated by the fields above). |
| `account` / `refreshToken` / `expiresAt` / `refreshAt` / `metadata` | Platform-specific metadata; the host stores it without interpreting it, except `expiresAt` (Unix seconds) — past that point the credential is expired: an implicit reference (a `flux.fetch` without an explicit `authRef`) silently skips injection, while an explicit `authRef` rejects with `authentication_required`. |

`flux.fetch`'s `authRef` resolution: when passed explicitly, a missing or expired credential rejects with an error whose message **contains** `authentication_required` (the host adds a `插件运行时错误: ` prefix, so test with `String(e.message).includes('authentication_required')`, not `startsWith`); when omitted (or empty), the host derives the default reference from plugin+site and silently skips injection on a miss or expiry — the request still goes out, just without credentials. A credential whose stored site differs from the request's site, or that belongs to another plugin, rejects.

### `flux.storage`

Persistent key-value store, private to your plugin, survives app restarts (backed by the FluxDown database).

- `flux.storage.get(key)` → `Promise<string | null>` — `null` when the key is unset.
- `flux.storage.set(key, value)` → `Promise<void>` — rejects when a single value exceeds **64 KB** or the plugin would exceed **100 keys** (overwriting an existing key never counts as a new key).

Keys and values are converted with `String(…)`; JSON-encode anything structured yourself. There is no delete call; overwrite with an empty string if you need to clear a key.

### `flux.fs`

Always available — no `permissions` entry required. Scratch-file storage for a plugin's own workspace, distinct from `flux.storage`'s key-value store: use it when a managed tool needs a real file on disk rather than a string in memory. The workspace is the **same directory** `flux.ytdlp` runs in (its cwd) — anything written here is reachable by yt-dlp (or any tool call) as a plain relative filename. It is not the folder of a downloaded file; for that see `flux.ffmpeg` in `onDone`.

- `flux.fs.writeFile(name, content)` → `Promise<void>` — write (or overwrite) a text file; rejects on an invalid name or a limit violation.
- `flux.fs.readFile(name)` → `Promise<string | null>` — read a text file back; `null` if it doesn't exist **or the name is invalid**.
- `flux.fs.remove(name)` → `Promise<void>` — delete a file; a missing file is not an error (idempotent); an invalid name rejects.
- `flux.fs.list()` → `Promise<string[]>` — top-level file names in the workspace (sub-directories and yt-dlp's own `.cache` are not included).

`name` must be a flat, safe filename: non-empty, containing none of `/`, `\`, `:` or NUL, not `.` or `..`, and at most 255 bytes — anything else rejects. Limits: **8 MB** per file, **64 MB** total per plugin workspace, **100 files** max (overwriting an existing file never counts as a new file). Content is text only (no binary). On Unix, files are created with mode `0600`. The workspace is deleted when the plugin is uninstalled.

Typical use: materialize an input file for a managed tool — cookies, a config, subtitles — write it, reference it by relative name in the tool call, then remove it. Example — feed a cookie jar to `flux.ytdlp`:

```js
await flux.fs.writeFile('cookies.txt', netscapeCookieText);
try {
  const r = await flux.ytdlp.run({ args: ['--cookies', 'cookies.txt', '-J', ctx.url] });
  // ... use r.stdout
} finally {
  await flux.fs.remove('cookies.txt');
}
```

### `flux.settings`

A plain object with your manifest-declared settings, already typed per `settings[].type`:

| Type | JS value |
|---|---|
| `string` | the string; `null` when the user set nothing **and** there is no `default` |
| `number` | the number; `null` when unset with no `default` (or the stored text is not a number) |
| `boolean` | `true` only when the value is exactly `"true"`; otherwise `false` (an unset boolean is `false`) |

Unset fields with a `default` carry the default. Always guard against `null` for `string` and `number`. A field marked `required` with neither a value nor a `default` stops `resolve`, `authenticate` and `subscribe` **before** your script runs (`缺少必填设置项: 插件 <id> 需先配置「<title>」`), so inside those entries required settings are never `null`.

For one-shot entries the object is a snapshot built at call time. In `main` it is a live object that the host updates in place when the user changes a setting, and `flux.settings.update` exists (see [main-only facades](#main-only-facades)).

### `flux.info`

`{ identity, version, appVersion }` — your plugin's ID and version, and the FluxDown version hosting it.

### `flux.logger` and `console`

`flux.logger.info/warn/error(...)` write to FluxDown's log file, prefixed with `[plugin:<id>]`. `console.log/info/warn/error/debug` are mapped to the same place (`log`, `info` and `debug` log at info level). Multiple arguments are joined with spaces; non-strings are JSON-stringified. Each line is truncated at 4 KB. Note that **`warn` is written at the same level as `info`**; only `error` is logged as an error. Calls return `undefined` immediately. For where the log file is and how to search it see [Debugging](/docs/en/plugins/debugging/).

### `flux.task.requestRetry(opts)`

`flux.task.requestRetry({ delayMs: 5000 })` — ask FluxDown to retry the failed task after a delay of `delayMs` milliseconds (a non-negative integer; anything else becomes `0`). The call is synchronous and returns nothing. Only meaningful inside `onError`; called anywhere else it logs `flux.task.requestRetry 仅 onError 钩子可用，已忽略` and does nothing. Retries share the task's automatic-retry budget (`max_auto_retries`), so a plugin cannot retry forever. Because `onError` also fires for failures FluxDown is already going to retry, avoid requesting a retry for errors the app retries by itself.

### `flux.task.recordArtifact(name)`

`await flux.task.recordArtifact('video.srt')` — register a file your `onDone` hook created next to the download as a **derived artifact** of the task: when the user deletes the task **and its files**, the artifact is deleted too. `onDone` only; elsewhere the call rejects with `flux.task.recordArtifact 仅 onDone 钩子可用`.

`name` must be a bare file name in the task's save folder:

- non-empty, ≤ 512 bytes, none of `/`, `\`, `:`, and not `.` or `..`;
- starts with the stem of the task's main file (the name without extension) — an unrelated file cannot be registered;
- the file must already exist in the save folder.

Violations reject with `recordArtifact: …`. Write the file first (typically with `flux.ffmpeg.run`), then record it.

### `flux.ffmpeg`

Available **only** when the manifest declares `permissions: ["ffmpeg"]` — otherwise `flux.ffmpeg` is `undefined`, so guard with `if (flux.ffmpeg)`. It runs the ffmpeg FluxDown resolves (a user-set path → the managed install → system `PATH`), so ffmpeg must also actually be present (installable from the app's Settings → Extensions → Components tab); otherwise `run` rejects with `ffmpeg 未安装或不可用`.

- `flux.ffmpeg.available()` → `Promise<{ available, version, source }>` — probe the effective ffmpeg. `source` is `"manual"` / `"managed"` / `"system"` / `"none"`.
- `flux.ffmpeg.run(spec)` → `Promise<outcome>` — run ffmpeg. `spec`:

| Field | Default | Notes |
|---|---|---|
| `args` | — | Required, non-empty, at most 512 items, each ≤ 8 KB and without NUL. ffmpeg argument array (no program name; `-nostdin` is prepended for you). |
| `subdir` | none | Working sub-directory under the jail root; safe relative path, may not escape. Created if missing. |
| `timeoutMs` | 300000 | Per-call timeout, capped at 1800000 (30 min). |

Resolves to `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }` — `code` is the exit code (`-1` when killed / none), `stdout`/`stderr` are truncated (256 KB / 64 KB), `timedOut` is `true` when the timeout killed the run (then `stdout`/`stderr` are empty). A non-zero exit code **resolves**; check `code`.

**The jail.** `flux.ffmpeg.run` only works inside `onDone` (the one hook with a produced file); in every other context the call rejects with `flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用` (`available()` still works). The working directory is the finished file's own folder, and that folder is the jail — reference files by **relative** name (the basename), prefixing with `./` in case a name starts with `-`.

Arguments are screened; a spawn is refused (`ffmpeg 参数 '<arg>' 被拒: <reason>`) when any token is:

| Blocked | Examples |
|---|---|
| a URL scheme / protocol | `http://…`, `file:…`, `concat:…`, `crypto:…` |
| an absolute path / drive letter | `/etc/x`, `C:\x`, `\\host\share` |
| parent traversal | `../x`, `a/../b` |
| an embedded absolute path | `subtitles=/etc/x` |
| a segment of a filter / multi-output expression that starts with `/` or a drive letter | `a.mkv\|/abs`, `[v]/abs` |
| the `tee` muxer | `tee` |
| options that read option values or filter graphs from a file, or override protocol safety | `-/…`, `-safe`, `-protocol_whitelist`, `-protocol_blacklist`, `-filter_script`, `-filter_complex_script` |

Quotes and backslashes in a token are removed before the path checks run, so quoting cannot hide a path. Ordinary ffmpeg syntax is untouched — division (`30000/1001`), stream specifiers (`0:a`, `-c:v`), filters (`scale=1280:720`) all pass. With no URL and no absolute path reachable, ffmpeg can only touch files inside the jail, so there's no network path either. At most 2 ffmpeg/ffprobe processes run at once across all plugins, and each child is killed on timeout or cancellation.

Example — convert a non-MP4 result to MP4 in `onDone`:

```js
globalThis.onDone = async (ctx) => {
  if (!flux.ffmpeg) return;
  const name = ctx.filePath.split(/[\\/]/).pop();
  if (/\.mp4$/i.test(name)) return;
  const out = name.replace(/\.[^.]+$/, '') + '.mp4';
  const r = await flux.ffmpeg.run({
    args: ['-i', './' + name, '-c:v', 'libx264', '-c:a', 'aac',
           '-movflags', '+faststart', '-y', './' + out],
  });
  if (r.code !== 0) {
    flux.logger.error('convert failed', (r.stderr || '').slice(-400));
    return;
  }
  await flux.task.recordArtifact(out); // deleted together with the task's files
};
```

### `flux.ffprobe`

Gated by the same `permissions: ["ffmpeg"]` declaration as `flux.ffmpeg` — no separate permission exists. It shares the exact same jail (`run` works **only** inside `onDone`, rejecting elsewhere with `flux.ffprobe.run 仅在有产物文件的钩子（如 onDone）中可用`), the same path-resolution order (user-set → managed install → system `PATH`), and the same argument screening. ffprobe ships alongside the managed ffmpeg install — installing ffmpeg from the Settings → Extensions → Components tab also places ffprobe in `<data_dir>/bin`, so no separate install step is needed. If it is missing, `run` rejects with `ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）`.

- `flux.ffprobe.run(spec)` → `Promise<outcome>` — run ffprobe. `spec` and the resolved `outcome` are identical in shape to `flux.ffmpeg.run` (`args` / `subdir` / `timeoutMs`, resolving to `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`). Unlike ffmpeg, no `-nostdin` is prepended. There is no `available()`.

Use it for structured probing instead of scraping ffmpeg's stderr:

```js
const out = await flux.ffprobe.run({
  args: ['-v', 'quiet', '-print_format', 'json', '-show_format', '-show_streams', './in.mp4'],
});
const info = JSON.parse(out.stdout);
```

### `flux.ytdlp`

Available **only** when the manifest declares `permissions: ["ytdlp"]` — otherwise `flux.ytdlp` is `undefined`, so guard with `if (flux.ytdlp)`. It runs the yt-dlp FluxDown resolves (a user-set path → the managed install → system `PATH`), so yt-dlp must also actually be present (installable from the app's Settings → Extensions → Components tab); otherwise `run` rejects with `yt-dlp 未安装或不可用`.

- `flux.ytdlp.available()` → `Promise<{ available, version, source }>` — probe the effective yt-dlp. `source` is `"manual"` / `"managed"` / `"system"` / `"none"`. A quick `run({ args: ['--version'] })` and checking `code === 0` works as a lighter-weight liveness probe too.
- `flux.ytdlp.run(spec)` → `Promise<outcome>` — run yt-dlp. `spec`:

| Field | Default | Notes |
|---|---|---|
| `args` | — | Required, non-empty, at most 512 items, each ≤ 8 KB and without NUL. yt-dlp argument array (no program name). |
| `subdir` | none | Working sub-directory under the jail root; safe relative path, may not escape. Created if missing. |
| `timeoutMs` | 300000 | Per-call timeout, capped at 3600000 (60 min). |

Resolves to `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }` — `code` is the exit code (`-1` when killed / none), `stdout`/`stderr` are truncated (16 MB by default / 256 KB), `timedOut` is `true` when the timeout killed the run (then `stdout`/`stderr` are empty). Check `truncatedStdout` before parsing `stdout` as JSON and surface a clear error instead of letting `JSON.parse` fail on a truncated tail.

The stdout limit defaults to 16 MB and is clamped to 1–64 MB. Administrators of unattended deployments can change it with the config key `plugin.<id>.ytdlp.stdout_cap` (per plugin, highest priority), `component.ytdlp.stdout_cap` (global) or the environment variable `FLUXDOWN_YTDLP_STDOUT_CAP` (default). Values are bytes or use a `k`/`m`/`g` suffix (for example `32m`).

**The jail.** Unlike `flux.ffmpeg`, `flux.ytdlp` has no dependency on a produced file, so it works in every entry that exposes it (see the [matrix](#which-flux-apis-exist-where)) — but **not** in `authenticate`. The jail isn't a task's output folder; it's a scratch directory the bridge keeps per plugin (lazily created under FluxDown's data directory), reused across calls. That's the working directory for the call, and `subdir` carves out a sub-folder inside it. Reference any files you read or write there by **relative** name. This is the same workspace `flux.fs` reads and writes — it's how you feed yt-dlp a cookie jar, config file, or subtitles: `flux.fs.writeFile('cookies.txt', …)` beforehand, reference the file by its relative name in `args`, then `flux.fs.remove('cookies.txt')` once the call returns.

#### Argument allow-list

yt-dlp is a network tool, so URL arguments and outbound network access are allowed. The arguments are checked against an **exact allow-list**: anything not listed below is refused, including abbreviated long options (`--no-warn`) and short options glued to their value (`-fbest`, `-Jq`). Write each short option as its own argument (`['-f', 'best']`). Long options may be written `--name value` or `--name=value`.

- **Positional arguments** must be `http://` or `https://` URLs. After a lone `--`, every remaining argument is positional.
- **Flags** (no value) — `-J` `-j` `-q` `-v` `-s` `-x` `-F` `-i` `-4` `-6` `-k` `--version` `--dump-json` `--dump-single-json` `--simulate` `--skip-download` `--quiet` `--verbose` `--no-warnings` `--no-progress` `--ignore-errors` `--no-abort-on-error` `--no-playlist` `--yes-playlist` `--flat-playlist` `--no-flat-playlist` `--lazy-playlist` `--playlist-reverse` `--force-ipv4` `--force-ipv6` `--geo-bypass` `--no-geo-bypass` `--no-check-certificates` `--check-formats` `--list-formats` `--live-from-start` `--no-live-from-start` `--no-mtime` `--restrict-filenames` `--windows-filenames` `--no-cache-dir` `--extract-audio` `--keep-video` `--no-keep-video` `--write-info-json` `--write-description` `--write-thumbnail` `--write-subs` `--write-auto-subs` `--no-write-subs` `--embed-subs` `--embed-thumbnail` `--embed-metadata` `--embed-chapters` `--no-part` `--no-continue` `--no-js-runtimes` `--mark-watched` `--no-mark-watched` `--hls-prefer-native` `--hls-use-mpegts` `--no-hls-use-mpegts`. A flag must not be given an `=value`.
- **Options with one value** — `-f` `-S` `-N` `-r` `-R` `--format` `--format-sort` `--extractor-args` `--playlist-items` `--playlist-start` `--playlist-end` `--max-downloads` `--proxy` `--socket-timeout` `--source-address` `--geo-verification-proxy` `--geo-bypass-country` `--geo-bypass-ip-block` `--xff` `--user-agent` `--referer` `--add-header` `--sleep-interval` `--min-sleep-interval` `--max-sleep-interval` `--sleep-requests` `--sleep-subtitles` `--retries` `--extractor-retries` `--fragment-retries` `--file-access-retries` `--retry-sleep` `--concurrent-fragments` `--limit-rate` `--throttled-rate` `--http-chunk-size` `--buffer-size` `--min-filesize` `--max-filesize` `--date` `--datebefore` `--dateafter` `--match-filters` `--match-filter` `--age-limit` `--download-sections` `--merge-output-format` `--remux-video` `--recode-video` `--audio-format` `--audio-quality` `--sub-langs` `--sub-format` `--convert-subs` `--convert-thumbnails` `--impersonate` `--print` `--parse-metadata` `--compat-options` `--video-password` `--username` `--password` `--twofactor` `--encoding` `--output-na-placeholder`.
- **`--js-runtimes`** — one value made only of letters, digits, `,`, `_` and `-` (runtime names, never paths).
- **Path options** — `--cookies`, `--cache-dir`, `--download-archive`: one value that is a path **inside the workspace** (relative).
- **Output options** — `-o` / `--output` and `-P` / `--paths`: one value, optionally prefixed with an output type (`home:`, `temp:`, `default:`, `subtitle:`, `thumbnail:`, `description:`, `annotation:`, `infojson:`, `link:`, `pl_thumbnail:`, `pl_description:`, `pl_infojson:`, `chapter:`, `pl_video:`, `pl_subtitle:`, `pl_chapter:`); the path part is checked like a path option. Output templates such as `%(title)s.%(ext)s` are fine.
- **`--print-to-file TEMPLATE FILE`** — two separate arguments; `FILE` is checked like a path option (`--print-to-file=…` is refused).

A path value is refused when it starts with `~`, contains `$` or a `%NAME%` environment reference, is absolute (`/…`, `\…`, `C:…`), contains a `..` segment, or contains `:/` / `:\`.

Everything else is refused, notably: `--exec` and `--exec-before-download`, `--downloader` / `--external-downloader` and their `-args` variants, `--use-postprocessor`, `--alias`, `--config-location(s)`, `--plugin-dirs`, `--ffmpeg-location`, `--batch-file` / `-a`, `--load-info` / `--load-info-json`, `--netrc*`, `--cookies-from-browser`. A refused call rejects with one of: `yt-dlp 参数被拒: 开关 '--x' 不在允许列表内`, `…开关 '--x' 不接受值`, `…开关 '--x' 缺少值`, `…开关 '--x' 的值被拒: <reason>`, `…位置参数 '<arg>': 位置参数只允许 http/https URL` (all prefixed `插件输出非法: `).

What FluxDown adds for you:

- `--ignore-config` is always prepended, so none of yt-dlp's own config files get read.
- `--ffmpeg-location` pointing at the resolved managed/system ffmpeg is injected when ffmpeg is installed, so merges (bestvideo+bestaudio), `-x` audio extraction, remuxing and recoding work. A plugin-supplied `--ffmpeg-location` is still rejected; only the host's own path is trusted.
- `--cache-dir <workspace>/.cache` keeps yt-dlp's cache inside the workspace; your own `--cache-dir` (a relative path) overrides it.
- `--proxy`: when the app decides an exit for the call (a configured proxy, a site rule, the task's own proxy, or Auto switching to a proxy), the host appends its `--proxy` **after** your arguments, so the app's choice wins; a direct exit chosen by a rule or by the task is passed as `--proxy ""`. Without a configured proxy (or while Auto tries the direct route), your own `--proxy` is honoured. Rules are matched against the first URL argument.
- The child process gets a complete `PATH` (on Windows including user-level entries) so yt-dlp can find JavaScript runtimes such as Node or Deno.

At most 2 yt-dlp processes run at once across all plugins; time spent waiting for a free slot is not charged to your script's time budget. Each child is killed on timeout or cancellation.

Example — resolve a page by asking yt-dlp for its metadata JSON and picking a direct link out of it:

```js
globalThis.resolve = async (ctx) => {
  if (!flux.ytdlp) return null;
  const r = await flux.ytdlp.run({ args: ['-J', '--no-warnings', ctx.url] });
  if (r.code !== 0) throw new Error('yt-dlp failed: ' + (r.stderr || '').slice(-400));
  if (r.truncatedStdout) throw new Error('yt-dlp output was truncated');
  const info = JSON.parse(r.stdout);
  const direct = info.url || info.formats?.[info.formats.length - 1]?.url;
  if (!direct) return null;
  return { url: direct, fileName: info.title ? `${info.title}.${info.ext || 'mp4'}` : undefined };
};
```

In `resolve` the script's whole time budget (10 s by default, at most 30 s) also bounds the call, so a `timeoutMs` larger than that has no effect there.

### Main-only facades

These exist only in the resident `main` script. Behavior in detail is in [Views](/docs/en/plugins/views/) and the [portable UI guide](/docs/en/plugins/ui-guide/).

| API | Summary |
|---|---|
| `flux.ui.view(id, { init, render, event, dispose })` | Registers a portable view; `id` must match a declared view. |
| `flux.ui.h(kind, props, ...children)` / `flux.ui.Fragment` | Build `{k, p, c}` nodes (children flattened, `null` / `false` / `true` dropped). JSX via the [SDK](/docs/en/plugins/sdk/#jsx) produces the same shape. |
| `flux.ui.toast(level, text)` | `info`, `success`, `warning` or `error` (anything else is shown as `info`). |
| `flux.commands.register(id, handler)` | Handler receives `{ taskIds }`. |
| `flux.rpc.handle(name, handler, opts?)` | `handler(args, ctx)`. Callable from your own desktop views with `fluxdown.invoke(name, args)`; then `ctx` is `{ caller: "desktop" }`. Pass `{ page: true }` as `opts` to also expose the handler to your [page scripts](/docs/en/plugins/page-scripts-guide/): they call it with `fluxdown.invoke(name, args)` and `ctx` is `{ caller: "page", pageUrl }`. A handler registered without `page: true` is rejected for page calls with `rpc not exposed to page: <name>`. `pageUrl` and `args` come from a web page, so treat them as untrusted. Older handlers that take only `args` keep working. |
| `flux.events.on(name, handler)` | `name` is a hook name (`onStart`, `onDone`, `onError`, `onMetaProbed`, `onCancel`) listed in `main.events`. These deliveries are **not** filtered by `hooks.match.urls`. A throwing handler is logged and skipped. Open views re-render afterwards. |
| `flux.timers.setTimeout(fn, ms)` / `setInterval(fn, ms)` / `clear(id)` | At most 64 live timers (`too many timers (max 64)`). While a view session is open an interval is clamped to ≥ 1 s, otherwise ≥ 60 s. These are *not* global `setTimeout`/`setInterval`. |
| `flux.tasks.list()` / `get(id)` / `create(params)` / `pause(id)` / `resume(id)` / `remove(id, deleteFiles?)` | Needs permission `tasks`. They call the daemon's `daemon.task.list` / `get` / `create` / `pause` / `resume` / `delete` with the same parameters and results as the [RPC API](/docs/en/api/overview/) (`create` takes `{ request: { url, … } }`; `get`/`pause`/`resume`/`remove` take a task id string). `list()` without arguments returns every task but is rejected when there are more than 5000; pass `list({ cursor, limit })` to page in ascending `taskId` order (`cursor` = the last `taskId` of the previous page, `limit` ≤ 5000, a page shorter than `limit` is the last). Only these six methods are reachable. `create` is limited to **20 calls per plugin per 60 s** (`tasks.create rate limited`). Every call is written to the audit log. |
| `flux.settings.update({ key: "value" })` | Writes your plugin's own settings. **Values must be strings**; they are validated against `manifest.settings` and a bad key or value rejects. |
| `flux.ffmpeg`, `flux.ffprobe`, `flux.ytdlp`, `flux.auth` | As in the [matrix](#which-flux-apis-exist-where). `flux.ffmpeg.run` always rejects in `main` (no produced file). |

## JavaScript environment

Scripts run in the QuickJS engine bundled with FluxDown (a recent QuickJS-based engine, checked against the pinned source). Each call gets a standard context with these globals:

| Available | Notes |
|---|---|
| ECMAScript built-ins | `Object`, `Array`, `String`, `Number`, `Boolean`, `Symbol`, `Math`, `JSON`, `Date`, `RegExp`, `Promise`, `Map`, `Set`, `WeakMap`, `WeakSet`, `WeakRef`, `Proxy`, `Reflect`, `BigInt`, typed arrays / `ArrayBuffer`, `eval`, `globalThis`, `parseInt`, `parseFloat`, `encodeURIComponent` / `decodeURIComponent`, `escape` / `unescape`. |
| `atob`, `btoa` | Base64 of binary strings, built in. |
| `queueMicrotask` | Standard. |
| `performance.now()` | Milliseconds since the context was created; useful for measuring, not a wall clock. |
| `console` | Replaced by FluxDown's logger (see above). |
| `flux` | The injected API. |

**Not available**: `setTimeout`, `setInterval`, `clearTimeout` (so there is no way to sleep inside `resolve` or a hook; in `main` use `flux.timers`), `fetch` (use `flux.fetch`), `URL`, `URLSearchParams`, `TextEncoder`, `TextDecoder`, `crypto`, `structuredClone`, `Intl`, `require`, `import`, `process`, `window` / DOM, `localStorage`. Bring your own implementation (for example an MD5 or HMAC function) when a site needs one. Names starting with `__flux_` and `__fluxHost` are internal and may change.

Async code works as usual (`async` / `await`); promises that are still pending when the entry returns are not awaited — always `await` what you start.

## Runtime limits

Budgets are per call. "CPU budget" is the time your JavaScript may execute; time spent waiting on a host call (`flux.fetch`, `flux.ffmpeg.run`, `flux.ytdlp.run`, `flux.settings.update`, `flux.tasks.*`) is credited back, so waiting on the network does not use it up. "Wall clock" is the total time including waiting.

| Entry | Wall clock | CPU budget | Memory |
|---|---|---|---|
| `resolve` | 10 s default; `resolvers[0].timeoutMs` replaces it, **30 s hard ceiling** | same value, ≤ 30 s | 64 MB default; `resolvers[0].memoryLimitMb` replaces it, 512 MB ceiling |
| `subscribe` | 10 s default; `subscriptions[0].timeoutMs` replaces it, 30 s ceiling | same | 64 MB |
| `authenticate` | 30 s default; `auth.timeoutMs` replaces it, 30 s ceiling | same | 64 MB |
| Hooks | 5 s | 5 s | 32 MB |
| `archive` | 5 s | 5 s | Same as the `resolve` default (64 MB) |
| Hooks of a plugin with `ytdlp` permission (any hook), or `ffmpeg` permission in `onDone` | ~30 min (1830 s) | 30 s | 32 MB |
| `main` (per call: render, command, event, timer, rpc) | 30 s | 2 s | 64 MB for the whole runtime |

`timeoutMs` values must be greater than `0`.

Other fixed limits:

| Item | Limit |
|---|---|
| Log line | 4 KB |
| `flux.fetch` | 10 s per request, 8 MB body, 8 concurrent (global), 30 redirects |
| `flux.storage` | 64 KB per value, 100 keys |
| `flux.fs` | 8 MB per file, 64 MB and 100 files per workspace, 255-byte names |
| `flux.auth.save` | 64 KB per profile |
| `flux.ffmpeg` / `flux.ffprobe` | 300 s default, 1800 s max per call; stdout 256 KB, stderr 64 KB; 2 concurrent (global) |
| `flux.ytdlp` | 300 s default, 3600 s max per call; stdout 16 MB (configurable, 1–64 MB), stderr 256 KB; 2 concurrent (global) |
| `flux.tasks.create` | 20 per 60 s per plugin |
| `main` timers | 64 per plugin |
| `main` idle | A host without the `background` permission is stopped after 5 minutes with no open view, no timer and no running call; a view session is closed after 90 s without keep-alive. |

### Concurrency and overload

- `resolve` and `subscribe` share one pool. Its size follows the app's concurrent-download limit (never below the runtime's worker count, at most 4 workers). A call waits up to **3 s** for a free slot; after that it fails with `插件运行时过载（并发已满）` (fail-closed, the task enters the error state).
- `authenticate` has its own pool of **2** with the same 3 s wait, so login polling can never starve resolves.
- `archive` has its own pool of **2** for the whole app. A call waits up to **3 s** for a slot; after that it is skipped as overloaded (no subfolder, the download is not affected).
- Hooks use a pool the size of the worker count (at most 4). When it is full, further hook notifications are **dropped** (logged as `钩子并发已满，本次钩子被丢弃`), never queued.
- Hooks of plugins with `ffmpeg` or `ytdlp` permission use a separate pool of the same size. When it is busy, up to **64** notifications wait in a queue; beyond that the notification is dropped (`外部工具钩子排队已满，本次钩子被丢弃`).
- `main` handles one call at a time per plugin; calls are queued in order.

### Circuit breaker

Three **consecutive** timeouts (`脚本执行超时`) or memory-limit hits (`脚本超出内存上限`) in `resolve`, `subscribe`, `archive` or `main` calls auto-disable the plugin. The app shows a notice and the plugin stays off until it is re-enabled manually. Any other outcome — a success, or a different error such as an invalid return value or a network failure — resets the count to zero.

Not counted: hooks (their timeouts and memory errors are only logged), `authenticate` and overload rejections. A timeout or out-of-memory error in `main` also destroys the runtime; the next call starts a fresh one and **all in-memory state is lost**.

## Errors you will see

Errors reach your code as `Error` objects whose `message` is built by the host. Messages from `flux.*` calls carry a category prefix: **`插件运行时错误: `** (runtime / network / permission failure) or **`插件输出非法: `** (bad argument or invalid output). Match on a stable substring rather than the whole text.

### Failing a call (task error text or RPC error)

For `resolve` the task shows `[插件] <identity>: <message>`.

| Message | Meaning and fix |
|---|---|
| `脚本编译失败: …` | Syntax error, or an exception thrown at the top level of the script. |
| `插件运行时错误: …` | Your code threw, or a rejected `await` was not caught. The text after the colon is your error. Includes `入口 resolve 未定义` when the function does not exist. |
| `脚本执行超时` | Wall-clock or CPU budget exceeded. Counts toward the circuit breaker (for `resolve` / `subscribe` / `archive` / `main`). |
| `脚本超出内存上限` | Memory limit exceeded. Counts toward the circuit breaker. |
| `插件运行时过载（并发已满）` | No free slot within 3 s. See [Concurrency](#concurrency-and-overload). |
| `缺少必填设置项: 插件 <id> 需先配置「<title>」` | A `required` setting has no value. Ask the user to fill it in. |
| `插件 <id> 当前版本未提供 resolver（或源码读取失败）；…` | The installed version declares no resolver, or the resolver file disappeared after loading. A task still bound to the plugin fails closed instead of downloading the page URL. A resolver file that is missing at load time is reported earlier as a load failure — see [Debugging](/docs/en/plugins/debugging/). |
| `插件输出非法: resolve 返回值非法: …` | The returned object could not be read (for example a non-integer `totalBytes`). |
| `插件输出非法: url scheme 不允许: <scheme>` | The link does not start with `http`/`https`/`ftp`/`magnet`/`ed2k` (an empty `url` ends up here). |
| `插件输出非法: url 超过 8KB` | Link longer than 8 KB. |
| `插件输出非法: file_name 非法: <name>` | `fileName` contains `/`, `\`, `..` or a control character. |
| `插件输出非法: variants 过多: N > 50` / `variant label 须非空且 ≤200 字符` | Variant limits. |
| `插件输出非法: manifest 与 url/variants/audioUrl 互斥` | Returned `manifest` together with another result field. |
| `插件输出非法: 二段解析（resolverItem 非空）不可返回 manifest：防递归裂变` | A second-stage resolve returned a manifest. |
| `插件输出非法: manifest items 数量非法: N（须 1..=1000）` | Wrong item count. |
| `插件输出非法: manifest item id 非法` / `name 不可为空` / `path 非法` / `path 深度超限: N > 8` / `落盘路径过长: N > 180` / `kind 非法` / `variants 过多` / `variant id/label 不可为空` | The matching manifest item rule was broken. |
| `插件输出非法: authenticate 返回值非法: …` | `authenticate` returned something that is not the expected object (for example a string). |
| `插件输出非法: 订阅返回值非法: …` | `subscribe` returned `null` or a malformed value. See [`subscribe`](#subscribectx). |
| `插件 <id> 不存在` / `未启用` / `未声明 auth 权限` / `未提供 auth 入口` | `authenticate` called on a plugin that is missing, disabled, lacks the permission or has no `auth.entry`. |
| `订阅 provider <id> 未安装或已禁用` | Subscription provider lookup failed. |

### Rejections from `flux.*`

| Message (substring) | Raised by | Meaning |
|---|---|---|
| `flux.fetch 仅支持 http/https` | `fetch` | Other scheme. |
| `blocked: non-routable IP` | `fetch` | The literal IP is loopback / private / link-local etc. |
| `fetch 失败: …` | `fetch` | Network error, TLS error, timeout, DNS failure (including every address filtered out), `too many redirects`, `blocked: non-routable redirect target`. The detail follows. |
| `URL 非法: …` / `HTTP method 非法: …` | `fetch` | Unparseable URL or method (also produced when `opts` is malformed). |
| `读取响应体失败: …` | `fetch` | The connection broke while reading the body. |
| `插件未声明 auth 权限，不能使用认证凭据` | `fetch` | An explicit `authRef` without permission `auth`. |
| `authentication_required: 未找到认证凭据 <ref>` / `认证凭据已过期 <ref>` | `fetch` | Explicit `authRef`: no credential, or it expired. |
| `认证凭据不属于当前插件` / `认证凭据不属于当前请求站点` | `fetch`, `auth.get` | Credential owner or site mismatch. |
| `认证档案必须提供 site` / `认证档案的 site 必须是有效的 URL 或 host` / `authRef 必须等于当前插件和规范化站点组成的引用` / `认证档案超过 65536 字节上限` | `auth.save` | Profile problems. |
| `authRef 必须属于当前插件` | `auth.remove` | The reference does not start with `<your id>::`. |
| `storage 值超过 65536 字节上限` / `storage 键数超过 100 上限` | `storage.set` | Limits. |
| `flux.fs 文件名 '<name>' 非法: <reason>` | `fs.*` | Bad file name. |
| `flux.fs 文件超过 8388608 字节上限` / `工作区总量超过 67108864 字节上限` / `文件数超过 100 上限` | `fs.writeFile` | Limits. |
| `flux.task.recordArtifact 仅 onDone 钩子可用` / `recordArtifact: …` | `task.recordArtifact` | Wrong hook, bad name, file does not start with the task's file stem, or file does not exist. |
| `flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用` / `flux.ffprobe.run …` | `ffmpeg.run` / `ffprobe.run` | Called outside `onDone`. |
| `ffmpeg 未安装或不可用` / `ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）` / `yt-dlp 未安装或不可用` | `run` | The tool is not installed. |
| `ffmpeg args 不可为空` / `yt-dlp args 不可为空` | `run` | Empty `args` (also produced when `spec` is malformed). |
| `ffmpeg 参数过多（>512）` / `ffmpeg 参数过长` / `ffmpeg 参数含 NUL` (same for `yt-dlp`) | `run` | Argument count / size limits. |
| `ffmpeg 参数 '<arg>' 被拒: <reason>` | `ffmpeg.run`, `ffprobe.run` | Blocked argument. |
| `yt-dlp 参数被拒: …` | `ytdlp.run` | Argument not on the allow-list. |
| `ffmpeg subdir '<x>' 非法` / `… subdir 逃逸牢笼` (same for `yt-dlp`) | `run` | Unsafe `subdir`. |
| `启动 ffmpeg 失败: …` / `ffmpeg 执行失败: …` (same for `yt-dlp`) | `run` | The process could not be started or crashed in the host. A normal non-zero exit **resolves** instead. |
| `tasks.create rate limited` | `tasks.create` | More than 20 creates in 60 s. |
| `method not allowed for plugins: …` | `tasks.*` | Internal guard; only the six task methods are reachable. |
| `任务参数非法: …` / `settings.update 参数非法: …` | `tasks.*`, `settings.update` | Params were not valid JSON-serializable data / not a string map. |

### Logged only

| Log text | Meaning |
|---|---|
| `hook <name> 执行失败（已忽略）: <error>` | A hook threw or timed out. |
| `钩子并发已满，本次钩子被丢弃` | Hook pool full; notification dropped. |
| `外部工具钩子排队已满，本次钩子被丢弃` | External-tool hook queue (64) full. |
| `flux.task.requestRetry 仅 onError 钩子可用，已忽略` | `requestRetry` outside `onError`. |
| `[plugin] subscription output: skipped N of M items, first reason: …` | Some subscription items were invalid. |
| `setInterval interval clamped to …ms` | `main` interval raised to the minimum. |

## Where to go next

- Declaring entries and permissions: [Manifest](/docs/en/plugins/manifest/) and [Permissions](/docs/en/plugins/permissions/).
- Views and node vocabulary: [Views](/docs/en/plugins/views/).
- Tooling and typed entry points: [SDK](/docs/en/plugins/sdk/).
- Diagnosing failures: [Debugging](/docs/en/plugins/debugging/).
- Working examples: the [feature-demo](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo) and [youtube-down](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/youtube-down) plugins.
