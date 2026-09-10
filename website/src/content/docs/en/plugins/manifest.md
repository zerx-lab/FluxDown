---
title: Manifest Reference
description: Every manifest.json field, the settings widget matrix, and URL pattern rules.
section: plugins
order: 3
---

`manifest.json` sits at the root of the plugin folder. All keys are camelCase. **Unknown keys are rejected** — a typo fails the whole manifest rather than being silently ignored.

## Top-level fields

| Field | Required | Type | Rules |
|---|---|---|---|
| `identity` | yes | string | `^[a-z0-9_-]+@[a-z0-9_-]+$` — lowercase name, `@`, author. Dots are forbidden. Permanent ID: settings, storage and the install folder are all keyed by it. |
| `name` | yes | string | Display name, must be non-empty. |
| `version` | yes | string | `MAJOR.MINOR.PATCH`, three integers. |
| `description` | no | string | Shown in the plugin list. |
| `homepage` | no | string | Project/author URL. |
| `icon` | no | string | Path relative to the plugin folder. No `..`, no absolute paths, no drive letters, no leading `/` or `\`. |
| `minAppVersion` | no | string | Three-part version. If the running FluxDown is older, the plugin is skipped at load (logged, not an error). |
| `resolvers` | no | array | **At most one entry** in v1. See below. |
| `hooks` | no | object | See below. |
| `permissions` | no | array | Capability grants. v1 accepts `"ffmpeg"` and `"ytdlp"`. Unknown values are rejected. |
| `settings` | no | array | Declarative settings fields. See below. |

A plugin may declare a resolver, hooks, or both. A manifest with neither is valid but does nothing.

## `resolvers[0]`

```json
{
  "resolvers": [
    {
      "match": { "urls": ["*://host.com/share/*", "*://cdn.host.com/*"] },
      "entry": "resolver.js",
      "timeoutMs": 15000
    }
  ]
}
```

| Field | Required | Rules |
|---|---|---|
| `match.urls` | yes | Non-empty list of URL patterns (see pattern rules below). |
| `entry` | yes | Script file, safe relative path. Must define `globalThis.resolve`. |
| `timeoutMs` | no | Per-call timeout in milliseconds. Must not be `0`. Replaces the 10 s default but is capped at the 30 s hard ceiling regardless of what you write. |

## `hooks`

```json
{
  "hooks": {
    "entry": "hooks.js",
    "events": ["onStart", "onDone", "onError"],
    "match": { "urls": ["*://host.com/*"] }
  }
}
```

| Field | Required | Rules |
|---|---|---|
| `entry` | yes | Script file, safe relative path. Must define a global function per subscribed event. |
| `events` | yes | Non-empty subset of `onStart`, `onError`, `onDone`, `onMetaProbed`, `onCancel`. Anything else is rejected. |
| `match` | no | Optional URL filter, same pattern rules. If present, `urls` must be non-empty. Omitted = the hooks fire for every task. |

Caveat: if the same plugin also declares a resolver, `onMetaProbed` never fires for its tasks — resolver tasks skip the metadata probe entirely. FluxDown logs a warning if you subscribe to it anyway.


## `permissions`

Extra host capabilities a plugin opts into. Empty or omitted = the base sandbox (network via `flux.fetch`, `flux.storage`, logging). v1 recognises two values:

| Value | Grants |
|---|---|
| `ffmpeg` | The `flux.ffmpeg` API — run the resolved ffmpeg on a finished file (see the [API reference](/docs/en/plugins/api-reference/)). |
| `ytdlp` | The `flux.ytdlp` API — run the resolved yt-dlp from `resolve` or any hook (see the [API reference](/docs/en/plugins/api-reference/)). |

```json
{ "permissions": ["ffmpeg", "ytdlp"] }
```

Unknown values fail the whole manifest, so an older FluxDown that doesn't know a permission rejects the plugin rather than silently ignoring it — pair a new permission with a `minAppVersion` bump.

## `settings[]`

Each entry describes one field; the app generates the form. Keys must be unique and non-empty.

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
| `key` | yes | Unique within the plugin. Read back as `flux.settings.<key>`. |
| `title` | yes | Form label. |
| `description` | no | Help text below the field. |
| `type` | yes | `string`, `number`, or `boolean`. |
| `widget` | no | Defaults by type: string→`text`, number→`number`, boolean→`toggle`. |
| `options` | select only | Non-empty `{value, label}` list. |
| `default` | no | **Always a string**, even for numbers (`"3"`) and booleans (`"true"`/`"false"`). For select, must be one of the option values; for number, must parse and fall inside `min`/`max`. |
| `required` | no | The form refuses to save an empty value. |
| `min` / `max` | number only | Finite, inclusive bounds, `min ≤ max`. |
| `pattern` | string only | A **JavaScript RegExp** (not Rust regex) validated against the value on save. |
| `helperScript` | string only | A snippet the host shows a **Copy** button for, next to the field. The host only copies the text to the clipboard — it never runs it. The user pastes it into the target site's browser DevTools console (typical use: read `document.cookie` and copy a login cookie back into the field). Non-empty, ≤ 4 KB. |
| `helperLabel` | with `helperScript` | Button label (≤ 60 chars). Only valid together with `helperScript`; defaults to a generic "Copy helper script" when omitted. |

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

Examples:

| Pattern | `https://www.youtube.com/watch?v=abc` |
|---|---|
| `*://www.youtube.com/watch*` | matches |
| `https://x.com/a` (no wildcard) | exact match only |
| `*://x.com/*` vs `https://y.com/a` | no match — `://x.com/` not found |
| `youtube.com/*` | no match — pattern is prefix-anchored, URL starts with `https://` |

## Validation summary

The manifest is validated when the plugin is installed and every time it's loaded. On failure the plugin is skipped and the reason lands in the log. The checks, in order: identity format → name non-empty → version format → `minAppVersion` format → icon path safety → at most one resolver → resolver entry path / non-empty `match.urls` / `timeoutMs ≠ 0` → hooks entry path / non-empty valid `events` / non-empty `match.urls` if present → settings key uniqueness and the per-field rules above → `permissions` are all recognised values.

Both entry scripts are also compile-checked at install time — a syntax error is rejected up front rather than discovered on the first download.
