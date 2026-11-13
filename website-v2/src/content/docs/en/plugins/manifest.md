---
title: Manifest Reference
description: Every manifest.json field with its validation rules, the settings widget matrix, URL pattern rules, and how the manifest is checked at install and load time.
section: plugins
order: 10
---

`manifest.json` sits at the root of the plugin package and declares what the plugin is, which entry scripts it ships, what it contributes to the UI, and which capabilities it needs. All keys are camelCase. **Unknown keys are rejected** — a typo fails the whole manifest instead of being silently ignored.

This page is the field-by-field reference. For a guided walkthrough see [Your first plugin](/docs/en/plugins/your-first-plugin/) (hand-written JavaScript) or [TypeScript project](/docs/en/plugins/typescript-project/) (SDK, build and pack). Permissions and consent are covered in [Permissions, Consent & Audit](/docs/en/plugins/permissions/), and packaging in [Packaging & Plugin Market](/docs/en/plugins/packaging/).

## A complete example

```json
{
  "identity": "acme@demo",
  "name": "Acme Demo",
  "version": "1.0.0",
  "description": "Resolves Acme share links and tidies finished files.",
  "homepage": "https://example.com/acme-demo",
  "icon": "icon.png",
  "minAppVersion": "0.9.0",
  "resolvers": [
    { "match": { "urls": ["*://acme.example/share/*"] }, "entry": "resolve.js", "timeoutMs": 15000, "multi": true }
  ],
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone", "onError", "onCancel"],
    "match": { "urls": ["*://acme.example/*"] }
  },
  "main": { "entry": "main.js", "events": ["onDone"], "background": false },
  "contributes": {
    "views": [{ "id": "dashboard", "title": "Dashboard", "location": "activity" }],
    "commands": [{ "id": "refresh", "title": "Refresh", "palette": true }],
    "archive": "archive.js"
  },
  "settings": [
    { "key": "quality", "title": "Preferred quality", "type": "string", "widget": "select",
      "options": [ { "value": "hd", "label": "HD" }, { "value": "sd", "label": "SD" } ], "default": "hd" }
  ],
  "permissions": ["archive"]
}
```

A plugin may declare any combination of `resolvers`, `subscriptions`, `hooks`, `interceptors`, `auth`, `main` and `contributes`. A manifest that declares none of them is valid but does nothing.

## Top-level fields

| Field | Required | Type | Rules |
|---|---|---|---|
| `identity` | yes | string | `^[a-z0-9_-]+@[a-z0-9_-]+$` — lowercase author, `@`, name. Dots are forbidden. Permanent ID: settings, storage, consent and the install folder are all keyed by it. |
| `name` | yes | string | Display name; must not be empty or whitespace only. |
| `version` | yes | string | Three integer segments `MAJOR.MINOR.PATCH`. See [Version strings](#version-strings) for how strictly this is parsed. |
| `description` | no | string | Shown in the plugin list. |
| `homepage` | no | string | Project or author URL. |
| `icon` | no | string | Path inside the package. No `..`, no absolute path, no drive letter, no leading `/` or `\`, no empty segments. |
| `minAppVersion` | no | string | Minimum FluxDown version, same parsing as `version`. See [`minAppVersion` behaviour](#minappversion-behaviour). |
| `resolvers` | no | array | **At most one entry.** See [`resolvers[0]`](#resolvers0). |
| `subscriptions` | no | array | **At most one entry.** Subscription provider. See [`subscriptions[0]`](#subscriptions0). |
| `hooks` | no | object | Download lifecycle callbacks. See [`hooks`](#hooks). |
| `interceptors` | no | object | Task interceptors (request rewrite, segment filtering, post-processing). See [`interceptors`](#interceptors). |
| `auth` | no | object | Platform login entry. See [`auth`](#auth). |
| `main` | no | object | Always-on script for portable views, commands and background logic. See [`main`](#main). |
| `contributes` | no | object | Views, commands, archive classification, browser sniff rules and page scripts. See [`contributes`](#contributes). |
| `permissions` | no | array | Capability grants; unknown values are rejected. See [`permissions`](#permissions). |
| `settings` | no | array | Declarative settings fields. See [`settings[]`](#settings). |

### Version strings

`version` and `minAppVersion` are compared as three integers; anything after the third segment's number is ignored by the client.

| Where | What is accepted |
|---|---|
| FluxDown client (install and load) | `MAJOR.MINOR.PATCH`, optionally prefixed with `v`, optionally followed by `-pre` or `+build` (the suffix is cut off before comparing). `1.2.3`, `v1.2.3` and `1.2.3-rc.1` all parse as 1.2.3. Exactly three segments are required. |
| Plugin index and `fluxdown-plugin validate` | Strict: exactly `MAJOR.MINOR.PATCH`, digits only, no leading zeros, no `v` prefix, no suffix. |

Always write the strict form. A version the client tolerates can still be rejected when you publish to the market.

### `minAppVersion` behaviour

If the running FluxDown is older than `minAppVersion` (or either side cannot be parsed), the plugin is **not loaded**. It appears in the plugin list with a *failed* load status and the reason `需要 App ≥ X，当前 Y` (“requires App ≥ X, current is Y”) — it is not silently skipped. Installing such a package is rejected with `安装后未能加载插件（校验失败或版本门槛不满足）` (“the plugin could not be loaded after install (validation failed or version gate not met)”) and rolled back.

Raise `minAppVersion` whenever you start using a field, event or permission that older versions do not know: an older client rejects an unknown key or permission outright as an invalid manifest, so the version gate is what produces a clear message instead.

## `resolvers[0]`

A resolver turns a page URL into one or more downloadable files.

```json
{
  "resolvers": [
    {
      "match": { "urls": ["*://host.com/share/*", "*://cdn.host.com/*"] },
      "entry": "resolver.js",
      "timeoutMs": 15000,
      "multi": false
    }
  ]
}
```

| Field | Required | Rules |
|---|---|---|
| `match.urls` | yes | Non-empty list of URL patterns (see [URL pattern rules](#url-pattern-rules)). |
| `entry` | yes | Script file; safe relative path. Must define `globalThis.resolve`. |
| `timeoutMs` | no | Per-call timeout in milliseconds. Must not be `0`. Replaces the 10 s default but is capped at the 30 s hard ceiling whatever you write. |
| `memoryLimitMb` | no | QuickJS heap limit of one call in MiB. Must not be `0`. Replaces the 64 MiB default; anything above 512 MiB is capped at 512 MiB. Meant for resolvers that really process bulky data in script (for example parsing a multi-megabyte site script into a syntax tree); the limit is a ceiling, nothing is reserved up front. |
| `multi` | no | Boolean, default `false`. `true` declares that this resolver may return a multi-file manifest and should take part in **pre-resolution** (the preview that lists the files before a task is created). With `false`, a single-file resolver is not run ahead of time just to find out; if it returns a multi-file list when the task starts, the engine still splits it into separate tasks, so `multi` is a hint for pre-resolution and not a requirement for returning lists. |

The `resolve(ctx)` contract and its return shapes are in the [API reference](/docs/en/plugins/api-reference/).

## `subscriptions[0]`

A subscription provider feeds the subscription feature from a source other than plain RSS — for example a site's API that lists new episodes.

```json
{
  "subscriptions": [
    { "providerId": "acme-feed", "entry": "subscribe.js", "timeoutMs": 20000 }
  ]
}
```

| Field | Required | Rules |
|---|---|---|
| `providerId` | yes | `^[a-z0-9_-]{1,64}$`. Stable: it is stored on subscription records. `rss` is reserved for the built-in provider and is rejected. |
| `entry` | yes | Script file; safe relative path. Must define `globalThis.subscribe`; the function name is fixed. |
| `timeoutMs` | no | Per-call timeout in milliseconds. Must not be `0`. Default 10 s; can be set up to the 30 s hard ceiling. |

At most one provider per plugin. The `subscribe(ctx)` contract, the item shape and the validation applied to returned items are explained in the [Subscription guide](/docs/en/plugins/subscription-guide/).

## `hooks`

Hooks run your code on download lifecycle events.

```json
{
  "hooks": {
    "entry": "hooks.js",
    "events": ["onStart", "onDone", "onError", "onCancel"],
    "match": { "urls": ["*://host.com/*"] }
  }
}
```

| Field | Required | Rules |
|---|---|---|
| `entry` | yes | Script file; safe relative path. Must define a global function named after each subscribed event. |
| `events` | yes | Non-empty list; each item one of `onStart`, `onError`, `onDone`, `onMetaProbed`, `onCancel`. Anything else is rejected. |
| `match` | no | URL filter with the same pattern rules. If present, `urls` must be non-empty. Omitted = the hooks fire for every task. |

Caveat: if the same plugin also declares a resolver, `onMetaProbed` never fires for its tasks — resolver tasks skip the metadata probe entirely. FluxDown logs a warning if you subscribe to it anyway.

Event payloads, `requestRetry`, ffmpeg/ffprobe and yt-dlp post-processing are covered in the [Hooks guide](/docs/en/plugins/hooks-guide/).

## `interceptors`

Interceptors let a plugin read and change a task while the engine runs it — request identity, concurrency, streaming segments, post-processing and retry decisions. At most one `interceptors` object per plugin. It requires the `intercept` permission (elevated risk, see [Permissions](/docs/en/plugins/permissions/)).

```json
{
  "permissions": ["intercept"],
  "interceptors": {
    "entry": "intercept.js",
    "stages": ["prepare", "segments"],
    "match": { "urls": ["https://example.com/*"] },
    "timeoutMs": 3000,
    "onFailure": "continue"
  }
}
```

| Field | Required | Rules |
|---|---|---|
| `entry` | yes | Script file, safe relative path. Must define the single global `intercept(ctx)`; it dispatches on `ctx.stage`. |
| `stages` | yes | Non-empty, no duplicates. Each item one of `prepare`, `probed`, `refresh`, `selectVariant`, `selectPeriod`, `segments`, `liveSegments`, `finalize`, `failed`. An unknown name is rejected with the list of valid ones. |
| `match` | yes | URL filter, same pattern rules; `urls` must be non-empty. A task is intercepted when a pattern matches its original URL or its resolved URL. |
| `timeoutMs` | no | Wall-clock limit of one call, must not be `0`. Default 5 s, capped by the hard ceiling; `liveSegments` is further capped at 2 s and `refresh` at 5 s. |
| `onFailure` | no | `continue` (default) or `fail`. With `continue`, an error or invalid return value of this plugin is noted in the task activity and ignored; with `fail`, the task fails with `[插件] <identity>（<stage>）：<message>`. |

Validation runs after `auth`, in this order: `entry` path → `stages` non-empty → every stage known → no duplicate stage → `match.urls` non-empty → `timeoutMs ≠ 0` → `permissions` includes `intercept`. The entry file must exist in the package and is read at load time like `hooks.entry`. Stage contexts, return values, the `RequestPatch` rules and `defineInterceptors` are in the [Interceptors guide](/docs/en/plugins/interceptors-guide/).

## `auth`

```json
{
  "auth": { "entry": "auth.js", "timeoutMs": 20000 }
}
```

| Field | Required | Rules |
|---|---|---|
| `entry` | yes | Script file; safe relative path. Must define `globalThis.authenticate`. |
| `timeoutMs` | no | Per-call timeout in milliseconds for `authenticate`. Must not be `0`. Replaces the 30 s default but is capped at the 30 s hard ceiling; independent from `resolvers[0].timeoutMs`. |

`permissions` must include `"auth"` when `auth.entry` is declared, otherwise validation fails. The five login actions and the full `flux.auth` surface are in the [Auth guide](/docs/en/plugins/auth-guide/) and the [API reference](/docs/en/plugins/api-reference/#authenticatectx).

## `main`

```json
{ "main": { "entry": "main.js", "events": ["onDone"], "background": false } }
```

| Field | Required | Rules |
|---|---|---|
| `entry` | yes | Classic script (not an ES module); safe relative path inside the package, extension `.js`, file must exist. |
| `events` | no | Events delivered to the script; each must be one of `onStart`, `onError`, `onDone`, `onMetaProbed`, `onCancel`. |
| `background` | no | Boolean. `true` keeps the script running with the daemon. Requires the `background` permission. |
| `imageHosts` | no | Defaults to `[]`. Lowercase DNS hostnames or IP literals whose remote images `main` may load through `ctx.images` / `flux.images`. At most 64 unique entries; same host syntax as `desktop.networkHosts`. Included in network consent. |

Remote image loading is performed by the host, never directly by the UI. Declare every image source host in `main.imageHosts`; requests to other hosts are rejected. Existing manifests may omit this field (equivalent to `[]`). See the [API reference](/docs/en/plugins/api-reference/).

`main` is the always-on part of a plugin: portable views, commands and background logic run here, inside the daemon. See the [UI guide](/docs/en/plugins/ui-guide/).

## `contributes`

```json
{
  "contributes": {
    "views": [{ "id": "dashboard", "title": "Dashboard", "location": "activity",
                "desktop": { "entry": "ui/dashboard.js", "networkHosts": ["api.example.com"] },
                "portable": true, "icon": "icons/dashboard.svg" }],
    "commands": [{ "id": "refresh", "title": "Refresh", "palette": true, "taskMenu": false }],
    "sniffRules": "rules.json",
    "pageScripts": [{ "id": "hello", "title": "Hello", "matches": ["https://*.example.com/*"], "js": ["page/hello.js"] }],
    "archive": "archive.js"
  }
}
```

Empty `views` / `commands` arrays are the same as leaving them out. `archive` chooses a child folder for category-routed tasks from the known filename and optional source metadata; it does not rename files or take over the save root.

### `archive`

`contributes.archive` names an in-package `.js` file and requires `permissions: ["archive"]`. The host calls `globalThis.archive({ fileName, metadata?, isGroup })` when the task follows authorized default-directory routing, including when no category matches; an explicitly chosen save directory bypasses it. `metadata` may carry `platform`, `mediaKind`, `creatorId`, `creatorName`, `seriesId`, `seriesTitle`, `collectionId` and `collectionTitle`. Each value is a label limited to 180 UTF-8 bytes, with no control characters. This is source metadata, not file-content recognition; URLs, local paths and credentials are not exposed. `flux.settings` is available, but network, filesystem, task and process APIs are not.

For groups the callback runs once at the root, with `isGroup: true`; member relative paths and filenames are preserved. Return `null` / `undefined`, or `{ directories: ["Video", "Bilibili", "Creator", "Collection"] }` to create a relative directory below the authorized download base. Up to 8 safe components are allowed, with a joined relative path of at most 180 UTF-8 bytes. Legacy `{ folderName: "The Expanse" }` keeps its single-child behavior under a matched category; group category routing still requires every member to share that category. Empty or invalid output is ignored (fail-open), without failing the download. The first valid result in plugin-identity order wins. The folder decision is persisted and reused on resume and completion; the downloaded filename is unchanged. Multi-level output requires a host implementing this callback extension; do not assume older released hosts support it.
### `archiveRules`

`contributes.archiveRules` declares a bounded JSON classification pack verified against a publisher-pinned Ed25519 public key. `entry` is a safe package-relative `.json` file; the signed seed is validated during package installation. `publicKey` is 64 hexadecimal characters. Optional `updateUrl` must be a public HTTPS URL without credentials or a fragment; its hostname is included in the consent network-host list. Updates are checked daily when automatic updates are enabled and can also be requested explicitly or manually. A failed fetch or verification retains the last verified rules. Rules only classify future eligible files; they never reclassify historic files.

Hosts that predate this key and permission reject the plugin as an invalid manifest (unknown key and unknown permission), so set `minAppVersion` to the next release or later.

The entry is `globalThis.archive = (ctx) => result` with `ctx = { fileName }`. It returns `{ folderName: string }`, or `null` / `undefined` for no folder, and may be sync or async. It is called only for tasks that use category routing, when routing picks a category directory (a matching rule that has a directory, or the "Other" fallback directory); the saved folder is reused afterwards and the file name is never changed. The script only has `flux.settings`, `flux.logger` and `console`. The full behaviour (call timing, multiple plugins, `folderName` rules, failures, limits) is in the [API reference](/docs/en/plugins/api-reference/#archivectx).

### `views[]`

At most 16.

| Field | Required | Rules |
|---|---|---|
| `id` | yes | Matches `^[a-z][a-z0-9-]{0,31}$`; unique among views. |
| `title` | yes | 1–40 characters. |
| `location` | yes | `activity`, `settings`, `taskDetail`, `window` or `auth`. At most one view may use `settings`, and at most one may use `auth`. An `auth` view requires the top-level `auth` declaration (`auth.entry`, which in turn requires the `auth` permission), otherwise the manifest fails with `location 为 auth 的 view 要求声明 auth`. It replaces the content of the plugin's **Log in** dialog in the desktop app (GPUI) and the Web UI (the Web UI only shows portable implementations); mobile apps do not render it and keep the default login flow. See [Custom login UI](/docs/en/plugins/auth-guide/#custom-login-ui). Hosts that predate this location reject the plugin as an unknown `location`, so set `minAppVersion` to the next release or later. |
| `desktop` | no | Native desktop implementation, see below. A view **without** `desktop` is portable and requires `main`. |
| `portable` | no | Boolean, default `false`. Only allowed together with `desktop`. `true` means `main` also implements this view (via `flux.ui.view`) so clients that cannot run desktop scripts can show it; requires `main`. |
| `icon` | no | Package-relative path of a `.svg` file (any `location`; currently only used by the `activity` bar). At most 16 KiB (16384 bytes), valid UTF-8, an SVG document (starts with `<svg` or `<?xml` and contains `<svg`). The host treats it as a **monochrome mask** tinted with the theme color and only uses its alpha channel, so a 24×24 `viewBox` with `stroke="currentColor"` or `fill="currentColor"` (lucide style) works best; colors are flattened to one tone. Omit it to get the default icon. |

Where a view is shown depends on its declaration:

| Declaration | Desktop (GPUI) | Web | Mobile |
|---|---|---|---|
| no `desktop` (portable) | rendered from `main` | rendered from `main` | nothing is rendered |
| `desktop` + `portable: true` | native desktop script; falls back to the `main` implementation if the script package is not readable locally | rendered from `main` | nothing is rendered |
| `desktop` only | native desktop script; when the plugin's script package is not readable on this machine (for example the daemon runs on a remote host) the GPUI client shows a desktop-only notice | not shown at all — the Web UI lists only portable views and never shows a "desktop only" card | nothing is rendered |

Use `portable: true` when you want the same view on Web, and keep the desktop script as an enhancement. Details of the node tree and events are in the [Views reference](/docs/en/plugins/views/) and the [UI guide](/docs/en/plugins/ui-guide/); native scripts are covered by the [Desktop UI guide](/docs/en/plugins/desktop-ui-guide/).

### `views[].desktop`

| Field | Required | Rules |
|---|---|---|
| `entry` | yes | UI script; safe relative path inside the package. Extension must be `.js` or `.mjs`, and the file must exist in the package. |
| `width` / `height` | no | Initial window size in logical pixels. Only allowed when the view's `location` is `window`. Finite number within `[240, 4096]`. |
| `networkHosts` | no | Hosts the UI script may connect to. At most 64 entries per view; each must be a lowercase DNS hostname (RFC 1123 labels, at most 253 characters) or an IP literal — no scheme, port, path or wildcard — and unique within the view. |

Any view with `desktop` requires the `desktop` permission. `desktop.entry` is an ES module executed by the GPUI Shell built into the GPUI desktop client (it may `import` from `"gpui-kit"`, `"gpui-base"` and `"gpui-component"`). The engine and daemon **never** compile or execute it; they only validate the path and check that the file exists. Web and mobile clients do not run it.

`networkHosts` is an enforced allow-list: the script's network access is limited to exactly these hostnames. File system, process and clipboard access are **not** restricted — the script runs with full trust in the desktop process, so install only plugins you trust. See [Permissions](/docs/en/plugins/permissions/#full-trust-desktop-scripts). The union of all views' `networkHosts` is shown in the consent dialog and is part of what you approve.

### `commands[]`

At most 32; requires `main`.

| Field | Required | Rules |
|---|---|---|
| `id` | yes | Same pattern as view ids; unique among commands. |
| `title` | yes | 1–40 characters. |
| `palette` | no | Boolean. Show in the command palette. |
| `taskMenu` | no | Boolean. Show in the task context menu. |

### `sniffRules`

Safe relative path to a `.json` rule pack for the browser extension. Requires the `browserRules` permission.

- The file must exist in the package, be at most 256 KiB, parse as JSON, contain `"format": "fluxdown-sniff-pack"` and a `rules` array of at most 500 entries.
- The format of each rule is the one documented in the [Sniffing rules reference](/docs/en/browser-extension/sniff-rules/).

A minimal `rules.json`:

```json
{
  "format": "fluxdown-sniff-pack",
  "packId": "acme-demo",
  "sequence": 1,
  "engine": 1,
  "issuedAt": "2026-10-17T00:00:00Z",
  "rules": [
    {
      "id": "video",
      "version": 1,
      "tier": "plugin",
      "stage": "response",
      "match": { "host": ["acme.example"], "ext": ["xyz"] },
      "action": { "kind": "resource", "type": "video" }
    }
  ]
}
```

How the rules land in the browser extension:

- Only rules from plugins that are **enabled, loaded and consented** are served. Disabling the plugin, or leaving a new permission unapproved, removes its rules.
- Each rule `id` is rewritten to `plugin.<identity with @ replaced by .>.<your id>` — the rule above becomes `plugin.acme.demo.video`. If the rewritten id is longer than 64 characters, the rule is dropped (and logged).
- `tier` is forced to `plugin` whatever you write. Rules that are not objects or have no string `id` are dropped.
- Plugin rules rank below `local` and `official` rules and above `community` and `built-in` ones.

### `pageScripts[]`

Scripts the browser extension runs on web pages. At most 16 entries; requires the `pageScripts` permission (**Full** risk — the script can read and change the page and the user's logged-in session on every matching site). The guide is [Page scripts](/docs/en/plugins/page-scripts-guide/).

```json
{
  "permissions": ["pageScripts"],
  "contributes": {
    "pageScripts": [{
      "id": "bili-button",
      "title": "Download button",
      "matches": ["https://*.bilibili.com/*"],
      "excludeMatches": ["https://www.bilibili.com/blackboard/*"],
      "js": ["page/bili.js"],
      "runAt": "document_idle",
      "world": "isolated",
      "allFrames": false
    }]
  }
}
```

| Field | Required | Rules |
|---|---|---|
| `id` | yes | Matches `^[a-z0-9][a-z0-9_-]{0,31}$`; unique among the plugin's page scripts. |
| `title` | yes | 1–40 characters. Shown in the consent dialog and the plugin details. |
| `matches` | yes | 1–32 match patterns (below). |
| `excludeMatches` | no | 0–32 match patterns, same rules; default `[]`. |
| `js` | yes | 1–8 files; safe relative paths inside the package, extension `.js`, no duplicates. |
| `runAt` | no | `document_start`, `document_end` or `document_idle` (default). |
| `world` | no | `isolated` (default): runs in the extension's user-script world and gets the `fluxdown` bridge. `main`: runs in the page's own JavaScript world and has **no** bridge. |
| `allFrames` | no | Boolean, default `false`. |

A match pattern has the form `<scheme>://<host><path>`:

- `scheme` is `http`, `https` or `*`.
- `host` is a domain or `*.domain`. The domain contains only `a-z`, `0-9`, `.` and `-`, must contain a `.`, and must not start or end with `.` or `-`. An IPv4 literal is allowed (it still contains dots).
- `path` must start with `/`.
- Rejected: `<all_urls>`, a bare `*` host, ports and any other special form.

File content rules, checked when the package is installed or loaded: each `js` file is at most 256 KiB of valid UTF-8, and one script's files joined together are at most 384 KiB. The files are joined in `js` order with `"\n;\n"` between them; the SHA-256 of the joined UTF-8 bytes identifies the version of the script.

## `permissions`

Extra host capabilities a plugin opts into. Empty or omitted = the base sandbox (network via `flux.fetch`, `flux.storage`, logging, view and command contributions). Recognised values:

| Value | Grants |
|---|---|
| `ffmpeg` | The `flux.ffmpeg` / `flux.ffprobe` APIs — run the resolved ffmpeg on a finished file. |
| `ytdlp` | The `flux.ytdlp` API — run the resolved yt-dlp from `resolve` or any hook. |
| `auth` | Declare `auth.entry` for a platform login flow, and allow `flux.fetch` / `flux.auth` to use stored credentials. |
| `tasks` | `flux.tasks.*` — operate on download tasks from `main`. |
| `browserRules` | Contribute browser-extension sniff rules via `contributes.sniffRules`. |
| `desktop` | Run desktop scripts (`contributes.views[].desktop`) with full trust in the desktop process. |
| `pageScripts` | Run the plugin's scripts on the web pages listed in `contributes.pageScripts` (Full risk). |
| `background` | Keep `main` running with the daemon (`main.background`). |
| `intercept` | Declare `interceptors` to read and change a task's request, segments, quality and finished file while it runs. |
| `archive` | Declare `contributes.archive`: choose one subfolder inside the task's category folder from the file name. It can read plugin settings and write the plugin log; it has no file, network, task or process access (Elevated risk). |

Unknown values fail the whole manifest. Risk levels, consent and audit are explained in [Permissions, Consent & Audit](/docs/en/plugins/permissions/); the API behind each permission is in the [API reference](/docs/en/plugins/api-reference/).

Permissions that a declaration depends on:

| Declaration | Requires |
|---|---|
| `auth.entry` | `auth` |
| `main.background: true` | `background` |
| any `views[].desktop` | `desktop` |
| `contributes.sniffRules` | `browserRules` |
| `contributes.pageScripts` (non-empty) | `pageScripts` |
| `contributes.archive` | `archive` |

(`tasks`, `ffmpeg` and `ytdlp` are checked when the API is called, not by the manifest.)

## `settings[]`

Each entry describes one field; the app generates the form on desktop and Web. Keys must be unique and non-empty.

```json
{
  "settings": [
    { "key": "apiToken", "title": "API token", "type": "string", "widget": "password", "required": true },
    { "key": "quality",  "title": "Preferred quality", "type": "string", "widget": "select",
      "options": [ { "value": "hd", "label": "HD" }, { "value": "sd", "label": "SD" } ],
      "default": "hd" },
    { "key": "maxRetries", "title": "Max retries", "type": "number", "min": 0, "max": 10, "default": "3" },
    { "key": "verbose", "title": "Verbose logging", "type": "boolean", "default": "false" }
  ]
}
```

| Field | Required | Notes |
|---|---|---|
| `key` | yes | Unique within the plugin, not empty. Read back as `flux.settings.<key>`. |
| `title` | yes | Form label. |
| `description` | no | Help text below the field. |
| `type` | yes | `string`, `number`, or `boolean`. |
| `widget` | no | Defaults by type: string→`text`, number→`number`, boolean→`toggle`. |
| `options` | select only | Non-empty list of `{ "value", "label" }`. |
| `default` | no | **Always a string**, even for numbers (`"3"`) and booleans (`"true"` / `"false"`). For select, must be one of the option values; for number, must parse as a finite number and fall inside `min` / `max`. |
| `required` | no | Boolean. The form refuses to save an empty value. |
| `min` / `max` | number only | Finite, inclusive bounds, `min ≤ max`. Rejected on other types. |
| `pattern` | string only | A **JavaScript RegExp** (not a Rust regex) validated against the value on save. It is compiled at install time; an invalid pattern rejects the install. |
| `helperScript` | string only | A snippet the host shows a **Copy** button for, next to the field. The host only copies the text to the clipboard — it never runs it. The user pastes it into the target site's browser DevTools console (typical use: read `document.cookie` and copy a login cookie back into the field). Non-empty, at most 4 KB. |
| `helperLabel` | with `helperScript` | Button label, at most 60 characters. Only valid together with `helperScript`; a generic "Copy helper script" label is used when omitted. |

### Widget × type matrix

Only these combinations are valid — anything else fails validation:

| Widget | Allowed type |
|---|---|
| `text`, `password`, `textarea`, `folder`, `select` | `string` |
| `toggle` | `boolean` |
| `number` | `number` |

## URL pattern rules

Patterns are not regular expressions and not browser match patterns. The rules, in full:

- `*` is the **only** wildcard and matches any run of characters (including `/` and `:`).
- Text between `*`s must appear in order as substrings.
- If the pattern doesn't start with `*`, the first segment must be a **prefix** of the URL.
- If it doesn't end with `*`, the last segment must be a **suffix**.
- Matching is case-insensitive (both sides are lowercased whole, path included).
- A bare `*` matches everything.

Examples against `https://www.youtube.com/watch?v=abc`:

| Pattern | Result |
|---|---|
| `*://www.youtube.com/watch*` | matches |
| `https://x.com/a` (no wildcard) | exact match only |
| `*://x.com/*` vs `https://y.com/a` | no match — `://x.com/` not found |
| `youtube.com/*` | no match — the pattern is prefix-anchored and the URL starts with `https://` |

## Validation

The manifest is validated when the plugin is installed and every time it is loaded. A plugin that fails validation does not run; it is listed as failed with the reason, and the reason is written to the log (see [Debugging](/docs/en/plugins/debugging/)).

### Order of checks

identity format → `name` non-empty → `version` format → `minAppVersion` format → `icon` path safety → at most one resolver → resolver `entry` path, non-empty `match.urls`, `timeoutMs ≠ 0`, `memoryLimitMb ≠ 0` → at most one subscription → `providerId` format and not `rss`, `entry` path, `timeoutMs ≠ 0` → hooks `entry` path, non-empty valid `events`, non-empty `match.urls` if present → auth `entry` path, `auth` permission present, `timeoutMs ≠ 0` → `main` (path, `.js`, valid `events`, `background` permission) → `contributes` (view and command limits and ids, `settings` location at most once, `portable` only with `desktop`, `main` required for portable views and commands, desktop entry and size and hosts rules, `desktop` permission, view `icon` path / `.svg` extension, `sniffRules` path / extension / `browserRules` permission, `archive` package-relative path / `.js` extension / `archive` permission, then `pageScripts`: at most 16 → `pageScripts` permission → per script: `id` format and uniqueness, `title` length, `matches` count and patterns, `excludeMatches` count and patterns, `js` count, safe `.js` paths, no duplicates) → `settings` (keys unique, per-field rules and widget × type matrix) → every `permissions` value known.

### Files that must exist

At install and load time the package must contain every declared entry script (`resolvers[0].entry`, `hooks.entry`, `auth.entry`, `subscriptions[0].entry`, `main.entry`, `contributes.archive`), every `desktop.entry`, every view `icon` (which must also pass the icon content check above), the `sniffRules` file (which must also pass the content check above) and every file listed in a page script's `js` (each at most 256 KiB of valid UTF-8, at most 384 KiB per script once joined). A missing file is reported as `包内缺少文件 '<path>'` (“the package is missing file '<path>'”); an entry script that exists but cannot be read as UTF-8 text is reported as `读取入口文件 '<path>' 失败: …` (“failed to read entry file”). This applies to dev-mode plugins too: the plugin is listed as **Load failed** instead of failing later, on the first call.

### Compile checks at install

Syntax errors are rejected at install time rather than discovered on the first download. The compile check covers the **resolver, hooks, interceptors, auth, subscription and archive** entry scripts (up to six, one per declared kind). Each script is evaluated once in a bare context where `flux` and `console` do not exist, so top-level code must not call them. It does **not** cover `main` (compiled on demand) or desktop scripts (never compiled by the engine). Each `settings[].pattern` is also compiled as a JavaScript RegExp at install time; an invalid pattern rejects the install.

A failed install changes nothing: a first install leaves no folder behind, and an update restores the previous version.

### Checking before you ship

`fluxdown-plugin validate` runs the same rules plus the stricter listing checks (strict version format, every entry and the icon present in the package, entry and size limits). See the [SDK reference](/docs/en/plugins/sdk/) and, for validating by hand before you publish, [Packaging](/docs/en/plugins/packaging/#check-locally-before-you-publish).
