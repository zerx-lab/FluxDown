---
title: Your First Plugin
description: Build, install and debug a working URL-resolver plugin in plain JavaScript, step by step.
section: plugins
order: 2
---

This walkthrough builds a resolver plugin from scratch with nothing but a text editor: it intercepts links from a fictional file host, calls the host's API to get the real download link, and hands that link to FluxDown. When you are done you will know the full edit–test loop.

If you would rather have types, several source files and a UI, follow [TypeScript project](/docs/en/plugins/typescript-project/) instead; the concepts are the same. The [overview](/docs/en/plugins/overview/) explains how to choose.

## 1. Create the folder

A plugin is just a folder. Make one anywhere on disk:

```
my-resolver/
├── manifest.json
└── resolver.js
```

## 2. Write the manifest

`manifest.json` declares who you are and which URLs you want:

```json
{
  "identity": "yourname@my-resolver",
  "name": "Example Host Resolver",
  "version": "1.0.0",
  "description": "Resolves example-files.com share links to direct downloads.",
  "resolvers": [
    {
      "match": { "urls": ["*://example-files.com/share/*"] },
      "entry": "resolver.js"
    }
  ]
}
```

Notes on the fields (full list in the [manifest reference](/docs/en/plugins/manifest/)):

- `identity` must match `author@name` using only lowercase letters, digits, `_` and `-`. No dots. It is your permanent ID — settings and storage are keyed by it.
- `version` is plain `MAJOR.MINOR.PATCH`.
- `match.urls` uses `*` as the only wildcard, matched case-insensitively against the whole URL. `"*://example-files.com/share/*"` matches both `http` and `https` links under that path.
- `entry` is a path relative to the plugin folder. `..`, absolute paths and drive letters are rejected.

## 3. Write the resolver

`resolver.js` must define a global function named `resolve`. A top-level `function` declaration is enough — scripts are loaded as classic scripts, so it lands on `globalThis` automatically:

```js
async function resolve(ctx) {
  // ctx.url is the original task URL, e.g. https://example-files.com/share/abc123
  const id = ctx.url.split("/share/")[1];
  if (!id) return null; // null/undefined = pass through, FluxDown downloads ctx.url as-is

  // Call the host's API for the real link.
  const res = await flux.fetch({
    url: "https://example-files.com/api/file/" + id,
    headers: { "Referer": ctx.url },
  });
  if (res.status !== 200) {
    throw new Error("API returned " + res.status);
  }

  const data = JSON.parse(res.body);
  flux.logger.info("resolved", ctx.url, "->", data.directUrl);

  return {
    url: data.directUrl,        // required: the direct download link
    fileName: data.name,        // optional: override the file name
    totalBytes: data.size,      // optional: file size, if the API tells you
    ephemeral: true,            // the link is signed/expiring -> skip the metadata probe
    rangeSupported: true,       // the host honours Range -> keep multi-segment downloads
  };
}
```

What each part means:

- `ctx` carries `taskId`, `url`, `cookies`, `referrer`, `userAgent` and `extraHeaders` — everything the task knows (plus `authRef` and `resolverItem`, see the [API reference](/docs/en/plugins/api-reference/)).
- Returning `null` or `undefined` means "not mine, download the original URL".
- Throwing an error puts the task into the error state (fail-closed). That is correct behavior for a resolver: better to fail visibly than to save an HTML page with a `.mp4` name.
- `ephemeral: true` tells FluxDown the link is one-shot or anti-hotlinked, so it skips the extra HEAD probe that could burn the link. If your host's links are stable, leave it out — the probe gets you better resume integrity (ETag checks).
- `rangeSupported: true` promises the host serves HTTP Range requests. Without a probe FluxDown would otherwise start conservatively with a single connection; with the promise it plans multi-segment right away. Only declare it when you know the host supports Range — a false promise can waste quota-limited links.
- Keep `flux.*` calls **inside** `resolve`. When you install the plugin, FluxDown evaluates the script once in a bare context (no `flux`) just to check that it compiles, so a top-level `flux.logger.info(...)` would fail the install.

## 4. Install in dev mode

In the desktop app: **Settings → Extensions → Plugins**, turn the **Dev mode** switch on (it is off by default; the install area appears once it is on), choose `my-resolver/` under **Install from directory (dev mode)** and press **Install**. In the Web UI the same field takes the path **on the machine that runs FluxDown**, not on the machine with the browser.

Dev mode records your folder's path instead of copying it. The plugin row gets a **Dev** badge and a **Reload** button. This plugin declares no permissions, so it is enabled right away. (A plugin that declares permissions starts as **Authorization required**: the app opens an approval dialog, and the enable switch stays off until you grant them — see [Permissions](/docs/en/plugins/permissions/).)

Your edit loop now depends on what you change:

| You edit | What to do |
|---|---|
| `resolver.js` (and hooks, auth, subscription scripts) | Save, then add or resume a matching download. These files are **re-read on every call** — no reinstall, no reload. |
| `manifest.json` (patterns, version, settings, permissions, entry paths) | Press **Reload** on the plugin row. It re-reads the manifest and every script, re-runs the install checks, and shows the error if something is wrong — your dev registration is kept so you can fix it and reload again. |
| a resident `main` script (views, commands — see [UI guide](/docs/en/plugins/ui-guide/)) | Press **Reload**. Unlike a resolver, `main` keeps running between calls and is not re-read on every call. |

## 5. Test and debug

Add a download with a URL matching your pattern. Watch what happens:

- **Success** — the task downloads from the resolved link. The task detail shows the resolved state.
- **Your script threw / timed out** — the task shows an error such as `[插件] yourname@my-resolver: 插件运行时错误: API returned 404` (`[插件]` = “plugin”, `插件运行时错误` = “plugin runtime error”, followed by your error message). Right-click the failed task and choose **Ignore plugin retry** to download the original URL without your plugin.
- **Logs** — `flux.logger.*` and `console.log` both write to FluxDown's background-service log, each line tagged `[plugin:yourname@my-resolver]`; lines are truncated at 4 KB. Search the log file for `[plugin:`. The log lives in `logs/` inside FluxDown's data directory:

  | System | Log directory |
  |---|---|
  | Linux | `$XDG_DATA_HOME/fluxdown/logs/` (default `~/.local/share/fluxdown/logs/`) |
  | macOS | `~/Library/Application Support/fluxdown/logs/` |
  | Windows, installed | `%LOCALAPPDATA%\FluxDown\logs\` |
  | Windows, portable | `<app folder>\portable_data\logs\` |
  | Any system with `FLUXDOWN_DATA_DIR` set (servers, Docker) | `$FLUXDOWN_DATA_DIR/logs/` |

  The files are named `fluxdown_YYYY-MM-DD.log`. The easiest route is **Settings → About → Export Logs**, which bundles the daemon logs in a zip. Note that the menu entry **Open Logs Folder** opens the desktop app's own diagnostic logs, not the plugin output. More in [Debugging](/docs/en/plugins/debugging/).

Common first-run mistakes:

- **Nothing happens at all** → your `match.urls` pattern doesn't match. Patterns are compared against the full URL and the first and last segments are anchored; `example-files.com/*` does not match `https://example-files.com/...` because the part before the first `*` must be a prefix of the URL — use `*://example-files.com/*`.
- **`flux.fetch` rejects the URL** → only `http`/`https` to publicly routable addresses is allowed. Requests to `localhost`, LAN or cloud-metadata IPs fail by design with `插件输出非法: blocked: non-routable IP` (`插件输出非法` = “plugin output invalid”).
- **The install fails with `脚本编译失败: …`** (“script compile failed”) → a syntax error, or top-level code that uses `flux`. Fix the script and install again; if the plugin is already installed, press **Reload** instead.
- **Plugin got auto-disabled** → three consecutive timeouts or out-of-memory errors in the resolver trip the circuit breaker. Fix the script, then re-enable it in settings.

To check the plumbing before the real host exists, temporarily make the resolver return a fixed link and add a download for `https://example-files.com/share/test`:

```js
async function resolve(ctx) {
  flux.logger.info("resolve called for", ctx.url);
  return { url: "https://proof.ovh.net/files/1Mb.dat", fileName: "test.dat" };
}
```

The log shows `[plugin:yourname@my-resolver] resolve called for https://example-files.com/share/test`, and the task downloads the test file. (The original host does not need to exist: the resolver runs before any connection to it.)

## 6. Add a setting (optional)

Suppose the host needs an API token. Declare it in the manifest:

```json
{
  "settings": [
    {
      "key": "apiToken",
      "title": "API token",
      "description": "Get one at example-files.com/account.",
      "type": "string",
      "widget": "password",
      "required": true
    }
  ]
}
```

Press **Reload**; the app now renders a settings form for your plugin (gear button on the plugin row). In the script, read it as `flux.settings.apiToken` — typed as a string (numbers and booleans arrive as their real JS types too):

```js
const res = await flux.fetch({
  url: "https://example-files.com/api/file/" + id,
  headers: { "Authorization": "Bearer " + flux.settings.apiToken },
});
```

Because the field is `required`, a download that starts before the token is filled in fails with `缺少必填设置项: 插件 yourname@my-resolver 需先配置「API token」` (“missing required setting: the plugin needs “API token” configured first”) instead of calling your script.

## 7. Ship it

Zip the folder into a `.fxplug` and share it, or publish it to the plugin market — see [Packaging & market](/docs/en/plugins/packaging/).

## Next steps

- [TypeScript project](/docs/en/plugins/typescript-project/) — the same plugin with types, plus a hook, a view and settings, built with the SDK.
- [Hooks guide](/docs/en/plugins/hooks-guide/) — react to task events and post-process finished files.
- [UI guide](/docs/en/plugins/ui-guide/) — pages, commands and settings tabs from a resident `main` script.
- [Debugging](/docs/en/plugins/debugging/) — logs, reload rules and an error-message table.
