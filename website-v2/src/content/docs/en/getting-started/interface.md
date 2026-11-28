---
title: Interface Overview
description: "A tour of the FluxDown desktop window: activity rail, sidebar, toolbar, task list, View menu, detail panel, status bar, themes, shortcuts, and tray."
section: getting-started
order: 3
---
The main window has five areas: an activity rail, a sidebar, a toolbar, the task list, and a status bar. A detail panel opens inside the task list area when you need it.

![FluxDown desktop main window with numbered callouts for the activity rail, sidebar, toolbar, task list, and status bar](/docs/desktop/main-window-annotated.en.webp)

1. **Activity rail** - switches between pages (Downloads, RSS, Notifications); account and **Settings** sit at the bottom.
2. **Sidebar** - filters the list by status, category, and queue.
3. **Toolbar** - New Task, task actions, search, and the **View** menu.
4. **Task list** - one row per task.
5. **Status bar** - live speeds, speed limits, proxy, power menu, and version.

## Activity Rail

The narrow icon column on the far left:

- **Downloads** - the page described here.
- **RSS** and **Notifications** - RSS subscriptions and push-notification channels. Each can be hidden under **Settings → General → Activity Bar**.
- **Account** and **Settings** - at the bottom of the rail.

## Sidebar

The sidebar has a **STATUS** folder and a **QUEUES** section. Click an entry to filter the task list; each shows a live task count.

- **All** - every task. Click it to expand the category sub-items nested under it: Video, Audio, Document, Image, Programs, Archive, Other, plus any custom categories. Only **All** nests categories; there is no separate Categories section. Right-click a category to **Edit Category** or **Add Category**.
- **Downloading**, **Completed**, **Seeding**, **Failed**, **Paused** - one entry per state. Seeding tasks also count as completed.
- **Main Queue**, **Download Later**, and any queues you created. The manage button in the **QUEUES** section opens the queue manager; right-click a queue for **Start Queue** / **Stop Queue**, **Manage Queue**, and **Delete Queue**. See [Quick Start](/docs/en/getting-started/quickstart/#queues-and-speed-limits).
- A **Devices** section appears only when other devices are linked to your account.

Right-click a section header and choose **Hide this section**, or manage them under **Settings → General → Sidebar Sections**. The toolbar's sidebar button collapses and expands the sidebar; drag its edge to resize it.

## Toolbar

From left to right:

- **Sidebar toggle** - **Collapse sidebar** / **Expand sidebar**.
- **New Task** - opens the New Download window. In layout edit mode (see below) it can be shown as an icon or as a labelled button.
- **Clear Completed Tasks** - removes finished tasks from the list.
- **More Actions** - **Clean up invalid tasks** and **Clean up selected paused or failed tasks**.
- **Pause All**, **Resume All**, **Retry failed tasks**.
- **Search tasks...** (<kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>F</kbd>) - filters the list as you type. To search commands and settings instead, press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>K</kbd>.
- **View** - columns, grouping, sorting, and display options.

## Task List

Each row shows the file name with its category and source site underneath, then the columns you have enabled. By default these are **File Name**, **Size**, **Progress**, **Status** (speed, remaining time, and downloaded / total), and **Created**. To hide the source site, turn on **Hide task source** under **Settings → General**.

- **Select** - click a row; <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+click toggles rows and <kbd>Shift</kbd>+click selects a range. With several rows selected a selection bar shows **{n} selected** and bulk actions: pause/resume, open file/folder, **Copy task information**, **Export task information (.txt)**, delete, and **Deselect All**.
- **Double-click** - opens a completed task's file. For any other task it opens the detail panel.
- **Hover actions** - while the pointer is over a row, its right end shows Pause / Resume (Retry for failed tasks), Open File, Open Folder, and **Details**.
- **Right-click** - the context menu. Items depend on the task type and state: **Pause** / **Resume**, **Boost Download** / **Cancel Boost**, **Open File**, **Open Folder**, **Copy File**, **Rename**, **Change link**, **Edit task**, **Verify Integrity**, **Re-download**, **Move to Queue**, **Copy URL**, **Select**, **Select All**, **Delete Task**, **Delete Task & File**, and **Details**.

## View Menu

**View** in the toolbar has four pages. The View menu **Columns** page lists every column with a checkbox and a drag handle; drag to reorder, and **Reset to Default Columns** restores the defaults. Optional columns: **Speed**, **ETA**, **Protocol**, **Source**, **Queue**.

| Page | Options |
|---|---|
| **Columns** | Show, hide, and reorder columns. |
| **Group By** | **No Grouping**, **Status**, **Date**, **Type**, **Queue**, **Site**, **Directory**, **Series title**. |
| **Sort** | **Smart**, **Created Time**, **Name**, **Size**, **Progress**, **Speed**, **Status**, each **Ascending** or **Descending**. |
| **Display** | **Density** (**Comfortable** / **Compact** / **Relaxed**), **Header Alignment**, and the **Detail Panel**: **Show Detail Panel** plus **Bottom** or **Right** placement. |

Groups can be collapsed by clicking the group header. The screenshot shows **Group By → Status**.

![Task list grouped by status into Downloading, Error, Paused, and Completed groups](/docs/desktop/group-by-status.en.webp)

## Detail Panel

Select a task and press <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>I</kbd>, or choose **Details** from the context menu, to open the panel. By default it docks at the bottom; switch to the right side under **View → Display**. A button in the panel pops it out into its own window. With nothing selected it shows **Select a task to view details**.

HTTP tasks have these tabs:

- **General** - status, size, downloaded, speed, remaining time, save path, route, and the source breakdown.
- **Speed** - a speed chart with current, average, and peak speed.
- **Log** - the task's activity events (retries, segment splits, CDN and network events).
- **Advanced** - the proxy route and TLS option the task uses.

BitTorrent tasks add **Files**, **Seeding**, **Trackers**, and **Peers**; eD2K tasks add their own sources and network tabs; plugins can contribute more.

## Status Bar

The status bar runs along the bottom of the window.

![Status bar with seven numbered callouts: download speed, upload speed, download limit, upload limit, proxy, power menu, version](/docs/desktop/statusbar.en.webp)

1. **Download speed** - total speed of all tasks.
2. **Upload speed** - total upload speed (BitTorrent).
3. **Download limit** - click to choose **Unlimited**, a preset (1, 5, 10, or 50 MB/s), or **Custom**. Recent custom values are listed and can be cleared with **Clear recent**.
4. **Upload limit** - the same choices; limits BitTorrent upload, including seeding.
5. **Proxy** - switch between **No Proxy**, **System Proxy**, **Manual**, and **Auto (Recommended)**. If no proxy is configured it points you to **Settings** first.
6. **Power menu** (**Scheduled Power Actions**) - pick **Shut down**, **Sleep**, or **Quit FluxDown**, then when: **Immediately when done**, after 1 / 5 / 10 / 30 minutes, **Custom**, or **When download speed stays low**. Once armed, the status bar shows the countdown; click it (or use the tray menu) to cancel.
7. **Version** - the installed version; an update prompt appears here when one is available.

A **Free space** readout is available but hidden by default (see layout editing below).

## Themes

Choose **Light**, **Dark**, or **System** under **Settings → Appearance → Theme Mode**. The command palette (<kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>K</kbd>) also has **Toggle theme**.

![FluxDown main window in the dark theme](/docs/desktop/dark-theme.en.webp)

## Customizing the Layout

Right-click the toolbar, the activity rail, or the status bar and choose **Customize layout...**. Drag items to reorder them or move them between the title bar, activity bar, and status bar; click the eye icon on an item to hide or show it. **Reset layout** restores the defaults and **Done** leaves edit mode.

## Keyboard Shortcuts

| Shortcut | Action | Where |
|---|---|---|
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>N</kbd> | New download | Any window |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>,</kbd> | Open settings | Any window |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>K</kbd> | Command palette (commands and settings) | Any window |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>W</kbd> | Close window | Any window |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>F</kbd> | Focus search | Task list |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>A</kbd> | Select all tasks | Task list |
| <kbd>Esc</kbd> | Clear selection | Task list |
| <kbd>Space</kbd> | Pause / resume selected | Task list |
| <kbd>Delete</kbd> / <kbd>Backspace</kbd> | Delete selected | Task list |
| <kbd>Shift</kbd>+<kbd>Delete</kbd> | Delete selected with files | Task list |
| <kbd>Ctrl</kbd>/<kbd>Cmd</kbd>+<kbd>I</kbd> | Toggle detail panel | Task list |
| <kbd>Shift</kbd>+<kbd>D</kbd> | Cycle list density | Task list |
| <kbd>G</kbd> | Cycle grouping | Task list |
| <kbd>S</kbd> | Cycle sort order | Task list |

All of these can be changed under **Settings → Shortcuts**.

## System Tray

With **Minimize to Tray on Close** on (the default), closing the window keeps FluxDown running in the tray. Left-click the tray icon to bring the window back, or right-click for the menu:

- **Show Window**
- **Pause All**, **Resume All**
- **Settings**
- **Restart**
- **Exit** - quits FluxDown.

While a scheduled power action is armed, a **Cancel Scheduled Shutdown** (or Sleep / Quit) item also appears.
