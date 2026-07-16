---
title: Settings Reference
description: Every setting in FluxDown, grouped by the categories in the Settings sidebar, with defaults.
section: getting-started
order: 4
---

Open **Settings** from the top bar's gear icon. The settings sidebar has nine categories, in this order: General, Appearance, Download, BitTorrent, eD2K, Proxy, API Service, Extensions, and About. This page documents every setting in each one, along with its default value. The Extensions category hosts plugin management and optional external components (FFmpeg, yt-dlp) in two tabs — see the [Plugins docs](/docs/en/plugins/overview/) for those. A search box at the top of the Settings sidebar can jump straight to any setting by name.

## General

| Setting | Purpose | Default |
|---|---|---|
| Launch at Startup | Automatically run FluxDown when the system starts. | Off |
| Minimize to Tray on Close | Hide to the system tray instead of quitting when you click the close button. | On |
| Desktop Floating Ball | An always-on-top desktop widget showing download speed and progress; drag URLs or torrent files onto it to start a download. Unavailable on Wayland (see the note below). | Off |
| Clipboard Watcher *(Linux Wayland only)* | While the main window is hidden, watch the clipboard for download links and notify you. Only shown as a fallback when the floating ball is unavailable on Wayland. | Off |
| Associate .torrent Files | Make FluxDown the default handler for `.torrent` files. FluxDown prompts once on first launch if this hasn't been set. | Off |
| Completion Notifications | Show a system notification when a task finishes. | On |
| Keep Awake While Downloading | Prevent the system from sleeping or turning off the display while any task is downloading; restores automatically once idle. | Off |
| Sidebar Sections (Status / Queues / Category) | Choose which of the three sidebar sections are visible. Turning all three off hides the sidebar entirely. | All on |
| Titlebar Buttons (Pause All / Resume All / Settings / Theme) | Choose which tool buttons appear in the top bar; you can also right-click a button to hide it directly. | All on |
| Custom Categories | Define extra file-type filters for the sidebar's Category section, matched by extension or regular expression. | None |

## Appearance

| Setting | Purpose | Default |
|---|---|---|
| Language | Interface language: follow system, Chinese, or English. | Follow System |
| Theme Mode | Light, dark, or follow system. | Follow System |
| Theme | Five built-in themes, filtered to the current light/dark mode: **Default Dark**, **Midnight Blue**, and **Nord** for dark mode; **Default Light** and **Warm Light** for light mode. You can also import a custom theme JSON and export the current one. | Default Dark / Default Light |
| Theme Color | The app's accent color: Blue, Green, Violet, Rose, or Custom (hue slider + hex input). | Blue |
| Interface Scale | Overall UI scale, from 80% to 150%. | 100% |
| App Icon *(Windows only)* | Choose the icon used by the window, taskbar, and tray: the built-in icon, a built-in "bolt" alternative, or a custom image you provide. | Built-in icon |

## Download

| Setting | Purpose | Default |
|---|---|---|
| Default Save Directory | Where new downloads are saved unless you pick another folder or a queue overrides it. | Platform downloads folder |
| Remember Last Save Location | Use the folder from your last download as the default for the next one, instead of the fixed directory above. | Off |
| Silent Download | Skip the confirmation dialog for external download requests (e.g. from the browser extension) and start them immediately with default settings. | Off |
| Default Threads | Default segment count for new downloads (0 = automatic, based on file size and CPU count). | Auto |
| Max Concurrent Downloads | Maximum number of tasks downloading at the same time, across all queues that don't override it. | 5 |
| Speed Limit | Global download speed cap in KB/s (0 = unlimited). Also editable from the status bar. | 0 (unlimited) |
| Auto-retry Attempts | Automatic retries after transient errors like network drops (-1 = unlimited, 0 = off). | 3 |
| Retry Interval | Seconds to wait before each automatic retry. | 5 |
| User-Agent | The browser identity sent with download requests; presets for Chrome/Firefox/Edge/Safari. Empty uses the built-in Chrome UA. | Empty (built-in Chrome UA) |
| File manager command | Custom command template for opening a file or folder in a third-party manager. Placeholders: `{path}` (current path) and `{dir}` (folder). Empty uses the platform default (Explorer/Finder/Nautilus). | Empty |
| Default Queue | Which queue new downloads join when none is explicitly chosen (used by the browser extension too). | Main Queue (built-in) |

### Third-party file managers

"Show in Folder" and "Open Folder" follow your system's **default file manager**, so most setups need no configuration:

- **Windows** — respects the default folder handler registered under `Directory\shell` (the app that opens when you double-click a folder). A third-party manager set as the default is used automatically. **OneCommander is supported out of the box** this way; Directory Opus, Total Commander, Files, and other managers that register themselves as the default folder handler are picked up the same way. Because Windows lets only Explorer select a specific file, when a third-party manager is the default, "Show in Folder" opens the containing folder instead of highlighting the file.
- **macOS** — uses the Launch Services default for `public.folder`.
- **Linux** — uses the `inode/directory` default from `mimeapps.list` (via `xdg-open`).

To use a manager that is **not** your system default — or to highlight the exact file inside a third-party manager — fill in the **File manager command** template above. One command covers both "Show in Folder" (a downloaded file) and "Open Folder" (a directory); FluxDown fills the placeholders according to the case. Placeholders are **already shell-quoted, so don't add your own quotes** around them — but the executable path still needs quotes if it contains spaces:

- `{path}` — the current path: the full file path when showing a file, the folder path when opening a directory.
- `{dir}` — the directory (the file's parent when showing a file, the folder itself when opening a directory).

Example commands on Windows (adjust the install path; use `{path}` for managers that can navigate to a specific file, `{dir}` for those that only open a folder):

| Manager | Command |
|---|---|
| OneCommander | `"C:\Program Files\OneCommander\OneCommander.exe" {path}` |
| Everything | `"C:\Program Files\Everything\Everything.exe" -select {path}` |
| Directory Opus | `"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe" /cmd Go {path} NEW` |
| Total Commander | `"C:\totalcmd\TOTALCMD64.EXE" /O /T {dir}` |

A configured template always takes priority over auto-detection; if it fails to launch, FluxDown falls back to the platform default.

## BitTorrent

| Setting | Purpose | Default |
|---|---|---|
| Listen Port Range | The port range FluxDown listens on for incoming BT connections. | 6881–6891 |
| Tracker List | Custom tracker servers for peer discovery, one per line, on top of 25 built-in trackers (Asia-priority ordering). Can be reset to the built-in list. | 25 built-in trackers |
| Tracker Subscription | Periodically fetch up-to-date trackers from community-maintained lists and merge them with the list above. | On |

Some BT changes require restarting the BT engine to take effect (FluxDown shows a reminder in this category). DHT and UPnP are always enabled internally and have no separate toggle. Proxy settings (below) don't apply to BitTorrent downloads.

## eD2K

| Setting | Purpose | Default |
|---|---|---|
| Server List | Manually added eD2K servers (`host:port`, one per line) for finding sources, merged with the subscription list. | Empty |
| Server Subscription | Periodically fetch up-to-date servers from community-maintained `server.met` lists and merge them with the list above. | On |
| Kad DHT source finding | Find sources via the decentralized Kad network, even when all servers are unreachable. | On |
| UPnP port mapping | Automatically map a port via UPnP to obtain a HighID, improving connectivity. | On |
| Listen port | TCP/UDP port for the eD2K client (0 = let the OS choose). | 0 (auto) |

## Proxy

| Setting | Purpose | Default |
|---|---|---|
| Mode | No Proxy, System Proxy (read from OS settings), or Manual. | No Proxy |
| Type *(Manual mode)* | HTTP, HTTPS, SOCKS4, or SOCKS5. | HTTP |
| Server Address / Port *(Manual mode)* | The proxy server's host and port. | Empty |
| Username / Password *(Manual mode)* | Optional proxy authentication. | Empty |
| Bypass List | Comma-separated addresses that skip the proxy entirely. | Empty |
| Test Connection | Verifies the configured proxy and reports latency or an error. | — |

Proxy settings apply to HTTP/HTTPS/FTP/eD2K downloads only — BitTorrent downloads always connect directly.

## API Service

A local-only HTTP API (127.0.0.1) used by the browser extension, aria2-compatible tools, and automation scripts.

| Setting | Purpose | Default |
|---|---|---|
| Enable API Service | Master switch for the local HTTP server; the three feature toggles below only work while this is on. | On |
| Listen Port | The port the local server binds to (1024–65535). | 17800 |
| Access Token | Token used to authenticate API requests; generate or copy it with the buttons next to the field. Required once Management API is enabled. | Empty |
| Browser Script Takeover | Lets the FluxDown userscript take over browser downloads at `http://127.0.0.1:<port>`. Includes a button to copy the userscript. | On |
| aria2 RPC Compatible | Implements the aria2 JSON-RPC protocol (`addUri`, `getVersion`, `getGlobalStat`, `multicall`, …) at `/jsonrpc`, for "send to aria2" scripts or clients like AriaNg. | On |
| Management API | An HTTP API for querying and controlling tasks at `/api/v1`, for MCP servers and automation scripts. Always requires the access token. | Off |

See the [API documentation](/api-docs) for the full endpoint reference.

## About

| Item | Purpose | Default |
|---|---|---|
| Version info | Shows the current version, plus the latest available version and its publish date once an update is found. | — |
| Auto-check for Updates | Silently check GitHub Releases for a newer version shortly after startup. Also exposes manual Check Now / Download / Install & Restart actions. | On |
| Max Log Size | Total size cap for FluxDown's log files (5/10/20/50/100 MB); the oldest logs are cleaned automatically once the cap is exceeded. | 10 MB |
| Export Logs | Package recent logs into a `.zip` for attaching to a bug report, or open the log folder directly. | — |
