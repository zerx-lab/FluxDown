---
title: Debugging & Troubleshooting
description: Where plugin logs are and how to search them, what to do after each kind of change, and a table of the error messages you will meet with their causes and fixes.
section: plugins
order: 9
---

This page is for the moment something does not work. It covers where to look, what each plugin state means, which action a given edit needs, and — most useful — the **error messages** with their causes.

> **About the language of messages.** Most errors raised by FluxDown's plugin engine are in Chinese, and that is exactly what you see in the app and in logs. This page quotes them **verbatim** so you can search for them, and adds an English gloss in parentheses. Messages from the SDK command line are in English.

## Quick checklist

Work through this in order when a plugin "does nothing":

1. **Is it loaded?** In **Settings → Extensions → Plugins** the row must say **Loaded**. **Load failed** means FluxDown could not read it; open the error (“Plugin load error”, with a copy button) — see [Install and validation errors](#install-and-validation-errors).
2. **Is it enabled?** Look for **Disabled**, **Auto-disabled**, **Revoked by marketplace** or **Authorization required** next to the name; see [Plugin states](#plugin-states).
3. **Did you reload?** Manifest and `main` changes only take effect after **Reload**; see [The dev loop](#the-dev-loop).
4. **Does the URL match?** Resolver, hook and interceptor patterns are compared with the whole URL; see the checks in [Resolver problems](#resolver-problems) and [Interceptor problems](#interceptor-problems).
5. **What does the log say?** Search for `[plugin:<your identity>]`; see [Logs](#logs).

## Logs

### What ends up in the log

| You write | Logged as |
|---|---|
| `flux.logger.info(...)`, `console.log`, `console.info`, `console.debug` | an INFO line |
| `flux.logger.warn(...)`, `console.warn` | an INFO line — **warn is not distinguishable from info** in the file |
| `flux.logger.error(...)`, `console.error` | an ERROR line (flushed to disk immediately) |

Every line carries the prefix `[plugin:<identity>]`, then your arguments: strings as they are, everything else as JSON, joined by single spaces. A line looks like

```
14:02:31.512 INFO … [plugin:me@mirror-demo] rewriting https://mirror-demo.invalid/files/1Mb.dat -> https://proof.ovh.net/files/1Mb.dat
```

(the middle part is source-location information added by the logger). Each message is cut at 4 KB.

Practical consequences:

- **Filter by identity, not by level:** search for `[plugin:me@mirror-demo]`. Searching for “warn” or “error” will miss your own warnings.
- An `Error` object logs as `{}` (it has no enumerable properties). Log `String(error)` or `error.message` instead.
- The engine itself writes lines starting with `[plugin]` (no identity after the colon) for load, enable and breaker events, for example `[plugin] 跳过 …` (“skipping …”, a plugin could not be loaded), `[plugin] <identity> 连续超时，已自动熔断禁用` (“consecutive timeouts, circuit breaker tripped”). Search for `[plugin` to see both kinds.
- `console.log` in `main` goes to the **daemon log**, like all other plugin output — not to a browser console, even when the view is shown in the Web UI.

### Where the files are

Plugin code runs inside FluxDown's background service (the daemon), so plugin output goes into the **daemon's** log directory: `logs/` inside the daemon's data directory. Files are named `fluxdown_YYYY-MM-DD.log` (`fluxdown_YYYY-MM-DD.N.log` after a rollover); a file rolls over at 2 MB, logs older than 7 days are deleted and the directory is capped at 10 MB by default (**Settings → About → Max Log Size**).

| System | Daemon log directory |
|---|---|
| Linux | `$XDG_DATA_HOME/fluxdown/logs/` (default `~/.local/share/fluxdown/logs/`) |
| macOS | `~/Library/Application Support/fluxdown/logs/` |
| Windows, installed | `%LOCALAPPDATA%\FluxDown\logs\` |
| Windows, portable (a `portable` marker file next to the exe) | `<app folder>\portable_data\logs\` |
| **Any** system with `FLUXDOWN_DATA_DIR` set — headless server, Docker | `$FLUXDOWN_DATA_DIR/logs/` (Docker image: `/data/logs/`) |

The desktop app and the agent write their **own** diagnostic logs (`desktop.log`, `agent.log`) into a separate `agent/logs/` folder under the app's data root. Plugin output is **not** there.

### Opening and exporting logs in the app

| Where | What it does |
|---|---|
| **Settings → About → Export Logs** (desktop and Web UI) | Saves a zip with the agent logs and the daemon logs (inside `daemon/logs/`), with secrets such as tokens and cookies masked. This is the reliable way to get plugin output from any setup, including a remote host. |
| **Settings → About → Open folder** (desktop) and the **Open Logs Folder** menu / command-palette entry | Opens the desktop app's **agent** log folder (`desktop.log`, `agent.log`) — useful for crashes of the app itself, but it does **not** contain your plugin's lines. Open the daemon directory from the table above instead. |

The Web UI has no folder to open; use **Export Logs**. (The old `/api/v1/logs` REST endpoints of the previous server no longer exist.)

### Getting more detail

The log level is INFO by default and your `flux.logger` lines are INFO, so you normally need nothing more. To raise the daemon's own verbosity, set `FLUXDOWN_LOG_LEVEL` (`error`, `warn`, `info`, `debug`, `trace`) or `RUST_LOG` in the environment of the process that starts the daemon, and restart it.

## The dev loop

Install your plugin from a directory (**Install from directory (dev mode)**) to get the fast loop. A dev plugin stores your folder's path and reads from it. The **Reload** button on its row (tooltip: “Reload (re-read manifest and scripts)”) re-reads the manifest and every script and re-runs the install checks. If something fails, the error is shown and the dev registration is **kept**, so you can fix the file and reload again.

| What you changed | What to do |
|---|---|
| A resolver, hooks, interceptors, auth or subscription script | Nothing — dev plugins read these files on **every call**. For SDK projects, rebuild (`npm run dev` does it on save). |
| The `main` script | **Reload.** `main` is one long-lived context that is read when it starts; open view sessions end (“The view session has ended” with a **Reopen** button) and the reopened view runs the new code. |
| `manifest.json` (anything: patterns, entries, settings, views, commands, permissions, version) | **Reload.** The manifest is only parsed when plugins are loaded. |
| A manifest change that adds permissions or network hosts | **Reload**, then approve the dialog (see [Plugin states](#plugin-states)). |
| A desktop script (`desktop.entry`) | Close and reopen the view; **Reload** if you also changed the manifest. |
| A plugin setting's value | Nothing — it is passed on the next call (and pushed to a running `main`). |
| The `config` of the SDK (`fluxdown-plugin.config.json`) | Restart `npm run dev`, then **Reload**. |
| A plugin installed from a `.fxplug` or the market | Reinstall: non-dev plugins are loaded into memory once. |

Remember that `npm run build`, `npm run pack` and `npm run dev` delete `dist/` first. If `dist/` is your installed dev folder, wait until the build is done before you test; if the row shows **Load failed** afterwards, press **Reload**.

Toggling the enable switch off and on also reloads all plugins as a side effect, but only **Reload** reports the error back to you.

A dev registration takes precedence over a normally installed copy of the same identity. To test the packed `.fxplug`, uninstall the dev copy first.

## Plugin states

| Row shows | Meaning | What to do |
|---|---|---|
| **Loaded** | The manifest parsed and the plugin is in memory. | — |
| **Load failed** | The manifest or a required file is broken, or the app is too old. Click for “Plugin load error”; **Copy error** copies it. | Fix it and **Reload** (dev) or reinstall. |
| **Authorization required** | The manifest requests permissions or network hosts you have not approved. The plugin is disabled, the enable switch is locked and an **Authorize** button is shown. Enabling it by other means fails with a message containing `consent required`. | Press **Authorize** and approve. A new version that requests *more* permissions needs a new approval; fewer permissions never do. |
| **Disabled** | You switched it off. | Switch it on. |
| **Auto-disabled** | The circuit breaker tripped, see below. | Fix the script, then switch it on (installing an update also re-enables it). |
| **Revoked by marketplace** | The market withdrew this version. Enabling fails with `该插件版本已被插件市场撤回，请升级后再启用` (“this plugin version was withdrawn by the marketplace; upgrade before enabling”). | Update the plugin. |
| **Dev** badge | Installed from a directory. | — |

### The circuit breaker

Three **consecutive** timeouts or out-of-memory errors in a resolver, an interceptor, a subscription provider or a `main` call disable the plugin automatically (any other outcome resets the count; hooks and `authenticate` do not count). You get a notice (“Plugin “…” was auto-disabled after repeated failures”), the log has `[plugin] <identity> 连续超时，已自动熔断禁用`, and a task that needs the plugin fails with `插件 <identity> 已被熔断自动禁用；重新启用插件，或选择「忽略插件重试」按原始链接下载` (“…was disabled by the circuit breaker; re-enable the plugin or choose “Ignore plugin retry”…”). Typical causes: a `flux.fetch` waiting on a host that never answers, an endless loop, or loading huge responses into memory. Lower `timeoutMs` in the manifest to fail faster than the default (10 s for resolvers, 5 s for interceptors), add error handling around `flux.fetch`, then re-enable.

## Install and validation errors

These appear when you install, reload or run `npm run validate`. The install dialog shows `Install failed: …` / `Plugin installation failed: …` followed by the message; the same text goes to the log.

### Messages from the engine (and `npm run validate`)

The engine reports these with the prefix `manifest 非法: ` (“manifest invalid:”). `npm run validate` and the SDK validator use the same rules and almost the same wording, without that prefix, and report every problem at once instead of stopping at the first. Where the wording differs, the table shows the engine's text.

| Message | Cause | Fix |
|---|---|---|
| `JSON 解析失败: …` (“JSON parse failed”) | `manifest.json` is not valid JSON, has a wrong value type, or contains a field the engine does not know (the detail then reads `unknown field …`). | Fix the syntax; remove the unknown field — spelling and nesting are checked, see the [manifest reference](/docs/en/plugins/manifest/). With the SDK you get `manifest: 未知字段 <key>` (“unknown field”) instead. |
| `identity '…' 非法：须匹配 ^[a-z0-9_-]+@[a-z0-9_-]+$` | The identity is not `author@name` with lowercase letters, digits, `_`, `-`. | Rename, e.g. `me@my-plugin`. No dots, no uppercase. |
| `name 不可为空` (“name must not be empty”) | Empty `name`. | Set it. |
| `version '…' 非法：须为 MAJOR.MINOR.PATCH` | Not three dot-separated numbers. | Use `1.0.0`. The SDK and the plugin index are stricter: no `v` prefix, no suffix. |
| `v1 每插件至多一个 resolver` / `v1 每插件至多一个 subscription provider` | More than one entry in `resolvers` / `subscriptions`. | Keep one. |
| `resolver entry 路径 '…' 非法` (and the same for `hooks entry`, `auth entry`, `subscription entry`, `main entry`, `icon`, `sniffRules`) | Absolute path, drive letter, `..`, empty segment or control character. | Use a relative path inside the package, e.g. `resolve.js`. |
| `resolver match.urls 不可为空` / `hooks match.urls 不可为空` | An empty pattern list. | Add a pattern, or omit `hooks.match`. |
| `interceptors entry 路径 '…' 非法` | Same path rule as the other entries. | Use a relative path inside the package, e.g. `intercept.js`. |
| `interceptors.stages 不可为空` (“stages must not be empty”) | `interceptors.stages` is an empty list. | List at least one stage. |
| `未知拦截阶段 '…'，合法：[…]` (“unknown interception stage”) | A stage name that does not exist. | Use one of the names the message lists (`prepare`, `refresh`, `probed`, …). |
| `interceptors.stages 重复：'…'` (“duplicate stage”) | The same stage is listed twice. | Remove the duplicate. |
| `interceptors match.urls 不可为空` | An empty pattern list. | Add at least one pattern; `interceptors.match` is required. |
| `interceptors timeoutMs 不可为 0` | `timeoutMs` is `0`. | Use a positive integer, at most 30000 effective. |
| `声明 interceptors 时 permissions 必须包含 intercept` | `interceptors` without the `intercept` permission. | Add `"intercept"` to `permissions`. |
| `resolver timeoutMs 不可为 0` | `timeoutMs` is `0` (also checked for `auth` and `subscription`). | Use a positive integer, at most 30000 effective. |
| `hooks.events 不可为空` | `hooks` without events. | List at least one event. |
| `未知事件 '…'，合法：[…]` | Not one of `onStart`, `onError`, `onDone`, `onMetaProbed`, `onCancel`. | Fix the name (also applies to `main.events`). |
| `未知权限 '…'，合法：[…]` | A permission name that does not exist. | Use one of `ffmpeg`, `ytdlp`, `auth`, `tasks`, `browserRules`, `desktop`, `background`, `intercept`, `pageScripts`, `archive`. |
| `声明 auth.entry 时 permissions 必须包含 auth` | `auth` without the `auth` permission. | Add `"auth"` to `permissions`. |
| `声明 sniffRules 时 permissions 必须包含 browserRules` | Sniff rules without the permission. | Add `"browserRules"`. |
| `main.background 为 true 时 permissions 必须包含 background` | `main.background` is `true` without the `background` permission. | Add `"background"`. |
| `声明 pageScripts 时 permissions 必须包含 pageScripts` | `contributes.pageScripts` without the permission. | Add `"pageScripts"` to `permissions`. |
| `pageScript '<id>' …` (id, title, matches, js errors) | A page script breaks a rule in the [manifest reference](/docs/en/plugins/manifest/#pagescripts). | Fix the named field. |
| `view '<id>' 有可移植实现，必须声明 main` | A view without `desktop` (or with `portable: true`) needs a `main` script. | Declare `main`. |
| `声明 commands 时必须声明 main` | Commands need `main`. | Declare `main`. |
| `view id '…' 非法` / `command id '…' 非法：须匹配 ^[a-z][a-z0-9-]{0,31}$` | Id is not lowercase-letter-first, `a-z0-9-`, at most 32 characters. | Rename. |
| `view id '…' 重复` / `command id '…' 重复` / `setting key '…' 重复` | Duplicate id or key. | Make it unique. |
| `contributes.views 至多 16 项` / `contributes.commands 至多 32 项` | Too many contributions. | Reduce. |
| `view '<id>' title 须为 1–40 个字符` / `command '<id>' title 须为 1–40 个字符` | Title empty or longer than 40 characters. | Shorten. |
| `view '…' networkHosts '…' 非法` / `…重复` | A network host that is not a lowercase DNS name or IP literal (no scheme, port, path or wildcard), or a duplicate. | Use `api.example.com`. |
| `setting key 不可为空` | Empty setting key. | Set it. |
| `setting '<key>': …` | A setting field is wrong — widget and type do not fit, `select` without `options`, `default` not among the options, `min` > `max`, wrong default for a number or boolean. | See the [manifest reference](/docs/en/plugins/manifest/#settings). |
| `setting '<key>': pattern 非法（JS RegExp 编译失败）` | `pattern` is not a valid JavaScript regular expression. | Fix the regex. |
| `包内缺少文件 '…'` (“file missing in the package”) | A file the manifest declares is not in the folder or zip: a `main`, `resolvers`, `hooks`, `interceptors`, `auth` or `subscriptions` entry, a `desktop` entry, or `sniffRules`. The message names the path as written in `manifest.json`. A dev plugin whose `dist/` was wiped or never built is reported like this at install and **Reload** (the row shows **Load failed**) — not later, when a task calls the resolver. SDK validation also checks `icon`. | Create the file, or fix the path. For SDK projects: is the script listed under `entries` in `fluxdown-plugin.config.json`, and does the output name match? Rebuild, then **Reload**. |
| `读取入口文件 '…' 失败: …` (“failed to read entry file '…'”) | The entry file exists but cannot be read (no permission, the path is a directory, or the file is not valid UTF-8); the text after the colon is the OS error. Same places as above. | Fix the permissions or the file (UTF-8 text), then **Reload**. |
| `main entry '…' 扩展名须为 .js` | `main` is not a `.js` file. | Rename. |
| `sniffRules '…' 扩展名须为 .json` / `sniffRules format 须为 fluxdown-sniff-pack` / `sniffRules 至多 500 条规则` / `sniffRules 文件超过 262144 字节` | Bad sniff rule pack. | See the [manifest reference](/docs/en/plugins/manifest/). |
| `view '<id>' icon 路径 '…' 非法` / `view '<id>' icon '…' 扩展名须为 .svg` | The view `icon` is not a safe package-relative path, or is not a `.svg` file. | Use a relative `.svg` path inside the package. |
| `view '<id>' icon 超过 16384 字节` / `view '<id>' icon 不是合法 UTF-8` | The icon file is larger than 16 KiB or is not UTF-8 text. | Minify the SVG; save it as UTF-8. |
| `view '<id>' icon 不是 SVG 文档` | After an optional BOM and whitespace the file does not start with `<svg` or `<?xml`, or contains no `<svg`. | Provide a real SVG file. |
| `需要 App ≥ X，当前 Y` (“requires app ≥ X, current Y”) | `minAppVersion` is newer than your app. | Update FluxDown or lower `minAppVersion`. |
| `脚本编译失败: …` (“script compile failed”) | A resolver, hooks, interceptors, auth, subscription or archive script has a syntax error — **or throws at top level**. The check evaluates the script once in a bare context where `flux` and `console` do not exist. | Fix the syntax; move every `flux.*` / `console.*` call into an entry function. For TypeScript projects, run `npm run typecheck` and check that the bundle was rebuilt. |
| `安装后未能加载插件（校验失败或版本门槛不满足）` (“the plugin could not be loaded after install (validation failed or version gate not met)”) | The checks above failed during install; the previous version, if any, was restored. | Read the earlier lines in the log for the actual reason. |
| `读取 manifest 失败: …` / `读取 manifest.json 失败: …` | `manifest.json` is not in the folder you picked. | Pick the folder that **directly** contains `manifest.json` — for SDK projects `dist/`, not the project root. |
| `zip 内未找到根 manifest.json` | The zip has no `manifest.json` at its root (or in its single top-level folder). | Zip the *contents* of `dist/`; `npm run pack` does this correctly. |
| `zip 条目数 N 超过上限 200` / `解压总量超过 … 字节上限（疑压缩炸弹）` | More than 200 files, or more than 50 MB unpacked. | Trim the package. |
| `zip 含非法路径（zip-slip）` | The archive contains `..` or absolute paths. | Re-create it with a normal zip tool. |
| `插件加载失败: …` (“plugin load failed”) | **Reload** of a dev plugin found a load failure; the text after the colon is the cause (one of the messages above). | Fix and reload. |
| `不是开发模式插件: …` (“not a dev-mode plugin”) | **Reload** was requested for an installed (non-dev) plugin. | Reinstall instead. |

### Messages from the SDK command line

| Message | Cause |
|---|---|
| `dist/ not found; run \`fluxdown-plugin build\` first` | `validate` without a previous build. |
| `dist/manifest.json missing` | Build output incomplete. |
| `package has N files; the limit is 200`, `package is N bytes; the limit is 52428800` | Package limits. |
| `fluxdown-plugin.config.json: unknown key '…'` / `…: entries must be an object of string → string` / `…: entries key '…' is not a safe relative path` | Config file mistakes. |
| `<path> is not empty` | `create` into a non-empty folder. |
| `<flag> needs a value` | A `--flag` without its value. |
| esbuild error blocks with `file:line:column` | A TypeScript/JSX error in a bundled file; fix it at that location. |

## Resolver problems

When a resolver fails, the task goes to **Download Failed** and its message is prefixed `[插件] <identity>: ` (“[plugin] …”) followed by the error text. FluxDown **never** falls back to the original page URL by itself (fail-closed), because that could save an HTML page as a video.

### Ignore plugin retry

On a failed task, right-click and choose **Ignore plugin retry** (a confirmation dialog explains it). FluxDown clears the task's plugin binding and retries with the **original link** — use it when the plugin is broken, uninstalled or disabled and you want the plain download. Uninstalling a plugin applies the same escape hatch to all of its tasks.

### Task-time messages

All of these appear as `[插件] <identity>: <message>`. Messages from the host are wrapped as `插件运行时错误: …` (“plugin runtime error”), `插件输出非法: …` (“plugin output invalid”), and so on.

| Message | Cause | Fix |
|---|---|---|
| `插件运行时错误: <your error message>` | Your script threw (or a rejected `await` was not caught). `<your message>` is `error.message`. | Read the log lines before it, and wrap `flux.fetch` in `try/catch` if you want to handle failures. |
| `插件运行时错误: 入口 resolve 未定义` (“entry resolve is not defined”) | The script ran, but did not put a function on `globalThis.resolve` — a missing `defineResolver(...)`, a wrongly named function, or an ES `export` instead of the global. | Define `resolve`. Hooks: the names are `onStart`/`onError`/`onDone`/`onMetaProbed`/`onCancel`; auth: `authenticate`; subscription: `subscribe`. |
| `脚本编译失败: …` | Syntax error in the script. | Fix it. |
| `脚本执行超时` (“script execution timed out”) | The call did not finish within the budget (10 s by default, at most 30 s). Counts toward the circuit breaker. | Speed up or lower the number of requests; raise `timeoutMs` up to 30000 if a slow host needs it. |
| `脚本超出内存上限` (“script exceeded the memory limit”) | More memory used than the budget (64 MB by default). Counts toward the breaker. | Do not hold big responses; use `truncated` bodies carefully. A resolver that really needs more can declare `memoryLimitMb` (up to 512). |
| `插件运行时过载（并发已满）` (“plugin runtime overloaded”) | Too many resolves at once; waiting for a free slot timed out after 3 s. | Retry the task. |
| `插件输出非法: resolve 返回值非法: …` (“resolve return value invalid”) | The returned object has wrong field types, e.g. `totalBytes` that is not a number. | Return the shape from the [API reference](/docs/en/plugins/api-reference/). |
| `插件输出非法: url scheme 不允许: …`, `url 超过 8KB`, `file_name 非法: …`, `variants 过多: N > 50`, `manifest 与 url/variants/audioUrl 互斥` | The result violates a rule: unsupported URL scheme, over-long URL, an unsafe file name, too many variants, or `manifest` combined with `url`. | Fix the returned fields. |
| `缺少必填设置项: 插件 <identity> 需先配置「<title>」` (“required setting missing: the plugin needs “<title>” configured first”) | A setting with `required: true` has no value and no default. The script is not even called. | Fill it in under the plugin's settings (gear button), or give it a `default`. |
| `插件运行时错误: 插件 <identity> 已卸载或加载失败；可在任务菜单选择「忽略插件重试」按原始链接下载` | The task is bound to a plugin that is uninstalled or failed to load. | Reinstall/fix the plugin, or **Ignore plugin retry**. |
| `插件运行时错误: 插件 <identity> 已被禁用；重新启用插件，或选择「忽略插件重试」按原始链接下载` | The plugin is disabled (the same message says `已被熔断自动禁用` after a breaker trip, `已被插件市场撤回` if revoked). | Enable it, or **Ignore plugin retry**. |
| `插件运行时错误: 插件 <identity> 当前版本未提供 resolver（或源码读取失败）…` | The installed version declares no resolver, or the resolver file disappeared after the plugin had loaded (for example `dist/` was wiped while the plugin was running). A resolver file that is already missing when plugins load is reported earlier as **Load failed** with `包内缺少文件 '…'` (see above). | Check the `entry` file exists; rebuild and **Reload**. |

**Nothing happens at all** (the original URL is downloaded unchanged)? Then the resolver was never selected:

- The pattern did not match. Patterns compare against the full URL, case-insensitively; `*` is the only wildcard; the text before the first `*` must be a prefix of the URL and the text after the last `*` a suffix. `example.com/*` does **not** match `https://example.com/x` — use `*://example.com/*`.
- The plugin is disabled, still **Authorization required**, or not loaded.
- Another plugin was chosen. If several plugins match, FluxDown asks once and may remember the choice; check **Settings → Extensions → Site Defaults**. Unattended downloads use the site default or the plugin with the smallest identity.
- The URL is added in a way that skips plugins — check by adding the link in the normal **New download** dialog.

Also see the [resolver walkthrough](/docs/en/plugins/your-first-plugin/#5-test-and-debug).

### `flux.fetch` and blocked addresses

`flux.fetch` only reaches **public** HTTP(S) hosts. Loopback, private LAN, link-local, cloud-metadata and other non-routable addresses are blocked on purpose, including through DNS names and redirects.

| Message (inside `插件输出非法:` / `插件运行时错误:`) | Cause |
|---|---|
| `URL 非法: …` (“URL invalid”) | The `url` could not be parsed. |
| `flux.fetch 仅支持 http/https` (“flux.fetch supports only http/https”) | A `file:`, `ftp:`, … URL. |
| `blocked: non-routable IP` | The URL is a literal IP such as `127.0.0.1`, `192.168.x.x`, `169.254.x.x`. |
| `blocked: non-routable redirect target` | The server redirected to such an address. |
| `fetch 失败: …` (“fetch failed”) + detail | Connection error, TLS error, timeout (10 s per request), or a DNS name that resolves only to non-public addresses. |
| `插件未声明 auth 权限，不能使用认证凭据` (“the plugin does not declare the auth permission”) | You passed `authRef` without the `auth` permission. |

**Exception: developer-mode plugins.** A plugin loaded via Settings → Extensions → Plugins → "Install from directory (developer mode)" is not restricted to public addresses: its `flux.fetch` can reach `localhost` or LAN services, so you can develop against a local server. It follows the app's proxy settings and site rules like any plugin, but loopback, private LAN ranges and local domains are always reached directly. Normally installed plugins are unchanged. The alternatives below apply to non-developer-mode installs.

How to **test against your own server** when developing: you cannot point `flux.fetch` at `localhost` or your LAN. Options: expose a staging endpoint on a public host (HTTPS, any port), use a public mock service, or put your test logic behind a setting that returns canned data instead of fetching. For URL-rewriting resolvers you do not need a server at all — the resolver can return a fixed `url` (see the `.invalid` host trick in [TypeScript project](/docs/en/plugins/typescript-project/#4-add-a-resolver)). The response body is text only (`truncated: true` beyond 8 MB).

## Hook problems

Hooks cannot fail a task, so their errors only show in the log, as lines of `[plugin:<identity>]`:

| Log text | Meaning |
|---|---|
| `hook onDone 执行失败（已忽略）: <error>` (“hook onDone failed (ignored)”) | The hook threw, timed out or its entry is missing (`入口 onDone 未定义`). The hook name is the event handler. |
| `钩子并发已满，本次钩子被丢弃` (“hook concurrency full, this hook was dropped”) | Too many hooks were running; this notification was dropped, not queued. |
| `外部工具钩子排队已满，本次钩子被丢弃` (“external-tool hook queue full…”) | Same for hooks that use ffmpeg / yt-dlp. |

Remember: a resolver and `onMetaProbed` on the same plugin do not mix — tasks handled by a resolver skip the metadata probe, and the load log warns `同时声明 resolver 与 onMetaProbed；带 resolver 的任务跳过探测，该钩子不会触发`. And hooks only run for **enabled** plugins whose `hooks.match` (if any) matches the task URL. See the [Hooks guide](/docs/en/plugins/hooks-guide/) for ffmpeg and retries.

## Interceptor problems

An interceptor can change a task, so its effects are visible in three places: the task's activity log, the task's error message, and the audit log.

### Activity entries

Open the task details and the **Log** tab: entries of kind **Plugin** are written by interceptors. They name fields, stages and file names only — never header values, cookies or request bodies.

| Entry | Meaning |
|---|---|
| `插件 <identity> 改写了请求：<fields>` (“plugin … changed the request”) | A `prepare` (or resolver `request`) patch was applied; the fields are listed, e.g. `请求头`, `Cookie`, `线程数`. |
| `插件 <identity> 的文件名改写已忽略（任务已开始下载）` (“file name change ignored, the task already started”) | `fileName` only takes effect on the first run of a task; resumes and retries ignore it. |
| `插件 <identity> 调整了并发：…` (“changed concurrency”) | A `probed` result changed `segments` or `maxConnections`. |
| `插件 <identity> 刷新了凭据（HTTP <status>）：<fields>` (“refreshed credentials”) | A `refresh` call answered a 401 / 403 / 410 with new credentials. |
| `插件 <identity> 选择了第 N 个画质` / `…个Period` (“selected variant / Period N”) | `selectVariant` / `selectPeriod` picked an entry (counted from 1). |
| `插件 <identity> 过滤了 N 个视频分片` / `…音频分片` (“dropped N video / audio segments”) | `segments` or `liveSegments` dropped segments. |
| `插件 <identity> 改写了 N 个…分片地址` (“rewrote N segment addresses”) | `segments` `rewrite` replaced segment URIs. |
| `插件 <identity> 请求在 N 毫秒后重试` (“requested a retry after N ms”) | A `failed` call asked for a retry. |
| `插件 <identity> 的 <stage> 阶段出错，已忽略：<message>` (“stage failed, ignored”) | The call threw, timed out, returned something invalid or an invalid patch, and `onFailure` is `continue`. The task carries on without that change. |
| `正在执行插件下载后处理` / `插件后处理已跳过：找不到下载文件` / `插件后处理已跳过：读取任务信息失败` | `finalize` is running, or was skipped because the file or the task record could not be read. |
| `插件后处理已取消，任务按原文件完成` (“post-processing cancelled, the task completes with the original file”) | The task was paused or cancelled during `finalize`. |
| `插件 <identity> 将下载文件替换为 <name>` (“replaced the downloaded file with …”) | `finalize` returned `replaceWith`. |

### Failure messages

When `onFailure` is `fail`, or a stage always aborts (`fail` from `probed`, `finalize`, or a rejected resolver patch), the task goes to **Download Failed** with

```
[插件] <identity>（<stage>）：<message>
```

(“[plugin] identity (stage): message”). A rejected patch from a resolver reads `[插件] <identity>: <message>`. The `<message>` is the plugin's own `fail` text, or the engine's reason, for example `请求头 '<name>' 由引擎管理，不可设置` (“header '<name>' is managed by the engine”), `仅 HTTP 任务可声明 ephemeral` (“only HTTP tasks can declare ephemeral”), `url 只能在同一主机内改写（跨站改写请使用 resolver）` (“url can only be rewritten within the same host”) or `url 不能改变下载协议类别`. A patch is applied all-or-nothing: if one field is invalid, none of it is applied.

### Audit events

Every call writes an audit row `intercept.<stage>` (for example `intercept.prepare`) with the task id as target and outcome `ok` or `error`; the detail holds the error text. See [Permissions](/docs/en/plugins/permissions/#audit-log).

### Common mistakes

- **Nothing is called.** The plugin lacks the approved `intercept` permission (row shows **Authorization required**), is disabled, or `interceptors.match.urls` matches neither the task's original URL nor the URL a resolver produced. Patterns work as described in [Resolver problems](#resolver-problems): `*://example.com/*`, not `example.com/*`. BitTorrent and eD2K tasks are never intercepted.
- **A stage is not listed.** Only stages in `interceptors.stages` are called.
- **`fileName` has no effect.** It applies on a task's first run only; look for the `文件名改写已忽略` entry.
- **`ephemeral` is rejected.** Only HTTP tasks can skip the probe; `totalBytes` and `rangeSupported` are only accepted together with `ephemeral: true`.
- **A header is rejected.** Headers the engine manages itself (such as `Range`) cannot be set; the error names the header.
- **A change disappears after a restart.** Patches are never stored; they are re-applied on every run, so the plugin must return them again each time.

## Views, commands and `main`

`main` and view code runs in the resident context in the daemon; its errors go to the log, and view errors are also shown in the view.

| Symptom | Cause and fix |
|---|---|
| The view does not appear in the activity bar / Web UI | The plugin is not loaded or enabled; or the view is `desktop`-only — the Web UI shows only views that are portable (no `desktop` entry, or `"portable": true`). Mobile apps do not render plugin views. |
| An error bar at the top of the view, last content still visible | `render` or `event` threw, or returned an invalid tree. The bar shows the message; the next successful render clears it. Messages from `main`: `render() returned no node` (render returned nothing), `view not registered: <id>` (the id in `flux.ui.view` does not match `contributes.views[].id`), `command not registered: <id>` (no `flux.commands.register` for a manifest command), `unknown session: …`. |
| `invalid view node root/…: …` | The tree broke a rule; the path is the node id. Typical: `a node must be an object {k, p, c}` (a **string** child — wrap text in `<text text="…"/>`), `<kind> unknown prop "…"; allowed props: …`, `<kind> prop "…": expected string, got a number` (convert numbers with `String()`), `<kind> missing required prop "…"`, `duplicate sibling id: …` (two siblings share a `key`), `the tree has more than 5000 nodes`, `nesting is deeper than the limit of 64 levels`. See [Views](/docs/en/plugins/views/). |
| The first open fails and the view shows a load failure with **Reopen** | The very first render was invalid or threw. Fix, **Reload**, press **Reopen**. |
| “The view session has ended” with **Reopen** | The plugin was disabled, reloaded or its `main` was reclaimed after a failure (timeout or out of memory). Reopen. |
| `TypeError` on `flux.tasks` / `flux.auth` / `flux.ffmpeg` / `flux.ytdlp` | These objects exist only when the matching permission is declared **and approved**. Declare it, **Reload**, **Authorize**. |
| `tasks.create rate limited` | `flux.tasks.create` allows 20 calls per minute per plugin. |
| Button press does nothing | Wrong node id in `ev.node` — it is `root/<key>/…`; log `ev.node` once to see the real ids. |
| Typing in an input jumps or is lost | The input must be controlled: keep its text in state and pass it back as `value`. See the [UI guide](/docs/en/plugins/ui-guide/). |
| A command does not show up | It needs `palette: true` (command palette) or `taskMenu: true` (task context menu), a `main` script, and a matching `flux.commands.register`. |
| The Web UI says desktop-only or shows nothing | Native `desktop` views exist on the desktop app only. |

`main` calls run one at a time and have a 30 s wall-clock, 2 s CPU and 64 MB budget per call; hitting them ends all view sessions of the plugin and counts toward the circuit breaker.

## Headless servers and Docker

- The plugin logic and its logs live on the **server**. Plugin output is in `$FLUXDOWN_DATA_DIR/logs/fluxdown_YYYY-MM-DD.log` (in the Docker image: `/data/logs/`, so a bind-mounted or named volume keeps it). Use **Settings → About → Export Logs** in the Web UI to download them.
- **Install from directory** in the Web UI takes a path **on the server / inside the container**. Bind-mount your build output, for example `-v "$PWD/dist:/plugins-dev/mirror-demo:ro"`, and enter `/plugins-dev/mirror-demo`. Uploading a `.fxplug` from the browser needs no mount.
- The dev loop is the same: **Reload** works from the Web UI.
- Without a desktop session, `desktop` views have nothing to show; the Web UI shows portable views only.
- `flux.fetch` runs on the server, so it sees the server's network, proxy settings and DNS — not your browser's.

## FAQ

**My edit to `resolver.js` has no effect.** Dev plugin or installed copy? Installed (zip/market) plugins are cached when they load: reinstall. For a dev plugin pointing at `dist/`, did the build finish and does `dist/resolve.js` contain the change?

**I changed `manifest.json` and nothing happened.** Press **Reload**. With the SDK also make sure `npm run dev` or `npm run build` copied it into `dist/`.

**Reload says “Reload failed: …”.** The text is the load failure (see [Install and validation errors](#install-and-validation-errors)). The dev registration stays in place; fix and reload again.

**The plugin row says Authorization required after Reload.** The manifest now asks for permissions or hosts you have not approved yet. Approve them; nothing runs before that.

**`console.log` output does not show up in my browser.** Plugin code runs in the daemon, not in the browser. Read the daemon log or export the logs, see [Logs](#logs).

**`flux.settings.x` is `undefined`.** The key is not declared in `manifest.settings`, or the setting has no value and no `default` (only `required` ones block the call).

**`setTimeout` / `URL` / `TextEncoder` is not defined.** The plugin environment is QuickJS without browser APIs. Available extras are `atob`, `btoa`, `queueMicrotask` and `performance.now()`; in `main` use `flux.timers`. TypeScript projects catch most of this, because `lib` excludes the DOM.

**Where can I see what a plugin did?** The audit log (**Settings → Extensions → Plugins → Audit log**, or per plugin **Audit records**) lists consents, installs, fetches, task operations and more — see [Permissions](/docs/en/plugins/permissions/).

**Still stuck?** Export the logs (**Settings → About → Export Logs**), and when reporting a problem include the plugin's identity and version, the exact error text from the table above, and the `[plugin:…]` lines around it.
