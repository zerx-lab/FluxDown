---
title: Settings Reference
description: Every setting in FluxDown, grouped by the categories in the Settings window, with defaults.
section: getting-started
order: 4
---

Open **Settings** from the gear at the bottom of the activity rail, or press `⌘ ,` / `Ctrl ,`. The window has a category list on the left, a search box above it, and the selected page on the right. This page follows the same order.

![FluxDown desktop Settings window: search box and category list on the left, General page on the right](/docs/desktop/settings-general.en.webp)

The categories are **General**, **Account**, **Appearance**, **Shortcuts**, **Download**, **BitTorrent**, **eD2K**, **Proxy**, **API Service**, **Notifications**, **Extensions**, **Doctor** and **About**. A **Refer & earn** page appears after **Account** only when it is enabled for your account.

Type in the search box to filter settings by name; matching categories and rows stay visible and the rest are hidden.

Defaults below are the values on a fresh install. Most settings apply immediately. The [headless Web UI](/docs/en/headless-server/web-ui/) has the same pages except **Shortcuts** and **Notifications**, and its **API Service** port is fixed by `FLUXDOWN_BIND`.

## General

Launch, tray, system integration and which parts of the main window are visible.

| Setting | What it does | Default |
|---|---|---|
| **Show Menu Bar** | Show the File, Tasks, View, Tools and Help menus in the title bar. Windows and Linux only; macOS uses the system menu bar. | Off |
| **Launch at Startup** | Run FluxDown at sign-in; it stays in the tray without opening the main window when a tray is available. | Off |
| **Minimize to Tray on Close** | The close button hides to the system tray instead of quitting. | On |
| **Start minimized to tray** | When opened manually, start hidden in the tray. | Off |
| **Tray Download Status** | Show unread download status in the Windows notification area. Windows only. | On |
| **Clipboard Watcher** | Offer to download copied links that look like files (direct file links, magnet, ed2k, thunder). Ordinary web pages are ignored. | Off |
| **Probe extensionless links online** | Send one HEAD request to decide whether an extensionless link is a file. Login and verification links are never probed. Needs the clipboard watcher. | On |
| **Custom watched extensions** | Extra file extensions the clipboard watcher should recognize. Needs the clipboard watcher. | Empty |
| **Associate .torrent Files**, **Take Over Magnet Links**, **Associate ed2k Links** | Make FluxDown the system handler. A switch is disabled when the platform cannot register it. | Reflects the system state |
| **Keep Awake While Downloading** | Block system sleep and screen-off while downloads are active. | Off |
| **Anonymous Usage Statistics** | Send anonymous usage data; no personal or sensitive data is collected. | On |

If the system tray is unavailable (for example a Linux desktop without a StatusNotifier host), the tray switches are disabled and the description explains what closing the window does instead.

**Sidebar Sections** and **Activity Bar** control the main window layout:

| Setting | What it does | Default |
|---|---|---|
| **Show Status** | Show the Status folder in the sidebar. | On |
| **Show Queues** | Show the Queues section. | On |
| **Show Category** | Nest category sub-items inside the **All** status folder. | On |
| **Show device sync** | Show the device section in the sidebar. Left unset, it appears automatically once another device is added. | Auto |
| **Hide task source** | Hide the source site under file names in the task list and details. | Off |
| Activity bar entries (**Account**, **RSS subscriptions**, **Notifications**) | Choose which optional entries appear in the activity bar; the bar hides when all are off. | On |

See [Interface Overview](/docs/en/getting-started/interface/) for the layout these switches affect.

## Account

Sign-in and cloud features. This page hosts the account card; everything else in Settings works without an account.

## Appearance

| Setting | What it does | Default |
|---|---|---|
| **Language** | Display language: **System** or one of the bundled languages. | System |
| **Theme Mode** | **System**, **Light** or **Dark**. | - |
| **Theme** | Pick a theme card for the current mode. Light: Default Light, Warm Light, Graphite Light. Dark: Default Dark, Midnight Blue, Nord, Graphite. Custom themes can be imported or exported, and **More Themes** opens the online gallery. | - |
| **Theme Color** | Accent color: Blue, Green, Violet, Rose or a custom color. | Blue |
| **File Icons** | Icon style for files in the task list, independent of the theme. | System |
| **File size units** | Show sizes and speeds as KB / MB / GB or KiB / MiB / GiB. Both use 1024-based units. | KB / MB / GB |
| **Interface Scale** | 80%, 90%, 100%, 110%, 120%, 130% or 150%. | 100% |
| **Font family** | A font installed on this computer; unavailable fonts fall back to the theme font. | Theme font |
| **Font Weight** | Default body text weight; the system picks the closest weight the font offers. | - |
| **Font Size** | Body text size, 11-22 px; other text, rows and controls scale with it. Combines with **Interface Scale**. | Theme default |
| **App Icon** | Default, Bolt, or a custom image applied to the window, taskbar/Dock and tray. | Default |

Appearance settings are stored per device.

![FluxDown main window in the dark theme](/docs/desktop/dark-theme.en.webp)

## Shortcuts

Two tabs: **In-app** (active while a FluxDown window is focused) and **Global** (handled by the background agent, so they keep working after the window is closed). Click a shortcut to record a new key combination; **Restore all defaults** resets the tab. Global shortcuts are unbound by default.

| Action | Scope | Default (macOS / Windows, Linux) |
|---|---|---|
| **New download** | All windows | `⌘N` / `Ctrl+N` |
| **Open settings** | All windows | `⌘,` / `Ctrl+,` |
| **Command palette** | All windows | `⌘K` / `Ctrl+K` |
| **Close window** | All windows | `⌘W` / `Ctrl+W` |
| **Minimize window**, **Toggle full screen**, **Quit FluxDown** | All windows | `⌘M`, `⌃⌘F`, `⌘Q` on macOS; unbound elsewhere |
| **Hide FluxDown**, **Hide other apps** | macOS only | `⌘H`, `⌥⌘H` |
| **Focus search** | Download list | `⌘F` / `Ctrl+F` |
| **Select all** | Download list | `⌘A` / `Ctrl+A` |
| **Clear selection** | Download list | `Esc` |
| **Pause / resume selected** | Download list | `Space` |
| **Delete selected** | Download list | `Delete`, `Backspace`, plus `⌘Backspace` / `Ctrl+Backspace` |
| **Delete selected with files** | Download list | `Shift+Delete` |
| **Toggle detail panel** | Download list | `⌘I` / `Ctrl+I` |
| **Cycle list density** | Download list | `Shift+D` |
| **Cycle grouping** | Download list | `G` |
| **Cycle sort order** | Download list | `S` |
| **Pause all**, **Resume all**, **Open torrent file** | Download list | Unbound |
| **Show FluxDown window**, **New download**, **Open settings**, **Pause all**, **Resume all** | Global | Unbound |

## Download

Five tabs: **General**, **Connection**, **Files**, **Advanced** and **Custom Categories**.

### General tab

| Setting | What it does | Default |
|---|---|---|
| **Default Save Directory** | Where new downloads go. | Empty |
| **Remember Last Save Location** | Use the last folder as the default for new downloads. | Off |
| **Remember Last Download Target** | Restore the device chosen for the previous download in **Download to**. This device only. | Off |
| **Remember Save Path History** | Keep recently used folders in the save directory dropdown of **New Download**. | On |
| **Silent Download** | Start external downloads (such as from the browser extension) immediately with default settings instead of opening **New Download**. | Off |
| **Auto-categorize silent downloads** | Pick the category automatically in silent mode. Shown when silent download is on. | On |
| **Skip follow-up choices in silent mode** | Skip later prompts such as BT file selection, quality or plugin choice. Shown when silent download is on. | Off |
| **Default Queue** | Queue used for browser-extension and new downloads. | Empty |

### Connection tab

| Setting | What it does | Default |
|---|---|---|
| **Thread Count** | **Auto** adapts the number of connections per task; **Custom** sets a per-task cap. | Auto |
| **Auto Mode Thread Cap** | Upper bound the scheduler may use in Auto mode. Shown in Auto mode. | 16 |
| **Multi-CDN Concurrent Download (Experimental)** | Fetch the same file from several CDN node IPs. HTTPS direct links only; disabled while a proxy is active. | Off |
| **Max CDN Nodes** | **Auto** or **Custom** (1-8 nodes). Shown when multi-CDN is on. | Auto |
| **Multi-NIC Aggregated Download** | Download one file over several network adapters at once. | Off |
| **Learned Server Policies** | After a server rejects parallel connections (403/429), the engine remembers its cap for 24 h. Clear it to relearn. | - |
| **Max Concurrent Downloads** | Simultaneous tasks, 1-1024. BT/magnet tasks are not counted. | 5 |
| **Download Limit** / **Upload Limit** | Global speed limits; 0 = unlimited. | 0 |
| **Auto-retry Attempts** | Retries after transient errors, -1 to 20. | 3 |
| **Retry Interval** | Seconds to wait before each retry; grows with each attempt. | 5 |
| **Resume unfinished downloads on startup** | Continue tasks that were downloading when the app last closed. | Off |

A very high thread count shows a risk warning because many servers throttle or ban IPs that open that many connections.

### Files tab

| Setting | What it does | Default |
|---|---|---|
| **When file exists** | **Auto rename**, **Overwrite**, **Skip download** or **Ask every time**. Asking needs an open FluxDown window. | Auto rename |
| **Use Server File Time** | Set the file's modified time from the server. | Off |
| **Compute file hash after download** | Hash finished files. | Off |
| **Hash algorithm** | MD5, SHA-1, SHA-256 or SHA-512. | SHA-256 |
| **When the file is deleted or moved** | **Keep the task record** or **Delete the task record** when a completed task's file disappears. | Keep |
| **Auto-remove completed records** | **Never**, **Immediately**, after 1 / 7 / 30 days, or a custom number of days. Only the record is removed; files stay. Seeding tasks are removed after seeding stops. | Never |
| **Periodically check completed files while idle** | Re-check whether completed files still exist even when nothing is downloading. Turn it off to avoid waking a sleeping NAS or external drive. | Off |

### Advanced tab

- **User-Agent** - browser identity for download requests: Default, Chrome, Edge, Firefox, Safari or Custom. **Manage presets** edits the saved list.
- **File manager command** - custom command used to reveal files; empty uses the platform default.
- **Aria2 advanced options** - a draft editor for aria2-style options. Size values use 1024-based K/M/G suffixes, `split=0` means automatic, and omitted keys stay unchanged. Changes take effect only after you apply them.

### Custom Categories tab

Built-in categories (Video, Audio, Document, Image, Programs, Archive, Other) can be reordered and given their own save folder. **Add Category** creates a custom one: name, icon, save directory (empty = global default), and a match rule **By Extension**, **By URL** or **By Regex**. Custom categories take priority over built-in ones; drag rows to reorder. **Reset Defaults** restores the built-in list, and **Auto Category Folders** turns on per-category folders. You can also create or edit categories from the right-click menu of a category under **All** in the sidebar.

## BitTorrent

Five tabs: **General**, **Connection**, **Tracker**, **Seeding** and **Anti-leech**. While BitTorrent is off, the **Connection**, **Tracker** and **Seeding** tabs are hidden. See the [BitTorrent guide](/docs/en/protocols/bittorrent/) for how magnets, file selection and trackers work.

**General** - **Enable BitTorrent** (on); **Piece Picking** for new tasks, **Rarest First** (default) or **Sequential**; **Download First and Last Pieces First** (off); **Auto-Select Default Files After 60 Seconds** (on).

**Connection**:

| Group | Settings (defaults) |
|---|---|
| Listening | **Start Port** / **End Port** (a random fixed port is generated on first use; set both to the same value for a single port). A live listener status row shows whether the port is reachable. Port changes need a restart. |
| Transport | **Enable DHT** (on), **Port mapping (UPnP / NAT-PMP)** (on), **Local peer discovery (LSD)** (on), **Enable uTP** (on), **uTP congestion control** (LEDBAT or CUBIC; LEDBAT), **Protocol encryption (MSE)** (Disabled / Enabled / Forced; Enabled). |
| Peers and sources | **Upload slots per torrent** (0 = automatic), **HTTP sources (web seeds)** (on). |
| Connection limits | **Maximum connections** (0 = unlimited), **Maximum connections per task** (512). |
| I2P anonymous network | **Enable I2P** (off) routes `.i2p` torrents through a local I2P router over the SAM bridge (**SAM host** 127.0.0.1, **SAM port** 7656, **Mixed mode** off). An I2P router such as i2pd must be running. |
| Announce policy | **Announce to all tiers** (on), **Announce to all trackers in a tier** (on). |

**Tracker** - **Tracker List** (one URL per line, added to every new BT task; running tasks are unaffected) and **Tracker Subscription** (on) with subscription URLs, merged and deduplicated with your list.

**Seeding**:

| Setting | What it does | Default |
|---|---|---|
| **Seed after download completes** | Start seeding when a task finishes. Seeding keeps the disk awake. | On |
| **Max active seeding tasks** | 0 = unlimited; extra finished tasks queue. | 0 |
| **Auto-resume seeding on startup** | Continue seeding finished tasks after a restart unless you stopped them manually. | On |
| **When total share ratio reaches** / **When post-completion share ratio reaches** | Ratio limits; 0 = off. | 0 |
| **When seeding time reaches** / **When inactive seeding time reaches** | Time limits with a minutes/hours/days unit; 0 = off. | 0 |
| **Condition combination** | **Match any condition** or **Match all conditions**. | Match any |
| **Then** | **Stop seeding**, **Delete task** or **Delete task & files**. | Stop seeding |

**Anti-leech** - settings, **Block list** and **Event log** for the leech guard. Defaults: the guard is on, the fingerprint (client name) rule enforces, and the other rules start in observe-only mode. Details are in [BT Anti-Leech](/docs/en/protocols/bt-guard/).

## eD2K

Three tabs. See the [eD2K guide](/docs/en/protocols/ed2k/) for link handling and hash verification.

| Tab | Setting | What it does | Default |
|---|---|---|---|
| General | **Kad DHT source finding** | Find sources through the decentralized Kad network even when all servers are down. | On |
| General | **UPnP port mapping** | Map ports via UPnP to get a HighID. | On |
| General | **Listen port** | eD2K TCP port; 0 lets the system choose on each start. A random fixed port is saved on first use. | Random fixed |
| General | **HighID / LowID check** | Log in to a server with the current port to check whether you are reachable from the internet. | - |
| Servers | **Server List** | Manually entered servers, merged with the subscription. | Empty |
| Servers | **Server subscription** and **Subscription URLs** | Fetch a community `server.met` periodically. | On |
| Kad Nodes | **nodes.dat subscriptions** | URLs that provide Kad bootstrap nodes, one per line. | Empty |
| Kad Nodes | **Bootstrap** | Contact Kad nodes now to check reachability and learn fresh nodes. | - |

## Proxy

**Proxy Settings** is the global mode for HTTP, FTP, HLS and DASH downloads, RSS feeds and plugin requests:

| Mode | Behavior |
|---|---|
| **No Proxy** | Connect directly (default in the settings catalog). |
| **System Proxy** | Use the proxy detected from the OS; the page shows what was found and has **Test Connection**. |
| **Manual** | Use the proxy you enter. |
| **Auto (Recommended)** | Starts direct and treats every manual and system proxy as a parallel path, measures them with real segments and moves connections to the faster one. Multi-CDN acceleration still applies to direct. |

**Manual** fields: **Proxy Type** (HTTP, HTTPS, SOCKS4, SOCKS5), **Server Address**, **Port**, **Username**, **Password**, **Bypass List** (comma separated) and **Test Connection**.

Other groups on this page:

- **BitTorrent** - **Use a proxy for BitTorrent** (off) with **Proxy source** (**Manual proxy** or **System proxy**) and **Proxy peer connections too** (on). HTTP and SOCKS4 proxies cannot relay UDP, so UDP trackers and uTP are turned off; SOCKS5 has no such limit.
- **Protocol-specific proxies** - separate endpoints for **HTTP and HTTPS**, **FTP**, **HLS**, **DASH**, **BitTorrent** and **eD2K**. They override the global proxy for that protocol; task proxies and site rules still win. Leave empty for the previous behavior, or enter `direct://` to connect directly. eD2K needs a SOCKS5/5h URL.
- **Site rules** - send specific sites through a chosen exit. Each rule matches a **Domain** or a **Regular expression** and uses **Direct**, **System proxy**, **Manual proxy** or **Custom proxy**. Rules apply in every proxy mode, including **No Proxy** and **Auto**; the first match from the top wins. Rules cover HTTP/HTTPS, FTP, HLS/DASH, RSS, plugin requests and FluxDown's own downloads (updates, components, plugin packages), but not tasks or feeds with their own proxy, nor webhook deliveries. **Test a URL** shows which exit a URL would use. Rules are stored on this device only.
- **Saved Site Credentials** - HTTP authentication saved per site, stored only on this device and applied to new downloads; delete one or clear all.

The proxy picker in the status bar changes the same global mode.

## API Service

Local HTTP service for the browser userscript, aria2 clients, the management API and MCP. See [API Overview](/docs/en/api/overview/) for routes and authentication.

| Setting | What it does | Default |
|---|---|---|
| **Listen Port** | Service port, 1024-65535; changing it restarts the service and keeps the old one if the new one fails to start. Read-only where the startup environment fixes it (headless: `FLUXDOWN_BIND`). | 17800 |
| **Address** | Shows `http://127.0.0.1:<port>` with a copy button. | - |
| **Allow LAN / VPN access** | Bind `0.0.0.0` so devices on your network can reach the service; applies at the next service start. Only enable on trusted networks and set an access token. | Off |
| **Access Token** | Token required for API requests; type your own or click Generate. Required once the management API is enabled. | - |

**Feature Toggles** (disabled as a group when the master switch is off):

| Toggle | Purpose | Default |
|---|---|---|
| **Browser Script Takeover** | Lets the FluxDown userscript take over browser downloads. | On |
| **Tampermonkey Userscript** | Copy a ready-made script with the active port and token. Do not share it; copy again after changing the port or token. | - |
| **aria2 RPC Compatible** | aria2 JSON-RPC at `/jsonrpc`. | On |
| **Management API** | HTTP API at `/api/v1` for automation; authentication required. | Off |
| **MCP Endpoint** | Model Context Protocol endpoint at `/mcp` for AI clients; shares the management token. | Off |
| **Allow cross-origin access from any website (CORS)** | Lets any web page call the service. Leave off unless you need it. | Off |

## Notifications

| Setting | What it does | Default |
|---|---|---|
| **Completion Notifications** | System notification when a task completes. | On |
| **Failure Notifications** | System notification when a task fails after all automatic retries. This device only. | On |
| **Sounds** | Per-event sound for **Task started**, **Task completed**, **Task failed**, **Queue started** and **Queue finished**: off, the system sound, or a sound file you choose. | Off |
| **Show download progress window** | Open a separate progress window without stealing focus when you add a download while not looking at the main window. | On |
| **Show download complete window** | After a download you started finishes, show a window to open the file or its folder. | On |

Push channels (email, webhooks and similar) live in the **Notifications** entry of the activity rail, not here.

## Extensions

Tabs: **Plugins**, **Site Defaults** (appears only when a site has a saved default plugin) and **Components**. **Components** manages the optional FFmpeg and yt-dlp, with a per-component status card. See the [plugin docs](/docs/en/plugins/overview/) and the [browser extension install guide](/docs/en/browser-extension/install/).

## Doctor

**Environment diagnostics** checks browser extension registration (NMH), the local HTTP service, protocol handlers and the log directory. **Run diagnostics** produces a report, **Copy report** puts it on the clipboard for feedback, and **Repair NMH registration** rewrites the native-messaging registration (restart the browser afterwards).

## About

| Setting | What it does | Default |
|---|---|---|
| **Current Version**, **Protocol version** | Read-only. | - |
| **Update Channel** | **Stable** or **Frontier** (preview). | Stable |
| **Auto Check for Updates** | Check for updates automatically. | On |
| **Silent Automatic Updates** | Install updates only when every desktop window is closed and no downloads or seeding are active, without administrator prompts; restarts background services only. Needs automatic checking. | Off |
| **Check for Updates** | Check now. | - |
| **Host Log Level** | Log verbosity: Auto, Debug, Info, Warn, Error. Shown when the host reports it. | - |
| **Max Log Size** | Disk budget for logs, 1-1024 MB. | 10 MB |
| **Export Logs** | Export logs for troubleshooting. | - |

The page also hosts the **Browser Extension** card (install or refresh the extension) and **Support FluxDown**.
