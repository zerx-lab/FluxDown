---
title: Web UI
description: Sign in and manage downloads from the browser-based interface bundled with the headless server.
section: headless-server
order: 3
---

Once the server is running (see [Server Setup](/docs/en/headless-server/setup/)), open its address in a browser - for example `http://<host>:17800/` - to reach the Web UI. It follows the desktop app's layout and task workflow, adapted for remote use and for narrow screens.

The Web UI talks to the agent over the `/rpc` WebSocket (JSON-RPC), not aria2's `/jsonrpc`. It is served from the same origin as the server, so there is no server address to enter.

## Signing in

### First run: set an access key

While the server has no access key yet, every address redirects to a setup card.

![First-run setup card with setup code, new access key, confirm access key and a Generate button](/docs/web-ui/setup.en.webp)

1. **Setup code** - a one-time code that proves you control the server. Read the `setup-bootstrap` file in the agent data directory (Docker: `/data/agent/setup-bootstrap`). On NAS packages it is `FluxDown-setup-code.txt` in the download shared folder. It is not the access key.
2. **New access key** and **Confirm access key** - type your own, or click **Generate** for a random one. Save it somewhere safe; you need it on every browser.
3. Click **Save and continue**.

An access key is 8 to 128 printable ASCII characters (no spaces) with at least one letter and one digit. Wrong setup codes are rejected, and too many wrong attempts lock setup for a few minutes. If the key was already set elsewhere, the page tells you to sign in instead.

Until a key exists, `/download*` and `/jsonrpc` (POST/WS) return 403, so aria2 clients and userscripts cannot take over the server before you set one. The setup page and its endpoints stay reachable.

### Sign in

![Sign-in card with an access key field and a Remember this device checkbox](/docs/web-ui/login.en.webp)

Enter the **Access key** and click **Sign in**. The eye button shows or hides what you typed. A key that is wrong or breaks the key rules shows "Access key rejected". If the server still needs setup, the page sends you to the setup card.

**Remember this device** decides where the key is stored in this browser:

| State | Storage | Lifetime |
|---|---|---|
| Checked (default) | `localStorage` | Survives browser restarts |
| Unchecked | `sessionStorage` | Cleared when the tab closes |

The key is only ever sent to the server itself. To sign out, open **Settings -> API Service** and click **Sign out** under **Sign out of this browser**; this clears the key saved in the browser and returns to the sign-in page. Changing the key on the server updates the stored copy in place, keeping your Remember choice.

The key you set during setup is also the **Access Token** shown under **Settings -> API Service**. See [Server Setup](/docs/en/headless-server/setup/) for environment variables that preset or force it.

## Main layout

![Web UI main screen with activity rail, sidebar, toolbar, task list and status bar](/docs/web-ui/main.en.webp)

- **Activity rail** (far left) - **Downloads**, **RSS**, and **Push Notifications** pages; the account entry and **Settings** sit at the bottom. Plugins can add their own entries.
- **Sidebar** - the **Status** folder, **Queues**, and optionally **Devices**. See below.
- **Title bar / toolbar** - **New Task**, **Pause All**, **Resume All**, **Retry failed tasks**, a search box, and the **View** button. A dot on the right shows the connection state.
- **Task list** - one row per task, with a detail panel for the selected task.
- **Status bar** - total download and upload speed, download and upload limits, the proxy picker, and the version. Click the version when an update is available.

Open the right-click menu on the toolbar or the activity rail and choose **Customize layout...** to drag items between the title bar, activity rail and status bar, or hide them. **Reset layout** restores the default. A free-space readout is available there too and is hidden by default.

### Sidebar

The **Status** folder lists **All**, **Incomplete**, **Completed**, **Seeding**, **Failed** and **Paused**, each with a live count. Only **All** nests category children (Video, Audio, Document, Image, Programs, Archive, Other, plus your own). There is no separate categories section. Right-click a category child to create or edit categories.

**Queues** lists the built-in **Main Queue** and **Download Later** plus your own queues. The gear next to **QUEUES** opens the queue manager, where you create queues with **New Queue**. Right-click a queue for its menu:

- **Start Queue** / **Stop Queue** - stopping pauses every task in the queue and keeps them from auto-starting; starting resumes them in queue order. The Main Queue always runs and cannot be stopped.
- **Manage Queue** - rename the queue and set its default save directory, **Max Concurrent Downloads** and speed limit. **Enable daily schedule** adds a **Start time** and **Stop time** (either or both) and the weekdays under **Repeat**. The Main Queue cannot be scheduled, and built-in queues cannot be renamed.
- **Delete Queue** - tasks in a deleted queue move back to the Main Queue.

Which sidebar sections and activity-rail entries show is controlled under **Settings -> General** (**Show Status**, **Show Queues**, **Show Category**, **Show device sync**, and the activity bar entries).

### Narrow screens

At 820 px width or less the activity rail becomes a bottom tab bar, the sidebar and detail panel open as sheets, dialogs go full screen, and the list switches to cards. The same features apply.

## Finding and arranging tasks

### Search

Type in the search box to filter by name. Press `Ctrl+F` (`Cmd+F` on macOS) or `/` anywhere on the page to jump into it; `Esc` clears it. Search combines with the sidebar status, category and queue.

### View menu

![View menu with Columns, Group By, Sort and Display tabs, and a column checklist with drag handles](/docs/web-ui/view-menu.en.webp)

The **View** button opens a popover with four tabs:

| Tab | What it does |
|---|---|
| **Columns** | Show or hide columns and drag handles to reorder them. Choices: **File Name**, **Progress**, **Size**, **Speed**, **Remaining**, **Status**, **Created**, **Protocol**, **Source**, **Queue**. At least one column stays visible. **Reset to Default Columns** restores the default set. |
| **Group By** | **No Grouping**, **Status**, **Date**, **Type**, **Queue**, **Site**, **Directory**, **Series title**. |
| **Sort** | **Smart**, **Created Time**, **Name**, **Size**, **Progress**, **Speed**, **Status**, with **Ascending** or **Descending** (not for Smart). |
| **Display** | Row density (**Compact**, **Comfortable**, **Relaxed**), header alignment, and the **Detail Panel**: show or hide it, placed at the **Bottom** or **Right**. |

View preferences are saved on the server in the `desktop.downloads.view` preference, so they follow you across browsers.

### Selecting several tasks

Click a task to select it; Ctrl/Cmd-click toggles one and Shift-click selects a range. `Ctrl+A` (`Cmd+A`) selects all, `Esc` clears the selection. With one or more tasks selected, a bar replaces the column headers (a floating bar on narrow screens) with **{n} selected** and these actions: **Resume**, **Pause**, **Download File** (completed tasks only), copy task info, **Export task information (.txt)**, delete, and clear selection.

The delete button offers **Delete Task** (keeps the files) and **Delete Task & File** (confirms first). From the keyboard, `Delete` removes the selected tasks without their files and `Shift+Delete` asks to delete them with files.

Bulk pause, resume and delete send the whole ID set in one `daemon.task.pauseMany`, `resumeMany` or `deleteMany` call. The daemon deduplicates IDs and ignores unknown ones.

## Task actions

![Task right-click menu with Pause, Boost Download, Open Folder, Edit task, Move to Queue, Select, Delete Task and Details](/docs/web-ui/context-menu.en.webp)

Right-click a task row for its actions. Which items appear depends on the task state and type:

| Group | Items |
|---|---|
| Run | **Resume** / **Pause** (**Resume** / **Stop seeding** for seeding BT tasks), **Boost Download** / **Cancel Boost** (pauses other tasks to give this one the bandwidth; they resume when it finishes) |
| Files | **Download File** (sends a finished file to your browser), **Open File**, **Open Folder**, **Copy File** |
| Edit | **Rename**, **Change link**, **Edit task**, **Change thread count**, **Verify Integrity**, **Re-download** |
| BitTorrent | **Piece Picking** (rarest first, sequential, first/last pieces first), **Force recheck**, **Move to...**, **Task speed limits...**, **Add Web Seed...**, **Super seeding**, **Copy magnet link**, **Export .torrent file...** |
| Other | **Copy URL**, **Move to Queue**, **Select**, **Select All**, plugin commands, **Delete Task**, **Delete Task & File**, **Details** |

**Open File**, **Open Folder** and **Copy File** act on the machine the agent runs on. They show up only when the connection allows it, typically when the browser and the agent are on the same machine, and are not offered for a remote server.

## Details panel

Select a task and open **Details** from the menu, or turn the panel on in **View -> Display**. It sits at the bottom by default and can move to the right. Besides the built-in tabs, plugins may add tabs.

![Details panel General tab with status, size, downloaded, speed, remaining time and a Source breakdown donut](/docs/web-ui/detail.en.webp)

| Tab | Shows |
|---|---|
| **General** | Status, size, downloaded, speed, remaining time, started and finished time, save path, queue, proxy, hash verification, and the **Source breakdown** donut (how much came from the origin, multi-CDN nodes, smart proxy and multi-NIC). Buttons copy the link, edit the task, change the link or change the thread count when allowed. |
| **Speed** | Live speed chart plus current, average and peak speed, download time and ETA. |
| **Log** | Recent events for the task. Events are kept by the download service, so gaps after a reconnect are filled in. |
| **Advanced** | Per-task advanced details such as proxy and request options. |

BitTorrent tasks add **Files**, **Seeding**, **Trackers** and **Peers** tabs; eD2K tasks add **Sources** and **Network**.

![Details panel Speed tab with a speed chart and current, average, peak speed and ETA](/docs/web-ui/detail-speed.en.webp)

## Starting a new download

Click **New Task** in the toolbar, or drop links, a `.torrent` file, or a `.txt`, `.url` or `.list` file of links onto the page.

![New Download dialog with three pasted URLs, range hint, import buttons, save directory with category picker, and Threads](/docs/web-ui/new-task.en.webp)

Paste one URL per line (HTTP, FTP, magnet). The top-right counter shows how many were recognized. For numbered files, write a range in the file name, for example `https://example.com/comic/[001-215].webp`. A preview shows the first and last result.

| Control | Notes |
|---|---|
| **Open .torrent file** | Pick a `.torrent` from your computer; the browser uploads it to the server. |
| **Seed local files** | Create a torrent from files you already have and seed it. |
| **Import TXT file** | Reads URLs from a text file into the box. |
| **Save Directory** | Left dropdown: **Auto** (picks a category folder by file type), **No category**, or a specific category. Then the folder path and **Browse**, which lists folders on the **server**, not on your computer. Recent folders are remembered if enabled under **Settings -> Download**. |
| **Rename** | Single URL only; leave empty to detect the name. |
| **Threads** | **Auto**, 4, 8, 16, 32, 64, or **Custom**. |

`Ctrl+Enter` (`Cmd+Enter`) submits the dialog.

![New Download dialog with Advanced Options expanded: Task Proxy, User-Agent with Manage presets, and Cookie](/docs/web-ui/new-task-advanced.en.webp)

**Advanced Options** adds **HTTP Authentication** (single non-magnet link), **Task Proxy** (**Follow Global** uses the proxy mode and site rules; any other choice ignores site rules for this task), **User-Agent** with **Manage presets**, **Cookie**, **Source page (Referer)**, **Hash Verification**, **Custom Headers**, and **Ignore HTTPS certificate errors**.

The buttons at the bottom:

- **Export task information (.txt)** - save the entered tasks as a text file.
- **Download {n} files** (**Start** for one URL) - create and start the tasks. The arrow lets you choose a queue.
- **Download Later** - create the tasks without starting them, in **Download Later** or the queue you pick from the arrow.

Tasks are created directly through the agent's `/rpc` (`daemon.task.create` and `createMany`), with no extra confirmation, because you are already in an authenticated session.

### HLS quality and file selection

Some downloads ask for a decision while they prepare. The question arrives over the WebSocket, so a dialog appears only when the Web UI is open and connected:

- **Select Quality** - an HLS (M3U8) playlist with several variants lists them; pick one.
- **Select Files to Download** - a multi-file torrent or magnet link lists its files with sizes, a **Filter files...** box and a running selected count and size on the confirm button, so you can skip what you do not want.

If nobody answers within 60 seconds, the engine takes its default (the best HLS variant, or all torrent files) and the dialog closes. For torrents, turn off **Auto-Select Default Files After 60 Seconds** under **Settings -> BitTorrent** to keep waiting; the task then stays in Preparing. With no FluxDown window or web page open, all files start right away.

## Retrieving finished files

Completed files stay on the server. To copy one to your computer, use **Download File** in the right-click menu or the selection bar. The browser downloads it from `GET /api/web/files/tasks/{id}` with the original file name. Browser downloads cannot set headers, so the request carries the access key as a `?token=` query parameter; a missing or wrong key gets 401, and 503 means the download service is offline. Tasks whose file is missing from disk cannot be downloaded this way. Several files at once are started 0.4 s apart.

## Settings

Open **Settings** from the bottom of the activity rail.

![Settings page General with navigation list and sidebar section toggles](/docs/web-ui/settings.en.webp)

| Page | Covers |
|---|---|
| **General** | Which sidebar sections, activity-rail entries and task-list details are shown, plus anonymous usage statistics. |
| **Account** | FluxDown account and device linking. |
| **Appearance** | **Language**, **Theme Mode** (**System**, **Light**, **Dark**), **Theme Color**, **Interface Scale**, font and file-size units. |
| **Download** | Tabs **General** (default save directory, remember last location and path history, **Silent Download**, **Default Queue**), **Connection**, **Files**, **Advanced** and **Custom Categories**. |
| **BitTorrent**, **eD2K** | Engine and network parameters for those protocols. |
| **Proxy** | Outbound proxy used by the server. "System proxy" reads the server's own settings, so manual configuration is the practical choice. |
| **API Service** | Endpoints, access token and feature toggles. See below. |
| **Extensions** | Installed plugins. |
| **Doctor** | **Run diagnostics** checks the server environment. |
| **About** | Server version, update channel, link to the server releases, and **Export Logs** for a ZIP of runtime logs, handy when troubleshooting a NAS or remote box. |

Settings are stored on the server and are shared with the desktop app's matching pages; see [Settings](/docs/en/getting-started/settings/) for what each field means.

### API Service

![API Service page with listen port, aria2 RPC, MCP and Management API addresses, access token, and feature toggles](/docs/web-ui/settings-api.en.webp)

In server mode the listen address and port are fixed by the `FLUXDOWN_BIND` environment variable, so the page shows them read-only. It lists copyable addresses: **aria2 RPC Compatible** (`/jsonrpc`), **MCP Endpoint** (`/mcp`) and **Management API** (`/api/v1`). The **Access Token** row shows, copies, edits or regenerates the access key (same rules as setup). Changing it signs out other browsers. **Feature Toggles** switch **Browser Script Takeover**, **aria2 RPC Compatible**, **Management API**, **MCP Endpoint**, and cross-origin access (CORS).

### Language

The Web UI is localized. The language comes from the first source that is set: the agent preference `general.locale` (**Settings -> Appearance -> Language**; **System** moves on to the next source), then the server's `FLUXDOWN_LANG` environment variable, then your browser language. The sign-in and setup pages cannot read the preference before sign-in, so they use `FLUXDOWN_LANG` or the browser language, with the last resolved language cached in the browser.

## Connection status and reconnects

A dot in the title bar shows the link state, and a banner above the task list appears whenever it is not healthy:

| State | Meaning |
|---|---|
| Connecting / Connected | Opening the WebSocket / working normally. |
| Reconnecting | The connection dropped. Downloads keep running on the server; the banner counts down to the next try and shows the attempt number. |
| The server's download service is offline | The page is connected, but tasks cannot be controlled until the service recovers. |
| The FluxDown service has stopped / not compatible | The service stopped, or the web UI version does not match the server. |

Reconnects use exponential backoff: 0.5 s, doubling up to a 15 s cap, with random jitter of about +-20%. After the server reports that the event stream fell behind, the client reconnects sooner (from 250 ms, up to 3 s). Click **Retry now** to skip the wait. Live progress, speed and logs resume on their own once the socket is back; there is no need to reload.
