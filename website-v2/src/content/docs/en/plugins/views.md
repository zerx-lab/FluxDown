---
title: Views, Commands & the UI API
description: Reference for portable plugin views, commands and the resident main script — nodes, props, events, limits, lifecycle and the client support matrix.
section: plugins
order: 12
---

This page is the reference for plugin UI: the resident **`main` script**, the `flux.ui` / `flux.commands` / `flux.rpc` / `flux.events` / `flux.timers` APIs, every node kind with its props, events and limits, and which client shows what. For step-by-step walkthroughs see [Building a UI with portable views](/docs/en/plugins/ui-guide/) and [Native desktop UI](/docs/en/plugins/desktop-ui-guide/).

A plugin can show UI without shipping any UI code. The `main` script returns a tree of **data nodes** `{ k, p, c }` (kind, props, children); the desktop app (GPUI) and the Web UI render that tree with their own native controls, using the active theme. Because the tree is data, one view works in every client that supports portable views. This is called a **portable view**. A view may also have a **desktop implementation** (see [Desktop implementations](#desktop-implementations)).

## Client support

| `location` | Desktop app (GPUI) | Web UI | Mobile apps |
|---|---|---|---|
| `activity` | Portable and desktop implementations. A page in the activity bar. | Portable only. A page at `/plugins/<identity>/<viewId>`. | Not rendered |
| `settings` | Portable and desktop. A tab in the plugin's settings dialog. | Portable only. A tab in the plugin's settings dialog (only while the plugin is enabled, loaded and fully authorized). | Not rendered |
| `taskDetail` | Portable and desktop. A tab in the task detail panel and in stand-alone task detail windows. | Portable only. | Not rendered |
| `window` | **Desktop implementation only.** Opened from the plugin row. | No entry. | Not rendered |
| `auth` | Portable and desktop. The whole content of the plugin's **Log in** dialog (replaces the default site / login-info form). The dialog title stays "Log in <plugin>". | Portable only. Same. | Not rendered; the default login flow is used |

- Only plugins that are **enabled, loaded and have no pending consent** contribute views and commands. The Web settings tab additionally requires the plugin to be usable in the same sense.
- On the Web a view is shown only when its `portable` flag is true. A view that declares `desktop` without `portable: true` never appears on the Web; opening its address directly shows the "The view session has ended" placeholder, not a "desktop only" card.
- Mobile apps keep the protocol but ignore plugin view events.
- In the desktop app, when a view has both implementations, the desktop script is used if its package file is readable on this machine; otherwise the portable view is used (see [Fallback rules](#fallback-rules)).

## Declaring views and commands

```json
{
  "identity": "acme@task-board",
  "name": "Task Board",
  "version": "1.0.0",
  "description": "Lists download tasks in an activity-bar page.",
  "permissions": ["tasks"],
  "main": { "entry": "main.js", "events": ["onDone"] },
  "contributes": {
    "views": [
      { "id": "board", "title": "Task Board", "location": "activity" },
      { "id": "options", "title": "Options", "location": "settings" },
      { "id": "files", "title": "Files", "location": "taskDetail" }
    ],
    "commands": [
      { "id": "refresh", "title": "Refresh task board", "palette": true, "taskMenu": true }
    ]
  }
}
```

| View field | Rules |
|---|---|
| `id` | `^[a-z][a-z0-9-]{0,31}$`, unique among the plugin's views. |
| `title` | 1–40 characters. Shown as the page / tab / window title. |
| `location` | `activity`, `settings`, `taskDetail`, `window` or `auth`. At most one `settings` view and at most one `auth` view per plugin. `auth` requires the top-level `auth` declaration (see [Custom login UI](/docs/en/plugins/auth-guide/#custom-login-ui)); older apps reject the unknown location, so set `minAppVersion`. |
| `portable` | Only valid together with `desktop`. `true` means `main` also implements the view. Without `desktop` a view is always portable. |
| `desktop` | `{ entry, networkHosts?, width?, height? }`; needs the `desktop` permission. See [Native desktop UI](/docs/en/plugins/desktop-ui-guide/). |

At most 16 views and 32 commands. A view that has a portable implementation (no `desktop`, or `portable: true`) and any command require a `main` entry. Command `id` follows the same pattern as view ids; `title` is 1–40 characters. [Manifest reference](/docs/en/plugins/manifest/#contributes) lists every validation rule.

## The `main` script

`main` is a **classic script** (not an ES module) evaluated once in a QuickJS VM inside the daemon. Everything is registered through `flux.*` while the script is evaluated. Besides the APIs below, `main` has the same `flux.fetch`, `flux.storage`, `flux.fs`, `flux.logger`, `flux.info` and `flux.settings` as other entry scripts, plus `flux.auth`, `flux.ffmpeg` / `flux.ffprobe` and `flux.ytdlp` when the matching permission is declared (see [API reference](/docs/en/plugins/api-reference/)).

### Lifecycle

- **Start.** `main` starts on demand: when a view is opened, a command runs, an `invoke` arrives, or an event listed in `main.events` fires. With `main.background: true` (needs the `background` permission) it starts with the daemon and stays resident.
- **Idle reclaim.** A non-background host with no open view sessions, no timers and no running calls is shut down after 5 minutes.
- **One VM for everything.** Global variables survive between calls. All view sessions, commands, `invoke` calls, event handlers and timers of the plugin share the one VM and run **one at a time**; a slow `await` in one handler delays all the others.
- **Budgets.** Each call may use up to 2 seconds of CPU and 30 seconds of wall time, and the VM at most 64 MiB of memory.
- **Fatal failures.** If a call exceeds the CPU/wall budget or the memory limit, the host is discarded: all its JavaScript state is lost, timers are cancelled, and every open view session of the plugin ends with reason `pluginUnloaded`. Clients show "The view session has ended" with a **Reopen** button; reopening starts a new host and runs `init` again. Three consecutive failures trip the circuit breaker and disable the plugin (see [Permissions & audit](/docs/en/plugins/permissions/#audit-log)).
- **Reloading.** The host is also discarded when the plugin's manifest or `main` source changes (for a dev-mode plugin: press **Reload** on the plugin row, which re-reads the manifest and every script) and when the plugin is disabled. Open sessions end with `pluginUnloaded`.

### Example

The manifest above, plus `main.js`:

```js
const { h } = flux.ui;
const STATUS = ["Pending", "Downloading", "Paused", "Completed", "Error", "Preparing"];
const open = new Set(); // states of the open sessions

async function reload(state) {
  state.tasks = await flux.tasks.list(); // needs the `tasks` permission
}

async function refreshAll() {
  for (const state of open) {
    await reload(state);
    state.ctx.refresh();
  }
}

flux.ui.view("board", {
  async init(ctx) {
    const state = { ctx, tasks: [], filter: "" };
    await reload(state);
    open.add(state);
    return state;
  },

  render(state) {
    const needle = state.filter.toLowerCase();
    const rows = state.tasks
      .filter((t) => t.fileName.toLowerCase().includes(needle))
      .map((t) => ({
        key: t.taskId, // string, non-empty, unique
        name: t.fileName,
        size: t.totalBytes,
        done: t.totalBytes > 0 ? t.downloadedBytes / t.totalBytes : 0,
        status: STATUS[t.status] ?? String(t.status),
      }));
    return h("stack", { gap: "md", padding: "md" },
      h("heading", { text: "Task board" }),
      h("stack", { key: "toolbar", direction: "row", gap: "sm", align: "center" },
        h("input", { key: "filter", value: state.filter, placeholder: "Filter by name" }),
        h("button", { key: "refresh", text: "Refresh", variant: "primary" })),
      h("table", {
        key: "tasks",
        columns: [
          { key: "name", title: "Name" },
          { key: "size", title: "Size", kind: "bytes" },
          { key: "done", title: "Done", kind: "percent" },
          { key: "status", title: "Status", kind: "badge" },
        ],
        rows,
        rowActions: [
          { id: "pause", label: "Pause" },
          { id: "resume", label: "Resume" },
          { id: "remove", label: "Remove", tone: "danger" },
        ],
        emptyText: "No tasks",
      }));
  },

  async event(state, ev) {
    if (ev.node === "root/toolbar/filter" && ev.type === "change") {
      state.filter = String(ev.value);
    } else if (ev.node === "root/toolbar/refresh" && ev.type === "press") {
      await reload(state);
    } else if (ev.node === "root/tasks" && ev.type === "rowAction") {
      if (ev.action === "pause") await flux.tasks.pause(ev.key);
      if (ev.action === "resume") await flux.tasks.resume(ev.key);
      if (ev.action === "remove") await flux.tasks.remove(ev.key, false);
      await reload(state);
    }
  },

  dispose(state) {
    open.delete(state);
  },
});

flux.commands.register("refresh", refreshAll);
flux.events.on("onDone", refreshAll);
```

The tasks returned by `flux.tasks.list()` use camelCase fields (`taskId`, `fileName`, `totalBytes`, `downloadedBytes`, `status`, `url`, …); there is no `id` field. `flux.tasks` exists only when the manifest declares the `tasks` permission — without it `flux.tasks` is `undefined`.

## `flux.ui.view`

```ts
flux.ui.view(id: string, def: {
  init?(ctx): State | Promise<State>;
  render(state, ctx): Node | Promise<Node>;
  event?(state, ev, ctx): void | State | Promise<void | State>;
  dispose?(state): void | Promise<void>;
})
```

`id` must be a non-empty string and match a declared view id; registering the same id twice replaces the first definition. `render` is required. Opening a view whose id was declared but never registered fails with `view not registered: <id>`.

| Callback | When it runs | Notes |
|---|---|---|
| `init(ctx)` | When a client opens a session. | Returns the initial state; returning `undefined` keeps the default `{}`. |
| `render(state, ctx)` | After `init`, after every `event`, after `ctx.refresh()`, and after every `flux.events.on` handler (for every open session). | Must return a node. Returning `null`/`undefined` fails with `render() returned no node`. Treat it as a pure function of `state`. |
| `event(state, ev, ctx)` | When the user interacts with a node that emits events. | Mutate `state` in place, or return a value to replace the state. A render follows automatically. |
| `dispose(state)` | When the session closes. | Not called when the host is discarded. |

### `ctx`

| Field | Value |
|---|---|
| `session` | Unique id of this open session. |
| `props` | Always `{}` today: neither the desktop app nor the Web UI passes props. |
| `taskId` | The task a `taskDetail` view is attached to; `null` for every other location. |
| `locale` | The client's UI language when the session was opened (for example `zh-CN`, `en`); an empty string if the client did not report one. It is not updated when the user switches language. |
| `images.load(opts)` | Loads an approved remote image through the host and returns a `flux-image:<uuid>` resource reference for this session. The URL host must be declared in `main.imageHosts` and approved; clients never fetch the URL themselves. Session close or plugin unload releases the resource. See [Image loading](/docs/en/plugins/api-reference/#image-loading-view-sessions-only). |
| `images.release(src)` | Returns a promise; explicitly releases a session image after removing all uses, including prefetched state. Identical images share a reference, so release invalidates every use of it. |
| `refresh()` | Requests a re-render of this session. Keep `ctx` (for example inside `state`) to refresh from timers, commands or event handlers. |
| `close()` | Closes the container that hosts this view and ends the session: the login dialog (`auth`), the settings dialog (`settings`) or a stand-alone window. A view embedded in a page (activity page, task-detail tab) has no container, so it shows "The view session has ended" with a **Reopen** button instead. Calling it again is a no-op. It may be called inside `init` (for example when the user is already logged in): the client shows the view and closes it right away. |
| `navigate(target)` | Returns a promise. Brings the main window to the front and switches to a page. `target` is `"downloads"`, `"rss"`, `"webhooks"`, or `{ view: "<id>" }` for one of this plugin's `activity` views. An unknown target rejects with `navigate: unknown target …`; a `{ view }` that is not an `activity` view of this plugin rejects with `navigate: view '<id>' is not an activity view of this plugin`; calling it after `close()` rejects with `navigate: the view has been closed; call navigate() before close()`. It may be called inside `init`. If the page is not available on the client (for example the Web UI cannot show a desktop-only view) the client ignores the request and logs a warning. Only the client that opened this session is affected. |

TypeScript types `ViewContext`, `ViewEvent` and `ViewDefinition` are exported by `fluxdown-plugin-sdk` (see [SDK reference](/docs/en/plugins/sdk/)).

```jsx
flux.ui.view("profile", {
  async init(ctx) {
    const avatar = await ctx.images.load({
      url: "https://images.example.com/avatar.jpg",
      maxWidth: 256,
      maxHeight: 256,
    });
    return { avatar };
  },
  render: ({ avatar }) => <image src={avatar} alt="Profile" />,
});
```

The `flux-image:<uuid>` source is a host-owned session resource; existing data URI sources remain supported. It is not a remotely fetched URL and cannot be reused after its session closes.

Example: an activity page that adds a download and then jumps to the download list:

```js
flux.ui.view("add", {
  init: (ctx) => ({ ctx, url: "" }),
  render: (state) =>
    h("stack", { gap: "sm", padding: "md" },
      h("input", { key: "url", value: state.url, placeholder: "https://…" }),
      h("button", { key: "add", text: "Add and show downloads", variant: "primary" })),
  async event(state, ev) {
    if (ev.node === "root/url" && ev.type === "change") state.url = String(ev.value);
    if (ev.node === "root/add" && ev.type === "press") {
      await flux.tasks.create({ request: { url: state.url } }); // needs the `tasks` permission
      await state.ctx.navigate("downloads");
    }
  },
});
```

### Event objects

`event(state, ev, ctx)` receives:

| Field | Present on | Meaning |
|---|---|---|
| `ev.node` | always | Id of the node that fired, for example `root/toolbar/refresh`. |
| `ev.type` | always | `press`, `change`, `submit`, `rowAction` or `select`. |
| `ev.value` | `change`, `submit` | `boolean` for `switch` / `checkbox`; `string` for everything else. |
| `ev.key` | `rowAction`, `select` | The key of the row / list item. |
| `ev.action` | `rowAction` | The `id` of the clicked row action. |

**Dispatching.** Give every interactive node a `key` and match on `ev.node`, or match on `ev.type` plus `ev.key` / `ev.action` for collections. A node id is built from the root (`root`) and the keys of each ancestor: `root/toolbar/filter` is the node with key `filter` inside the node with key `toolbar` that is a direct child of the root. A child without a key uses its position among its siblings (`root/0/1`) — fragile when the layout changes, so key anything that fires events. Table rows and list items fire events on the **table / list node** and identify themselves through `ev.key`.

### Event timing

1. A client sends the event; the daemon queues it and returns immediately.
2. Events of one session run in the order they were sent, one at a time, together with all other calls of the plugin (see [Lifecycle](#lifecycle)).
3. After the handler, `render` runs. Renders of one session are throttled to one every 100 ms; renders requested in between are merged and only the latest tree is sent.
4. The daemon compares the new tree with the previous one and sends only the **difference** to the clients, so large tables stay cheap.
5. A session that receives no keep-alive for 90 seconds is closed. Clients send keep-alives every 30 seconds.

If `event` or a later `render` throws, or produces an invalid tree, the view shows an error bar with the message and the session stays open; the next successful render clears it. If the **first** render (the one that follows `init`) fails, opening the session itself fails: the client shows a load-failure page with a **Reopen** button.

## Other `main` APIs

| API | Notes |
|---|---|
| `flux.ui.h(kind, props, ...children)` | Builds `{ k, p, c }`. Children are flattened; `null`, `undefined`, `true` and `false` are dropped. Strings and numbers are **not** valid children. If `kind` is a function it is called with `{ ...props, children }` (a component). |
| `flux.ui.Fragment` | Groups children without a node; cannot be a view root. |
| `flux.ui.toast(level, text)` | `info`, `success`, `warning` or `error` (anything else is shown as `info`). Has no session: it is broadcast to **every connected client** (desktop app and Web UI; mobile ignores it). |
| `flux.commands.register(id, handler)` | `handler({ taskIds })`. Running an unregistered command fails with `command not registered: <id>`. |
| `flux.rpc.handle(name, handler, opts?)` | Handler receives `(args, ctx)` and may return any JSON-serializable value (`undefined` becomes `null`). Called by your own desktop views with `fluxdown.invoke(name, args)` (`ctx = { caller: "desktop" }`). With `opts = { page: true }` it is also callable from your [page scripts](/docs/en/plugins/page-scripts-guide/) (`ctx = { caller: "page", pageUrl }`, untrusted input). An unknown name fails with `rpc not registered: <name>`; a page call to a handler without `page: true` fails with `rpc not exposed to page: <name>`. |
| `flux.events.on(name, handler)` | `name` is one of the `main.events` entries (`onStart`, `onDone`, `onError`, `onCancel`, `onMetaProbed`). Several listeners per name are allowed. An exception in a listener is logged as a warning and does not stop the others. All open sessions re-render afterwards. Payloads: see [Hooks guide](/docs/en/plugins/hooks-guide/). |
| `flux.timers.setTimeout / setInterval / clear` | At most 64 live timers (`too many timers (max 64)`). An interval is clamped to ≥ 1 s while any view session is open and ≥ 60 s otherwise. Timers do not re-render views by themselves: call `ctx.refresh()`. |
| `flux.tasks.*` | `list()`, `get(id)`, `create(params)`, `pause(id)`, `resume(id)`, `remove(id, deleteFiles?)`. Needs the `tasks` permission. `create` takes the `daemon.task.create` parameters (for example `{ request: { url, startPaused } }`) and is limited to 20 per minute per plugin. |
| `flux.settings.update({ key: "value" })` | Writes the plugin's settings (string values, validated against `manifest.settings`). |

## Node reference

Every node is a plain object `{ k, p, c }`: `k` is the kind (string), `p` the props (object, optional), `c` the children (array, optional). Any other field is rejected (`unknown node field`). The `h` function and the JSX runtime of the [SDK](/docs/en/plugins/sdk/#jsx) produce exactly this shape. The JSX runtime drops `null`, `undefined` and `false` children (not `true`); in both forms a string or number child is rejected with `a node must be an object {k, p, c}` — wrap text in a `text` node.

### Validation rules

| Rule | Detail |
|---|---|
| Unknown props | Rejected: `<kind> unknown prop "x"; allowed props: …`. |
| Required props | Missing → `<kind> missing required prop "x"`. Required props are marked below. |
| Defaults | Optional props that are omitted take the default shown in the tables. |
| Types | Strict. A `string` prop does not accept a number (`text: 5` fails; use `String(5)`); a `bool` prop does not accept `"true"`; integers must be whole numbers; enums must match exactly. |
| `null` | A `null` prop is treated as not provided (default applies, or "missing required"). The only prop that accepts `null` as a value is `progress.value` (indeterminate). `null` inside an array item field is likewise treated as missing. |
| Children | Only containers (`stack`, `scroll`, `card`, `section`) accept children. |
| `key` | Optional on every node, not part of `p` after validation. A string must be non-empty and must not contain `/`; a number must be an integer. Keys must be unique among siblings. |
| Node ids | `root` for the root, `<parent id>/<key>` for a child (its index if it has no key). |
| Table rows / list items | `key` is **required**, must be a non-empty **string** (a numeric key is rejected) and unique within the array. |
| Array items | Unknown fields are rejected, except table `rows`, which carry arbitrary cell values next to `key`. |
| Limits | 5,000 nodes in total, depth 64, 10,000 table rows, 5,000 list items, 1 MiB for an image `src`, 1 MiB for a single frame (larger updates are sent as a full replacement). |

Use stable keys for anything that fires events, for table rows and for list items: they are what makes updates incremental and keeps focus and scroll position.

A tree that violates a rule is not shown. After the first frame the view displays the message in an error bar; the message names the node and the problem, for example:

```
invalid view node root/tasks: <table> prop "rows"[0].key: expected a string, got a number
invalid view node root/toolbar: duplicate sibling id: another child of root already uses it (keys must be unique; a child without key uses its position as key)
```

### Shared values

| Enum | Values |
|---|---|
| tone | `neutral`, `accent`, `success`, `warning`, `danger`, `info`, `muted` |
| gap (also `padding`) | `none`, `xs`, `sm`, `md`, `lg`, `xl` |
| size | `xs`, `sm`, `md`, `lg`, `xl` |
| direction | `row`, `column` |
| align | `start`, `center`, `end`, `stretch` |
| justify | `start`, `center`, `end`, `between` |
| weight | `normal`, `medium`, `semibold` |
| button variant | `primary`, `outline`, `ghost`, `danger` |
| input kind | `text`, `password`, `number` |
| column kind | `text`, `bytes`, `speed`, `percent`, `progress`, `time`, `badge` |

Tones follow the active theme: `neutral` → `colors.foreground`, `accent` → `colors.primary`, `success` → `colors.success`, `warning` → `colors.warning`, `danger` → `colors.destructive`, `info` → `colors.info`, `muted` → `colors.mutedForeground`. An unknown tone is not valid; there is no way to set a raw color from a portable view.

### Layout and containers

| Kind | Props (default) | Children |
|---|---|---|
| `stack` | `direction` (column), `gap` (sm), `align` (stretch), `justify` (start), `wrap` (false), `padding` (none), `grow` (false) | yes |
| `scroll` | `maxHeight` number ≥ 0 (unlimited) | yes |
| `card` | `title`, `padding` (md) | yes |
| `section` | `title` **required**, `description` | yes |
| `divider` | — | no |

### Content

| Kind | Props (default) | Notes |
|---|---|---|
| `text` | `text` **required**, `tone` (neutral), `size` (sm), `weight` (normal), `mono` (false), `truncate` (false) | One run of text. |
| `heading` | `text` **required**, `level` integer 1–3 (2) | |
| `markdown` | `text` **required** | Paragraphs and code blocks only. |
| `code` | `text` **required** | Monospace block. |
| `link` | `text` **required**, `href` **required** | `href` must be a well-formed `http://` or `https://` URL without whitespace. |
| `image` | `src` **required**, `alt` (""), `width` ≥ 0, `height` ≥ 0 | `src` may be a PNG/JPEG/WebP base64 data URI (non-empty payload, at most 1 MiB) or a `flux-image:<uuid>` session resource returned by `ctx.images.load`. Arbitrary remote URLs are not accepted. |
| `badge` | `text` **required**, `tone` (neutral) | |
| `progress` | `value` number 0–1 or `null` (`null`), `label` | `null` shows an indeterminate bar. |
| `spinner` | `label` | |
| `kv` | `items` array (`[]`) | Each item: `key` **string, required**, `value` **string, required**. |
| `callout` | `text` **required**, `tone` (info), `title` | |
| `empty` | `title` **required**, `description` | |

### Controls

| Kind | Props (default) | Events |
|---|---|---|
| `button` | `text` **required**, `variant` (outline), `disabled` (false), `loading` (false) | `press` |
| `switch` | `label`, `checked` (false), `disabled` (false) | `change` — `ev.value` is a boolean |
| `checkbox` | `label`, `checked` (false), `disabled` (false) | `change` — boolean |
| `select` | `options` **required**, `value`, `placeholder`, `disabled` (false) | `change` — `ev.value` is the chosen option's `value` |
| `input` | `value` (""), `placeholder`, `kind` (text), `disabled` (false) | `change` (debounced, see below), `submit` |
| `textarea` | `value` (""), `placeholder`, `rows` integer 1–50 (4), `disabled` (false) | `change` (debounced) |
| `tabs` | `items` **required**, `value` **required** | `change` — the chosen item's `value` |

`select.options` and `tabs.items` entries: `value` **string, required**, `label` **string, required**. `tabs` only draws the tab strip; you render the content for the selected `value` yourself.

### Data

| Kind | Props (default) | Events |
|---|---|---|
| `table` | `columns` **required**, `rows` (`[]`, ≤ 10,000), `rowActions`, `emptyText` | `rowAction` (`ev.key`, `ev.action`), `select` (`ev.key`) |
| `list` | `items` (`[]`, ≤ 5,000) | `select` (`ev.key`) |

- `table.columns[]`: `key` **string, required**, `title` **string, required**, `kind` (text), `width` number ≥ 0.
- `table.rows[]`: `key` **non-empty string, required, unique**; every other field is a cell value looked up by the column's `key`.
- `table.rowActions[]`: `id` **string, required**, `label` **string, required**, `tone`.
- `list.items[]`: `key` **non-empty string, required, unique**, `title` **string, required**, `description`, `badge` (string), `tone`.

Column `kind` decides how a cell value is formatted: `text` (string), `bytes` (byte count), `speed` (bytes per second), `percent` (a number from 0 to 1), `progress` (a number from 0 to 1 drawn as a bar), `time` (unix milliseconds, shown in the viewer's local time zone), `badge` (a string, or `{ text, tone }`).

## Controlled values and text input

Every control is **controlled**: the props you render are the source of truth, and an event is only a *request* to change. Store the value from the event in your state and render it back on the next frame.

- `switch`, `checkbox` and `tabs` display the `checked` / `value` you rendered in both clients; if the handler does not update the state, the control snaps back to the old value.
- `select` should be treated the same way. (The Web UI snaps back; the desktop app may keep the user's pick on screen until your `value` changes, so do not rely on either behaviour.)
- `input` and `textarea` keep what the user types while they are typing: a re-render never overwrites a focused field, nor a value equal to the last one the user sent. Frame values are adopted once the field loses focus.
- `change` from `input` / `textarea` is **debounced by 300 ms**, and is sent immediately when the field loses focus.
- Pressing Enter in an `input` first flushes any pending `change`, then sends `submit`. So a handler always sees the `change` with the final text before `submit`; `submit`'s `ev.value` is the text at that moment.
- `input` with `kind: "number"` still delivers `ev.value` as a **string**.
- `textarea` has no `submit` event.

## Commands

A command is declared in `contributes.commands` and implemented with `flux.commands.register(id, handler)`.

| Flag | Where it appears | `taskIds` |
|---|---|---|
| `palette: true` | Desktop: command palette, titled `<Plugin name>: <command title>`. Web: a button in the header of the plugin's activity page. | Desktop: the task ids currently selected on the Downloads page (empty if none). Web: always `[]`. |
| `taskMenu: true` | The task context menu on the Downloads page (desktop and Web). | The selected task ids. |

The handler runs inside `main` like any other call. If it throws, the client shows an error notification; a successful command shows nothing unless you call `flux.ui.toast`.

## Opening, closing and reopening

- Desktop `activity` pages are created the first time the page is opened and cached for the life of the main window. The Web UI serves them at `/plugins/<identity>/<viewId>` (both segments URL-encoded).
- Each open view has a daemon-side session. It ends when the client closes the view, when keep-alives stop for 90 seconds (reason `expired`), or when the plugin's host is discarded (reason `pluginUnloaded`). Clients show "The view session has ended" with **Reopen**; reopening creates a new session and runs `init` again, so state is lost.
- After a daemon restart or a lost connection, clients first try to re-sync the session; if the daemon no longer knows it, they open a new one.
- Opening fails with an error page when the plugin is missing (`plugin not found: <identity>`), disabled (`plugin disabled: <identity>`), has no `main` (`plugin has no main: <identity>`), the view is not declared (`view not declared: <id>`) or is desktop-only (`view is desktop-only: <id>`).

## Desktop implementations

A view may declare `desktop` (permission `desktop`): an ES module that the desktop app runs in-process for a richer, fully native UI. Desktop scripts run with **full trust**; read [Permissions & audit](/docs/en/plugins/permissions/) before shipping one, and [Native desktop UI](/docs/en/plugins/desktop-ui-guide/) for the walkthrough.

| Declaration | Desktop app | Web UI |
|---|---|---|
| `desktop` only | Desktop script | Not shown |
| `desktop` + `portable: true` | Desktop script (or the portable view when the script file is not readable) | Portable view from `main` |
| no `desktop` | Portable view | Portable view |

The `fluxdown` module is available inside desktop scripts only: `identity()`, `taskId()`, `call(method, params)`, `invoke(name, args)`, `theme()`, `locale()` and `toast(level, text)`. It is documented in [Native desktop UI](/docs/en/plugins/desktop-ui-guide/#the-fluxdown-module).

### Fallback rules

For each view the desktop app picks an implementation when the view is created:

1. If the view has `desktop` and the script file exists at the package path **on this machine**, the desktop script is used. If it then fails to load or throws while mounting, an error panel is shown; there is no automatic fallback to the portable view.
2. Otherwise (no `desktop`, or the package is not readable, for example when the desktop app is connected to a daemon on another machine) the portable view is used if the view is portable.
3. Otherwise a "this view is only available on the desktop" notice is shown.
