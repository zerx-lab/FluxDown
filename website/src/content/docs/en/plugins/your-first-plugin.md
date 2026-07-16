---
title: Your First Plugin
description: Build, install and debug a working URL-resolver plugin step by step.
section: plugins
order: 2
---

This walkthrough builds a resolver plugin from scratch: it intercepts links from a fictional file host, calls the host's API to get the real download link, and hands that link to FluxDown. When you're done you'll know the full edit–test loop.

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
  "identity": "my-resolver@yourname",
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

- `identity` must match `name@author` using only lowercase letters, digits, `_` and `-`. No dots. It's your permanent ID — settings and storage are keyed by it.
- `version` is plain `MAJOR.MINOR.PATCH`.
- `match.urls` uses `*` as the only wildcard, matched case-insensitively. `"*://example-files.com/share/*"` matches both `http` and `https` links under that path.
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

- `ctx` carries `taskId`, `url`, `cookies`, `referrer`, `userAgent` and `extraHeaders` — everything the task knows.
- Returning `null` or `undefined` means "not mine, download the original URL".
- Throwing an error puts the task into the error state (fail-closed). That is correct behavior for a resolver: better to fail visibly than to save an HTML page with a `.mp4` name.
- `ephemeral: true` tells FluxDown the link is one-shot or anti-hotlinked, so it skips the extra HEAD probe that could burn the link. If your host's links are stable, leave it out — the probe gets you better resume integrity (ETag checks).
- `rangeSupported: true` promises the host serves HTTP Range requests. Without a probe FluxDown would otherwise start conservatively with a single connection; with the promise it plans multi-segment right away. Only declare it when you know the host supports Range — a false promise can waste quota-limited links.

## 4. Install in dev mode

In the desktop app: **Settings → Extensions → Plugins → install from directory**, pick `my-resolver/`, leave the **dev mode** switch on.

Dev mode records your folder's path instead of copying it, and re-reads `resolver.js` **on every invocation**. Your loop becomes:

1. Edit `resolver.js`, save.
2. Add (or resume) a matching download in FluxDown.
3. Read the result — no reinstall, no restart.

Only `manifest.json` changes need a reload: toggle the plugin off and on in the settings page.

## 5. Test and debug

Add a download with a URL matching your pattern. Watch what happens:

- **Success** — the task downloads from the resolved link. Task detail shows the resolved state.
- **Your script threw / timed out** — the task shows an error message prefixed with the plugin marker. Right-click the failed task for the "ignore plugin retry" escape hatch, which re-downloads using the original URL without your plugin.
- **Logs** — `flux.logger.*` and `console.log` both go to FluxDown's log file (`logs/fluxdown_YYYY-MM-DD.log` next to the app on Windows, `~/.local/share/fluxdown/logs/` on Linux). Log lines are truncated at 4 KB.

Common first-run mistakes:

- **Nothing happens at all** → your `match.urls` pattern doesn't match. Patterns are compared against the full URL; `example-files.com/*` does not match `https://example-files.com/...` because the first segment is prefix-anchored — use `*://example-files.com/*`.
- **`flux.fetch` rejects the URL** → only `http`/`https` to publicly routable addresses is allowed. Requests to `localhost`, LAN or cloud-metadata IPs are blocked by design.
- **Plugin got auto-disabled** → three consecutive timeouts or out-of-memory errors trip the circuit breaker. Fix the script, then re-enable it in settings.

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

The app renders a settings form for your plugin automatically. In the script, read it as `flux.settings.apiToken` — already typed as a string (numbers and booleans arrive as their real JS types too).

## 7. Ship it

Zip the folder into a `.fxplug` and share it, or publish to the plugin market — see [Packaging & market](/docs/en/plugins/packaging/).
