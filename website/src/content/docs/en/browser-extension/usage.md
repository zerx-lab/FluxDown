---
title: Using the Extension
description: How download interception, interception modes, resource sniffing, and the popup controls work.
section: browser-extension
order: 2
---

## How download interception works

When **Download Intercept** is on (it is by default) and you click something the extension recognizes as a downloadable file, FluxDown steps in before the browser's own download UI appears — the native "Save As" prompt and download bar stay hidden — and hands the request to the FluxDown desktop app over the local connection described in [Install the Extension](/docs/en/browser-extension/install/). By default the desktop app then opens its own quick-download dialog so you can confirm the filename, save folder, and thread count before the transfer starts. This works the same way whether the download came from a plain link, a JavaScript redirect, or a form submission — the extension catches it regardless of how the page triggered it.

If the desktop app can't be reached at the moment a download is attempted, the extension automatically lets the browser download the file normally instead of blocking it, and shows one "FluxDown app not detected" notification. It won't repeat that notification for further downloads for about 30 seconds, and it silently resumes intercepting as soon as the app answers again.

<!-- TODO(screenshot): a browser download click flowing into FluxDown's quick-download confirmation dialog -->

## Choose an interception mode

Open the popup → **Quick Settings** → **Intercept Mode**:

| Mode | Label in the popup | Behavior |
| --- | --- | --- |
| `smart` | Smart *(default)* | Combines file extension, MIME type, and file size. Falls back to intercepting anything the browser itself already treats as a download, unless it looks like an ordinary web resource (HTML/CSS/JS, small inline images). |
| `extension` | Extension Only | Only intercepts when the filename or URL ends in one of the extensions configured below. Nothing else is considered. |
| `all` | Intercept All | Intercepts every download except URLs on an excluded domain. |

Whichever mode is active, two checks always run first: excluded domains are skipped outright, and files below **Min File Size** are left to the browser.

## Fine-tune what gets intercepted

- **Min File Size** (Quick Settings): `No limit`, `100 KB`, `512 KB`, `1 MB`, `5 MB`, or `10 MB`. Downloads smaller than this are left to the browser — handy for keeping tiny files like icons or snippets out of FluxDown. It only applies when the browser reports a size; downloads with an unknown size are still evaluated normally.
- **Intercept File Types**: the extension ships with roughly three dozen extensions pre-listed — archives, installers, disk images, video, audio, office documents, plus `.apk`, `.ipa`, and `.torrent`. Click the **+** button to add one (`pdf` or `.pdf` both work) and the **×** on a tag to remove it. This list is the only thing that matters in Extension Only mode; in Smart mode it's one of several signals.
- **Excluded Domains**: downloads from a listed domain are always left to the browser, regardless of mode. Click **+** to type a domain in manually, or **Current Site** to exclude whatever domain the active tab is on with one click. Remove one with the **×** next to it.

<!-- TODO(screenshot): popup Intercept File Types and Excluded Domains sections with a few entries added -->

## Turn interception on or off

The **Download Intercept** toggle at the top of the popup is the master switch. Turn it off and every download goes back through the browser's normal flow, including the confirmation dialog interception would otherwise suppress.

You don't need to open the popup to flip it: press **Alt+Shift+D** anywhere in the browser to toggle interception instantly, confirmed by a brief system notification. That's the fastest way to grab one file with the browser's own downloader — flip it off, click the link, flip it back on — for example on a site where FluxDown's detection guesses wrong for that one link. (Remap the shortcut from the browser's extension-shortcuts settings page if Alt+Shift+D collides with something else.) For a site you never want intercepted, add it to **Excluded Domains** instead — it's permanent and doesn't rely on remembering to switch anything back on.

The toolbar icon itself reflects the current state too (it dims when interception is off), so you can tell at a glance without opening the popup.

## The fluxdown:// protocol mode

Normally the extension hands downloads to the FluxDown app over Native Messaging. The **FluxDown Protocol** toggle in the popup's Quick Settings and on the options page (off by default) switches to a different channel: an intercepted download navigates to a `fluxdown://download?url=...&filename=...` URL, and the operating system wakes whatever app is registered for that protocol — the FluxDown desktop or Android app.

Where you'd use it depends on the platform:

- **Android** — this is the *only* channel. Android browsers that support extensions (Edge, Firefox, Kiwi, and other Chromium forks) have no Native Messaging at all, so the toggle must be on for interception to reach the app. The protocol URL fires a system VIEW intent that wakes the FluxDown Android app and opens its new-download sheet with the URL — and the suggested filename, when the page provided one — already filled in; you confirm the folder and thread count and start the download there. Batch downloads (for example a multi-select from the resource panel) are delivered one link at a time, a moment apart, and merge into the same open sheet as extra lines, so a batch becomes one form with all the URLs in it.
- **Desktop (Windows, macOS, Linux)** — the toggle works here too: with it on, downloads reach the app through the protocol handler instead of Native Messaging, landing in the same external-download flow (quick-download confirmation, or silent task creation if you enabled no-prompt downloads). Native Messaging remains the better channel — it carries cookies, headers, request method, and body, which the protocol cannot — so keep the toggle off on desktop unless Native Messaging is unavailable, for example under a browser policy that blocks it or a portable setup where the host was never registered.

Two limitations apply on every platform:

- **No credentials travel with the link.** The protocol URL carries only the address and a filename hint — cookies, `Authorization` headers, request method, and body all stay behind in the browser. Downloads that need a logged-in session will come out as an error page or a permission denial; the protocol mode is for publicly reachable files.
- **Paired video+audio downloads fall back to the browser.** A protocol URL can express only one address, so sniffed media that needs a separate audio track merged in can't be handed off this way — rather than produce a silent video, the extension lets the browser download it normally.

It goes without saying that the [FluxDown app](/#download) must be installed on the device — without it, nothing is registered to answer the `fluxdown://` URL and the navigation fizzles.

## Send links, images, and media to FluxDown from the right-click menu

Right-clicking exposes FluxDown entries independent of the Download Intercept toggle — they work even while interception is off:

| Right-click target | Menu item |
| --- | --- |
| A link | Download this link with FluxDown |
| An image | Download this image with FluxDown |
| A video or audio element | Download this video/audio with FluxDown |
| Empty page area | Download this page with FluxDown |

The last option sends the current page's own URL to FluxDown — useful when the "page" the browser opened is actually a raw file, like a video or PDF viewed directly in a tab.

## Detect media and downloadable resources on a page

FluxDown continuously scans each page for downloadable resources — video and audio elements, streaming manifests (HLS `.m3u8`, DASH `.mpd`), subtitles, and links or network responses that look like files — combining a DOM scan with live monitoring of the page's network activity. Detected items are classified (video, audio, document, archive, image, torrent, stream, subtitle, magnet link, other) and lightly filtered, so the list stays about real downloadable content instead of every icon, ad ping, or preload fragment: very small files are dropped per type, tracker/analytics traffic is ignored, and raw streaming segments stay hidden unless they're large enough to plausibly be a real file on their own. Image sniffing is off by default, so ordinary page images don't clutter the results.

Two pieces of on-page UI surface what's found:

- **Floating ball** — a small draggable dot that docks to the left or right edge of the page (drag it and let go; it snaps to the nearest edge) and shows a badge with the number of resources found. Click it to open the resource panel. Hide it from the popup's **Floating Ball** toggle, or from the eye icon in the resource panel's own header, if you'd rather not see it — detection and the toolbar badge keep working either way.
- **Resource panel** — opens next to the floating ball, listing detected resources grouped into tabs (All, Video, Audio, Docs, Archive, Stream, Subtitle, Magnet, Other) with per-tab counts. Check individual items — or use **Select All** — and click the **Download** button to send everything checked to FluxDown in one batch, or use the small download button on a single row to grab just that one.

Hovering over a video element also pops up a small floating download button for that video specifically, without opening the panel.

The extension's toolbar icon shows a numeric badge with the resource count for the current tab, so you know something downloadable is on the page without opening anything.

<!-- TODO(screenshot): floating ball docked to the page edge next to an open resource panel with tabs and a batch selection -->

### Install the FFmpeg component for merged output

Many of the resources you'll sniff — streaming manifests especially — don't arrive as one finished file. DASH streams (`.mpd`) routinely split audio and video into separate tracks, so downloading one produces two files (for example `video.mp4` and `video.audio.m4a`) unless FluxDown can merge them. That merge is done with **FFmpeg** (a fast stream copy, no re-encoding), which isn't bundled with the app — it's an optional component you install on demand from **Settings → Extensions → Components** inside the desktop app. With it installed, DASH audio/video tracks are merged into a single playable file automatically; without it, the tracks are kept separate for you to merge yourself. Installing FFmpeg also unlocks plugins that declare the `ffmpeg` capability (see the [plugin API reference](/docs/en/plugins/api-reference/)), and installs `ffprobe` alongside it. HLS (`.m3u8`) downloads don't need FFmpeg — FluxDown reassembles and remuxes those to MP4 on its own.

## Track today's stats

The popup's **Today's Stats** section shows two running counters for the current day: **Intercepted** (downloads successfully handed off to FluxDown) and **Failed** (hand-offs that didn't go through, typically because the desktop app was unreachable at the time). Both reset automatically at the start of a new day, or immediately if you click **Reset Stats** in the popup's footer.

## Switch theme and language

- **Theme** — the sun/moon icon in the header toggles the popup between light and dark. Until you click it, the popup follows your OS theme automatically.
- **Language** — the language button in the header (showing `EN` or `中`) switches the popup and the on-page floating ball/resource panel between English and Chinese together. FluxDown picks a default from your browser's language the first time it runs; using this button overrides that choice and is remembered from then on.
