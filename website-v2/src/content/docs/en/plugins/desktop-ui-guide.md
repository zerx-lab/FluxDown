---
title: Native Desktop UI
description: Build native desktop windows and embedded panels for a plugin with a gpui-shell ES module — manifest, View classes, the fluxdown module, theming, trust model and troubleshooting.
section: plugins
order: 8
---

A **desktop script** is an ES module that the FluxDown desktop app (GPUI) runs inside its own process to draw a fully native interface: your own layout, controls and styling instead of the fixed node kinds of [portable views](/docs/en/plugins/ui-guide/). This guide builds two plugins from scratch — a stand-alone **window** and an **embedded panel** — and then covers everything a desktop script can and cannot do.

Desktop scripts are a power feature. They run with **full trust** and exist only in the desktop app; they do not run in the daemon, on the Web UI or on mobile. If your interface can be expressed with the portable node kinds, a portable view is simpler, safer and works everywhere.

## Desktop or portable?

| | Portable view | Desktop view |
|---|---|---|
| Written as | `main` script returning data nodes `{ k, p, c }` | ES module with a `View` subclass drawing gpui elements |
| Where it runs | Daemon (QuickJS) | Desktop app process (gpui-shell, QuickJS) |
| Clients | Desktop app, Web UI | Desktop app only |
| Look | The fixed node set, themed by the client | Anything gpui can draw, styled by your code |
| Locations | `activity`, `settings`, `taskDetail`, `auth` | all five, including `window` |
| Permission | none beyond what `main` needs | `desktop` (risk level **Full**) |
| Files, processes, clipboard, network | through `flux.*` APIs only | direct access (network: listed hosts only) |
| Survives without a window open | yes (daemon) | no |

Choose desktop when you need a stand-alone window, custom drawing, local files or processes, or controls the node set lacks. Choose portable for everything else. You can also ship **both** for one view: the desktop app prefers the desktop script and the Web UI uses the portable view (see [Coexisting with a portable view](#coexisting-with-a-portable-view)).

## How it works

- The manifest declares a view with a `desktop` entry. The file is part of the plugin package; the engine only validates its path and existence and never executes it.
- When the user opens the view, the desktop app loads the file into an **isolated gpui-shell runtime for that plugin** and mounts the class you default-export.
- The script talks to FluxDown through the `fluxdown` module, and to the plugin's own `main` script through `invoke` ↔ `flux.rpc.handle`.
- Because the script is trusted code in the desktop process, installing a plugin with `desktop` shows the user a full-trust consent dialog (see [Permissions & audit](/docs/en/plugins/permissions/)).

## Tutorial 1: a stand-alone window

We will build **Release Checker**: a window with a *Check* button that fetches the latest FluxDown release from the GitHub API, shows it, and remembers the last release it saw through the plugin's `main` script. It needs no portable view and no UI in the Web client.

### 1. The manifest

```json
{
  "identity": "acme@release-checker",
  "name": "Release Checker",
  "version": "1.0.0",
  "description": "Looks up the latest FluxDown release in a native window.",
  "permissions": ["desktop"],
  "main": { "entry": "main.js" },
  "contributes": {
    "views": [
      {
        "id": "checker",
        "title": "Release Checker",
        "location": "window",
        "desktop": {
          "entry": "ui/release.js",
          "networkHosts": ["api.github.com"],
          "width": 520,
          "height": 360
        }
      }
    ]
  }
}
```

- `permissions: ["desktop"]` is mandatory for any `desktop` entry.
- `location: "window"` makes the view a stand-alone window. `width` / `height` (logical pixels, 240–4096, default 960 × 640) are only allowed on `window` views.
- `networkHosts` lists every host the script may contact (see [Network access](#network-access)). Without `api.github.com` the `fetch` below would be refused.
- `main` is optional for desktop-only views; we add it here to demonstrate `invoke`. A view without `portable: true` does not need `main`.

### 2. The `main` script

`main.js` is a classic script that stores the last seen tag with `flux.storage` (available without any permission) and exposes two RPC handlers:

```js
flux.rpc.handle("lastSeen", async () => ({ tag: await flux.storage.get("lastSeenTag") }));

flux.rpc.handle("remember", async (args) => {
  const tag = args && typeof args.tag === "string" ? args.tag : "";
  if (tag === "") return { ok: false, error: "tag is required" };
  await flux.storage.set("lastSeenTag", tag);
  return { ok: true };
});
```

Handlers may return any JSON-serializable value. Return failures **as data** (`{ ok: false, error }`): if a handler throws, the desktop side only receives the error code, not your message (see [Calling `main`](#calling-main-invoke)).

### 3. The window script

`ui/release.js`:

```js
import { View } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { Button, Label } from "gpui-component";
import { invoke, toast } from "fluxdown";

const LATEST = "https://api.github.com/repos/zerx-lab/FluxDown/releases/latest";

export default class ReleaseWindow extends View {
  init(_props, cx) {
    this.cx = cx; // an AsyncContext: still usable after `await`
    this.release = null;
    this.lastSeen = null;
    this.status = "Press Check to look for the latest release.";
    this.busy = false;
    cx.spawn(async (cx) => {
      try {
        const saved = await invoke("lastSeen");
        this.lastSeen = saved && saved.tag ? saved.tag : null;
      } catch (e) {
        this.status = "The plugin's main script is not available.";
      }
      cx.notify();
    });
  }

  check() {
    this.busy = true;
    this.status = "Checking…";
    this.cx.spawn(async (cx) => {
      try {
        const response = await fetch(LATEST, { headers: { Accept: "application/vnd.github+json" } });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const body = await response.json();
        this.release = { tag: body.tag_name, name: body.name, url: body.html_url };
        this.status = "Done";
        await invoke("remember", { tag: body.tag_name });
        this.lastSeen = body.tag_name;
        toast("success", `Latest release: ${body.tag_name}`);
      } catch (e) {
        this.status = `Check failed: ${e && e.message ? e.message : e}`;
        toast("error", this.status);
      }
      this.busy = false;
      cx.notify();
    });
  }

  render(cx) {
    const colors = cx.theme().colors;
    const column = v_flex().size_full().gap(12).p(20).bg(colors.background);
    column.child(Label.new("FluxDown releases").text_size(18));
    column.child(Label.new(this.status).text_color(colors.muted_foreground));
    if (this.release) {
      column.child(Label.new(`${this.release.name} (${this.release.tag})`));
      if (this.lastSeen === this.release.tag) {
        column.child(Label.new("Saved as last seen").text_color(colors.muted_foreground));
      }
    }
    return column.child(
      h_flex()
        .gap(8)
        .child(
          Button.new("check")
            .label("Check")
            .primary()
            .loading(this.busy)
            .on_click(() => this.check()),
        )
        .child(
          Button.new("open")
            .label("Open page")
            .disabled(!this.release)
            .on_click((_event, cx) => {
              if (this.release) cx.open_url(this.release.url);
            }),
        ),
    );
  }
}
```

What to notice:

- The module **default-exports a class that extends `View`** (imported from `gpui-kit`). That class is the whole interface.
- `init(props, cx)` runs once when the view is created. Put state on `this` and create long-lived things (tasks, timers) here.
- `render(cx)` returns a gpui element built with chained calls (`v_flex().gap(12).child(...)`). It runs when the view is invalidated, **not** every frame. After you change state, call `cx.notify()` to request a re-render.
- `cx.spawn(async (cx) => { ... })` runs async work. The `cx` it receives stays valid across `await`; a plain event-handler `cx` does not outlive its handler, which is why `init` stores the `AsyncContext` in `this.cx`.
- There is no browser DOM, no global `setTimeout`, and no `require`. Use `cx.timer.after / every` or `cx.sleep(ms)` for time; use `fetch` and `fs/promises` for I/O.
- Never store elements on the instance; build them inside `render`.

### 4. Install and open

1. Put the three files in a folder: `manifest.json`, `main.js`, `ui/release.js`.
2. In FluxDown open **Settings → Extensions → Plugins**, choose **From a directory** and pick the folder (dev mode).
3. Approve the consent dialog. It shows the **Full** risk level, the `desktop` permission and the host `api.github.com`.
4. Find the plugin's row. Because it has a `window` view with a desktop entry, the row shows a **window icon button** whose tooltip is the view title. Click it. The window opens at 520 × 360, centered, titled `Release Checker: Release Checker` (`<plugin name>: <view title>`).

That row button is the **only** way users open a `window` view: there is no command-palette entry and no activity-bar item. It appears when the plugin is enabled, loaded without errors, and the view has location `window` and a `desktop` entry. Clicking it again focuses the already open window.

## Tutorial 2: an embedded panel

Embedded views live inside the main window: as an **activity-bar page**, a **tab in the plugin settings dialog**, or a **tab in the task detail panel**. We will build **Task Inspector**, a tab in the task details that reads the selected task with `fluxdown.call`.

### 1. The manifest

```json
{
  "identity": "acme@task-inspector",
  "name": "Task Inspector",
  "version": "1.0.0",
  "description": "Adds a native Inspector tab to the task details panel.",
  "permissions": ["desktop"],
  "contributes": {
    "views": [
      {
        "id": "inspector",
        "title": "Inspector",
        "location": "taskDetail",
        "desktop": { "entry": "ui/inspector.js" }
      }
    ]
  }
}
```

There is no `main` and no `portable`: the tab exists only in the desktop app. (`width` and `height` are rejected here — they belong to `window` views.)

### 2. The script

`ui/inspector.js`:

```js
import { View } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { Button, Label } from "gpui-component";
import { taskId, call, toast } from "fluxdown";

const STATUS = ["Pending", "Downloading", "Paused", "Completed", "Error", "Preparing"];

// `fluxdown.call` rejects with the RPC error details serialized as JSON text.
function describeError(error) {
  const text = error && error.message ? error.message : String(error);
  try {
    const detail = JSON.parse(text);
    return detail.field ? `${detail.code} (${detail.field})` : String(detail.code);
  } catch {
    return text;
  }
}

export default class Inspector extends View {
  init(_props, cx) {
    this.cx = cx;
    this.task = null;
    this.error = null;
    this.loading = false;
    this.load();
  }

  load() {
    const id = taskId();
    if (id === null) {
      this.error = "Open this tab from a task.";
      return;
    }
    this.loading = true;
    this.cx.spawn(async (cx) => {
      try {
        this.task = await call("daemon.task.get", { taskId: id });
        this.error = null;
      } catch (e) {
        this.error = describeError(e);
        toast("error", `Could not load the task: ${this.error}`);
      }
      this.loading = false;
      cx.notify();
    });
  }

  row(cx, name, value) {
    return h_flex()
      .gap(12)
      .child(Label.new(name).w(96).text_color(cx.theme().colors.muted_foreground))
      .child(Label.new(String(value)));
  }

  render(cx) {
    const task = this.task;
    const body = v_flex().gap(8).p(16);
    if (this.error) {
      body.child(Label.new(this.error).text_color(cx.theme().colors.destructive));
    } else if (task) {
      const percent = task.totalBytes > 0 ? Math.round((task.downloadedBytes / task.totalBytes) * 100) : 0;
      body
        .child(this.row(cx, "File", task.fileName))
        .child(this.row(cx, "Status", STATUS[task.status] ?? task.status))
        .child(this.row(cx, "Progress", `${percent}%`))
        .child(this.row(cx, "URL", task.url));
    } else {
      body.child(Label.new("Loading…"));
    }
    return body.child(
      Button.new("reload")
        .label("Reload")
        .loading(this.loading)
        .on_click((_event, cx) => {
          this.load();
          cx.notify();
        }),
    );
  }
}
```

Points specific to embedded views:

- `taskId()` returns the task the tab belongs to (a string) for `taskDetail` views and `null` everywhere else. FluxDown creates one view per (tab, task) and caches it, so reading the id once in `init` is correct.
- `call("daemon.task.get", { taskId })` goes to the local agent. Task objects use camelCase fields (`taskId`, `url`, `fileName`, `status`, `downloadedBytes`, `totalBytes`, …); `status` is a number: 0 pending, 1 downloading, 2 paused, 3 completed, 4 error, 5 preparing.
- **Embedded views have no overlay host.** The `window.push_toast`, `window.open_dialog` and `window.open_sheet` APIs exist only in stand-alone windows. In a panel, report things with `fluxdown.toast` (and by drawing state into the view).
- An activity-bar page is created the first time it is opened and kept for as long as the main window lives; a task-detail tab is created per task and cached by the detail panel.

### 3. Install

Install the folder as in tutorial 1 (`manifest.json` + `ui/inspector.js`), approve the dialog, then open a task's details: an **Inspector** tab appears after the built-in tabs. Settings views and activity pages work the same way with `location` set to `settings` (at most one per plugin) or `activity`.

## The `fluxdown` module

Import it by bare name inside a desktop script: `import { identity, taskId, call, invoke, theme, locale, toast } from "fluxdown"`. Each view gets its own copy bound to its plugin and task.

| Function | Returns | Description |
|---|---|---|
| `identity()` | `string` | The plugin identity, for example `acme@release-checker`. |
| `taskId()` | `string \| null` | The task id for `taskDetail` views; `null` elsewhere. |
| `call(method, params?)` | `Promise<unknown>` | Calls an RPC method on the local agent. Only methods starting with `daemon.` or `agent.` are allowed. |
| `invoke(name, args?)` | `Promise<unknown>` | Calls a handler your `main` registered with `flux.rpc.handle(name, …)` (through `daemon.plugin.invoke`). |
| `theme()` | `{ revision, tokens }` | The fully resolved FluxDown theme. `revision` increases whenever the theme changes, and views are re-rendered automatically. |
| `locale()` | `string` | The UI language, for example `zh-CN` or `en`. |
| `toast(level, text)` | `void` | Shows a notification in the active window. `level` is `info`, `success`, `warning` or `error`. |
| `close()` | `boolean` | Closes the container hosting this view (the login dialog of an `auth` view, the settings dialog, a stand-alone window) and returns `true`; the close is deferred until the current call returns. Views embedded in a page (activity page, task-detail tab) have no container: nothing happens and it returns `false`. |
| `navigate(target)` | `void` | Brings the main window to the front and switches page. `target` is `"downloads"`, `"rss"`, `"webhooks"` or `{ view: "<id>" }` (an `activity` view of this plugin). Synchronous: an invalid shape throws `fluxdown.navigate: …`, and so does a target that is not one of this plugin's contributed activity-bar views. A valid target is executed deferred. |

### Error semantics

| Situation | What you see |
|---|---|
| `call` with a method outside `daemon.*` / `agent.*` | Throws **synchronously at the call**: ``fluxdown.call: `system.shutdown` is not allowed; only daemon.* and agent.* methods``. The attempt is recorded in the audit log as `denied`. |
| `call` / `invoke` whose RPC fails | The promise **rejects** with the error details serialized as JSON text, for example `{"code":"invalidArgument","retryable":false,"field":"name"}`. The fields are `code`, `retryable` and optionally `field`, `reason`, `revision`. Parse `error.message` (or the value itself) with `JSON.parse`. |
| `toast` with another level | Throws ``fluxdown.toast: unknown level `x`; expected info, success, warning or error``. |
| `theme()` when no theme is available | Throws `fluxdown.theme: theme is not available`. |
| `toast` outside a script call (for example in a stray callback) | Throws `fluxdown.toast: no active application scope`. |

Use `try`/`catch` around `await call(...)` and `await invoke(...)` as in the tutorials; a `catch` covers both the synchronous and the rejected form.

### Calling `main` (`invoke`)

`invoke(name, args)` ↔ `flux.rpc.handle(name, handler)` is the bridge between your desktop script and the resident `main` script in the daemon:

- `args` and the handler's return value must be JSON-serializable. A handler returning `undefined` yields `null`.
- The handler runs inside `main`'s VM, so it can use everything `main` can: `flux.tasks`, `flux.storage`, `flux.fs`, `flux.fetch`, `flux.ffmpeg`, … subject to the permissions you declared. This is the right place for logic that must keep working without a window, and for anything the desktop script should not do itself.
- `invoke` needs a `main` entry. If the plugin has none, is disabled, or the name is not registered, the promise rejects with an `invalidArgument` error (the message such as `rpc not registered: <name>` is not forwarded). If the handler throws, the desktop side also only gets the error code. **Return failures as data** — `{ ok: false, error: "…" }` — when the UI needs a message.
- `invoke` calls are serialized with everything else running in `main` (view sessions, commands, timers) and are subject to its budgets (2 s CPU, 30 s wall time per call).

## Styling and theme

- `cx.theme()` returns the base theme. Semantic colors are snake_case fields of `cx.theme().colors`: `background`, `foreground`, `surface`, `primary`, `primary_foreground`, `secondary`, `muted`, `muted_foreground`, `accent`, `destructive`, `border`, `input`, `ring`, and so on. Pass them straight to style calls such as `.bg(...)` and `.text_color(...)`.
- `fluxdown.theme().tokens` is the complete FluxDown token map keyed by path, for example `"colors.primary"`, `"colors.success"`, `"colors.warning"`, `"colors.info"`, `"colors.statusDownloading"`, `"radius.md"`, `"spacing.lg"`, `"typography.sm.size"`, `"density.control"`. Colors are `#rrggbbaa` strings and accepted directly by color style methods; sizes and radii are numbers. The SDK exports the path union as `ThemeTokenPath` and the value type as `ThemeTokenValue`.
- When the user switches theme the host bumps `theme().revision` and re-renders every live script view, so reading tokens inside `render` is enough.
- FluxDown keeps its own theme in force. If a script overwrites the global theme object, FluxDown reinstalls its own, so a plugin cannot permanently re-skin the app.
- Build with FluxDown tokens rather than hard-coded colors so your UI follows light/dark and custom themes.

## Where desktop views appear

| | Embedded (`activity`, `settings`, `taskDetail`, `auth`) | Window (`window`) |
|---|---|---|
| Container | A page / tab inside the main window or settings dialog | Its own resizable, minimizable window with FluxDown's title bar |
| How users reach it | Activity bar item / plugin settings button / task details tab | The window icon button on the plugin row (Settings → Extensions → Plugins) |
| `taskId()` | the task (taskDetail only) | `null` |
| `fluxdown.close()` | Closes the dialog for `auth` and `settings` views and returns `true`; returns `false` for `activity` / `taskDetail` | Closes the window; returns `true` |
| Overlay APIs `window.push_toast`, `window.open_dialog`, `window.open_sheet` | **Not available** — use `fluxdown.toast` | Available |
| `width` / `height` | rejected by the manifest | initial size, 240–4096 (default 960 × 640); the minimum size is the smaller of the declared size and 240 on each axis |

Windows: one window per view (clicking again focuses it), titled `<plugin name>: <view title>`, opened centered. A dialog from a window script looks like this:

```js
import { div } from "gpui-kit";
// inside an event handler of a window script:
window.open_dialog(() => div().child("Press Esc to close"));
```

## Network access

The script's network access is restricted to the hosts you list in `desktop.networkHosts`:

- Entries are lowercase DNS host names (RFC 1123 labels, up to 253 characters) or IP literals; no scheme, port, path or wildcard. At most 64 per view, no duplicates.
- The global `fetch`, `net` and `websocket` modules may only reach those exact hosts; every redirect target is checked too. A host that is not listed is refused — the call rejects.
- `fetch` has a 30 second timeout and buffers at most 8 MiB per response.
- The host list is shown in the consent dialog. If an update adds a host, the user is asked again for the new set.
- `fluxdown.call` is unaffected: it talks to the local agent and is limited to `daemon.*` / `agent.*` instead.

## Permissions and trust

A desktop script runs in the desktop app process with your user's privileges: it can read and write any file the user can, run any program (through the `process` module), use the clipboard and `localStorage`, and reach the declared network hosts. It cannot call `process.exit` (never granted). None of those native operations is audited one by one; the audit log records that the script was loaded (`desktop.load`, with the grants it ran with such as `fs:all process:all network:api.github.com clipboard:rw`), unloaded (`desktop.unload`), and each `fluxdown.call` / `fluxdown.invoke` it made. See [Permissions & audit](/docs/en/plugins/permissions/#full-trust-desktop-scripts).

Write desktop scripts as if reviewers will read them: ask for the smallest `networkHosts` list, never touch files you do not need, and prefer delegating privileged work to `main`, whose capabilities are limited to your declared permissions.

## Coexisting with a portable view

A view can have both implementations:

```json
{
  "id": "inspector",
  "title": "Inspector",
  "location": "taskDetail",
  "portable": true,
  "desktop": { "entry": "ui/inspector.js" }
}
```

`portable: true` states that `main` also registers `flux.ui.view("inspector", …)`, so a `main` entry is required. The rules:

| Client / situation | What is shown |
|---|---|
| Desktop app, script file readable on this machine | The desktop script |
| Desktop app, daemon on another machine (the package path is not readable locally) | The portable view if `portable: true`; otherwise the notice “This view is only available in the desktop app” |
| Web UI | The portable view if `portable: true`; otherwise nothing at all (the entry is hidden) |
| Mobile apps | Nothing (plugin views are not rendered) |

There is no fallback in the other direction: if the desktop script is chosen but fails to load or throws while mounting, the view shows an error panel (“The plugin interface failed to load” with the cause) instead of the portable view. `window` views need a `desktop` entry; a portable-only `window` view is never shown.

## Runtime, storage and data directory

- **One runtime per plugin.** The desktop app creates an isolated gpui-shell runtime when the first desktop view of a plugin is loaded and drops it when the last one closes. Do not rely on module-level variables surviving that; keep durable state in `localStorage`, files, or `main`.
- **Data directory.** Each plugin has its own directory under the desktop app's data directory, `plugin-ui/<encoded identity>/`, where letters, digits and `-` are kept and every other byte becomes `_xx` in lowercase hexadecimal (`acme@release-checker` → `acme_40release-checker`). `localStorage` persists to `store.json` there. It is separate from the daemon-side workspace used by `flux.fs`.
- **Modules.** Only the built-in modules can be imported by bare name: `gpui-kit` (alias `gpui`), `gpui-base`, `gpui-component`, `gpui-shell`, `gpui-fps`, `fluxdown`, `buffer`, `console`, `crypto`, `fs/promises`, `net`, `os`, `path`, `process`, `url`, `websocket`, `zlib`, plus the globals `fetch`, `TextEncoder`, `TextDecoder`, `localStorage` and `window`. There is no npm resolution at run time: bundle third-party code into your entry (the SDK does this). Relative imports between your own files resolve against the importing file.
- **No background execution.** The script only runs while a view is mounted. Anything that must happen without a window belongs in `main`.

## Building with the SDK

You can write the scripts by hand as above, or in TypeScript with [`fluxdown-plugin-sdk`](/docs/en/plugins/sdk/). List the desktop entries under `desktop` in `fluxdown-plugin.config.json`; the key is the output path inside `dist/`, the value the source file:

```json
{
  "entries": { "main.js": "src/main.ts" },
  "desktop": { "ui/release.js": "src/desktop/release.ts" }
}
```

`desktop` entries are bundled as **ES modules** (not IIFE like `entries`), with the host modules left as bare imports. For editor types add `src/env.d.ts`:

```ts
/// <reference types="fluxdown-plugin-sdk" />
/// <reference types="fluxdown-plugin-sdk/types/desktop" />
```

`fluxdown-plugin-sdk/types/desktop` declares every module above (`gpui-kit`, `gpui-base`, `gpui-component`, `fluxdown`, …) and the globals. A typed `ui/release.ts` looks like:

```ts
import { View } from "gpui-kit";
import type { AsyncContext, Context } from "gpui-kit";
import { v_flex } from "gpui-base";
import { Button, Label } from "gpui-component";
import { invoke, toast, theme } from "fluxdown";

export default class ReleaseWindow extends View {
  private cx!: AsyncContext;
  private tag: string | null = null;

  init(_props: unknown, cx: AsyncContext): void {
    this.cx = cx;
  }

  render(cx: Context) {
    const colors = cx.theme().colors;
    return v_flex()
      .gap(8)
      .p(16)
      .bg(colors.background)
      .child(Label.new(`Theme revision ${theme().revision}; last tag ${this.tag ?? "-"}`))
      .child(
        Button.new("ping").label("Ping main").on_click(() => {
          this.cx.spawn(async (cx) => {
            const reply = (await invoke("lastSeen")) as { tag: string | null };
            this.tag = reply.tag;
            toast("info", "main answered");
            cx.notify();
          });
        }),
      );
  }
}
```

Then:

```bash
npm run build     # bundles main.js and ui/release.js into dist/, validates
npm run pack      # produces acme@release-checker-1.0.0.fxplug
```

The validator checks that every `desktop.entry` exists in the package, has a `.js` or `.mjs` extension, that `desktop` permission is declared, and the `networkHosts` / size rules above. See [Packaging & publishing](/docs/en/plugins/packaging/) for the release flow.

## Development loop

1. Install the project (or its `dist/`) as a dev-mode plugin with **From a directory**.
2. Run `npm run dev` so every save rebuilds `dist/`.
3. FluxDown does **not** watch desktop scripts. A script is read when its view is created, so close and reopen a `window` view to run new code. An embedded page or tab that is already created keeps the code it loaded; close and reopen the main window (or restart the desktop app) to load the new script.
4. After you change `manifest.json` — views, `networkHosts`, permissions, `main` — press **Reload** on the plugin row (it re-reads the manifest and every script). Changes that add a permission or a network host require approving the consent dialog again.
5. Changing `main.js` also needs **Reload**: the daemon discards the old `main` host (open portable sessions end and can be reopened), so `invoke` calls reach the new code.

Check **Settings → Extensions → Plugins → Audit log** to confirm what actually ran: a `desktop.load` row shows the grants and the load outcome, `desktop.call` / `desktop.invoke` rows show each bridge call, including `denied` ones.

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| No window icon on the plugin row | The plugin is disabled, still waiting for consent, or failed to load; the view's `location` is not `window`; or the view has no `desktop` entry. Fix the manifest and press **Reload**. |
| Panel or window shows “The plugin interface failed to load” | The text below the title is the cause: ``plugin `<identity>` interface failed to load: …``. Typical reasons: syntax error in the script; the default export is missing or does not extend `View`; an import that is not a built-in module (an npm package or `node:` specifier that was not bundled); the file is missing from the package. The same message is written to the desktop log (**Open Logs Folder** in the app menu). |
| Panel shows “This view is only available in the desktop app” | The desktop script file is not readable on this machine (for example the desktop app is attached to a remote daemon) and the view is not portable. Add `portable: true` with a `main` implementation, or use the plugin on the daemon's own machine. |
| The window opens but shows the load-failure panel with a remote daemon | Same cause: the package directory lives on the daemon's machine. Windows have no portable fallback. |
| `fetch` rejects for a host you expect to work | The host (and every redirect target) must be listed in `desktop.networkHosts`, exactly, in lowercase, without scheme, port or wildcard. Press **Reload** after editing the manifest and approve the new host. |
| ``fluxdown.call: `x` is not allowed; only daemon.* and agent.* methods`` | You called a method outside `daemon.*` / `agent.*`. |
| `invoke` rejects with `{"code":"invalidArgument",…}` | The plugin has no `main`, `main` has not registered that name, the plugin is disabled, or the handler threw. Return failures as data to carry a message. |
| `window.open_dialog` / `window.push_toast` fails in a panel | Embedded views have no overlay host; use `fluxdown.toast`. |
| Edits to the script do not show up | Scripts are not hot-reloaded; reopen the window, or reopen the main window for embedded views (see [Development loop](#development-loop)). |
| The UI does not update after changing state | Call `cx.notify()` after mutating state outside `render`, including at the end of `cx.spawn` tasks. |
| A timer or `require` call throws a stub error | There are no browser timers or `require`; the error names the replacement. Use `cx.timer.after(ms, fn)`, `cx.timer.every(ms, fn)` or `await cx.sleep(ms)`, and `import` for modules. |
| Manifest rejected: `view 'x' 声明 desktop 时 permissions 必须包含 desktop` (“the view declares desktop, so permissions must include desktop”) | Add `"desktop"` to `permissions`. |
| Manifest rejected: `view 'x' 的 width 仅允许用于 location 为 window 的 view` (“width is only allowed for window views”) | Remove `width` / `height` or change `location` to `window`. |
| Manifest rejected: `view 'x' 有可移植实现，必须声明 main` (“the view has a portable implementation, so main must be declared”) | Add a `main` entry, or drop `portable: true`. |
| Manifest rejected: `view 'x' 的 portable 仅允许与 desktop 同时声明` (“portable is only allowed together with desktop”) | Remove `portable`, or add a `desktop` entry. |

## Next steps

- [Views reference](/docs/en/plugins/views/) — the portable node set, `main` lifecycle and the client support matrix.
- [Building a UI with portable views](/docs/en/plugins/ui-guide/) — the cross-client alternative.
- [Permissions & audit](/docs/en/plugins/permissions/) — consent, risk levels and what is logged.
- The [feature demo plugin](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo) contains a window script (`ui/window.js`, with `window.push_toast`, a dialog and file access) and an embedded panel (`ui/panel.js`) exercising every `fluxdown` function.
