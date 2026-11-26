---
title: Hooks Guide
description: React to download lifecycle events — notify, retry, post-process with ffmpeg, clean up on cancel, and drive yt-dlp.
section: plugins
order: 4
---

Hooks are small scripts that FluxDown runs when something happens to a download: it starts, its metadata is probed, it finishes, it fails, or it is cancelled. A hook cannot change the task, but it can react — send a notification, schedule a retry, convert the finished file with ffmpeg, or release a temporary resource.

This guide explains exactly when each event fires, what a hook is allowed to do, and then builds five complete plugins. The exhaustive signatures and limits live in the [API reference](/docs/en/plugins/api-reference/); the manifest fields in the [manifest reference](/docs/en/plugins/manifest/).

> Hooks run in short-lived, one-shot scripts. If you need long-lived state, views or commands, use the resident `main` script instead (see the [UI guide](/docs/en/plugins/ui-guide/)); it can receive the same events through `main.events`.

## How hooks work

### Declaring hooks

A plugin declares one hooks script and the events it wants:

```json
"hooks": {
  "entry": "hooks.js",
  "events": ["onDone", "onError"],
  "match": { "urls": ["*://files.example.com/*"] }
}
```

| Field | Required | Meaning |
|---|---|---|
| `entry` | yes | Script file, safe relative path. |
| `events` | yes | Non-empty list from `onStart`, `onMetaProbed`, `onDone`, `onError`, `onCancel`. |
| `match.urls` | no | Glob patterns that filter which tasks trigger the hooks. Omitted = every task. When present it must not be empty. |

The script defines one global function per event. In a classic script a top-level `function` declaration (or `globalThis.onDone = …`) is enough. A `const onDone = …` would **not** work, because it does not create a property on `globalThis`, which is where FluxDown looks.

```js
async function onDone(ctx) {
  flux.logger.info("finished", ctx.filePath);
}
```

If an event is listed in `events` but the function is missing, nothing happens and no error is logged.

### Execution model

- **Fresh context every time.** Each event evaluates the whole script in a new QuickJS context. Top-level variables do not survive between events. Persist anything you need with `flux.storage` (key-value) or `flux.fs` (files).
- **Fire-and-forget.** The return value is ignored. Exceptions and timeouts are logged as a warning (`hook onDone 执行失败（已忽略）: …` — “hook onDone failed (ignored)”) and swallowed. Nothing a hook does changes the task, except the two explicit facades `flux.task.requestRetry` (`onError`) and `flux.task.recordArtifact` (`onDone`).
- **Not part of the circuit breaker.** A hook that times out or runs out of memory does not count towards the three-strikes auto-disable that protects resolvers.
- **The event is delivered to every matching plugin**, not only to the plugin whose resolver handled the task. Filter with `hooks.match.urls` or by inspecting `ctx.url`.
- **`ctx.event`** carries the event name in lowercase camel case: `"start"`, `"metaProbed"`, `"done"`, `"error"`, `"cancel"`.
- **Settings.** `flux.settings` is available. `required: true` is **not** enforced for hooks (it is for resolvers, `authenticate` and `subscribe`), so check for missing values yourself. Unset string/number settings without a default are `null`; unset booleans are `false`.

### Budgets and back-pressure

| | Default hooks | `onDone` of a plugin with `ffmpeg`, and every hook of a plugin with `ytdlp` |
|---|---|---|
| Wall-clock limit per call | 5 s | about 30 minutes (1830 s) |
| Memory | 32 MB | 32 MB |
| Script CPU time | up to the 5 s limit | 30 s; time spent *awaiting* `fetch`, ffmpeg or yt-dlp does not count as CPU time |

Concurrency is limited. If every slot of the short-hook pool is busy when an event arrives, the notification is **dropped** and a warning `钩子并发已满，本次钩子被丢弃` (“hook concurrency is full, this hook invocation was dropped”) is logged. Hooks of plugins with `ffmpeg`/`ytdlp` run in a separate pool: they queue (up to 64 waiting) instead of being dropped immediately, and when the queue is full the log says `外部工具钩子排队已满，本次钩子被丢弃` (“the external-tool hook queue is full, this hook invocation was dropped”). Never rely on a hook being delivered exactly once under heavy load; make hooks idempotent.

## When each event fires

| Event | Fires when | Extra `ctx` fields |
|---|---|---|
| `onStart` | A task actually begins downloading for the first time. | — |
| `onMetaProbed` | The background HEAD probe learned a file name and/or size. | `fileName`, `totalBytes` |
| `onDone` | A task completed successfully. | `filePath`, `audioPath`, `muxed` |
| `onError` | A task entered the failed state — **every** time, including failures that will be retried automatically. | `message` |
| `onCancel` | The user closed a selection dialog (choose a plugin / choose a variant); the task was paused. | `reason` |

All events carry `event`, `taskId` and `url`.

### `onStart`

- Fires once when a task leaves the queue and starts, after the plugin resolver (if any) has finished. It does **not** fire again when the task is resumed or retried after an error or pause.
- For tasks handled by a resolver, `ctx.url` is the **resolved direct link**, not the URL the user added. The `hooks.match.urls` filter is applied to that same resolved URL. A hook that matches only the page URL (`*://share.example/*`) therefore never sees `onStart` for tasks resolved to a CDN host. Every other event uses the **original** task URL.
- If the resolver fails, the task never starts and `onStart` does not fire.

### `onMetaProbed`

- Fires after a quick background probe of the task URL finds a file name or size. The probe only runs for ordinary downloads that wait in the queue or are created paused. It is skipped when the task has a size hint from the browser extension, for BitTorrent/magnet tasks, and for **any task that has a resolver** (yours or another plugin's — probing the original page URL is meaningless). Tasks that start immediately are not probed this way.
- Do not use it to learn the final name of every download; use `onDone` for that.

### `onDone`

- Fires after the file is complete on disk. `filePath` is the absolute path; it may contain `/` and `\` mixed on Windows, so split with `/[\\/]/`.
- `audioPath` and `muxed` describe tracks that are downloaded separately (video + audio): `muxed: true` means the host merged them into one file; `muxed: false` with a non-null `audioPath` means merging failed and the audio stream sits in a sidecar file; for ordinary tasks `audioPath` is `null` and `muxed` is `false`.
- It does **not** fire when the user chose to **skip** a download because the file already existed (the host adopts the existing file; it is not a product of the task).
- The only hook with a produced file, hence the only place `flux.ffmpeg`, `flux.ffprobe` and `flux.task.recordArtifact` work.

### `onError`

- Fires for every failure the task reports, including those the host is about to retry on its own. Use `ctx.message` to decide what to do.
- It does not fire when the **resolver** fails (the task shows `[插件] <identity>: <error>` and stops before it starts).
- `flux.task.requestRetry` is only valid here — see [Example 2](#example-2-retry-selectively).

### `onCancel`

`ctx.reason` is always present and is one of:

| `reason` | Meaning |
|---|---|
| `user_cancelled` | The user closed the "choose a plugin" dialog. The task is **paused**, not deleted, and will ask again on resume. |
| `variant_selection_cancelled` | The user closed the quality/format picker of a `variants` resolve. The task is **paused**, not deleted, and will ask again on resume. |

`onCancel` is mutually exclusive with `onDone` and `onError` for a given outcome: a cancelled run never also reports completion or failure. It exists only so you can release temporary resources; it cannot retry. Because a paused task can be started again later, a task may see `onCancel` and then, after a resume, `onDone`.

`onCancel` is **not** delivered when a task is deleted, or when the daemon stops while a task runs — a hook that must free remote state in those cases needs a periodic sweep of its own.

## Filtering with `hooks.match.urls`

- `*` is the only wildcard and matches any run of characters, including `/`. Matching is case-insensitive and applies to the **entire URL**.
- A pattern without `*` must equal the URL exactly. The part before the first `*` is anchored to the start, the part after the last `*` to the end.
- `*://files.example.com/*` matches `http` and `https`; `files.example.com/*` matches nothing, because the first segment is anchored.
- Which URL is compared depends on the event: the resolved URL for `onStart`, the original task URL for all others.
- The filter applies to `hooks` only. Events delivered to the `main` script through `main.events` are not filtered.

## What a hook can use

| Facade | Needs permission | Available in |
|---|---|---|
| `flux.fetch`, `flux.storage`, `flux.fs`, `flux.logger` / `console`, `flux.settings`, `flux.info` | none | every hook |
| `flux.task.requestRetry({ delayMs })` | none | `onError` only |
| `flux.task.recordArtifact(name)` | none | `onDone` only |
| `flux.ffmpeg`, `flux.ffprobe` | `ffmpeg` | `onDone` only (they exist elsewhere but `run` rejects) |
| `flux.ytdlp` | `ytdlp` | every hook |
| `flux.auth` | `auth` | every hook |

`flux.tasks`, `flux.ui` and the other resident-script facades are **not** available in hooks. Calling a facade where it is not valid logs a warning (`flux.task.requestRetry 仅 onError 钩子可用，已忽略` — “requestRetry is only available in onError hooks, ignored”) or rejects with an error (`flux.task.recordArtifact 仅 onDone 钩子可用` — “only available in onDone hooks”).

Declaring `ffmpeg` or `ytdlp` makes FluxDown ask the user for approval on install. It also raises the time budget (see above) and moves the plugin's hooks to the separate queueing pool. With only `ffmpeg` the long budget applies to `onDone`; the plugin's other hooks keep the 5 s limit. See [Permissions](/docs/en/plugins/permissions/).

## Development loop

1. Install the plugin folder in **dev mode** (Settings → Extensions → Plugins → install from directory). The hooks script is re-read on **every event**, so edit, save, and trigger a download — no reinstall.
2. A change to `manifest.json` (events, `match`, settings, permissions) needs the **Reload** button on the dev plugin's row (`重新加载`), which re-reads the manifest and every script.
3. If the manifest declares permissions, approve them in the dialog that appears after installation; until you do, the plugin stays disabled.
4. Watch the plugin log for `flux.logger` output and for ignored failures — see [Debugging](/docs/en/plugins/debugging/).

A handy test download is any small file from a host that matches your `match.urls`; for `onCancel`, use a resolver that returns `variants` and close the picker dialog.

## Example 1: notify when a download finishes

A webhook notifier. It posts a JSON message to a URL the user configures and writes a log line. It needs no permissions: `flux.fetch` is always available.

`manifest.json`

```json
{
  "identity": "acme@done-notifier",
  "name": "Done Notifier",
  "version": "1.0.0",
  "description": "Posts a JSON message to a webhook when downloads finish or fail.",
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone", "onError"]
  },
  "settings": [
    {
      "key": "webhookUrl",
      "title": "Webhook URL",
      "description": "Receives a JSON POST for every finished or failed download.",
      "type": "string",
      "pattern": "^https?://\\S+$"
    },
    {
      "key": "notifyOnError",
      "title": "Also notify on failures",
      "type": "boolean",
      "default": "true"
    }
  ]
}
```

`hooks.js`

```js
function baseName(path) {
  return String(path).split(/[\\/]/).pop();
}

async function send(payload) {
  const url = flux.settings.webhookUrl;
  if (!url) {
    // "required" is not enforced for hooks, so check yourself.
    flux.logger.warn("webhookUrl is not configured; skipping notification");
    return;
  }
  const res = await flux.fetch({
    method: "POST",
    url,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (res.status >= 400) {
    flux.logger.warn("webhook answered HTTP", res.status);
  }
}

async function onDone(ctx) {
  flux.logger.info("finished", ctx.taskId, ctx.filePath);
  await send({
    text: "Downloaded " + baseName(ctx.filePath),
    taskId: ctx.taskId,
    source: ctx.url,
  });
}

async function onError(ctx) {
  if (flux.settings.notifyOnError !== true) return;
  // This fires for every failure, including ones FluxDown retries by itself.
  await send({
    text: "Download failed: " + ctx.message,
    taskId: ctx.taskId,
    source: ctx.url,
  });
}
```

Things to know:

- `flux.fetch` only reaches public `http`/`https` addresses. A webhook on `localhost` or your LAN is blocked by design; use a public endpoint or a relay.
- The default 5 s hook budget covers the whole call, including the HTTP round trip. A slow endpoint is cut off and the failure is only logged.
- Because `onError` also fires for failures that will be retried, a flaky connection can produce several messages. If that is noisy, skip messages whose `ctx.message` looks transient, or add `onDone`-only notification.

## Example 2: retry selectively

Signed download links expire. When one expires mid-download the server answers HTTP 403 or 410, which FluxDown does **not** retry automatically (it retries timeouts, connection resets and HTTP 408/500/502/503/504). A resolver plugin is the perfect pair: resuming a task runs the resolver again and fetches a fresh link. This hook asks for exactly that.

### How `requestRetry` interacts with the app's own retry

1. **The host decides first.** In `onError`, if the failure is one the host considers transient and it still has automatic attempts left, it has *already* scheduled its own retry. A `requestRetry` for the same failure is then silently ignored (no double counting).
2. **Same budget.** Plugin retries consume the same per-task counter as automatic retries — the **Auto-retry Attempts** download setting (`max_auto_retries`). When it is **Off** (0), `requestRetry` never does anything. When the counter is exhausted the request is ignored (the app log notes `重试已达上限，忽略 requestRetry` — “retry limit reached, requestRetry ignored”). The counter is cleared when the task finishes successfully.
3. **Unlimited means unlimited.** With the setting at **Unlimited**, nothing stops a plugin from retrying forever. Keep your own cap, as below.
4. **Only failed tasks.** The request is honoured only while the task is still in the failed state.
5. **A retry is a resume.** `onStart` does not fire again; if the retry fails, `onError` fires again. For resolver tasks the resolver runs again.
6. **Delay.** `delayMs` is a fixed delay in milliseconds (default `0`); the host's own automatic retries grow the delay with each attempt.

`manifest.json`

```json
{
  "identity": "acme@expiry-retry",
  "name": "Expired Link Retry",
  "version": "1.0.0",
  "description": "Retries downloads that failed because a signed link expired.",
  "hooks": {
    "entry": "hooks.js",
    "events": ["onError"],
    "match": { "urls": ["*://files.example.com/*"] }
  },
  "settings": [
    {
      "key": "maxPluginRetries",
      "title": "Maximum plugin retries per task",
      "type": "number",
      "min": 0,
      "max": 10,
      "default": "2"
    },
    {
      "key": "delaySeconds",
      "title": "Delay before retrying (seconds)",
      "type": "number",
      "min": 0,
      "max": 600,
      "default": "3"
    }
  ]
}
```

`hooks.js`

```js
// Remember how often we retried each task. A single key keeps us far below the
// 100-key storage limit; only the 50 most recent tasks are kept.
async function bump(taskId) {
  const raw = await flux.storage.get("retries");
  const counts = raw ? JSON.parse(raw) : {};
  counts[taskId] = (counts[taskId] || 0) + 1;
  const ids = Object.keys(counts);
  if (ids.length > 50) delete counts[ids[0]];
  await flux.storage.set("retries", JSON.stringify(counts));
  return counts[taskId];
}

function looksExpired(message) {
  // Drop URLs first: a link may contain "403" by accident.
  const text = String(message).replace(/\S*:\/\/\S*/g, "");
  return /\b(403|410)\b/.test(text) || /expired/i.test(text);
}

async function onError(ctx) {
  if (!looksExpired(ctx.message)) return;

  const limit = flux.settings.maxPluginRetries ?? 2;
  const attempt = await bump(ctx.taskId);
  if (attempt > limit) {
    flux.logger.warn("giving up on", ctx.taskId, "after", limit, "retries");
    return;
  }

  const delayMs = (flux.settings.delaySeconds ?? 3) * 1000;
  flux.logger.info("retrying", ctx.taskId, "attempt", attempt, "in", delayMs, "ms");
  flux.task.requestRetry({ delayMs });
}
```

`requestRetry` is synchronous and returns nothing; there is no way to find out whether the host accepted it. Verify the behaviour in the app log if a retry does not happen.

## Example 3: post-process with ffmpeg

Remux finished `.mkv`/`.ts`/`.flv` files into `.mp4` without re-encoding, using `ffprobe` to look before leaping, and register the new file as an artifact of the task so it is deleted together with the task.

`manifest.json`

```json
{
  "identity": "acme@mp4-remux",
  "name": "MP4 Remux",
  "version": "1.0.0",
  "description": "Remuxes finished mkv/ts/flv downloads to mp4 without re-encoding.",
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone"]
  },
  "permissions": ["ffmpeg"],
  "settings": [
    {
      "key": "extensions",
      "title": "File types to remux",
      "description": "Comma-separated extensions.",
      "type": "string",
      "default": "mkv,ts,flv"
    }
  ]
}
```

`hooks.js`

```js
async function onDone(ctx) {
  if (!flux.ffmpeg || !flux.ffprobe) return;

  const name = ctx.filePath.split(/[\\/]/).pop();
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return;
  const stem = name.slice(0, dot);
  const ext = name.slice(dot + 1).toLowerCase();

  const wanted = String(flux.settings.extensions || "")
    .split(",")
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
  if (!wanted.includes(ext)) return;

  // The working directory is the folder of the finished file, so refer to files
  // by relative name. The "./" prefix keeps names that start with "-" safe.
  const input = "./" + name;
  const output = "./" + stem + ".mp4";

  const probe = await flux.ffprobe.run({
    args: [
      "-v", "error",
      "-select_streams", "v:0",
      "-show_entries", "stream=codec_name",
      "-of", "default=nw=1:nk=1",
      input,
    ],
    timeoutMs: 60000,
  });
  const codec = probe.stdout.trim();
  if (probe.code !== 0 || !["h264", "hevc"].includes(codec)) {
    flux.logger.info("skip remux, video codec is", codec || "unknown", name);
    return;
  }

  const run = await flux.ffmpeg.run({
    args: ["-i", input, "-c", "copy", "-movflags", "+faststart", "-y", output],
    timeoutMs: 20 * 60 * 1000,
  });
  if (run.code !== 0) {
    flux.logger.error("ffmpeg failed", run.timedOut ? "(timed out)" : "", (run.stderr || "").slice(-400));
    return;
  }

  // Tie the new file to the task: it is removed with "delete task and files".
  await flux.task.recordArtifact(stem + ".mp4");
  flux.logger.info("remuxed", name, "->", stem + ".mp4");
}
```

Rules to keep in mind (the full table is in the [API reference](/docs/en/plugins/api-reference/)):

- **Permission and event.** `flux.ffmpeg`/`flux.ffprobe` exist only with `"ffmpeg"` in `permissions`, and `run` works only inside `onDone`. Elsewhere it rejects with `flux.ffmpeg.run 仅在有产物文件的钩子（如 onDone）中可用` (“only available in hooks that have a produced file, such as onDone”). ffmpeg must also be installed (Settings → Extensions → Components); otherwise `ffmpeg 未安装或不可用` (“ffmpeg is not installed or unavailable”). `flux.ffmpeg.available()` tells you beforehand. ffprobe is installed with ffmpeg; `ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）` means it is missing.
- **The jail.** The working directory is the folder that contains the finished file, and the process cannot step out of it. Use **relative** names.
- **Argument screening.** Each argument is checked before spawning. Rejected: URLs and protocol prefixes (`http://…`, `file:…`, `concat:…`), absolute paths and drive letters, `..` segments, absolute paths embedded after `=` or `:` (`subtitles=/etc/x`), the `tee` muxer, options that read values from files (`-/…`, `-filter_script`, `-filter_complex_script`), `-safe`, and `-protocol_whitelist`/`-protocol_blacklist`. Normal syntax such as `30000/1001`, `0:a`, `-c:v` or `scale=1280:720` passes. A rejection reads `ffmpeg 参数 '<arg>' 被拒: <reason>` (“ffmpeg argument rejected: <reason>”). `args` must be non-empty, at most 512 items of at most 8 KB each.
- **Timeouts.** `timeoutMs` defaults to 300 000 (5 min) and is capped at 1 800 000 (30 min); on timeout the process is killed and `timedOut` is `true`. The whole hook may run for about 30 minutes. At most two ffmpeg/ffprobe processes run at once across all plugins, so batches queue.
- **Output.** `stdout` is cut at 256 KB and `stderr` at 64 KB (`truncatedStdout`/`truncatedStderr`). Log only the tail of stderr.
- **`recordArtifact(name)`** takes a bare file name (no `/`, `\` or `:`, at most 512 characters). The file must already exist in the task's folder and its name must **start with the task file's stem** (`Show.mkv` → `Show…`), otherwise it rejects with `recordArtifact: 产物须以任务文件名主干 '…' 开头` (“the artifact must start with the task file stem”) or `recordArtifact: 产物文件不存在` (“artifact file does not exist”).

For track-pair tasks where the host failed to merge (`ctx.muxed === false` and `ctx.audioPath` set), `onDone` is also the place to merge `filePath` and `audioPath` yourself with `-c copy`.

## Example 4: clean up on cancel

A resolver that first copies a shared file into a temporary spot on the user's cloud drive, then hands out the drive's download link. When the task ends — finished, or paused because the user closed a selection dialog — the temporary copy must go. `onCancel` exists for this; `onDone` covers the success path. (The drive API below is fictional; the structure is what matters.)

`manifest.json`

```json
{
  "identity": "acme@drive-transfer",
  "name": "Drive Transfer",
  "version": "1.0.0",
  "description": "Downloads shared files through a temporary copy in your drive and removes it afterwards.",
  "resolvers": [
    {
      "match": { "urls": ["*://share.drive.example/*"] },
      "entry": "resolve.js"
    }
  ],
  "hooks": {
    "entry": "hooks.js",
    "events": ["onDone", "onCancel"],
    "match": { "urls": ["*://share.drive.example/*"] }
  },
  "settings": [
    {
      "key": "apiToken",
      "title": "API token",
      "type": "string",
      "widget": "password",
      "required": true
    }
  ]
}
```

`resolve.js`

```js
const API = "https://api.drive.example/v1";

async function loadTransfers() {
  const raw = await flux.storage.get("transfers");
  return raw ? JSON.parse(raw) : {};
}

async function saveTransfers(map) {
  const ids = Object.keys(map);
  while (ids.length > 50) delete map[ids.shift()];
  await flux.storage.set("transfers", JSON.stringify(map));
}

async function resolve(ctx) {
  const headers = { Authorization: "Bearer " + flux.settings.apiToken };
  const map = await loadTransfers();

  // A resume resolves again: drop the previous temporary copy first.
  if (map[ctx.taskId]) {
    await flux.fetch({ method: "DELETE", url: API + "/transfers/" + map[ctx.taskId], headers }).catch(() => {});
  }

  const res = await flux.fetch({
    method: "POST",
    url: API + "/transfers",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ source: ctx.url }),
  });
  if (res.status !== 200) throw new Error("transfer failed: HTTP " + res.status);

  const transfer = JSON.parse(res.body);
  map[ctx.taskId] = transfer.id;
  await saveTransfers(map);
  return { url: transfer.downloadUrl, fileName: transfer.name, ephemeral: true };
}
```

`hooks.js`

```js
const API = "https://api.drive.example/v1";

async function release(taskId, why) {
  const raw = await flux.storage.get("transfers");
  const map = raw ? JSON.parse(raw) : {};
  const id = map[taskId];
  if (!id) return;

  const token = flux.settings.apiToken;
  if (!token) return;
  const res = await flux.fetch({
    method: "DELETE",
    url: API + "/transfers/" + id,
    headers: { Authorization: "Bearer " + token },
  });
  if (res.status < 300 || res.status === 404) {
    delete map[taskId];
    await flux.storage.set("transfers", JSON.stringify(map));
    flux.logger.info("released temporary copy", id, "after", why);
  } else {
    flux.logger.warn("could not release", id, "HTTP", res.status);
  }
}

async function onDone(ctx) {
  await release(ctx.taskId, "done");
}

async function onCancel(ctx) {
  // ctx.reason: user_cancelled | variant_selection_cancelled
  await release(ctx.taskId, ctx.reason || "cancel");
}
```

Notes:

- `resolve.js` and `hooks.js` are separate scripts; each one repeats the small helpers it needs.
- Do **not** release in `onError`: the task may be retried and will want the same copy.
- `variant_selection_cancelled` pauses the task; releasing the copy is still correct because the next resume resolves — and transfers — again.
- `onCancel` is not delivered if the task is deleted, or if the daemon is stopped while the task is running. Add a periodic sweep if leaked copies are costly.

## Example 5: using yt-dlp

`flux.ytdlp` runs the yt-dlp that FluxDown manages (a user-set path, then the managed install, then the system `PATH`). It needs `"ytdlp"` in `permissions`, and yt-dlp installed from Settings → Extensions → Components. Without the permission `flux.ytdlp` is `undefined`; if yt-dlp is missing the call rejects with `yt-dlp 未安装或不可用` (“yt-dlp is not installed or unavailable”).

Where it works: `resolve`, **every** hook, `subscribe`, and the `main` script — but not `authenticate`. Its working directory is a per-plugin scratch folder shared with `flux.fs`, **not** the download folder. In practice yt-dlp is a *metadata* tool for plugins: let it find the direct link, then let FluxDown download it. A hook cannot use yt-dlp to drop files next to the download.

### What arguments are allowed

The rules are an **allow-list**, so anything not listed is refused, including abbreviations (`--no-warn`) and glued short options (`-fbest`, `-Jq`). A refusal reads `yt-dlp 参数被拒: 开关 '--x' 不在允许列表内` (“yt-dlp argument rejected: switch '--x' is not on the allow-list”).

- **Positional arguments** must be `http://` or `https://` URLs (`位置参数只允许 http/https URL`). After a bare `--` everything is positional.
- **Long options** use their full canonical name, with the value either as the next argument or as `--name=value`. **Short options** are accepted only as separate arguments and only the ones below; a value-taking switch with no value is `开关 '…' 缺少值` (“switch has no value”).
- **Switches without a value:** `-J -j -q -v -s -x -F -i -4 -6 -k`, `--version --dump-json --dump-single-json --simulate --skip-download --quiet --verbose --no-warnings --no-progress --ignore-errors --no-abort-on-error --no-playlist --yes-playlist --flat-playlist --no-flat-playlist --lazy-playlist --playlist-reverse --force-ipv4 --force-ipv6 --geo-bypass --no-geo-bypass --no-check-certificates --check-formats --list-formats --live-from-start --no-live-from-start --no-mtime --restrict-filenames --windows-filenames --no-cache-dir --extract-audio --keep-video --no-keep-video --write-info-json --write-description --write-thumbnail --write-subs --write-auto-subs --no-write-subs --embed-subs --embed-thumbnail --embed-metadata --embed-chapters --no-part --no-continue --no-js-runtimes --mark-watched --no-mark-watched --hls-prefer-native --hls-use-mpegts --no-hls-use-mpegts`.
- **Switches with one value:** `-f -S -N -r -R`, `--format --format-sort --extractor-args --playlist-items --playlist-start --playlist-end --max-downloads --proxy --socket-timeout --source-address --geo-verification-proxy --geo-bypass-country --geo-bypass-ip-block --xff --user-agent --referer --add-header --sleep-interval --min-sleep-interval --max-sleep-interval --sleep-requests --sleep-subtitles --retries --extractor-retries --fragment-retries --file-access-retries --retry-sleep --concurrent-fragments --limit-rate --throttled-rate --http-chunk-size --buffer-size --min-filesize --max-filesize --date --datebefore --dateafter --match-filters --match-filter --age-limit --download-sections --merge-output-format --remux-video --recode-video --audio-format --audio-quality --sub-langs --sub-format --convert-subs --convert-thumbnails --impersonate --print --parse-metadata --compat-options --video-password --username --password --twofactor --encoding --output-na-placeholder`.
- **Runtime selection:** `--js-runtimes <name>` takes a bare runtime name (letters, digits, `,`, `_`, `-`), never a path.
- **File-path switches** — `--cookies`, `--cache-dir`, `--download-archive`, `-o`/`--output`, `-P`/`--paths`, and the second argument of `--print-to-file <template> <file>` — accept only relative paths inside the scratch folder. Absolute paths, drive letters, `..`, `~`, `$VAR` and `%VAR%` are refused (`开关 '…' 的值被拒: …` — “the value of the switch was rejected”). `-o`/`-P` may carry a type prefix such as `home:` or `temp:`.
- **Never allowed:** anything that runs programs, loads config or plugins, or reads other credentials — for example `--exec`, `--exec-before-download`, `--downloader`, `--external-downloader`, `--config-locations`, `--plugin-dirs`, `--ffmpeg-location`, `--batch-file`, `--load-info-json`, `--netrc`, `--cookies-from-browser`, `--use-postprocessor`, `--alias`.
- Limits: at most 512 arguments, each at most 8 KB, no NUL bytes; `args` must not be empty.

The host prepends `--ignore-config`, `--ffmpeg-location` (its own ffmpeg) and `--cache-dir <scratch>/.cache` (your `--cache-dir` overrides it, still inside the folder), and appends `--proxy` when the user configured a proxy in FluxDown, which then wins over your own. `timeoutMs` defaults to 300 000 and is capped at 3 600 000; the call is also bounded by the script's own wall clock — the resolver's own budget (10 s by default, up to 120 s) in a resolver, about 30 minutes in a hook. At most two yt-dlp processes run at once across all plugins. `stdout` is cut at 16 MB by default (`truncatedStdout` tells you); `stderr` at 256 KB. The full details are in the [API reference](/docs/en/plugins/api-reference/).

### A yt-dlp resolver with a retrying hook

`manifest.json`

```json
{
  "identity": "acme@ytdlp-resolver",
  "name": "yt-dlp Resolver",
  "version": "1.0.0",
  "description": "Asks yt-dlp for a direct link and lets FluxDown download it.",
  "resolvers": [
    {
      "match": { "urls": ["*://www.youtube.com/watch*", "*://youtu.be/*"] },
      "entry": "resolve.js",
      "timeoutMs": 30000
    }
  ],
  "hooks": {
    "entry": "hooks.js",
    "events": ["onError"],
    "match": { "urls": ["*://www.youtube.com/watch*", "*://youtu.be/*"] }
  },
  "permissions": ["ytdlp"],
  "settings": [
    {
      "key": "format",
      "title": "yt-dlp format selector",
      "type": "string",
      "default": "best[ext=mp4]/best"
    }
  ]
}
```

The resolver budget defaults to 10 s; `timeoutMs: 30000` raises it to 30 s (the ceiling is 120 s; FluxDown 0.6.8 and earlier clamp at 30 s) so yt-dlp (given 25 s below) has room.

`resolve.js`

```js
function safeName(title, ext) {
  const clean = String(title || "video")
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_")
    .replace(/\.{2,}/g, ".")
    .trim()
    .slice(0, 150);
  return clean + "." + (ext || "mp4");
}

async function resolve(ctx) {
  if (!flux.ytdlp) throw new Error("the ytdlp permission is not granted");

  const run = await flux.ytdlp.run({
    args: ["-J", "--no-playlist", "--no-warnings", "-f", flux.settings.format || "best", ctx.url],
    timeoutMs: 25000,
  });
  if (run.timedOut) throw new Error("yt-dlp timed out");
  if (run.code !== 0) throw new Error("yt-dlp failed: " + (run.stderr || "").slice(-300));
  if (run.truncatedStdout) throw new Error("yt-dlp output was truncated");

  const info = JSON.parse(run.stdout);
  const headers = info.http_headers || {};

  // Separate video + audio streams (e.g. "bv*+ba"): hand both to FluxDown.
  const [video, audio] = info.requested_formats || [];
  if (video && audio) {
    return {
      url: video.url,
      audioUrl: audio.url,
      fileName: safeName(info.title, info.ext),
      extraHeaders: headers,
      ephemeral: true,
    };
  }

  if (!info.url) throw new Error("yt-dlp returned no direct URL");
  return {
    url: info.url,
    fileName: safeName(info.title, info.ext),
    extraHeaders: headers,
    ephemeral: true,
  };
}
```

`hooks.js`

```js
// Signed links expire; FluxDown does not retry HTTP 403/410 on its own, but
// resuming runs the resolver again and fetches a fresh link (see Example 2).
async function onError(ctx) {
  const text = String(ctx.message).replace(/\S*:\/\/\S*/g, "");
  if (/\b(403|410)\b/.test(text)) {
    flux.task.requestRetry({ delayMs: 2000 });
  }
}
```

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| The hook never runs | The event is not in `hooks.events`; `match.urls` does not match (remember `onStart` sees the resolved URL); the plugin is disabled or waiting for consent; the function is missing or was declared with `const`. |
| `onMetaProbed` never fires | The task has a resolver, starts immediately, is BitTorrent, or came with a size hint. |
| `requestRetry` does nothing | Called outside `onError`; **Auto-retry Attempts** is Off or used up; the host already scheduled its own retry for this failure. |
| `ffmpeg` is `undefined` | The `ffmpeg` permission is missing or not approved. |
| ffmpeg rejects with `…被拒…` | An argument is an absolute path, URL, `..` or other screened syntax — use relative names with `./`. |
| Warning `钩子并发已满，本次钩子被丢弃` in the log | Too many short hooks at once; make the hook cheaper or give it a permission pool. |

More in [Debugging](/docs/en/plugins/debugging/). To react to events with long-lived state or to show them in a view, continue with the [UI guide](/docs/en/plugins/ui-guide/); for sign-in see the [Auth guide](/docs/en/plugins/auth-guide/). A larger working example lives in the repository: [feature-demo](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/feature-demo) and [youtube-down](https://github.com/zerx-lab/FluxDown/tree/main/packages/plugin-sdk/examples/youtube-down).
