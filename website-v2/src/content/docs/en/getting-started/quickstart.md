---
title: Quick Start
description: Create your first download, control tasks, and set up queues and speed limits.
section: getting-started
order: 2
---

This walks through creating a download in FluxDown, controlling it while it runs, and organizing tasks with queues and speed limits. See [Installation](/docs/en/getting-started/installation/) if you haven't set up FluxDown yet, and [Interface Overview](/docs/en/getting-started/interface/) for a tour of the window.

## Create Your First Download

Click **New Task** in the toolbar or press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>N</kbd>. The **New Download** window opens.

![New Download window with three URLs pasted: a zip, an mp4, and a magnet link](/docs/desktop/new-download.en.webp)

Fields, top to bottom:

| Field | What it does |
|---|---|
| **Download URL** | One URL per line. HTTP, FTP, and magnet links work; a live **N URLs** count appears at the top right. For numbered files, type a range in the file name, for example `https://example.com/comic/[001-215].webp`. Ranges that expand to more than 1,000 links are rejected. |
| **Open .torrent file** / **Import TXT file** | Pick a local `.torrent`, or load a list of URLs from a text file. |
| **Save Directory** | The folder to save into. The picker on the left chooses **Auto** (the category folder is chosen from the file type), **No category**, or a specific category; **Browse** opens a folder dialog. The default folder comes from **Settings → Download**, or your last folder if **Remember Last Save Location** is on. |
| **Threads** | **Auto**, a preset (4, 8, 16, 32, 64), or **Custom** (up to 512). Above 64 a warning about throttling or IP bans appears. Not shown for magnet links. |
| **Rename (optional, auto-detect if empty)** | Overrides the detected file name. Only shown for a single URL. |

Click **Advanced Options** to override settings for these tasks only. Anything left empty follows your global settings.

| Option | What it does |
|---|---|
| **Task Proxy** | **Follow Global** (proxy mode and site rules), **Direct (no proxy)**, **System Proxy**, **Manual Proxy (global config)**, or **Custom...** with a URL such as `socks5://127.0.0.1:1080`. Any choice other than **Follow Global** ignores site rules for this task. |
| **User-Agent** | A preset (**Default (FluxDown)**, **Chrome**, **Firefox**, **Edge**, **Safari (macOS)**) or **Custom**. **Manage presets** edits your own list. |
| **Cookie** | Raw `name=value; name2=value2` pairs, for downloads that need a login. |
| **Source page (Referer)** | The Referer header to send. |
| **Hash Verification** | An algorithm and the expected hash. The file is checked after download. |
| **Custom Headers** | Extra HTTP headers as name/value rows (**Add header**). |
| **HTTP Authentication** | User name and password; **Save for this site** remembers them. Shown for a single link. |
| **Ignore HTTPS certificate errors** | Accept invalid certificates for these tasks. |

When a link is captured from your browser and it already carries HTTP authentication, the window says so; enter a user name under **HTTP Authentication** to use your own credentials instead.

The buttons at the bottom:

- **Download N files** (**Start** for a single link) - creates the tasks and starts them. They go to the queue currently selected in the sidebar, otherwise your **Default Queue** (**Settings → Download**), otherwise the **Main Queue**.
- **Download Later** - creates the tasks without starting them, in the built-in **Download Later** queue.
- The arrow on either button lists your queues. Choosing one submits immediately into that queue.
- **Export task information (.txt)** - shown when more than one URL is entered; saves the parsed list to a text file.
- **Cancel** - closes the window.

## Controlling Tasks

- **Double-click** a row - opens a completed task's file; for other tasks it opens the detail panel.
- **Hover actions** - Pause / Resume, Open File, Open Folder, and **Details** appear at the right end of a row. Failed tasks show a retry button.
- **Right-click** a row for the full menu: **Pause** / **Resume**, **Boost Download**, **Open File**, **Open Folder**, **Copy URL**, **Move to Queue**, **Delete Task**, **Delete Task & File**, and more. The items depend on the task's state and protocol.
- **Several tasks at once** - <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+click or <kbd>Shift</kbd>+click rows, press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>A</kbd> for all, or choose **Select** in the menu. A selection bar then offers pause/resume, open, **Copy task information**, **Export task information (.txt)**, delete, and **Deselect All**. <kbd>Space</kbd> pauses or resumes the selection; <kbd>Esc</kbd> clears it.
- **Everything at once** - **Pause All**, **Resume All**, and **Retry failed tasks** in the toolbar. **Resume All** skips stopped queues.

### Edit task properties

Choose **Edit task** from a task's right-click menu to change its save directory, download limit, connection settings, User-Agent, headers, cookies, authentication reference, category, tags, and an optional one-shot start time. The URL is read-only; use **Change link** to replace a source.

Connection changes reconnect an active task immediately, and sources that cannot resume may restart. File changes pause, move, and resume the task. Cookies are write-only: once configured they are never shown again, only cleared with **Clear cookies**. A property a task does not support shows a notice instead of being changed.

## Queues and Speed Limits

The status bar holds two limits, **Download Limit** and **Upload Limit**. Click one to pick **Unlimited**, a preset (1, 5, 10, or 50 MB/s), or **Custom**. The upload limit applies to BitTorrent uploads, including seeding. Both are unlimited by default.

Every task belongs to a queue. Two are built in:

- **Main Queue** - new downloads land here unless you choose another queue. It always runs; it cannot be stopped, scheduled, renamed, or deleted.
- **Download Later** - where **Download Later** parks tasks. It cannot be renamed or deleted.

### Create and edit queues

Click the manage button in the **QUEUES** section of the sidebar, or right-click a queue and choose **Manage Queue**, to open the queue manager. Choose **New Queue** to add one. Each queue has:

- **Queue Name** (locked for built-in queues)
- **Download Limit** and **Upload Limit**
- **Max Concurrent Downloads** (0 = no queue limit; the global limit still applies)
- **Threads** (0 = auto)
- **Default Save Directory**
- **Default User-Agent**

Click a queue in the sidebar to filter the task list to it. To move an existing task, right-click it and use **Move to Queue**. Deleting a custom queue moves its tasks to the Main Queue.

### Start and stop a queue

A queue is **Running** or stopped. Right-click it in the sidebar, or use the button in the queue manager:

- **Stop Queue** stops the queue. A stopped queue is skipped by **Resume All**, which tells you to start the queue first.
- **Start Queue** resumes its unfinished tasks.

This makes a queue a one-click switch for a group of downloads. Keep bulk downloads in their own queue, or park tasks in **Download Later** and start that queue overnight.

### Daily schedule

In the queue manager, turn on **Enable daily schedule** for a custom queue or **Download Later**. Set a start time, a stop time, or both (an empty side is not scheduled), and the weekdays under **Repeat**. At a scheduled time the queue starts or stops as if you had clicked the button. Times missed while the computer was asleep fire after it wakes the same day.

### Concurrency

**Max Concurrent Downloads** in **Settings → Download** (default 5) caps running tasks across all queues, unless a queue sets its own limit.

## Boost: Prioritize a Download

Need one file now? Right-click an unfinished task and choose **Boost Download**. FluxDown pauses the other active and queued tasks so the boosted task gets the full bandwidth, and marks it with a lightning-bolt icon. When it completes or stops, the paused tasks resume in order. Choose **Cancel Boost** to release it manually. Completed and failed tasks cannot be boosted; retry a failed task first.

## After a Download Completes

Under **Settings → Notifications** you can control what happens when a task finishes:

- **Completion Notifications** - a system notification.
- **Show download complete window** - a window with **Open File** and **Open Folder**, shown for downloads you started unless you are looking at the main window.
- **Show download progress window** - when you add a download while not looking at the main window, a separate progress window opens without taking focus. Closing it does not stop the download.

You can always double-click a finished task to open it, or right-click it for **Open File** / **Open Folder**.
