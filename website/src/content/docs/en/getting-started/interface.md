---
title: Interface Overview
description: A tour of FluxDown's three-panel layout, sidebar, top bar, task list, detail panel, status bar, shortcuts, and tray.
section: getting-started
order: 3
---

FluxDown's main window is a three-panel layout: a resizable **sidebar** on the left, the **task list** in the center, and an optional **detail panel** on the right that opens when you select a task.

<!-- TODO(screenshot): 主窗口整体布局,展示侧边栏+任务列表+详情面板三栏结构 -->

## Layout Overview

- **Sidebar** — drag its right edge to resize between 180px and 320px. If you hide every sidebar section (see below), the sidebar disappears entirely and the task list takes the full width.
- **Task list** — always takes up the remaining space between the sidebar and the detail panel.
- **Detail panel** — hidden until you click a task row; drag its left edge to resize between 240px and 420px. Click the row again, or the panel's close button, to hide it.

## Sidebar

Three collapsible sections, each independently toggleable:

- **Status** — All / Downloading / Completed / Paused / Failed, each showing a live task count; the Downloading entry also gets a small activity dot while something is transferring. Click an entry to filter the task list.
- **Queues** (expanded by default) — the built-in **Main Queue** and **Download Later** queues plus any named queues you've created (click **+** to create one). Each queue shows a running/stopped state dot; hover an entry for start/stop, manage, and (custom queues only) delete buttons, all also available from its right-click menu. The manager dialog has three tabs — **Settings** (name, speed limit, max concurrent, save directory, default threads, default User-Agent), **Schedule** (daily start/stop times plus weekdays; scheduled queues show a small alarm-clock icon), and **Task Order** (the order tasks resume in when the queue starts). See [Quick Start](/docs/en/getting-started/quickstart/#queues-and-speed-limits) for how queue start/stop works.
- **Category** (collapsed by default) — seven built-in file-type filters (All Files, Video, Audio, Document, Image, Archive, Other) plus any custom categories you define in **Settings → General → Custom Categories**, matched by extension or regular expression.

Right-click a section header for **Hide this section**; bring it back from **Settings → General → Sidebar Sections**. The sidebar's footer shows the current version and an inline update button once one is available.

## Top Bar

- **New Download** — opens the download dialog.
- **Search** (<kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>F</kbd>) — searches both task names/URLs and settings items as you type; use the arrow keys to move through results and Enter to jump to one.
- **Tool buttons** (top-right, next to the window controls) — Pause All, Resume All, Settings, and a light/dark theme toggle. Each can be hidden independently from **Settings → General → Titlebar Buttons**, or by right-clicking the button itself.

On macOS the traffic-light window controls sit in the top-left corner; on Windows and Linux the tool buttons and window controls are grouped in the top-right.

## Task List

The column header shows File Name / Progress / Speed / ETA / Status, with a manage-mode toggle next to the file name column.

- **Manage mode** — press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>A</kbd> or click the toggle button to enter it. The header switches to a checkbox, and the top bar becomes a batch action bar (Select All / Deselect All, a selected-count badge, Delete Task, Delete Task & File, Cancel).
- **Grouping** — an always-expanded **Active** group sits at the top for downloading/pending/preparing tasks (with its own pause-all shortcut), followed by collapsible time groups — Today, Yesterday, This Week, This Month, Older — based on when each task was created. Click a group header to collapse or expand it.
- **Double-click** a row to pause it (if active), resume it (if paused/failed), or open the finished file (if completed).
- **Right-click** a row for the full context menu, top to bottom: Pause/Resume, Boost Download / Cancel Boost, Open File (completed tasks only) / Open Folder, Copy URL, Delete Task, Delete Task & File.
- **Right-click empty space** below the list for New Download, Start All, and Pause All.

<!-- TODO(screenshot): 任务列表按时间分组(今天/昨天/本周)并展示某个分组折叠状态 -->

## Detail Panel

Click a task to open it. From top to bottom:

- **File info** — icon, extension, and full file name.
- **Progress** — a large percentage readout, a segmented progress bar, and (once segment data exists) an IDM-style pixel grid where each cell reflects how full that byte range is, plus a legend listing each segment's number and percentage. When FluxDown proactively or reactively splits a slow segment, a running split count and the most recent split's byte range are shown alongside the progress bar.
- **Info table** — Size, Downloaded, Speed, Remaining (ETA), Status, Threads (while active), Path, URL (with a one-click copy button), and an Error message row if the task failed.
- **Actions** — a Pause/Resume button, and a destructive Delete Task & File button.

<!-- TODO(screenshot): 详情面板的 IDM 风格分段网格,展示动态拆分产生的多个分段 -->

## Status Bar

A thin bar along the bottom of the window:

- **Left** — a status dot (green while any task is active, gray when idle) with a label, the live combined download speed, and an "N active · N paused · N total" summary.
- **Right** — a speed-limit popover (toggle plus presets of 128 KB/s / 512 KB/s / 1 MB/s / 2 MB/s / 5 MB/s, or a custom value), and a shutdown-after-completion popover (presets of 0/1/5/10/30 minutes with a live countdown once armed).

## Keyboard Shortcuts

These work anywhere in the main window (not while Settings or a dialog is open):

| Shortcut | Action |
|---|---|
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>F</kbd> | Focus the search box |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>A</kbd> | Select all visible tasks (enters manage mode) |
| <kbd>Esc</kbd> | Exit manage mode |
| <kbd>Delete</kbd> (or <kbd>Cmd</kbd>+<kbd>Backspace</kbd> on macOS) | Delete the checked tasks, with a confirmation prompt (manage mode only) |

## System Tray

FluxDown keeps a tray icon running while the window is minimized or closed to tray. Right-click it for:

- **Show Window** — bring the main window back to front.
- **Show Floating Ball** (checkbox) — an always-on-top desktop widget showing live speed and progress; you can drop URLs or torrent files onto it to start a download. Unavailable on Wayland — the tray falls back to showing live speed, and copied links are auto-filled into the download dialog when you reopen the main window.
- **Exit** — quits FluxDown.
