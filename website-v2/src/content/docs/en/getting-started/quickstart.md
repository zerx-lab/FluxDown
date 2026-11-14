---
title: Quick Start
description: Create your first download, control tasks, and set up queues and speed limits.
section: getting-started
order: 2
---

This walks through the full flow of creating a download in FluxDown, controlling it while it runs, and organizing tasks with queues and speed limits. See [Installation](/docs/en/getting-started/installation/) if you haven't set up FluxDown yet, and [Interface Overview](/docs/en/getting-started/interface/) for a tour of every panel mentioned here.

## Create Your First Download

Click **New Download** in the top bar (or press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>F</kbd> to search, then open it from a result). The dialog fields, top to bottom:

| Field | What it does |
|---|---|
| **Download URL** | A multi-line box — paste one URL per line to queue a batch download (magnet and `ed2k://` links work too), or a single URL for one task. FluxDown shows a live count of parsed URLs. Use **Open .torrent file** to pick a local `.torrent`, or **Import TXT file** to load a list of URLs from a text file. |
| **Save Directory** | Where the file lands. Defaults to your global save directory (**Settings → Download**), or your last-used folder if **Remember Last Save Location** is on. |
| **Threads** | Segments to split the download into: **Auto** (FluxDown picks based on file size and CPU count), a fixed preset (4/8/16/32/64), or a custom value from 1–512 (above 64 a warning about server throttling / IP bans appears). Hidden for magnet links and `.torrent` files, since BitTorrent manages its own connections. |
| **Rename (optional)** | Override the detected filename. Only shown for a single URL — batch downloads and torrents always use the detected/embedded name. |

Click **Advanced Options** to reveal per-task overrides that default to your global settings when left empty:

| Field | What it does |
|---|---|
| **Task Proxy** | A proxy just for this task, e.g. `socks5://127.0.0.1:1080` or `http://host:port`. Leave empty to use the global proxy. Doesn't apply to BitTorrent downloads. |
| **User-Agent** | Pick a preset (Chrome, Firefox, Edge, Safari) or type a custom string. |
| **Cookie** | Raw `name=value; name2=value2` pairs, for downloads that require you to be logged in. |
| **Hash Verification** | Pick an algorithm (MD5/SHA-1/SHA-256/SHA-512) and paste the expected hash. FluxDown verifies the file after download and flags a mismatch. Leave blank to skip. |
| **Custom Headers** | Extra HTTP headers as key/value rows (use **+ Add header** for more). Use the Cookie field above for cookies rather than a `Cookie` header here. |

Two action buttons submit the form, each with a queue picker on its arrow:

- **Start Download ▾** (shown as **Download N files** for a batch) — creates the task(s) and starts them right away. By default they join your default queue (**Settings → Download → Default Queue**, or the queue currently selected in the sidebar).
- **Download Later ▾** — creates the task(s) in a **paused** state without starting them, parked in the built-in **Download Later** queue. They don't occupy a concurrency slot until you start them (or start the whole queue — see below).

Click either button's small arrow to pick an explicit target queue instead — choosing a queue from the menu submits immediately into it.

<!-- TODO(screenshot): 新建下载对话框,展示 URL 多行输入 + 展开的高级选项 -->

## Controlling Tasks

- **Double-click** a row: pauses an active task, resumes a paused/failed one, or opens the file if it's already complete.
- **Right-click** a row for the full menu: Pause/Resume, Boost Download (see below), Open File (completed tasks only) / Open Folder, Copy URL, Delete Task, and Delete Task & File.
- **Multiple tasks at once**: press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>A</kbd> or click the manage-mode toggle next to the file name column to check tasks individually, then use the batch action bar to delete the selection (with or without the underlying files). Press <kbd>Esc</kbd> to leave manage mode.
- **Pause/Resume everything**: use the Pause All / Resume All buttons in the top bar, or right-click empty space below the list for Start All / Pause All.

### Edit task properties

Choose **Edit task** from a task's right-click menu or **General** details. The unified editor covers save directory, file name, proxy, task download limit, connections, User-Agent, headers, cookies, credential reference, category, tags, and an optional one-shot start time. The URL is read-only; use the existing **Change link** action when replacing a source.

Saving validates all changed fields before applying them. Relocation preserves existing files and progress, rejects destination conflicts, and rolls back on failure. Active transfers reconnect when their request settings or location change; changing a speed limit or tags does not restart them. No application or NAS service restart is needed. Sources without resume support may restart from zero, and live recordings may have gaps.

Cookies are write-only; secret headers are redacted and retained unless explicitly changed or removed. Credential references must be valid for the task's origin. A positive task speed limit overrides the queue/global limit; zero follows it again. A future start time pauses the task until it is due; a stopped queue must still be started.

Unsupported fields are disabled for the task's protocol or state. BT storage relocation and download limits use the existing BT core; BT file-tree renaming remains in its dedicated per-file actions. Task-level proxy and HTTP request fields do not apply to BT/eD2K. Manual categories override automatic classification; tags are searchable.


## Queues and Speed Limits

A global speed limit lives in the **status bar** at the bottom of the window — click the limit indicator to open a popover with presets (128 KB/s, 512 KB/s, 1 MB/s, 2 MB/s, 5 MB/s) or type a custom KB/s value. It's off (unlimited) by default.

Every task belongs to a queue. Two queues are built in and can't be deleted or renamed:

- **Main Queue** — where new downloads land unless you pick another queue.
- **Download Later** — where the **Download Later** button parks tasks in a paused state.

Create additional **named queues** from the **Queues** section of the sidebar (click the **+** button). Click any queue in the sidebar to filter the task list down to it.

### Queue settings

Hover (or right-click) a queue and open its manager dialog. The **Settings** tab holds the per-queue configuration:

- Name (locked for the two built-in queues)
- Speed limit in KB/s (0 = unlimited)
- Max concurrent downloads (0 = use the global setting)
- Default save directory
- Default thread count (0 = auto)
- Default User-Agent (empty = inherit the global one)

Deleting a named queue (built-in queues can't be deleted) moves its tasks back to the Main Queue. To move an existing task between queues, use the **Queue** tab of its detail panel.

### Starting and stopping a queue

A queue is always either **running** or **stopped** — the small dot next to its name in the sidebar shows which. Hover a queue for a play/pause button (also available from its right-click menu):

- **Stop Queue** pauses every waiting and active task in it. While a queue is stopped, nothing inside it is auto-started: **Resume All** and the scheduler skip stopped queues entirely.
- **Start Queue** flips it back to running and resumes its unfinished tasks, in queue order.

This makes a queue a one-click switch for a whole group of downloads — for example, keep bulk downloads in their own queue and stop it while you need the bandwidth, or park tasks in **Download Later** and start that queue overnight.

### Daily schedule

Each queue can start and/or stop itself automatically. The queue manager's **Schedule** tab has:

- A daily **start time** and **stop time** (`HH:MM`, picked from a time grid). You can fill either one or both — an empty side simply isn't scheduled.
- The **weekdays** the schedule applies to.

When the clock crosses a scheduled time on an active day, the queue starts or stops exactly as if you'd clicked the button; each edge fires at most once per day, and an edge that already passed earlier the same day is caught up when FluxDown launches. Queues with an active schedule show a small alarm-clock icon in the sidebar.

### Task order within a queue

The queue manager's **Task Order** tab lists the queue's tasks; move entries up or down to set the order in which they're started when the queue starts. Changes are persisted immediately.

### Concurrency

The global **Max Concurrent Downloads** cap (default 5) lives in **Settings → Download** and applies across all queues unless a queue overrides it with its own limit.

## Boost: Prioritize a Download

Need one file now? Right-click any task that isn't finished yet and choose **Boost Download**. FluxDown pauses every other active task and gives the boosted one full bandwidth; a banner appears at the top of the task list showing how many tasks will automatically resume once it completes, with a **Cancel** link to stop early. The boosted row also gets a small lightning-bolt badge. Right-click it again and choose **Cancel Boost** to release it manually.

## After a Download Completes

By default (**Settings → General → Completion Notifications**), FluxDown shows a system notification with **Open File** / **Open Folder** actions — a floating toast in the bottom-right corner on Windows, or a native notification on macOS/Linux. Notifications for multiple near-simultaneous completions are batched together automatically. You can also just double-click the finished task, or right-click it for Open File / Open Folder at any time.
