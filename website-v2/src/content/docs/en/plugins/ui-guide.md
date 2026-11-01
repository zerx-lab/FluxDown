---
title: Build a Portable UI
description: A step-by-step tutorial for plugin UI that runs on the desktop app and the Web UI — the resident main script, views, events, forms, live task data, commands, settings pages and task-detail tabs.
section: plugins
order: 7
---

A plugin can show real UI without shipping any UI code. Its resident **`main` script** returns a tree of plain data nodes (`stack`, `button`, `table`, …); the desktop app and the Web UI render that tree with their own native controls and the active theme. Because the tree is data, one implementation works in every client that renders plugin views. This guide builds such a UI step by step and ends with a complete plugin: a live task table, a quick-add form, two commands, a settings page and a task-detail tab.

This is the **portable** route. If you need fully native desktop widgets, windows or access to the desktop's resources, read [Build a desktop-native UI](/docs/en/plugins/desktop-ui-guide/) afterwards — a view can have both. For the complete list of node kinds and their props, keep the [Views reference](/docs/en/plugins/views/) open. To write the same code in TypeScript and JSX, see [TypeScript project](/docs/en/plugins/typescript-project/).

**You need:** FluxDown with plugin dev mode (see [Your first plugin](/docs/en/plugins/your-first-plugin/) for installing a folder as a dev-mode plugin) and a text editor. The examples are plain JavaScript; no build step.

## 1. How the `main` script runs

Read this section once; everything else builds on it.

**One script, one long-lived VM.** `main.js` is a *classic script* (no `import` / `export`). The daemon evaluates it once when the plugin's host starts. While it is evaluated it registers views, commands, RPC handlers, event listeners and timers through `flux.*`. Top-level variables then stay alive for as long as the host lives, and they are shared by every view and command of the plugin.

**Two kinds of state.**

| State | Where it lives | Lifetime |
|---|---|---|
| Session state | The object returned by a view's `init` and passed to `render` / `event` | One open view. Discarded when the view closes — and the Web UI closes a view whenever you navigate away from it. |
| Host state | Variables at the top level of `main.js` | The host's lifetime (see below). Lost when the host is released. |

Anything that must survive a host restart belongs in `flux.storage` (see the [API reference](/docs/en/plugins/api-reference/)) or in the plugin's settings.

**Everything runs one call at a time.** The host is a single actor. Opening a view, a UI event, a refresh, a command, a timer tick and a download event are queued and each runs to completion before the next starts. Two consequences:

- A slow `await` (a `flux.fetch`, a long task loop) delays *every* view and command of that plugin. Keep handlers short.
- A re-render you request from inside a handler (with `ctx.refresh()`) happens **after** that handler returns. You cannot show an intermediate "loading" frame from the same handler.

**When it starts and stops.**

- The host starts on demand: when a view is opened, a command runs, an event listed in `main.events` arrives, or a desktop view calls `fluxdown.invoke`.
- A normal host is released after **five idle minutes** — no open view session, no pending timer and no call in progress. All host state is lost; the next use evaluates `main.js` from scratch. An active `setInterval` therefore keeps the host alive for as long as it exists.
- With `"main": { "background": true }` (requires the `background` permission) the host starts with the daemon and is never reclaimed for being idle.

**Budgets.** Each call — evaluating `main.js`, a render, an event, a command, a timer tick — may use 30 seconds of wall-clock time, 2 seconds of CPU time (time spent waiting for `flux.fetch`, `flux.tasks.*` and other host calls is not counted as CPU) and 64 MiB of memory. If a call exceeds a limit:

1. The host is discarded and all JS state with it.
2. Every open view of the plugin ends; clients show "The view session has ended" with a **Reopen** button. The failing call reports `脚本执行超时` (“script execution timed out”) or `脚本超出内存上限` (“script exceeded the memory limit”).
3. The failure counts towards the circuit breaker: three consecutive failures disable the plugin automatically. A successful call resets the count.

A view session also ends if its client stops sending keep-alives for 90 seconds (for example when a browser tab is suspended); the client then offers **Reopen** as well.

**Reloading.** After you change `manifest.json` or `main.js` of a dev-mode plugin, press the **Reload** button on its row in Settings → Extensions → Plugins (tooltip “Reload (re-read manifest and scripts)”). If the manifest or `main.js` changed, the running host is replaced: open views end and must be reopened.

## 2. Your first view

A view needs two things: a declaration in the manifest and a `flux.ui.view` registration in `main.js` with the same id.

`manifest.json`:

```json
{
  "identity": "example@hello-ui",
  "name": "Hello UI",
  "version": "0.1.0",
  "main": { "entry": "main.js" },
  "contributes": {
    "views": [{ "id": "hello", "title": "Hello", "location": "activity" }]
  }
}
```

`main.js`:

```js
const { h } = flux.ui;

flux.ui.view("hello", {
  render: (state, ctx) =>
    h("stack", { gap: "md", padding: "md" },
      h("heading", { text: "Hello from main.js" }),
      h("text", { text: `Session ${ctx.session}`, tone: "muted", size: "xs" })),
});
```

Put both files in a folder, install the folder as a dev-mode plugin and approve the permission dialog. A **Hello** entry appears in the activity bar (see [section 9](#9-where-things-appear) for where each location shows up). Opening it starts the host, which opens a session and calls your `render`.

What the pieces mean:

- `view(id, definition)` — `id` must be the `id` of a view declared in `contributes.views` (lowercase letters, digits and `-`, at most 32 characters, starting with a letter). The definition has `render` (required) and optional `init`, `event` and `dispose`. All of them may be `async`.
- `init(ctx)` builds the session's initial state; the default is `{}`. `render(state, ctx)` turns the state into a tree. `dispose(state)` runs when the session closes.
- `h(kind, props, ...children)` builds one node. It is shorthand for `{ k: kind, p: props, c: [children] }`. Children are flattened, and `null`, `undefined`, `true` and `false` are dropped, so `cond ? node : null` and `list.map(...)` both work. A function as the first argument is called with `{ ...props, children }`, which lets you write your own components as plain functions.
- The root must be a node, not `undefined`. If `render` returns nothing the view fails with `render() returned no node`.

The `ctx` object passed to `init`, `render`, `event` is:

| Field | Meaning |
|---|---|
| `session` | Unique id of this open view session. |
| `taskId` | The task a `taskDetail` view is attached to; `null` for every other location. |
| `locale` | The client's UI language when the view was opened. It is not updated if the user switches language later. |
| `props` | Reserved for opening a view with parameters. The current clients always pass `{}`. |
| `refresh()` | Asks for a re-render of this session. Requests are merged; a session renders at most once every 100 ms. |
| `close()` | Closes the dialog or stand-alone window hosting this view and ends the session. A view embedded in a page (activity page, task-detail tab) just shows "ended" with a **Reopen** button. Idempotent; may be called inside `init`. |
| `navigate(target)` | `"downloads"`, `"rss"`, `"webhooks"` or `{ view }` (an `activity` view of this plugin). Brings the main window to the front and switches page; returns a promise that rejects on an invalid target or when called after `close()`. May be called inside `init`. Ignored (with a logged warning) by a client that cannot show the page. Only the client that opened the view is affected. Example: after `await flux.tasks.create(...)`, call `await ctx.navigate("downloads")`. |

> **Children are nodes, never strings.** `h("stack", {}, "hello")` is invalid. Put text in a `text` node: `h("text", { text: "hello" })`. Props that expect a string must be strings too: `text: 5` is rejected, use `String(5)`.

### TypeScript and JSX

The same view in TSX with [`fluxdown-plugin-sdk`](/docs/en/plugins/typescript-project/):

```tsx
import { defineMain } from "fluxdown-plugin-sdk/runtime";

defineMain((flux) => {
  flux.ui.view<{ clicks: number }>("hello", {
    init: () => ({ clicks: 0 }),
    render: (state) => (
      <stack gap="md" padding="md">
        <heading text="Hello" />
        <button key="inc" text={`Clicked ${state.clicks} times`} variant="primary" />
      </stack>
    ),
    event: (state, ev) => {
      if (ev.type === "press") state.clicks += 1;
    },
  });
});
```

Every lowercase tag is a node kind, `key` becomes the node key, and `null` / `false` / `undefined` children are dropped. Text is still a prop — `<text text="…" />` — not JSX text children. Props and event shapes are type-checked against the same schema the daemon enforces. Everything below applies unchanged.

## 3. Handling events

Interactive nodes send **events** to the view's `event(state, ev, ctx)`. After `event` returns, the host calls `render` again and sends the clients only the *difference*. The snippets in sections 3 and 4 assume `const { h } = flux.ui;` from section 2 and replace the `hello` view; try each one in turn.

```js
flux.ui.view("hello", {
  init: () => ({ clicks: 0 }),
  render: (state) =>
    h("stack", { gap: "md", padding: "md" },
      h("button", { key: "inc", text: `Clicked ${state.clicks} times`, variant: "primary" }),
      h("button", { key: "reset", text: "Reset" })),
  event: (state, ev) => {
    if (ev.type !== "press") return;
    if (ev.node === "root/inc") state.clicks += 1;
    if (ev.node === "root/reset") state.clicks = 0;
  },
});
```

`ev` has these fields:

| Field | Present on | Meaning |
|---|---|---|
| `node` | all | Id of the node that fired. |
| `type` | all | `press`, `change`, `submit`, `rowAction` or `select`. |
| `value` | `change`, `submit` | The new value: a string, or a boolean for `switch` / `checkbox`. |
| `key` | `rowAction`, `select` | The row or item `key`. |
| `action` | `rowAction` | The `id` of the clicked row action. |

| Node | Events |
|---|---|
| `button` | `press` |
| `switch`, `checkbox` | `change` with a boolean `value` |
| `select`, `tabs` | `change` with a string `value` |
| `input` | `change` (debounced), `submit` (Enter) with a string `value` |
| `textarea` | `change` (debounced) with a string `value` |
| `table` | `rowAction` (`key`, `action`), `select` (`key`: row clicked) |
| `list` | `select` (`key`) |

**Node ids and keys.** The root node has the id `root`. A child's id is `<parent id>/<key>`, or `<parent id>/<index>` when it has no `key`. Siblings must have distinct keys. Two rules follow:

- Give a `key` to every node that fires events, and to every node that appears conditionally. `null` children are removed *before* positions are counted, so an unkeyed node's id shifts when a sibling before it appears or disappears.
- The full id depends on nesting (`root/add/form-0/url`), so don't compare whole ids in larger views. Dispatch on the last segment instead, and keep interactive keys unique within the view:

```js
const keyOf = (ev) => ev.node.slice(ev.node.lastIndexOf("/") + 1);

event: async (state, ev) => {
  switch (`${ev.type}:${keyOf(ev)}`) {
    case "press:save": /* … */ return;
    case "change:name": state.name = ev.value; return;
    case "rowAction:tasks": /* ev.key, ev.action */ return;
  }
},
```

**What `event` may return.** Mutate `state` and return nothing (the usual style). If you return a value other than `undefined`, it *replaces* the state — so an arrow function like `(state, ev) => state.count += 1`, which returns a number, silently replaces your state with a number.

**Errors.** If `event` or `render` throws, or the tree breaks the schema, the client shows an error bar above the **last valid tree** and the session stays open; the next successful render clears it. Changes you made to `state` before the exception are kept. If the very first render fails, the view cannot open: the client shows the error with a **Reopen** button.

**Other ways to re-render.** The host re-renders a view automatically after its own `event`. For changes that don't come from a user event — a timer, a command, a download event — call `ctx.refresh()` on the contexts you kept (section 5).

## 4. Forms

Inputs are **controlled**: the client shows what the latest tree says. You store each value in `state` from its `change` event and render it back.

```js
flux.ui.view("hello", {
  init: () => ({ name: "", style: "plain", loud: false, result: "" }),
  render: (state) =>
    h("stack", { gap: "sm", padding: "md" },
      h("input", { key: "name", value: state.name, placeholder: "Your name" }),
      h("select", {
        key: "style",
        value: state.style,
        options: [{ value: "plain", label: "Plain" }, { value: "formal", label: "Formal" }],
      }),
      h("switch", { key: "loud", label: "Shout", checked: state.loud }),
      h("button", { key: "greet", text: "Greet", variant: "primary" }),
      state.result ? h("callout", { key: "result", tone: "success", text: state.result }) : null),
  event: (state, ev) => {
    switch (`${ev.type}:${ev.node.split("/").pop()}`) {
      case "change:name": state.name = ev.value; return;
      case "change:style": state.style = ev.value; return;
      case "change:loud": state.loud = ev.value; return;
      case "submit:name": // Enter in the input
      case "press:greet": {
        const hello = state.style === "formal" ? "Good day" : "Hello";
        const text = `${hello}, ${state.name || "stranger"}!`;
        state.result = state.loud ? text.toUpperCase() : text;
        return;
      }
    }
  },
});
```

How each control behaves:

| Control | Behavior |
|---|---|
| `switch`, `checkbox`, `select`, `tabs` | Show exactly the `checked` / `value` of the latest tree. If you don't store the new value and render it back, the control snaps back to the old one. |
| `input`, `textarea` | The user's typing appears immediately. A `change` event is sent 300 ms after the last keystroke, and immediately when the field loses focus. `input` also sends `submit` on Enter, always preceded by a flushed `change`. |
| `input` with `kind: "number"` | The `value` is still a **string**; convert it yourself. |
| All | A `disabled: true` prop greys the control out. `button` also has `loading: true`. |

**Focused inputs are not overwritten.** While a field has focus, or while its last typed value is still waiting for your render, a different `value` in a new tree is ignored; it is applied after the field loses focus. To clear or reset a field from code, change the `key` of the field or of a wrapper around it — the client then builds a fresh control. The final plugin does this after a successful submit with a form counter in the wrapper's key:

```js
h("stack", { key: `form-${state.formVersion}`, gap: "sm" },
  h("input", { key: "url", value: state.url }),
  /* … */)
// after a successful submit:
state.url = "";
state.formVersion += 1;
```

**Validate in the handler** and show problems with a `callout` node. Never trust a value just because the control restricts it.

## 5. Live data: tasks, events and timers

### Read and control downloads

`flux.tasks` exists only when the manifest has `"permissions": ["tasks"]`. Without it `flux.tasks` is `undefined` and the first access throws a `TypeError` in your code. Users must approve the permission when they install the plugin; until then the plugin's views and commands do not appear anywhere.

| Call | Result |
|---|---|
| `flux.tasks.list()` | Array of task objects. |
| `flux.tasks.get(id)` | One task object. |
| `flux.tasks.create({ request: { url, startPaused? } })` | `{ taskId }`. At most 20 creations per minute per plugin; beyond that it throws `tasks.create rate limited`. |
| `flux.tasks.pause(id)`, `flux.tasks.resume(id)` | Pause or resume. |
| `flux.tasks.remove(id, deleteFiles)` | Remove the task; `deleteFiles` defaults to `false`. |

A task object (camelCase) contains, among others:

| Field | Type | Meaning |
|---|---|---|
| `taskId` | string | Task id — use it as the row `key`. |
| `url`, `fileName`, `saveDir` | string | Source, file name and target folder. |
| `status` | number | `0` pending, `1` downloading, `2` paused, `3` completed, `4` failed, `5` preparing. |
| `downloadedBytes`, `totalBytes` | number | Progress. `totalBytes` is not positive while the size is unknown. |
| `errorMessage` | string | Failure reason; empty otherwise. |
| `createdAt` | string | Creation time as Unix **seconds**, in a string. |

Creating, pausing, resuming and removing tasks are recorded in the plugin's audit log; see [Permissions & audit](/docs/en/plugins/permissions/).

### A table with row actions

A `table` takes `columns`, `rows` and optional `rowActions`. A row is an object with a **string** `key` (unique, non-empty) plus one value per column `key`. Column `kind` formats the value: `bytes`, `speed`, `percent` and `progress` take numbers (the last two 0–1), `time` takes Unix **milliseconds**, `badge` takes a string or `{ text, tone }`, and `text` (the default) shows any value.

```js
const STATUS = {
  0: { text: "Pending", tone: "muted" },
  1: { text: "Downloading", tone: "accent" },
  2: { text: "Paused", tone: "warning" },
  3: { text: "Completed", tone: "success" },
  4: { text: "Failed", tone: "danger" },
  5: { text: "Preparing", tone: "info" },
};

const toRow = (t) => ({
  key: t.taskId,
  name: t.fileName || t.url,
  size: t.totalBytes > 0 ? t.totalBytes : null,
  progress: t.totalBytes > 0 ? Math.min(1, t.downloadedBytes / t.totalBytes) : null,
  status: STATUS[t.status] || { text: `Status ${t.status}`, tone: "neutral" },
  created: (Number(t.createdAt) || 0) * 1000,
});

h("table", {
  key: "tasks",
  columns: [
    { key: "name", title: "Name" },
    { key: "size", title: "Size", kind: "bytes", width: 100 },
    { key: "progress", title: "Progress", kind: "progress", width: 140 },
    { key: "status", title: "Status", kind: "badge", width: 120 },
    { key: "created", title: "Added", kind: "time", width: 170 },
  ],
  rows: tasks.slice(0, 500).map(toRow),
  rowActions: [
    { id: "pause", label: "Pause" },
    { id: "resume", label: "Resume", tone: "success" },
    { id: "remove", label: "Remove", tone: "danger" },
  ],
  emptyText: "No tasks match this filter.",
})
```

A click on a row action arrives as `{ type: "rowAction", key: <row key>, action: <action id> }`; a click on the row itself as `{ type: "select", key }`. A table holds at most 10,000 rows and a whole tree at most 5,000 nodes, so cap long lists.

### Keeping the data fresh

The daemon pushes no progress events to plugins, so there are three sources of change:

1. **User events** — your `event` handler updates the data; the re-render is automatic.
2. **Download events** — list them in `main.events` (`onStart`, `onDone`, `onError`, `onCancel`, `onMetaProbed`) and listen with `flux.events.on(name, handler)`. They fire for every task. The host re-renders all open views after your handler returns. An event that is not listed in `main.events` is never delivered, and nothing reports the mistake.
3. **Timers** — for progress, poll. Timers never re-render by themselves; call `ctx.refresh()` on the sessions that should update.

```js
const sessions = new Map(); // session id -> ctx
let pollTimer = null;

function refreshAll() {
  for (const ctx of sessions.values()) ctx.refresh();
}

function startPolling(seconds) {
  if (pollTimer !== null) return;
  pollTimer = flux.timers.setInterval(async () => {
    await loadTasks();
    refreshAll();
  }, seconds * 1000);
}

function stopPolling() {
  if (pollTimer === null) return;
  flux.timers.clear(pollTimer);
  pollTimer = null;
}

// in the view definition:
//   init(ctx)      { sessions.set(ctx.session, ctx); startPolling(3); … return { session: ctx.session, … }; }
//   dispose(state) { sessions.delete(state.session); if (sessions.size === 0) stopPolling(); }
```

`tasks` and `loadTasks()` in these snippets are a host-level variable and a helper that wraps `flux.tasks.list()`; both are defined in the complete plugin at the end.

Timer rules:

| API | Behavior |
|---|---|
| `flux.timers.setTimeout(fn, ms)` | One-shot; returns a numeric id. Not clamped. |
| `flux.timers.setInterval(fn, ms)` | Repeats. Clamped to at least **1 s** while the plugin has at least one open view session, otherwise at least **60 s**. The clamp is re-evaluated each time the timer re-arms; the first clamp is logged once. |
| `flux.timers.clear(id)` | Cancels either kind. |
| Limits | At most 64 live timers per plugin. The callback may be `async`; an exception in it is logged and does not stop an interval. |

Polling only while a view is open (start in `init`, stop in `dispose`) lets the host go idle, and be reclaimed, after the user leaves.

## 6. Commands

Commands appear in the command palette and in the task context menu, and run code in `main`.

Manifest (requires `main`; at most 32 commands, titles 1–40 characters, ids like view ids):

```json
"contributes": {
  "commands": [
    { "id": "pause-selected", "title": "Pause selected tasks", "palette": true, "taskMenu": true },
    { "id": "refresh-board", "title": "Refresh task board", "palette": true }
  ]
}
```

Handler:

```js
flux.commands.register("pause-selected", async ({ taskIds }) => {
  if (taskIds.length === 0) {
    flux.ui.toast("warning", "Select at least one task first.");
    return;
  }
  let paused = 0;
  for (const id of taskIds) {
    try {
      await flux.tasks.pause(id);
      paused += 1;
    } catch (e) {
      flux.logger.warn(`pause ${id} failed: ${e.message}`);
    }
  }
  flux.ui.toast(paused === taskIds.length ? "success" : "warning", `Paused ${paused} of ${taskIds.length} task(s).`);
});
```

- The handler receives `{ taskIds }` — an array of strings. **Always handle an empty array.**
  - *Task context menu* (`taskMenu: true`): the selected local tasks.
  - *Desktop command palette* (`palette: true`): the tasks currently selected on the main window's download page; the palette entry is titled `<plugin name>: <command title>`.
  - *Web* has no palette: `palette` commands become buttons in the header of the plugin's activity page, and run with `taskIds` set to `[]`. A plugin whose only UI is a `palette` command therefore has no Web entry point without an activity view.
- If the handler throws, the client shows the error message in an error toast. Return normally on success.
- `flux.ui.toast(level, text)` shows a notification in every connected desktop and Web client. `level` is `info`, `success`, `warning` or `error`; anything else counts as `info`.
- A command declared in the manifest but not registered fails with `command not registered: <id>`.

## 7. A settings page

Settings have two layers that you can combine:

- `settings[]` in the manifest declares typed fields (see [Manifest reference](/docs/en/plugins/manifest/)). FluxDown generates a form from it — the **Basic settings** tab — and exposes the values as `flux.settings.<key>`, typed: strings, numbers and booleans.
- A view with `"location": "settings"` (at most one per plugin) adds a **Plugin settings** tab next to it, rendered by your `main`. Use it for custom layouts, validation, previews or actions.

```json
"contributes": {
  "views": [{ "id": "preferences", "title": "Preferences", "location": "settings" }]
},
"settings": [
  { "key": "refreshSeconds", "title": "Refresh interval (seconds)", "type": "number", "min": 1, "max": 60, "default": "3" },
  { "key": "notifyOnDone", "title": "Notify when a download finishes", "type": "boolean", "default": "true" }
]
```

Declare at least one `settings[]` field: `flux.settings.update` only accepts declared keys, and the Web UI shows the settings button on the plugin row only for plugins that declare fields. The dialog is reached from **Settings → Extensions → Plugins** with the gear button on the plugin's row.

Write values with `flux.settings.update(entries)`:

```js
async function savePreferences(state) {
  try {
    await flux.settings.update({
      refreshSeconds: state.refreshSeconds.trim(), // values must be strings,
      notifyOnDone: String(state.notifyOnDone),    // even for numbers and booleans
    });
    state.notice = { tone: "success", text: "Saved." };
  } catch (e) {
    state.notice = { tone: "danger", text: e.message };
  }
}
```

- Values must be **strings**: `"5"`, `"true"`. A non-string value makes the call fail.
- The call validates every entry against the manifest rules (type, `min` / `max`, `select` options, `pattern`) and is **all-or-nothing**: one bad entry rejects the whole batch and writes nothing. The error message is the original engine text, for example `'refreshSeconds' 大于上限 60` (“'refreshSeconds' is above the upper limit 60”) or `未知设置项 'x'` (“unknown setting 'x'”). Show it to the user.
- A saved value reaches `flux.settings` once the current call has finished. Reading `flux.settings.x` right after the `await`, in the same handler, still returns the old value: keep the value you just saved in your own state, or pass it along explicitly (the final plugin does this when it restarts its poll timer).
- Changes the user makes in the **Basic settings** tab also update `flux.settings` in place, but they do not re-render open views by themselves.

## 8. A task-detail tab

A view with `"location": "taskDetail"` becomes a tab in the task detail panel. The view is opened for the selected task, and `ctx.taskId` is that task's id.

```json
{ "id": "detail", "title": "Board", "location": "taskDetail" }
```

```js
flux.ui.view("detail", {
  async init(ctx) {
    track(ctx); // register for polling, see section 5
    await loadTasks();
    return { session: ctx.session };
  },
  render(state, ctx) {
    const task = tasks.find((t) => t.taskId === ctx.taskId);
    if (!task) return h("empty", { title: "Task not found", description: "It may have been removed." });
    return h("stack", { gap: "sm", padding: "md" },
      h("heading", { text: task.fileName || "Unnamed task", level: 3 }),
      h("button", { key: "pause", text: "Pause" }));
  },
  async event(state, ev, ctx) {
    if (ev.type === "press" && ctx.taskId) await flux.tasks.pause(ctx.taskId);
  },
  dispose(state) {
    untrack(state.session);
  },
});
```

Always handle the task being gone: render an `empty` node, not `undefined`. The complete plugin below uses the shared task list; `flux.tasks.get(ctx.taskId)` is the alternative when you want a single fresh read.

## 9. Where things appear

| Contribution | Desktop app (GPUI) | Web UI | Mobile apps |
|---|---|---|---|
| `activity` view | An entry in the activity bar, titled with the view title; its icon is the SVG declared by the manifest `icon` (the default icon when not declared). | An entry in the activity bar, with the same `icon` SVG (default icon when not declared); the page lives at `/plugins/<identity>/<viewId>` (each segment URL-encoded, so `@` becomes `%40`). The page header shows the plugin name, the view title and the `palette` commands as buttons. | Not rendered |
| `taskDetail` view | A tab in the task detail panel and in the stand-alone task detail window. | A tab in the task detail panel. | Not rendered |
| `settings` view | **Plugin settings** tab in the plugin's settings dialog. | Same. | Not rendered |
| `palette` command | Command palette, as `<plugin name>: <title>`. | Button in the activity page header. | — |
| `taskMenu` command | Task context menu. | Task context menu. | — |
| `window` view | Desktop implementation only; see [desktop UI](/docs/en/plugins/desktop-ui-guide/). | Not shown. | Not rendered |
| `auth` view | Content of the plugin's **Log in** dialog (see [Custom login UI](/docs/en/plugins/auth-guide/#custom-login-ui)). | Same (portable only). | Not rendered |

Mobile apps keep the protocol but do not render plugin views yet. Contributions only appear for plugins that are enabled, loaded successfully and fully approved by the user.

A view that also declares a `desktop` entry and `"portable": true` uses the desktop implementation where it can run and falls back to your `main` view elsewhere; without `portable: true` the Web UI does not show it at all. See [Build a desktop-native UI](/docs/en/plugins/desktop-ui-guide/).

## 10. Common mistakes

| Symptom | Cause and fix |
|---|---|
| `invalid view node root/0: a node must be an object {k, p, c}` | A string (or number) was passed as a child. Wrap text in a `text` node. |
| `invalid view node root/1: <text> prop "text": expected a string, got a number` | Props are strictly typed; numbers are not converted. Use `String(n)`. This applies to `kv` item values and `list` / `select` labels too. |
| `<table> prop "rows"[0].key: expected a string, got a number` or `key must not be empty` | Row and list item keys must be non-empty strings. Use `String(id)` or the task id. |
| `duplicate sibling id: another child of root already uses it …` or `duplicate key "…"` | Two siblings (or two rows) share a `key`. Make keys unique. |
| `<button> unknown prop "label"; allowed props: …` | Unknown props are rejected. Check the node in the [Views reference](/docs/en/plugins/views/). |
| `<text> missing required prop "text"` | A required prop is missing; the reference marks each one. |
| `render() returned no node` | `render` returned `undefined` / `null` (often a missing `return` in a braced arrow). Return a node, for example `h("empty", …)`. |
| `view not registered: <id>` | The manifest declares the view, but `main.js` never called `flux.ui.view` with that id (or a typo in the id). |
| A view registered in `main.js` never appears | It is not declared in `contributes.views`, or the plugin is not enabled / approved. |
| `TypeError` on `flux.tasks` | The manifest lacks `"permissions": ["tasks"]`. |
| `flux.events.on("onDone", …)` never fires | `onDone` is missing from `main.events`. |
| The button's `loading` state never shows | The handler's `await` finishes before the next render; see section 1. Do the work, then render the result. |
| State suddenly becomes a number or string | `event` returned a value. Use a braced body without `return value`. |
| Cleared input keeps its text | The input still has focus; change its `key` (or a wrapper's) to reset it, section 4. |
| The view shows "The view session has ended" | The host was released: budget exceeded, plugin reloaded or disabled, or keep-alives stopped. Press **Reopen**. See [Debugging](/docs/en/plugins/debugging/) for logs and more messages. |
| Memory grows over time | Contexts kept in module-level collections were never removed. Delete them in `dispose`. |

Use `flux.logger.info(...)` / `warn` / `error` for diagnostics; [Debugging](/docs/en/plugins/debugging/) explains where the log lands.

## The complete plugin

Save both files in one folder, install it as a dev-mode plugin, approve the `tasks` permission, and open **Task Board** from the activity bar. Add a download from the form and watch the table update; use the gear button on the plugin row for **Plugin settings**; open a task's detail panel for the **Board** tab; run **Pause selected tasks** from the command palette or the task context menu.

`manifest.json`:

```json
{
  "identity": "example@taskboard",
  "name": "Task Board",
  "version": "1.0.0",
  "description": "A live task table with a quick-add form, commands and a settings page.",
  "permissions": ["tasks"],
  "main": { "entry": "main.js", "events": ["onDone", "onError"] },
  "contributes": {
    "views": [
      { "id": "board", "title": "Task Board", "location": "activity" },
      { "id": "detail", "title": "Board", "location": "taskDetail" },
      { "id": "preferences", "title": "Preferences", "location": "settings" }
    ],
    "commands": [
      { "id": "pause-selected", "title": "Pause selected tasks", "palette": true, "taskMenu": true },
      { "id": "refresh-board", "title": "Refresh task board", "palette": true }
    ]
  },
  "settings": [
    {
      "key": "refreshSeconds",
      "title": "Refresh interval (seconds)",
      "description": "How often the Task Board reloads while it is open.",
      "type": "number",
      "min": 1,
      "max": 60,
      "default": "3"
    },
    {
      "key": "notifyOnDone",
      "title": "Notify when a download finishes",
      "type": "boolean",
      "default": "true"
    }
  ]
}
```

`main.js`:

```js
// Task Board — resident `main` script (a classic script: no import/export).
const { h } = flux.ui;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------
const MAX_ROWS = 500;

// TaskDto.status codes.
const STATUS = {
  0: { text: "Pending", tone: "muted" },
  1: { text: "Downloading", tone: "accent" },
  2: { text: "Paused", tone: "warning" },
  3: { text: "Completed", tone: "success" },
  4: { text: "Failed", tone: "danger" },
  5: { text: "Preparing", tone: "info" },
};
const FILTER_STATUSES = { all: null, active: [0, 1, 5], paused: [2], done: [3], failed: [4] };
const FILTERS = [
  { value: "all", label: "All" },
  { value: "active", label: "Active" },
  { value: "paused", label: "Paused" },
  { value: "done", label: "Completed" },
  { value: "failed", label: "Failed" },
];
const MODES = [
  { value: "now", label: "Start now" },
  { value: "paused", label: "Add paused" },
];
const COLUMNS = [
  { key: "name", title: "Name" },
  { key: "size", title: "Size", kind: "bytes", width: 100 },
  { key: "progress", title: "Progress", kind: "progress", width: 140 },
  { key: "status", title: "Status", kind: "badge", width: 120 },
  { key: "created", title: "Added", kind: "time", width: 170 },
];
const ROW_ACTIONS = [
  { id: "pause", label: "Pause" },
  { id: "resume", label: "Resume", tone: "success" },
  { id: "remove", label: "Remove", tone: "danger" },
];

// ---------------------------------------------------------------------------
// Host-wide state: shared by every view session, lost when the host is reclaimed
// ---------------------------------------------------------------------------
let tasks = [];
let loadError = "";
const sessions = new Map(); // session id -> ctx of the views that show live data
let pollTimer = null;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
const errorText = (e) => String((e && e.message) || e);
const keyOf = (ev) => ev.node.slice(ev.node.lastIndexOf("/") + 1);

function formatBytes(n) {
  if (!(n > 0)) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

const statusOf = (task) => STATUS[task.status] || { text: `Status ${task.status}`, tone: "neutral" };
const fractionOf = (task) =>
  task.totalBytes > 0 ? Math.min(1, task.downloadedBytes / task.totalBytes) : null;

function countOf(codes) {
  return tasks.filter((t) => codes.includes(t.status)).length;
}

function toRow(task) {
  return {
    key: task.taskId, // must be a non-empty string, unique within the table
    name: task.fileName || task.url,
    size: task.totalBytes > 0 ? task.totalBytes : null,
    progress: fractionOf(task),
    status: statusOf(task), // badge cell: { text, tone }
    created: (Number(task.createdAt) || 0) * 1000, // createdAt is Unix seconds; `time` wants ms
  };
}

async function loadTasks() {
  try {
    tasks = await flux.tasks.list();
    loadError = "";
  } catch (e) {
    loadError = errorText(e);
  }
}

function refreshAll() {
  for (const ctx of sessions.values()) ctx.refresh();
}

// Poll only while a live view is open, so the host can go idle afterwards.
function startPolling(seconds) {
  if (pollTimer !== null) return;
  pollTimer = flux.timers.setInterval(async () => {
    await loadTasks();
    refreshAll();
  }, Math.max(1, seconds || 3) * 1000);
}

function stopPolling() {
  if (pollTimer === null) return;
  flux.timers.clear(pollTimer);
  pollTimer = null;
}

function track(ctx) {
  sessions.set(ctx.session, ctx);
  startPolling(Number(flux.settings.refreshSeconds));
}

function untrack(session) {
  sessions.delete(session);
  if (sessions.size === 0) stopPolling();
}

// ---------------------------------------------------------------------------
// View: "board" (activity bar)
// ---------------------------------------------------------------------------
async function addDownload(state) {
  const url = state.url.trim();
  if (!/^https?:\/\//i.test(url)) {
    state.notice = { tone: "warning", text: "Enter a full http:// or https:// URL." };
    return;
  }
  try {
    const { taskId } = await flux.tasks.create({
      request: { url, startPaused: state.mode === "paused" },
    });
    state.notice = { tone: "success", text: `Added task ${taskId}.` };
    state.url = "";
    state.formVersion += 1; // new key => the client builds a fresh, empty input
    await loadTasks();
  } catch (e) {
    state.notice = { tone: "danger", text: errorText(e) };
  }
}

async function runRowAction(state, taskId, action) {
  try {
    if (action === "pause") await flux.tasks.pause(taskId);
    else if (action === "resume") await flux.tasks.resume(taskId);
    else if (action === "remove") await flux.tasks.remove(taskId, false); // keep the files
    state.notice = { tone: "info", text: `${action}: ${taskId}` };
  } catch (e) {
    state.notice = { tone: "danger", text: errorText(e) };
  }
  await loadTasks();
}

flux.ui.view("board", {
  async init(ctx) {
    track(ctx);
    await loadTasks();
    return { url: "", mode: "now", filter: "all", formVersion: 0, notice: null, session: ctx.session };
  },

  render(state) {
    const allowed = FILTER_STATUSES[state.filter];
    const visible = tasks.filter((t) => allowed === null || allowed.includes(t.status));
    return h(
      "stack",
      { gap: "md", padding: "md" },
      h(
        "stack",
        { key: "summary", direction: "row", gap: "sm", align: "center" },
        h("heading", { text: "Task board", level: 2 }),
        h("badge", { key: "total", text: `${tasks.length} total`, tone: "accent" }),
        h("badge", { key: "active", text: `${countOf(FILTER_STATUSES.active)} active`, tone: "info" }),
      ),
      loadError
        ? h("callout", { key: "load-error", tone: "danger", title: "Could not load tasks", text: loadError })
        : null,
      state.notice ? h("callout", { key: "notice", tone: state.notice.tone, text: state.notice.text }) : null,
      h(
        "card",
        { key: "add", title: "Add a download" },
        h(
          "stack",
          { key: `form-${state.formVersion}`, gap: "sm" },
          h("input", { key: "url", value: state.url, placeholder: "https://example.com/file.zip" }),
          h(
            "stack",
            { direction: "row", gap: "sm", align: "center" },
            h("select", { key: "mode", value: state.mode, options: MODES }),
            h("button", { key: "add", text: "Add download", variant: "primary" }),
          ),
        ),
      ),
      h("tabs", { key: "filter", value: state.filter, items: FILTERS }),
      h("table", {
        key: "tasks",
        columns: COLUMNS,
        rows: visible.slice(0, MAX_ROWS).map(toRow),
        rowActions: ROW_ACTIONS,
        emptyText: "No tasks match this filter.",
      }),
      visible.length > MAX_ROWS
        ? h("text", { key: "more", text: `Showing the first ${MAX_ROWS} of ${visible.length} tasks.`, tone: "muted", size: "xs" })
        : null,
    );
  },

  async event(state, ev) {
    switch (`${ev.type}:${keyOf(ev)}`) {
      case "change:url":
        state.url = ev.value;
        return;
      case "change:mode":
        state.mode = ev.value;
        return;
      case "change:filter":
        state.filter = ev.value;
        return;
      case "submit:url":
        state.url = ev.value;
        await addDownload(state);
        return;
      case "press:add":
        await addDownload(state);
        return;
      case "rowAction:tasks":
        await runRowAction(state, ev.key, ev.action);
        return;
    }
  },

  dispose(state) {
    untrack(state.session);
  },
});

// ---------------------------------------------------------------------------
// View: "detail" (task detail tab; ctx.taskId is the selected task)
// ---------------------------------------------------------------------------
flux.ui.view("detail", {
  async init(ctx) {
    track(ctx);
    await loadTasks();
    return { session: ctx.session };
  },

  render(state, ctx) {
    const task = tasks.find((t) => t.taskId === ctx.taskId);
    if (!task) {
      return h("empty", { title: "Task not found", description: "It may have been removed." });
    }
    const status = statusOf(task);
    const fraction = fractionOf(task);
    return h(
      "stack",
      { gap: "sm", padding: "md" },
      h(
        "stack",
        { direction: "row", gap: "sm", align: "center" },
        h("heading", { text: task.fileName || "Unnamed task", level: 3 }),
        h("badge", { text: status.text, tone: status.tone }),
      ),
      h("progress", {
        value: fraction,
        label: fraction === null ? "Size unknown" : `${Math.round(fraction * 100)}%`,
      }),
      h("kv", {
        items: [
          { key: "Task id", value: task.taskId },
          { key: "URL", value: task.url },
          { key: "Saved to", value: task.saveDir },
          {
            key: "Downloaded",
            value: `${formatBytes(task.downloadedBytes)} of ${task.totalBytes > 0 ? formatBytes(task.totalBytes) : "unknown"}`,
          },
          { key: "Error", value: task.errorMessage || "None" },
        ],
      }),
      h(
        "stack",
        { direction: "row", gap: "sm" },
        h("button", { key: "pause", text: "Pause" }),
        h("button", { key: "resume", text: "Resume", variant: "primary" }),
      ),
    );
  },

  async event(state, ev, ctx) {
    if (ev.type !== "press" || !ctx.taskId) return;
    try {
      if (keyOf(ev) === "pause") await flux.tasks.pause(ctx.taskId);
      if (keyOf(ev) === "resume") await flux.tasks.resume(ctx.taskId);
    } catch (e) {
      flux.ui.toast("error", errorText(e));
    }
    await loadTasks();
  },

  dispose(state) {
    untrack(state.session);
  },
});

// ---------------------------------------------------------------------------
// View: "preferences" (the plugin's settings dialog)
// ---------------------------------------------------------------------------
async function savePreferences(state) {
  try {
    // Values must be strings, even for number and boolean settings.
    await flux.settings.update({
      refreshSeconds: state.refreshSeconds.trim(),
      notifyOnDone: String(state.notifyOnDone),
    });
    state.notice = { tone: "success", text: "Saved." };
    if (pollTimer !== null) {
      // flux.settings still holds the old values until this call returns, so pass the new one.
      stopPolling();
      startPolling(Number(state.refreshSeconds));
    }
  } catch (e) {
    state.notice = { tone: "danger", text: errorText(e) };
  }
}

flux.ui.view("preferences", {
  init() {
    return {
      refreshSeconds: String(flux.settings.refreshSeconds),
      notifyOnDone: flux.settings.notifyOnDone === true,
      notice: null,
    };
  },

  render(state) {
    return h(
      "stack",
      { gap: "md", padding: "md" },
      h("text", { text: "Refresh interval while the board is open (1–60 seconds).", tone: "muted" }),
      h("input", { key: "refresh", value: state.refreshSeconds, placeholder: "3", kind: "number" }),
      h("switch", { key: "notify", label: "Notify when a download finishes", checked: state.notifyOnDone }),
      state.notice ? h("callout", { key: "notice", tone: state.notice.tone, text: state.notice.text }) : null,
      h("button", { key: "save", text: "Save", variant: "primary" }),
    );
  },

  async event(state, ev) {
    switch (`${ev.type}:${keyOf(ev)}`) {
      case "change:refresh":
        state.refreshSeconds = ev.value;
        return;
      case "change:notify":
        state.notifyOnDone = ev.value;
        return;
      case "submit:refresh":
        state.refreshSeconds = ev.value;
        await savePreferences(state);
        return;
      case "press:save":
        await savePreferences(state);
        return;
    }
  },
});

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------
flux.commands.register("pause-selected", async ({ taskIds }) => {
  if (taskIds.length === 0) {
    flux.ui.toast("warning", "Select at least one task first.");
    return;
  }
  let paused = 0;
  for (const id of taskIds) {
    try {
      await flux.tasks.pause(id);
      paused += 1;
    } catch (e) {
      flux.logger.warn(`pause ${id} failed: ${errorText(e)}`);
    }
  }
  flux.ui.toast(paused === taskIds.length ? "success" : "warning", `Paused ${paused} of ${taskIds.length} task(s).`);
  await loadTasks();
  refreshAll();
});

flux.commands.register("refresh-board", async () => {
  await loadTasks();
  refreshAll();
  flux.ui.toast("info", `Loaded ${tasks.length} task(s).`);
});

// ---------------------------------------------------------------------------
// Download lifecycle events (must be listed in manifest main.events)
// ---------------------------------------------------------------------------
flux.events.on("onDone", async (ev) => {
  if (flux.settings.notifyOnDone) flux.ui.toast("success", `Finished: ${ev.filePath}`);
  await loadTasks(); // open views are re-rendered automatically after the handler returns
});

flux.events.on("onError", async (ev) => {
  flux.ui.toast("error", `Download failed: ${ev.message}`);
  await loadTasks();
});
```

## Next steps

- [Views reference](/docs/en/plugins/views/) — every node kind, prop, limit and event.
- [Build a desktop-native UI](/docs/en/plugins/desktop-ui-guide/) — fully native windows and widgets for the desktop app, side by side with your portable views.
- [TypeScript project](/docs/en/plugins/typescript-project/) — the same plugin with types, JSX and a build step.
- [Debugging](/docs/en/plugins/debugging/) — logs, reload behavior and error messages.
- [Manifest reference](/docs/en/plugins/manifest/) and [Permissions & audit](/docs/en/plugins/permissions/).
- The [`feature-demo` example](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo) shows every node kind in one plugin.
