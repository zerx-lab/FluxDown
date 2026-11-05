---
title: Plugin System Overview
description: What FluxDown plugins can do, which route to pick (plain JavaScript or TypeScript SDK), how they run, how they are installed, and which clients support what.
section: plugins
order: 1
---

A FluxDown plugin is a folder (or a `.fxplug` zip of that folder) containing a `manifest.json` and one or more JavaScript files. The manifest declares what the plugin contributes; FluxDown runs the scripts in a sandbox inside its background service and calls them at the right moments.

Plugins can:

- rewrite a task's URL before it starts (turn a share-page link into a direct link),
- react to task events and post-process finished downloads,
- log in to sites and reuse the credentials,
- provide new subscription sources next to the built-in RSS,
- add pages, settings tabs, task-detail tabs and commands to the app,
- contribute sniffing rules to the browser extension.

## What do you want to build?

Every row links to the tutorial for that capability and to the reference that lists every field.

| Capability | Declared in `manifest.json` | Needs permission | Tutorial | Reference |
|---|---|---|---|---|
| **Resolve URLs** — share link → direct link | `resolvers` | none | [Your first plugin](/docs/en/plugins/your-first-plugin/) | [API reference](/docs/en/plugins/api-reference/) |
| **React to task events**, run ffmpeg / ffprobe / yt-dlp on finished files | `hooks` | `ffmpeg`, `ytdlp` (only for those tools) | [Hooks guide](/docs/en/plugins/hooks-guide/) | [API reference](/docs/en/plugins/api-reference/) |
| **Intercept downloads** — read and change a task's request (URL, headers, cookies, body), connection count, streaming segments, credentials mid-run, and the finished file; decide whether a failed task is retried | `interceptors` | `intercept` | [Interceptors guide](/docs/en/plugins/interceptors-guide/) | [API reference](/docs/en/plugins/api-reference/) |
| **Account login** (QR code, verification code, …) and authenticated requests | `auth` | `auth` | [Auth guide](/docs/en/plugins/auth-guide/) | [API reference](/docs/en/plugins/api-reference/) |
| **Subscription provider** | `subscriptions` | none | [Subscription guide](/docs/en/plugins/subscription-guide/) | [API reference](/docs/en/plugins/api-reference/) |
| **Portable UI** — pages, settings tab, task-detail tabs, commands, timers; the same code renders on desktop and Web | `main`, `contributes.views`, `contributes.commands`, `settings` | `tasks` to manage tasks, `background` to stay resident | [UI guide](/docs/en/plugins/ui-guide/) | [Views](/docs/en/plugins/views/) |
| **Native desktop UI** — GPUI scripts with full desktop access | `contributes.views[].desktop` | `desktop` | [Desktop UI guide](/docs/en/plugins/desktop-ui-guide/) | [Views](/docs/en/plugins/views/) |
| **Browser sniffing rules** | `contributes.sniffRules` | `browserRules` | [Manifest reference](/docs/en/plugins/manifest/) | [Manifest reference](/docs/en/plugins/manifest/) |
| **Browser page scripts** — run your JavaScript on listed websites through the browser extension, with a bridge back to the plugin | `contributes.pageScripts` | `pageScripts` | [Page scripts](/docs/en/plugins/page-scripts-guide/) | [Manifest reference](/docs/en/plugins/manifest/#pagescripts) |
| **Plugin settings form** | `settings` | none | [Your first plugin](/docs/en/plugins/your-first-plugin/#6-add-a-setting-optional) | [Manifest reference](/docs/en/plugins/manifest/) |

The base sandbox **cannot** create tasks or read arbitrary files. It talks to the outside world through the `flux.*` API (HTTP fetch, key-value storage, scratch files, logging, retry requests). Anything beyond that is a permission the plugin declares, the user approves, and FluxDown records in an audit log; see [Permissions](/docs/en/plugins/permissions/).

## Choose your path

There are two ways to author the same plugin format. Both produce a folder with `manifest.json` plus classic `.js` files, and the engine cannot tell them apart.

| | Plain JavaScript | SDK + TypeScript |
|---|---|---|
| Tooling | a text editor | Node.js 20+ and `fluxdown-plugin-sdk` |
| Build step | none | `npm run build` bundles `src/` into `dist/` |
| Types and autocomplete for `flux.*` | no | yes |
| Several source files and imports | no — one script per entry | yes — bundled into one script per entry |
| JSX for views | no — build nodes with `flux.ui.h` | yes |
| Manifest validation before install | at install time, in the app | `npm run validate` locally, with the same rules the app and the plugin index use |
| Best for | a single resolver or hook, quick experiments | anything with a UI, several entries, or code you will maintain |

Start with [Your first plugin](/docs/en/plugins/your-first-plugin/) if you want to understand the format with nothing in the way. Start with [TypeScript project](/docs/en/plugins/typescript-project/) if you already know you want types, a UI, or several entries; the [SDK reference](/docs/en/plugins/sdk/) lists every command. You can move from one to the other at any time — the manifest stays the same.

## How plugins run

FluxDown has two execution models. Which one you get depends on the entry.

| | Stateless entries | Resident `main` |
|---|---|---|
| Entries | `resolvers[].entry`, `hooks.entry`, `interceptors.entry`, `auth.entry`, `subscriptions[].entry` | `main.entry` |
| Lifetime | a **fresh** JavaScript context (QuickJS) for every call; nothing survives, use `flux.storage` to persist | **one long-lived context per plugin**; global variables survive between calls |
| Called by | downloads (resolve, hooks, interceptors), the login dialog, subscription refresh | views, commands, timers, `flux.events` subscriptions |
| Started | on demand, per call | on demand when a view opens or an event arrives; reclaimed after 5 minutes without sessions, timers or running calls, unless `main.background` is `true` (needs the `background` permission) |
| Concurrency | calls run independently | calls of one plugin run **one at a time**: a slow `await` delays every other view, command and timer of that plugin |
| Extra API | `flux.*` core | the core plus `flux.ui`, `flux.commands`, `flux.rpc`, `flux.events`, `flux.timers`, and `flux.tasks` with the `tasks` permission |

Scripts are loaded as classic scripts, not ES modules. Entry functions must end up on `globalThis` (a top-level `function resolve(ctx) {...}` does that; with the SDK, `defineResolver` and friends do it for you).

> **Keep top-level code free of `flux.*`.** When a plugin is installed or reloaded, FluxDown evaluates each resolver, hooks, interceptors, auth and subscription script once in a bare JavaScript context to check that it compiles. `flux` and `console` do not exist there, so a top-level `flux.logger.info(...)` makes the install fail with a `脚本编译失败: …` (“script compile failed”) error. Do your work inside the entry function.

### Resolvers

- **Lazy.** FluxDown stores only your plugin's ID with the task, never the resolved URL. `resolve(ctx)` runs again on *every* start and resume. Direct links from file hosts usually expire, so re-resolving keeps old tasks downloadable.
- **Off the main loop.** Resolution runs on a dedicated thread pool; a slow script cannot freeze the app.
- **Fail-closed.** If your script throws, times out, or returns garbage, the task goes to the *error* state with a message prefixed `[插件] <identity>:` (the marker means “plugin”). FluxDown never falls back to downloading the original page URL — that would save an HTML page as a video file. The user can explicitly bypass a broken plugin with **Ignore plugin retry** on the failed task.

When several enabled plugins match the same URL, FluxDown asks the user which plugin to use the first time such a task starts (the dialog lists every matching plugin, sorted by `identity`) and can remember the choice as the site's default plugin; users manage these under Settings → Extensions → Site Defaults. Unattended downloads (RSS, remote dispatch, silent mode with follow-up choices skipped) never ask: they use the site's default plugin, otherwise the plugin with the lexicographically smallest `identity`.

Tasks handled by a resolver skip the normal metadata probe, so if the same plugin also subscribes to `onMetaProbed`, that hook never fires for its own tasks (FluxDown logs a warning at load time).

### Hooks

Hooks are fire-and-forget notifications:

- A hook that throws or times out is logged (as a log line containing `执行失败（已忽略）`, “failed, ignored”) and ignored. It can never affect the task.
- If the hook runtime is busy, the notification is dropped (logged as `钩子并发已满，本次钩子被丢弃`, “hook concurrency full, this hook was dropped”) rather than queued.
- The only way a hook can influence a task is `flux.task.requestRetry({ delayMs })`, and only from inside `onError`.

See the [Hooks guide](/docs/en/plugins/hooks-guide/).

### Interceptors

An interceptor is one `intercept(ctx)` entry that FluxDown calls at the stages the manifest lists (`prepare`, `refresh`, `probed`, `selectVariant`, `selectPeriod`, `segments`, `liveSegments`, `finalize`, `failed`). It runs on the daemon for tasks whose URL matches `interceptors.match.urls`, needs the `intercept` permission, and re-runs on every start, resume and retry. A failing call is either skipped with a note in the task's activity log or fails the task, depending on `onFailure`. See the [Interceptors guide](/docs/en/plugins/interceptors-guide/).

### Limits

| Limit | Value |
|---|---|
| Resolve / subscription timeout | 10 s default; the entry's `timeoutMs` can lower or raise it, hard ceiling 30 s |
| Auth timeout | 30 s default; `auth.timeoutMs` can change it, hard ceiling 30 s |
| Interceptor timeout | 5 s default; `interceptors.timeoutMs` can change it, hard ceiling 30 s. `liveSegments` is capped at 2 s and `refresh` at 5 s. `finalize` of a plugin with the `ffmpeg` or `ytdlp` permission gets the long external-tool budget, like hooks |
| Resolve, auth, subscription and interceptor memory | 64 MB |
| Hook timeout / memory | 5 s / 32 MB; a hook with the `ffmpeg` (on `onDone`) or `ytdlp` permission gets a much longer wall-clock budget, see the [Hooks guide](/docs/en/plugins/hooks-guide/) |
| `main` call | 30 s wall clock, 2 s CPU, 64 MB per call |
| Circuit breaker | 3 consecutive timeouts or out-of-memory errors in a **resolver, interceptor, subscription or `main`** call → plugin auto-disabled; any other outcome resets the count. Hooks and `authenticate` calls do not count. |

An auto-disabled plugin shows a notice in the app and an **Auto-disabled** badge. Re-enable it from **Settings → Extensions → Plugins** after fixing the script (installing an update re-enables it automatically). For every limit and error, see the [API reference](/docs/en/plugins/api-reference/) and [Debugging](/docs/en/plugins/debugging/).

## Installing plugins

Open **Settings → Extensions → Plugins** in the desktop app or the Web UI. Three ways in:

- **Install .fxplug / .zip plugin** — upload a `.fxplug` file (a zip of the plugin folder, see [Packaging](/docs/en/plugins/packaging/)). In the Web UI the file is picked in your browser and uploaded to the host. Shown only while the **Dev mode** switch is on (see below).
- **Install from directory (dev mode)** — point at a folder containing `manifest.json`. FluxDown records the folder path instead of copying it. The **Dev mode** switch (off by default, not remembered between visits) controls whether the whole install area — this directory input together with **Install .fxplug / .zip plugin** — is shown; with it off, install from the plugin market. Dev plugins show a **Dev** badge and a **Reload** button, see below.
- **Plugin market** — browse and install published plugins from the in-app market.

The path you type for directory install is a path **on the machine that runs FluxDown's background service**, not on the machine with the browser. With a desktop app these are the same machine. For a headless server or Docker container, use a path inside the server or container — for example, bind-mount your `dist/` folder into the container and enter the in-container path.

Installed (non-dev) plugins live under `<data dir>/plugins/<identity>/`. New installs are enabled by default, unless they request permissions: those stay disabled, showing **Authorization required**, until you approve them (see [Permissions](/docs/en/plugins/permissions/)).

### What to do after you edit a dev plugin

| You changed | Do |
|---|---|
| a resolver, hooks, interceptors, auth or subscription script | nothing — dev plugins re-read these on every call |
| `main` script, `manifest.json`, or a new entry | press **Reload** on the plugin row (tooltip: "Reload (re-read manifest and scripts)") — it re-reads the manifest and every script and re-checks them like an install; on failure the dev registration is kept and the error is shown |
| desktop UI script (`desktop.entry`) | **Reload**, then reopen the view |

The complete table, with logs and error messages, is in [Debugging](/docs/en/plugins/debugging/).

## Client support

Plugin logic always runs in FluxDown's background service (daemon). Clients differ in what they can *show*:

| | Desktop app | Web UI | Mobile apps |
|---|---|---|---|
| Resolvers, hooks, interceptors, auth, subscriptions | run in the daemon for every client | same | same |
| Install: zip, market, directory (dev) | yes | yes — directory path is on the host | zip and market on a connected host; no directory install |
| Portable views and commands (`main`) | yes | yes | not rendered |
| Native desktop views (`desktop`) | yes | not shown | not shown |
| `window` views | yes (plugin row button) | not shown | not shown |
| Login dialog for `auth` plugins | yes | yes | yes |

A view that has both a `desktop` entry and `"portable": true` uses the native one on desktop and the portable one on Web. Details are in [Views](/docs/en/plugins/views/#client-support).

## Where to go next

1. [Your first plugin](/docs/en/plugins/your-first-plugin/) — a working resolver in plain JavaScript.
2. [TypeScript project](/docs/en/plugins/typescript-project/) — resolver, hook, view and settings with the SDK, up to a packed `.fxplug`.
3. Guides: [Hooks](/docs/en/plugins/hooks-guide/), [Interceptors](/docs/en/plugins/interceptors-guide/), [Page scripts](/docs/en/plugins/page-scripts-guide/), [Auth](/docs/en/plugins/auth-guide/), [Subscriptions](/docs/en/plugins/subscription-guide/), [Portable UI](/docs/en/plugins/ui-guide/), [Desktop UI](/docs/en/plugins/desktop-ui-guide/).
4. [Debugging](/docs/en/plugins/debugging/) — logs, reload semantics, error messages and fixes.
5. Reference: [Manifest](/docs/en/plugins/manifest/), [API](/docs/en/plugins/api-reference/), [Views](/docs/en/plugins/views/), [SDK & CLI](/docs/en/plugins/sdk/), [Permissions](/docs/en/plugins/permissions/), [Packaging & market](/docs/en/plugins/packaging/).
