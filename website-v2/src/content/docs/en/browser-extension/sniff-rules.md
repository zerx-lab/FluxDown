---
title: Sniffing Rules Reference
description: What sniffing rules are, the five rule tiers, and every field of a rule — match conditions, stages, actions, limits, and revocation.
section: browser-extension
order: 3
---

Sniffing rules are small JSON documents that tell the extension **which network requests count as downloadable resources**, which URLs are really the same file, how to tell a video track from an audio track, where an API response hides the direct link, and which headers a download needs. Most pages work with the built-in rules; you only need this page when a site isn't detected correctly and you want to fix it yourself — or share the fix with everyone, see [Contributing Sniffing Rules](/docs/en/contributing/sniff-rules/).

Open the extension's options page → **Sniffing Rules**. There you'll find the rule list (with filters and a status for every rule), **Rule Sync**, your **Local Rules**, and the **Rule Tester**, which simulates a response and shows what the active rules would decide.

The list can be filtered by tier and searched by title, ID, host, extension, MIME or URL; it can be sorted by evaluation order or hit count, and shows each rule's priority, evaluation position and hit count for this session. Every tier's rules can be opened with "View rule" (except built-in rules in release builds) or copied to a local rule and modified; edit, delete and submit for local rules live in the list's action column. The Rule Tester lists every matching rule (marking the winner and the shadowed ones) and explains whether the response would end up captured; inside the local rule editor you can test an unsaved draft and add the result as a test case that is attached when you submit.

## Rule tiers and who can change them

Every rule belongs to one of five tiers. When several rules could apply, the tier decides who goes first:

| Tier | `tier` value | Where it comes from | Who can change it |
| --- | --- | --- | --- |
| Local | `local` | Rules you create on this device (Options → Sniffing Rules → Local Rules) | You — edit, import, export, delete |
| Official | `official` | Maintained in the public [rules repository](https://github.com/zerx-lab/fluxdown-sniff-rules); a snapshot ships inside the extension | Maintainers; you can only switch a rule on or off |
| Plugin | `plugin` | Rules declared by installed FluxDown plugins, delivered through the desktop app | The plugin author; you can switch rules on or off |
| Community | `community` | Contributed by authors through their own registered rule repositories (ids `community.<namespace>.<name>`) or the one-off `community.misc.*` submissions; see [Contributing](/docs/en/contributing/sniff-rules/) | The author, through their own repository or via review; you can only switch a rule on or off |
| Built-in | `builtin` | Shipped inside the extension | Updated with the extension; you can only switch a rule on or off |

**Priority: local > official > plugin > community > built-in.** Within one tier, a higher `priority` number goes first; rules with equal priority keep their order inside the pack. For rules that decide a single answer (such as "is this a resource or ignored?"), the **first rule that matches wins**.

How each tier stays up to date:

- **Built-in** rules and an **official** snapshot are bundled with the extension. The official pack can be hot-updated through the desktop app: when the desktop app serves a pack with a higher `sequence` than the bundled snapshot, the newer one replaces it — no extension release needed.
- **Community** rules need the desktop app: the desktop app fetches the pack from the rules repository (which picks up the registered repositories' Releases every hour), verifies its signature, and the extension syncs it from the desktop app (every 6 hours, or press **Sync now**). A community pack is valid for 30 days; if the pack has expired, or the revocation list is missing or expired, **the whole community pack pauses** (its rules show as *Expired*) until a fresh copy arrives.
- The **Enable community rules** switch turns the entire community pack off at once.
- A pack that needs a newer engine than your extension supports is skipped, and its rules show *Update extension*.

## Anatomy of a rule

```json
{
  "id": "community.example.video",
  "version": 1,
  "tier": "community",
  "title": "Example: treat .xyz files as video",
  "stage": "response",
  "match": { "host": ["example.com"], "ext": ["xyz"] },
  "exclude": [{ "url": ["*/preview/*"] }],
  "action": { "kind": "resource", "type": "video", "confidence": "high" }
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | Unique name: the tier, a dot, then 2–63 characters from lowercase letters, digits, `.`, `_`, `-`, starting with a letter or digit (e.g. `local.my-site.video`). The prefix must equal `tier`. Plugin rule ids are at most 64 characters in total. |
| `version` | yes | Positive integer. Bump it by one whenever you change a published rule — revocations apply by version. |
| `tier` | yes | `builtin`, `official`, `plugin`, `community`, or `local`; must match the pack the rule lives in. |
| `title` | no | Human-readable name shown in the rule list. |
| `enabled` | no | Defaults to `true`. A switch you flip in the rule list overrides it. |
| `priority` | no | Number, default `0`. Larger goes first within the same tier. |
| `stage` | yes | At which point of the pipeline the rule runs; decides which `action` kinds are allowed. See [Stages and actions](#stages-and-actions). |
| `requires` | no | Only `"sniffImages"`: the rule takes part in response classification only when image sniffing is switched on in settings. |
| `classifyOnly` | no | `true` = the rule is only used for "what type is this URL/MIME?" lookups, not for deciding live responses. |
| `match` | yes | Conditions a request must satisfy. See [Match conditions](#match-conditions). |
| `exclude` | no | A list of extra `match` objects. If **any** of them matches, the rule is treated as not matching. |
| `action` | yes | What to do. Its `kind` must be allowed by the rule's `stage`. |

Every rule in a pack must have a unique `id`. A rule that fails validation is dropped (the local editor and the rules repository both tell you why).

## Match conditions

`match` is an object whose fields are all optional. **When you write several fields, all of them must be satisfied.** Inside list fields (`host`, `url`, `ext`, `mime`, `resourceType`) **any one entry** is enough. `exclude` takes a list of such objects; if any one matches, the rule is skipped.

| Field | Type | Meaning |
| --- | --- | --- |
| `host` | list of strings | Host of the **requested URL**. `"example.com"` matches `example.com` and all its subdomains (`cdn.example.com`); `"=example.com"` matches only that exact host. Wildcard `"*.example.com"` is **not** supported. |
| `pageHost` | list of strings | Same syntax, applied to the host of the **top-level page** the user is on. |
| `url` | list of globs | Globs against the whole URL (including query). `*` is the only wildcard; the text before the first `*` must be a prefix, the text after the last `*` a suffix; case-insensitive. With no `*` the URL must match exactly. |
| `pathRegex` | string | Regular expression tested against the URL **path** only (no query), case-insensitive. Must fit the [safe subset](#regular-expression-safe-subset). |
| `ext` | list of strings | File extension in the URL path, lowercase and without the dot (`"mp4"`). URLs without an extension never match. |
| `hasExt` | boolean | `true` = the URL path has an extension, `false` = it has none. |
| `mime` | list of strings | Response `Content-Type`, compared in lowercase without parameters. An entry ending in `*` is a prefix (`"video/*"`); otherwise it is an exact value (`"application/x-mpegurl"`). A response without a MIME type never matches. |
| `resourceType` | list of strings | The browser's request type (`"media"`, `"xmlhttprequest"`, `"image"`, …). Requests observed from inside the page are reported as `"xmlhttprequest"`. |
| `status` | `[low, high]` | HTTP status within the closed range, e.g. `[200, 206]`. Needs a known status. |
| `minSize` / `maxSize` | number (bytes) | Size limits. If the size is **unknown**, the condition counts as satisfied. |
| `attachment` | boolean | Whether the response has `Content-Disposition: attachment`. |
| `header` | list of `{ "name", "regex" }` | Response headers; **every** listed header must be present and match its regex (case-insensitive). `name` is compared in lowercase. |

Examples:

```json
{ "host": ["example.com"], "ext": ["m3u8", "mpd"] }
{ "pageHost": ["=www.example.com"], "mime": ["video/*"], "minSize": 1048576 }
{ "host": ["cdn.example.com"], "pathRegex": "^/v/[0-9a-f]{8}/" }
{ "url": ["https://example.com/download/*"], "attachment": true }
{ "host": ["example.com"], "header": [{ "name": "x-content-kind", "regex": "^video$" }] }
```

Which fields have a value depends on the stage. The response stage sees everything. Other stages only know the request URL (and the page URL), so conditions such as `mime`, `status`, `size`, `attachment`, `header` or `resourceType` only make sense in the `response` stage (the `track` stage also knows the MIME type). In the `body` stage, `host` / `url` / `pathRegex` describe the **API URL** whose response is inspected, while `pageHost` describes the page that made the request.

### Regular expression safe subset

Every regular expression in a rule (`pathRegex`, `header[].regex`, and the `track` action's `pattern`) must:

- be at most **256 characters** and compile as a JavaScript regular expression (matched case-insensitively);
- not use back-references (`\1`, `\k<name>`);
- not use lookahead or lookbehind (`(?=`, `(?!`, `(?<=`, `(?<!`);
- not nest quantifiers such as `(a+)+`.

CSS selectors (`dom`, `title`, `player`) must be non-empty, at most **512 characters**, and valid.

## Stages and actions

A rule's `stage` says when it runs, and it fixes which `action.kind` values you can use:

| Stage | Meaning (label in the UI) | Allowed actions |
| --- | --- | --- |
| `response` | Decide what a network response is (Response) | `resource`, `ignore` |
| `normalize` | Treat URLs as the same file, find the extension, group mirrors (URL normalise) | `strip-params`, `filename`, `mirror` |
| `track` | Separate audio and video tracks (Audio/video track) | `track` |
| `body` | Read direct links out of an API's JSON response (Response body) | `extract`, `manifest` |
| `dom` | Read links out of page elements (Page element) | `dom` |
| `page` | Page-level hints (Page hints) | `title`, `player` |
| `request` | Extra headers when downloading (Download request) | `headers` |

### `resource` — treat as a resource (stage `response`)

Marks matching responses as downloadable resources.

| Field | Meaning |
| --- | --- |
| `type` | One of `video`, `audio`, `document`, `archive`, `image`, `executable`, `torrent`, `stream`, `subtitle`, `magnet`, `other`. |
| `confidence` | Optional: `high`, `medium` or `low`. |

```json
{
  "id": "local.example.xyz",
  "version": 1,
  "tier": "local",
  "title": "Example: .xyz is video",
  "stage": "response",
  "match": { "host": ["example.com"], "ext": ["xyz"] },
  "action": { "kind": "resource", "type": "video" }
}
```

Response rules are checked in tier/priority order and **the first match decides** — so a `resource` rule that's ahead of an `ignore` rule wins, and the other way round. Rules with `requires: "sniffImages"` are skipped unless image sniffing is on. When the extension only needs the *type* of a URL, it consults the `resource` rules: those with a `mime` condition first, then those with only `ext`, otherwise the type is `other`. The `ext` lists of all active `resource` rules also form the set of "known extensions" used by the `filename` action.

### `ignore` — never show (stage `response`)

No fields besides `kind`. Matching responses are dropped from the resource list.

```json
{
  "id": "local.example.no-thumbs",
  "version": 1,
  "tier": "local",
  "stage": "response",
  "match": { "host": ["img.example.com"], "pathRegex": "/thumb/" },
  "action": { "kind": "ignore" }
}
```

### `strip-params` — drop query parameters when comparing URLs (stage `normalize`)

`params` is a list of query parameter names (case-insensitive). When the extension decides whether two URLs are the same file, those parameters are removed, the fragment (`#…`) is dropped, and the remaining parameters are sorted. A rule whose `match` is empty (no condition at all, and no `exclude`) applies to every URL — only allowed for built-in, official and local rules. A scoped rule applies only to URLs it matches.

```json
{
  "id": "local.example.strip-token",
  "version": 1,
  "tier": "local",
  "stage": "normalize",
  "match": { "host": ["cdn.example.com"] },
  "action": { "kind": "strip-params", "params": ["token", "expires", "sign"] }
}
```

### `filename` — find the extension in the query (stage `normalize`)

For URLs whose path has no extension (`/download?file=a.zip`), take the extension from query parameters. `extFromQuery` is a list of parameter names, tried in order; a value is used when it is a known extension (see `resource` above). If none of them yields one, any query value ending in `.ext` with a known extension is used. The path's own extension always wins.

```json
{
  "id": "local.example.dl-ext",
  "version": 1,
  "tier": "local",
  "stage": "normalize",
  "match": { "host": ["example.com"], "pathRegex": "^/download$" },
  "action": { "kind": "filename", "extFromQuery": ["format", "file"] }
}
```

### `mirror` — mirror host groups (stage `normalize`)

`hosts` lists at least two host patterns (same syntax as `host`). Files with the same path on those hosts are treated as one file (used when merging manifests and recognising the playing stream). The rule's `match` isn't used to decide anything for mirror groups, but community and plugin rules must still include a `host` or `pageHost` condition.

```json
{
  "id": "community.example.mirrors",
  "version": 1,
  "tier": "community",
  "stage": "normalize",
  "match": { "pageHost": ["example.com"] },
  "action": { "kind": "mirror", "hosts": ["cdn1.example.net", "cdn2.example.net", "cdn3.example.net"] }
}
```

### `track` — audio or video track (stage `track`)

Many sites serve audio and video as separate files. This action reads a code out of the file name (or URL) and decides which track a file is.

| Field | Meaning |
| --- | --- |
| `source` | Always `"filenameOrUrl"`: the pattern is tested against the file name, or against the URL when there is none. |
| `pattern` | Regular expression (safe subset). |
| `group` | Which capture group holds the code (a non-negative integer; `0` is the whole match). |
| `audio` | Codes that mean **audio**. |
| `video` | Optional: codes that mean video. If omitted, every code not in `audio` counts as video. |
| `fallback` | `"video"`, `"audio"` or `"none"` — the answer when the pattern or the code doesn't match. |

Responses whose MIME type starts with `audio/` are always audio. The first matching `track` rule that gives an answer wins.

This is the official rule for bilibili's CDN (the real file also has a twin rule using `pageHost`):

```json
{
  "id": "official.bilibili.track",
  "version": 1,
  "tier": "official",
  "title": "bilibili: tell audio from video by m4s segment code (CDN domains)",
  "stage": "track",
  "match": { "host": ["bilibili.com", "bilivideo.com", "bilivideo.cn", "acgvideo.com"] },
  "action": {
    "kind": "track",
    "source": "filenameOrUrl",
    "pattern": "[-_](\\d{4,6})\\.m4s(?:[?#]|$)",
    "group": 1,
    "audio": ["30216", "30232", "30280", "30250", "30251"],
    "fallback": "video"
  }
}
```

For `…/x/xxx-1-30280.m4s` the capture group is `30280`, which is in `audio`, so the file is treated as the audio track; `…-30080.m4s` falls through to the `fallback` and counts as video.

### `extract` — read direct links from an API response (stage `body`)

Some sites don't expose the real file in the page; an API call returns JSON with the link. `extract` reads that JSON and produces resources.

| Field | Meaning |
| --- | --- |
| `when` | Optional list of `{ "path", "equals" }` conditions. **All** must hold, or the rule does nothing. `equals` is a string, number or boolean compared strictly (`1` is not `"1"`). |
| `each` | Optional dot-path to an **array** in the JSON; the rule then produces one resource per element. Without `each`, the whole JSON is the single element. If `each` doesn't point to an array, nothing is produced. |
| `fields` | A map of *field name → dot-path* read from each element. Values must be strings, numbers or booleans (stringified); anything else leaves the field undefined. |
| `emit` | How to build the resource from the fields. See [emit](#emit--building-a-resource). |

A **dot-path** walks the JSON with `.`: object keys by name and array items by index — `data.list.0.url` is the `url` of the first item of `data.list`. Keys are matched literally (a key that contains a dot can't be addressed).

The official rule for lanzou (蓝奏云), which turns the `ajaxm.php` response into a direct link:

```json
{
  "id": "official.lanzou.extract",
  "version": 1,
  "tier": "official",
  "title": "Lanzou: extract the direct link from the ajaxm.php response",
  "stage": "body",
  "match": { "url": ["*/ajaxm.php*"] },
  "action": {
    "kind": "extract",
    "when": [{ "path": "zt", "equals": 1 }],
    "fields": { "dom": "dom", "url": "url", "name": "inf" },
    "emit": {
      "mode": "preempt",
      "url": { "resolve": { "base": "${dom}", "ref": "${url}", "bareQueryPrefix": "/file/" } },
      "filename": "${name}"
    }
  }
}
```

For a response like `{ "zt": 1, "dom": "https://dl.cdn.com", "url": "?token", "inf": "a.zip" }` this yields `https://dl.cdn.com/file/?token` named `a.zip`; with `zt` ≠ 1, or a missing field, nothing is produced. (Community rules can't use `mode: "preempt"` — use `"resource"`.)

### `manifest` — declare a DASH manifest (stage `body`)

The JSON response contains a DASH structure (`video[]` and `audio[]` entries with `baseUrl`); the extension then handles it like a streaming manifest. `path` is an optional dot-path to the sub-tree holding that structure (default: the whole JSON).

```json
{
  "id": "community.example.dash-json",
  "version": 1,
  "tier": "community",
  "stage": "body",
  "match": { "host": ["api.example.com"], "pathRegex": "^/play/url$", "pageHost": ["example.com"] },
  "action": { "kind": "manifest", "path": "data.dash" }
}
```

### `dom` — read links from page elements (stage `dom`)

| Field | Meaning |
| --- | --- |
| `selector` | CSS selector of the elements to read (at most 512 characters). |
| `attr` | Attribute to read, e.g. `"data-src"`. For URL attributes (`href`, `src`, `data`, `poster`, `action`, `currentSrc`) the browser-resolved absolute address is used. Elements where the value is empty are skipped. |
| `emit` | How to build the resource. Template fields: `value` (the attribute value), `text` (the element's text, trimmed to 200 characters) and `page` (the page URL the rule matched). |

If the element is, or contains, a media element on the page, the produced resource is attached to that element (so the floating download button knows about it).

```json
{
  "id": "community.example.dom-video",
  "version": 1,
  "tier": "community",
  "title": "Example: take the link from data-video-url",
  "stage": "dom",
  "match": { "pageHost": ["example.com"], "pathRegex": "^/watch/" },
  "action": {
    "kind": "dom",
    "selector": "div.player[data-video-url]",
    "attr": "data-video-url",
    "emit": { "mode": "resource", "url": "${value}", "filename": "${text}", "type": "video" }
  }
}
```

### `title` — where the file name comes from (stage `page`)

Tells the extension which element holds the title of the video or audio being played. The title near the playing element becomes the candidate name and the default file name.

| Field | Meaning |
| --- | --- |
| `selector` | CSS selector. The matching node **closest to the media element** is used. |
| `attr` | Optional: read this attribute instead of the element's text. |

This is the official douyin rule:

```json
{
  "id": "official.douyin.title",
  "version": 1,
  "tier": "official",
  "title": "Douyin: use the video description as the file name",
  "stage": "page",
  "match": { "pageHost": ["douyin.com"] },
  "action": { "kind": "title", "selector": "[data-e2e=\"video-desc\"]" }
}
```

### `player` — how the floating button behaves (stage `page`)

Applies to media elements that match `selector`, or sit inside a matching element. `float` takes one of two values:

| `float` | Effect |
| --- | --- |
| `"hide"` | No floating download button — handy for background, ad or preview videos. |
| `"inset"` | A top-placed floating button stays at least `top` pixels below the top of the media element (`top` is an integer from 1 to 200), clear of the player's own top control strip (for example a mini-player's expand / close buttons). If the rest of the element is too short for the button, the button is hidden. Bottom placements are unaffected. Older extension versions don't know `inset` and skip the rule. |

```json
{
  "id": "community.example.hide-bg",
  "version": 1,
  "tier": "community",
  "stage": "page",
  "match": { "pageHost": ["example.com"] },
  "action": { "kind": "player", "selector": ".hero-bg video, .ad-slot video", "float": "hide" }
}
```

```json
{
  "id": "community.example.mini-player",
  "version": 1,
  "tier": "community",
  "stage": "page",
  "match": { "pageHost": ["example.com"] },
  "action": { "kind": "player", "selector": ".mini-player", "float": "inset", "top": 52 }
}
```

### `headers` — extra request headers (stage `request`)

Adds headers to the download request for URLs the rule matches (typically `Referer` or `Origin` for hot-link protection).

| Field | Meaning |
| --- | --- |
| `set` | A map of header name → value. 1 to 16 headers. Names: 1–64 letters, digits or `-`. Values: at most 1024 characters, no line breaks. |

These headers are **forbidden** because the browser or download engine owns them: `Cookie`, `Authorization`, `Proxy-Authorization`, `Host`, `Content-Length`, `Range`, `Connection`. If several rules set the same header, the first rule that matches wins.

```json
{
  "id": "community.example.referer",
  "version": 1,
  "tier": "community",
  "title": "Example: CDN requires a Referer",
  "stage": "request",
  "match": { "host": ["cdn.example.net"] },
  "action": { "kind": "headers", "set": { "Referer": "https://example.com/" } }
}
```

## emit — building a resource

`extract` and `dom` build resources with an `emit` object:

| Field | Meaning |
| --- | --- |
| `mode` | `"resource"` adds the link to the resource list. `"preempt"` is for one-time download addresses (such as a relay page): the link is **not** listed; the extension remembers it for 30 seconds so the browser's own download of that address isn't handed to FluxDown a second time. **Only built-in and official rules may use `preempt`.** |
| `url` | The link: a template string, or a `resolve` object (below). |
| `filename` | Optional template for the file name; ignored if it renders empty. |
| `type` | Optional resource type (same values as in `resource`). Without it, the type is worked out from the URL by the `resource` rules. |
| `quality` | Optional template for a quality label such as `1080p`; ignored if it renders empty. |

**Templates** support only `${name}` substitution — no expressions. In `extract`, `name` must be one of the keys of `fields`; in `dom`, one of `value`, `text`, `page`. Referring to anything else makes the rule invalid. If a field is **missing** when the rule runs (for example the JSON doesn't contain it), the template can't be rendered and **no resource is produced** — a missing `filename` or `quality` just omits that property.

**`url` as an object** is for APIs that return a relative reference:

```json
"url": { "resolve": { "base": "${dom}", "ref": "${url}", "bareQueryPrefix": "/file/" } }
```

Both `base` and `ref` are templates. The result depends on what `ref` looks like:

1. `ref` starts with `http://` or `https://` — used as is.
2. `ref` starts with `/` — joined onto the scheme and host of `base` (`https://dl.cdn.com` + `/file/?abc` → `https://dl.cdn.com/file/?abc`).
3. Anything else (a bare query like `?token`) — if `bareQueryPrefix` is set, the result is `base` + `bareQueryPrefix` + `ref` (`https://dl.cdn.com/file/?token`); without it, nothing is produced.

The final address must start with `http`.

## Limits by tier

| Restriction | builtin | official | plugin | community | local |
| --- | --- | --- | --- | --- | --- |
| `match` must contain `host` or `pageHost` | no | no | **yes** | **yes** | no |
| `emit.mode: "preempt"` | yes | yes | no | no | no |
| Global `strip-params` (no match conditions) | yes | yes | no | no | yes |
| `title` / `player` rules need `host` or `pageHost` | **yes** | **yes** | **yes** | **yes** | no |
| `headers` rules | need `host` or `pageHost` | need `host` or `pageHost` | not allowed | need `host` or `pageHost` | need `host` or `pageHost` |
| Download URLs produced by `extract` / `dom` must be on a domain within the rule's own `host` / `pageHost` (others are dropped) | no | no | no | **yes** | no |
| `mirror.hosts` must fall within the rule's `host` / `pageHost` | no | no | no | **yes** | no |

Limits that apply to every tier:

- At most **2000 rules** per pack.
- Regular expressions up to **256 characters** within the safe subset; CSS selectors up to **512 characters**.
- At most **16 headers** per `headers` rule, none from the forbidden list.
- `mirror` needs at least two hosts; `id` must be unique and its prefix must match `tier`.
- Host patterns must be valid domains; `"*.example.com"` is rejected.

## Revocation

If a rule turns out to be harmful or broken, maintainers can publish a **revocation list** entry for it. It works on any tier, including built-in rules.

| Field | Meaning |
| --- | --- |
| `ruleId` | The rule to revoke. |
| `versions` | `"*"` for every version, or `{ "max": N }` for versions up to and including `N`. A fixed rule with a higher `version` is unaffected. |
| `action` | `"soft"` or `"hard"`. |
| `reason` | Why — shown in the rule list. |
| `ref` | Optional link to the discussion. |

- **soft** — the rule is switched off and shows as *Revoked*, but you can press **Keep enabled** to turn it back on after you've read the reason. The confirmation belongs to the current revocation list, so it may have to be repeated when a new list is issued.
- **hard** — the rule is switched off and shows as *Revoked (locked)*; it can't be turned back on.

The revocation list itself is signed and expires; while it is missing or expired the community pack stays paused.

## More examples

**Treat `.m3u8` / `.mpd` on one site as streams, but ignore its ad domain** — two rules. The `ignore` rule has a higher `priority`, so it is checked first and the first match decides:

```json
{
  "id": "local.example.ads",
  "version": 1,
  "tier": "local",
  "priority": 10,
  "stage": "response",
  "match": { "pageHost": ["example.com"], "host": ["ads.example-cdn.com"] },
  "action": { "kind": "ignore" }
}
```

```json
{
  "id": "local.example.stream",
  "version": 1,
  "tier": "local",
  "stage": "response",
  "match": { "pageHost": ["example.com"], "ext": ["m3u8", "mpd"] },
  "action": { "kind": "resource", "type": "stream", "confidence": "high" }
}
```

**Only treat large `application/octet-stream` downloads as archives** — `minSize` and an `exclude`:

```json
{
  "id": "local.example.octet",
  "version": 1,
  "tier": "local",
  "stage": "response",
  "match": { "host": ["files.example.com"], "mime": ["application/octet-stream"], "minSize": 5242880 },
  "exclude": [{ "pathRegex": "/(thumb|preview)/" }],
  "action": { "kind": "resource", "type": "archive" }
}
```

The three official rules above (bilibili `track`, lanzou `extract`, douyin `title`) are complete, real examples; the full files live in the [rules repository](https://github.com/zerx-lab/fluxdown-sniff-rules/tree/main/rules/official). To try a rule, create it under **Local Rules**, then use the **Rule Tester**. When it works, share it — see [Contributing Sniffing Rules](/docs/en/contributing/sniff-rules/).
