---
title: HTTP / HTTPS
description: Multi-threaded segmented HTTP/HTTPS downloads with dynamic resegmentation, resume and checksum verification.
section: protocols
order: 1
---

HTTP and HTTPS are FluxDown's default protocol — anything that isn't a magnet link, a `.torrent` file, or an `.m3u8`/`.mpd` stream URL is downloaded this way. The engine splits large files across multiple connections, adapts the connection count while the download is running, and resumes exactly where it left off after a pause, crash, or restart.

## Intelligent segmentation

When you start an HTTP(S) download, FluxDown first probes the server (a `HEAD` request, or a `GET` with a small `Range`) to learn the file size and whether the server honours `Range` requests at all. Based on that it decides how many parallel connections ("threads") to use:

- Files of 2 MB or smaller always download on a single connection — there's no measurable benefit from splitting them.
- Larger files get one segment per roughly 1 MB, up to 4× your CPU's logical core count, capped at 64 segments total.
- If a short bandwidth probe is available, the segment count is then scaled down on slow links (as low as 50% of the size-based estimate below ~512 KB/s) and scaled up toward the full estimate as measured speed increases past ~5 MB/s and ~50 MB/s.
- If the server doesn't support `Range` requests, or the file size can't be determined, FluxDown automatically falls back to a single connection.

As an illustration, on an 8-core machine with a fast connection, segment count roughly follows file size: ≤2 MB → 1, ~4 MB → 4, ~8 MB → 8, ~16 MB → 16, ≥32 MB → 32 (the ceiling on an 8-core machine; it only grows further on machines with more cores, up to the hard limit of 64).

You don't have to rely on the automatic choice — the **Threads** selector in the New Download dialog and in *Settings → Download → Default Threads* lets you pick Auto, a preset (4/8/16/32/64), or a custom value from 1–512. Above 64 connections FluxDown shows a warning: many servers throttle or refuse that many concurrent connections from one IP, and security protection (WAF / CDN) may ban your IP; above 256 the warning escalates.

<!-- TODO(screenshot): New Download dialog with the Threads selector open, showing Auto / 4 / 8 / 16 / 32 / 64 / Custom -->

### Splitting while the download runs

FluxDown doesn't just fix the segment count at the start. While segments are downloading, any worker that finishes early looks for the largest segment still in progress and takes half of its remaining bytes — an "in-half division" that keeps every connection busy instead of idling once its own segment is done. This also runs proactively on a timer so a new chunk of work is usually ready the instant a worker becomes free. Near the very end of a download, segments as small as 64 KB can still be split this way, which avoids the classic "stuck at 99%" symptom where one slow segment blocks completion while everything else has already finished. You can watch this happen live in a task's detail panel, under the **Segments** tab, which visualizes each segment and animates every split.

<!-- TODO(screenshot): Task detail panel, Segments tab, showing the live segment visualization mid-split -->

## Resume support

Download progress — including the exact byte ranges of every segment — is written to FluxDown's local database roughly every 3 seconds. Pausing a task, closing the app, or a crash therefore loses at most a few seconds of progress, never the whole file. On resume:

- FluxDown re-validates the remote file (via `ETag`/`Last-Modified`) before continuing. If the file changed on the server since the download started, it discards the partial data and starts over instead of silently splicing old and new bytes together.
- If the remaining `Range` requests keep returning `200 OK` instead of `206 Partial Content`, FluxDown can try a full GET without `Range`. With at least 8 MiB already downloaded, it writes the replacement separately and keeps the original checkpoint until the new file is verified and saved. Progress starts again from zero because the whole file is downloaded again, and both copies need disk space until the replacement succeeds.
  If the full GET fails, FluxDown keeps the original checkpoint and waits for you to resume manually. If the server still ignores `Range`, manual resume can trigger another full download. If the server sends compressed data despite `Accept-Encoding: identity`, FluxDown stops the fallback and keeps the checkpoint.
  For transport failures, the task error includes the underlying cause and any detected network failure category. This is diagnostic information, not a signal to retry automatically.
  When a task starts again or is deleted, FluxDown tries to clear its `.fluxdown-restart-*` staging directory in the current save location. A crash or forced stop can leave a staging file as large as the download. If the save location changes, the old location is not checked; the fallback log identifies the staging path.
  If removing the old checkpoint fails after the replacement is saved, FluxDown logs the failure and leaves any remaining checkpoint data in place. If FluxDown crashes after naming the final file but before recording completion, the old checkpoint may remain, and a later resume may download the file again under a deduplicated name.

## Cookies, headers, User-Agent and Referer

The New Download dialog's advanced options let you tailor requests for sites that need authentication or a specific browser identity:

| Field | Purpose |
|---|---|
| Cookie | Sent as-is, format `name=value; name2=value2`. Leave empty to send no cookies. |
| Custom Headers | Arbitrary extra key/value HTTP headers (use the Cookie field above for cookies, not a header row). |
| User-Agent | Presets for Chrome (default), Firefox, Edge, or a custom string. Leave empty to use the global default from *Settings → Download → User-Agent*. |

FluxDown's built-in default User-Agent is a neutral `FluxDown/1.0` string rather than an impersonated Chrome UA — Cloudflare's bot detection compares the declared browser against the TLS fingerprint, and a non-Chrome TLS stack paired with a Chrome UA gets flagged as bot traffic on some CDNs. When a download is captured by the browser extension, the real captured User-Agent is used first; if the server rejects that request, FluxDown automatically retries once without it.

There's no separate manual Referer field: Referer is only sent for downloads captured by the browser extension, which forwards the originating page's real Referer automatically. Downloads you paste in yourself don't send one.

<!-- TODO(screenshot): New Download dialog advanced options, showing Cookie, User-Agent and Custom Headers fields -->

## Checksum verification

Paste a hash into the **Hash Verification** field (advanced options) and pick an algorithm — `md5`, `sha-1`, `sha-256` (default), or `sha-512` — and FluxDown verifies the finished file against it before marking the task complete. If the hash doesn't match, the downloaded file is deleted and the task is marked as failed so you know not to trust it; you'll need to start the download again.

## Proxy

Each task can use its own proxy, overriding the global setting in *Settings → Proxy*. Supported formats:

```
http://host:port
socks5://host:port
http://user:pass@host:port
socks5://user:pass@host:port
```

Supported types: `http`, `https`, `socks4`, `socks5`. Leave the field empty to use the global proxy configuration (None / System / Manual).

## Retries and error recovery

- **Probing** the file's name/size/Range support is retried up to 3 times with exponential backoff (1s, then 2s). The final retry drops any browser-captured User-Agent to work around Cloudflare's bot detection.
- **In-flight segments**: if a segment receives no data for 5 seconds it's treated as stalled — FluxDown drops that connection and resumes it from the last confirmed byte via `Range`, up to 5 attempts with increasing backoff (2s, 4s, 8s, 16s), roughly 80 seconds of total tolerance for one segment.
- **Server rejections**: FluxDown lowers concurrency when parallel requests are refused, without interrupting connections still transferring data. If no other transfer is still active, HTTP 429 (or a 403 after confirmed parallel refusals) can trigger up to three backoff retries with one connection from the existing segment offsets. An isolated 429 does not change the learned cap; normal ramp-up can resume once the server accepts the retry. If Range requests are still rate-limited after all retries and there is no data to preserve, FluxDown tries one GET without a Range header. Caps learned from parallel refusals expire after 24 hours and do not prevent the first encounter with a server limit.
- **Whole-task auto-retry**: if a task ultimately fails from a transient error, FluxDown can automatically resume it later — configurable in *Settings → Download → Auto-retry Attempts* (off, a fixed count up to 10, or unlimited) and *Retry Interval* (the wait before each attempt, increasing per attempt).

## Related settings

| Setting | Location |
|---|---|
| Default Threads | Settings → Download |
| Max Concurrent Downloads | Settings → Download |
| Speed Limit | Settings → Download |
| Auto-retry Attempts / Retry Interval | Settings → Download |
| User-Agent | Settings → Download |
| Proxy (None / System / Manual) | Settings → Proxy |

## Limitations & FAQ

**Why did my download start as a single connection?** Either the file is 2 MB or smaller, or the server doesn't support `Range` requests — both cases make multi-threading pointless or impossible, so FluxDown uses one connection automatically.

**The server returns compressed data — does that break the file?** No. FluxDown detects `Content-Encoding` (gzip, br, deflate, zstd) and transparently decompresses the stream while writing, so the saved file matches the original content. An encoding FluxDown doesn't recognize fails the download instead of silently writing corrupted bytes.

**A one-time signed download link stopped working.** If the browser extension already reported the file size, FluxDown skips the probe entirely so the link isn't "used up" by a preliminary request before the real download starts. Manually pasted signed links that expire quickly can still be consumed by the probe itself — download them promptly after copying.
