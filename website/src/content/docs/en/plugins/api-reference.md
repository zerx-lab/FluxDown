---
title: Plugin API Reference
description: Entry-point function signatures, the flux.* API, and all runtime limits.
section: plugins
order: 4
---

Everything a plugin script can see: five entry points FluxDown calls, and the `flux` object it injects. All field names crossing the JS boundary are camelCase.

## Entry points

Entry points are plain global functions. `async` functions and returned Promises are fully supported — FluxDown awaits the result.

### `resolve(ctx)`

Called before protocol dispatch, on **every** start and resume of a matching task (resolution is lazy — see the [overview](/docs/en/plugins/overview/)).

`ctx` fields:

| Field | Type | Meaning |
|---|---|---|
| `taskId` | string | Task UUID. |
| `url` | string | The original task URL (never a previously resolved one). |
| `cookies` | string | Cookie header value attached to the task. |
| `referrer` | string | Referrer attached to the task. |
| `userAgent` | string | Effective User-Agent. |
| `extraHeaders` | object | Extra request headers as string key-values. |

Return `null` or `undefined` to pass through (FluxDown downloads `ctx.url` unchanged). Otherwise return an object; every field except `url` is optional:

| Field | Type | Meaning |
|---|---|---|
| `url` | string | The rewritten direct link. An empty string keeps the original URL. |
| `audioUrl` | string | Separate audio stream link, for DASH-style split audio/video. |
| `fileName` | string | Override the saved file name. |
| `totalBytes` | number | File size in bytes, if known. |
| `extraHeaders` | object | Headers to send when downloading the resolved link. |
| `ephemeral` | boolean | `true` = the link is one-shot / anti-hotlinked: skip the metadata probe (at the cost of weaker resume-integrity checks). Default `false`: probe normally and keep ETag-based resume validation. |
| `rangeSupported` | boolean | `true` = you guarantee the resolved host honours HTTP Range requests (e.g. googlevideo). Combined with `ephemeral`, FluxDown still skips the probe but plans a full multi-segment download right away instead of the conservative single-stream start. Default `false`: without a probe, Range capability is learned from the first response. |
| `variants` | array | Multiple quality/format choices. When present with more than one entry, FluxDown shows a picker dialog and the user's choice collapses into a single link before download (in a headless server or do-not-disturb download, `defaultVariantIndex` is used silently, exactly like HLS quality selection). Each entry: `{ label, url, audioUrl?, fileName?, totalBytes?, bandwidth?, width?, height?, container? }` — `label` and `url` are required. When `variants` is non-empty the top-level `url` may be empty. Up to 50 entries; each `label` ≤ 200 chars. |
| `defaultVariantIndex` | number | Which variant is the default (used on 60 s timeout / do-not-disturb / headless). Out-of-range values fall back to `0`. Default `0`. |

After resolution, FluxDown re-examines the *resolved* URL to pick the protocol engine — a resolver may return an HLS playlist, a magnet link or an FTP URL and the right engine takes over.

Error behavior is fail-closed: an exception, timeout, invalid return value, or an uninstalled/disabled plugin all put the task into the error state. The original URL is never silently downloaded.

### `onStart(ctx)` / `onDone(ctx)` / `onError(ctx)` / `onMetaProbed(ctx)`

Notification hooks. All receive `{ event, taskId, url }` plus event-specific fields:

| Event | Extra fields |
|---|---|
| `onStart` | — |
| `onError` | `message` — the task's error text |
| `onDone` | `filePath` — absolute path of the finished file; `audioPath` — for track-pair tasks (separate video+audio streams) where muxing failed, the absolute path of the standalone audio file (`<stem>.audio.m4a`); `null` for single-file results (including successful mux); `muxed` — whether a track-pair task was successfully merged into a single file, always `false` for non-track-pair tasks |
| `onMetaProbed` | `fileName`, `totalBytes` — probe results |

`url` is always the task's original URL, and it's what the manifest's `hooks.match.urls` filter is applied to.

Hooks are fire-and-forget: exceptions and timeouts are logged and swallowed, and if the plugin runtime is saturated the notification is dropped. Nothing a hook does can change the task — with one exception, `flux.task.requestRetry`, valid only inside `onError`.

## The `flux` object

### `flux.fetch(opts)` → `Promise<response>`

HTTP client. `opts`:

| Field | Default | Notes |
|---|---|---|
| `method` | `"GET"` | |
| `url` | — | Required. |
| `headers` | `{}` | String key-values. |
| `body` | none | Request body, text only. |

Resolves to `{ status, headers, body, truncated }` — `status` is the numeric code, `body` is text (binary responses are not supported in v1), and `truncated` is `true` when the body hit the size cap. Network and guard failures reject the Promise.

Guard rails, all enforced host-side:

| Rule | Value |
|---|---|
| Schemes | `http` / `https` only |
| Destinations | Publicly routable addresses only. Loopback, LAN, link-local and cloud-metadata IPs are blocked — checked against the literal URL, at DNS resolution, and again on every redirect hop. |
| Response body cap | 8 MB, then truncated |
| Per-request timeout | 10 s |
| Concurrent requests | 8, shared across all plugins |
| Max redirects | 30 |

### `flux.storage`

Persistent key-value store, private to your plugin, survives app restarts (backed by the FluxDown database).

- `flux.storage.get(key)` → `Promise<string | null>`
- `flux.storage.set(key, value)` → `Promise<void>` — rejects when a single value exceeds **64 KB** or the plugin would exceed **100 keys**.

Values are strings; JSON-encode anything structured yourself.

### `flux.fs`

Always available — no `permissions` entry required. Scratch-file storage for a plugin's own workspace, distinct from `flux.storage`'s key-value store: use it when a managed tool needs a real file on disk rather than a string in memory. The workspace is the **same directory** `flux.ytdlp` runs in (its cwd) — anything written here is reachable by yt-dlp (or any tool call) as a plain relative filename.

- `flux.fs.writeFile(name, content)` → `Promise<void>` — write (or overwrite) a text file; rejects (throws) on an invalid name or a limit violation.
- `flux.fs.readFile(name)` → `Promise<string | null>` — read a text file back; `null` if it doesn't exist.
- `flux.fs.remove(name)` → `Promise<void>` — delete a file; a missing file is not an error (idempotent).
- `flux.fs.list()` → `Promise<string[]>` — top-level file names in the workspace (sub-directories and yt-dlp's own `.cache` are not included).

`name` must be a flat, safe filename: non-empty, containing none of `/`, `\`, `:`, not `.` or `..`, and at most 255 characters — anything else throws. Limits: **8 MB** per file, **64 MB** total per plugin workspace, **100 files** max. Content is text only (no binary). On Unix, files are written best-effort `0600`.

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

Read-only object with your manifest-declared settings, already typed: `string` fields arrive as strings, `number` as numbers, `boolean` as booleans. Unset fields carry their `default`.

### `flux.info`

`{ identity, version, appVersion }` — your plugin's ID and version, and the FluxDown version hosting it.

### `flux.logger` and `console`

`flux.logger.info/warn/error(...)` write to FluxDown's log file. `console.log/info/warn/error/debug` are mapped to the same place (`debug` logs at info level). Multiple arguments are joined with spaces; non-strings are JSON-stringified. Each line is truncated at 4 KB.

### `flux.task.requestRetry(opts)`

`flux.task.requestRetry({ delayMs: 5000 })` — ask FluxDown to retry the failed task after a delay. Only meaningful inside `onError`; called anywhere else it logs a warning and does nothing. Retries share the task's automatic-retry budget, so a plugin cannot retry forever.

### `flux.ffmpeg`

Available **only** when the manifest declares `permissions: ["ffmpeg"]` — otherwise `flux.ffmpeg` is `undefined`, so guard with `if (flux.ffmpeg)`. It runs the ffmpeg FluxDown resolves (a user-set path → the managed install → system `PATH`), so ffmpeg must also actually be present (installable from the app's Settings → Extensions → Components tab).

- `flux.ffmpeg.available()` → `Promise<{ available, version, source }>` — probe the effective ffmpeg. `source` is `"manual"` / `"managed"` / `"system"` / `"none"`.
- `flux.ffmpeg.run(spec)` → `Promise<outcome>` — run ffmpeg. `spec`:

| Field | Default | Notes |
|---|---|---|
| `args` | — | Required, non-empty. ffmpeg argument array (no program name; `-nostdin` is prepended for you). |
| `subdir` | none | Working sub-directory under the jail root; safe relative path, may not escape. |
| `timeoutMs` | 300000 | Per-call timeout, capped at 1800000 (30 min). |

Resolves to `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }` — `code` is the exit code (`-1` when killed / none), `stdout`/`stderr` are truncated (256 KB / 64 KB), `timedOut` is `true` when the timeout killed the run.

**The jail.** `flux.ffmpeg` only works inside `onDone` (the one hook with a produced file); in `resolve` and other events the call rejects. The working directory is the finished file's own folder, and that folder is the jail — reference files by **relative** name (the basename), prefixing with `./` in case a name starts with `-`.

Arguments are screened; a spawn is refused when any token is:

| Blocked | Examples |
|---|---|
| a URL scheme / protocol | `http://…`, `file:…`, `concat:…`, `crypto:…` |
| an absolute path / drive letter | `/etc/x`, `C:\x`, `\\host\share` |
| parent traversal | `../x`, `a/../b` |
| an embedded absolute path | `subtitles=/etc/x` |

Ordinary ffmpeg syntax is untouched — division (`30000/1001`), stream specifiers (`0:a`, `-c:v`), filters (`scale=1280:720`) all pass. With no URL and no absolute path reachable, ffmpeg can only touch files inside the jail, so there's no network path either. At most 2 ffmpeg processes run at once across all plugins, and each child is killed on timeout or cancellation.

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
  if (r.code !== 0) flux.logger.error('convert failed', (r.stderr || '').slice(-400));
};
```

### `flux.ffprobe`

Gated by the same `permissions: ["ffmpeg"]` declaration as `flux.ffmpeg` — no separate permission exists. It shares the exact same jail (available **only** inside `onDone`, rejecting elsewhere), the same path-resolution order (user-set → managed install → system `PATH`), and the same argument screening (no URL scheme, no absolute path, no parent traversal, no embedded absolute path). ffprobe ships alongside the managed ffmpeg install — installing ffmpeg from the Settings → Extensions → Components tab also places ffprobe in `<data_dir>/bin`, so no separate install step is needed.

- `flux.ffprobe.run(spec)` → `Promise<outcome>` — run ffprobe. `spec` and the resolved `outcome` are identical in shape to `flux.ffmpeg.run` (`args` / `subdir` / `timeoutMs`, resolving to `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }`).

Use it for structured probing instead of scraping ffmpeg's stderr:

```js
const out = await flux.ffprobe.run({
  args: ['-v', 'quiet', '-print_format', 'json', '-show_format', '-show_streams', './in.mp4'],
});
const info = JSON.parse(out.stdout);
```

### `flux.ytdlp`

Available **only** when the manifest declares `permissions: ["ytdlp"]` — otherwise `flux.ytdlp` is `undefined`, so guard with `if (flux.ytdlp)`. It runs the yt-dlp FluxDown resolves (a user-set path → the managed install → system `PATH`), so yt-dlp must also actually be present (installable from the app's Settings → Extensions → Components tab).

- `flux.ytdlp.available()` → `Promise<{ available, version, source }>` — probe the effective yt-dlp. `source` is `"manual"` / `"managed"` / `"system"` / `"none"`. A quick `run({ args: ['--version'] })` and checking `code === 0` works as a lighter-weight liveness probe too.
- `flux.ytdlp.run(spec)` → `Promise<outcome>` — run yt-dlp. `spec`:

| Field | Default | Notes |
|---|---|---|
| `args` | — | Required, non-empty. yt-dlp argument array (no program name; `--ignore-config` is prepended for you). |
| `subdir` | none | Working sub-directory under the jail root; safe relative path, may not escape. |
| `timeoutMs` | 300000 | Per-call timeout, capped at 3600000 (60 min). |

Resolves to `{ code, stdout, stderr, timedOut, truncatedStdout, truncatedStderr }` — `code` is the exit code (`-1` when killed / none), `stdout`/`stderr` are truncated (256 KB / 64 KB), `timedOut` is `true` when the timeout killed the run.

**The jail.** Unlike `flux.ffmpeg`, `flux.ytdlp` works in **every** context — `resolve` and every hook — since it has no dependency on a produced file. The jail isn't a task's output folder; it's a scratch directory the bridge keeps per plugin (lazily created under FluxDown's data directory), reused across calls. That's the working directory for the call, and `subdir` carves out a sub-folder inside it. Reference any files you read or write there by **relative** name. This is the same workspace `flux.fs` reads and writes — it's how you feed yt-dlp a cookie jar, config file, or subtitles: `flux.fs.writeFile('cookies.txt', …)` beforehand, reference the file by its relative name in `args`, then `flux.fs.remove('cookies.txt')` once the call returns.

yt-dlp is a network tool, so unlike ffmpeg's jail, URL arguments and outbound network access are allowed — extracting from a remote URL is its entire job. What's blocked is anything that would step outside yt-dlp itself or escape the jail:

| Blocked | Examples |
|---|---|
| absolute path / drive letter | `/etc/x`, `C:\x`, `\\host\share` |
| parent traversal | `../x`, `a/../b` |
| an embedded absolute path | `--paths home:/etc/x` |
| the `file:` local scheme | `file:///etc/passwd` |
| a switch that runs external programs, loads arbitrary config/plugins, or reads browser credentials | `--exec`, `--exec-before-download`, `--downloader`, `--external-downloader`, `--config-location`/`--config-locations`, `--plugin-dirs`, `--ffmpeg-location`, `--batch-file`, `-a`, `--load-info`/`--load-info-json`, `--cookies-from-browser` |

`--ignore-config` is always prepended, so none of yt-dlp's own config files — which could otherwise smuggle in a blocked switch — get read either. At most 2 yt-dlp processes run at once across all plugins, and each child is killed on timeout or cancellation.

FluxDown auto-injects `--ffmpeg-location` (pointing at the resolved managed/system ffmpeg) so merges (bestvideo+bestaudio), `-x` audio extraction, remuxing, and recoding all work — a plugin-supplied `--ffmpeg-location` is still rejected (see the table above), only the host's own injected path is trusted. It also auto-injects `--cache-dir <jail>/.cache`, keeping yt-dlp's cache inside the jail instead of leaking outside it.

Example — resolve a page by asking yt-dlp for its metadata JSON and picking a direct link out of it:

```js
globalThis.resolve = async (ctx) => {
  if (!flux.ytdlp) return null;
  const r = await flux.ytdlp.run({ args: ['-J', '--no-warnings', ctx.url] });
  if (r.code !== 0) throw new Error('yt-dlp failed: ' + (r.stderr || '').slice(-400));
  const info = JSON.parse(r.stdout);
  const direct = info.url || info.formats?.[info.formats.length - 1]?.url;
  if (!direct) return null;
  return { url: direct, fileName: info.title ? `${info.title}.${info.ext || 'mp4'}` : undefined };
};
```

## Runtime limits

Each invocation runs in a fresh QuickJS context: no globals survive between calls, timers and DOM APIs don't exist, and scripts load as classic scripts (top-level `function` declarations become globals; `export` syntax will not work).

| Budget | Resolve | Hooks |
|---|---|---|
| Timeout | 10 s (manifest `timeoutMs` overrides, 30 s hard ceiling) | 5 s |
| Memory | 64 MB | 32 MB |

Three consecutive timeouts or memory-limit hits trip the circuit breaker: the plugin is auto-disabled, the app shows a notice, and it stays off until manually re-enabled.

Hooks granted `permissions: ["ffmpeg"]` (running against a produced file, i.e. `onDone`) or `permissions: ["ytdlp"]` (any hook) get a raised wall-clock budget (~30 min) so a long external-tool run can finish; the 30 s CPU ceiling still bounds the JavaScript itself — time spent awaiting the subprocess doesn't count against it. `resolve` keeps its own budget (10 s default, `timeoutMs` override, 30 s hard ceiling) even where `flux.ytdlp` is reachable — plan long `run()` calls accordingly.
