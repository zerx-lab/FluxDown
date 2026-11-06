package com.fluxdown.fluxui.icons

import androidx.compose.ui.graphics.vector.ImageVector

/*
 * Lucide（ISC）母版的 1.5dp 线性图标（24 栅格），名称与设计原型 FD_ICONS 一致（kebab → PascalCase）；
 * 来源：`crates/components/assets/icons` 目录的 SVG + 原型补充集，元素（rect / circle / line / polyline）已展开为 path。
 * 新增图标：取 Lucide SVG，把各元素换算成 path 字符串后按 `fluxIcon(id, filled, paths…)` 追加；
 * `FluxIconsTest` 校验每个图标都能构建且路径非空。许可见 assets/licenses/lucide-LICENSE.txt。
 */
@Suppress("unused")
object FluxIcons {
    val Activity: ImageVector by lazy { fluxIcon("activity", false, "M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2") }
    val AppWindow: ImageVector by lazy { fluxIcon("app-window", false, "M4 4h16a2 2 0 0 1 2 2v12a2 2 0 0 1 -2 2h-16a2 2 0 0 1 -2 -2v-12a2 2 0 0 1 2 -2Z", "M10 4v4", "M2 8h20", "M6 4v4") }
    val Archive: ImageVector by lazy { fluxIcon("archive", false, "M3 3h18a1 1 0 0 1 1 1v3a1 1 0 0 1 -1 1h-18a1 1 0 0 1 -1 -1v-3a1 1 0 0 1 1 -1Z", "M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8", "M10 12h4") }
    val ArrowDown: ImageVector by lazy { fluxIcon("arrow-down", false, "M12 5v14", "m19 12-7 7-7-7") }
    val ArrowDownToLine: ImageVector by lazy { fluxIcon("arrow-down-to-line", false, "M12 17V3", "m6 11 6 6 6-6", "M19 21H5") }
    val ArrowLeft: ImageVector by lazy { fluxIcon("arrow-left", false, "m12 19-7-7 7-7", "M19 12H5") }
    val ArrowUp: ImageVector by lazy { fluxIcon("arrow-up", false, "m5 12 7-7 7 7", "M12 19V5") }
    val ArrowUpDown: ImageVector by lazy { fluxIcon("arrow-up-down", false, "m21 16-4 4-4-4", "M17 20V4", "m3 8 4-4 4 4", "M7 4v16") }
    val ArrowUpDown2: ImageVector by lazy { fluxIcon("arrow-up-down-2", false, "m3 8 4-4 4 4", "M7 4v16", "m21 16-4 4-4-4", "M17 20V4") }
    val ArrowUpRight: ImageVector by lazy { fluxIcon("arrow-up-right", false, "M7 7h10v10", "M7 17 17 7") }
    val BadgeCheck: ImageVector by lazy { fluxIcon("badge-check", false, "M3.85 8.62a4 4 0 0 1 4.78-4.77 4 4 0 0 1 6.74 0 4 4 0 0 1 4.78 4.78 4 4 0 0 1 0 6.74 4 4 0 0 1-4.77 4.78 4 4 0 0 1-6.75 0 4 4 0 0 1-4.78-4.77 4 4 0 0 1 0-6.76Z", "m9 12 2 2 4-4") }
    val BatteryCharging: ImageVector by lazy { fluxIcon("battery-charging", false, "M15 7h1a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-2", "M6 7H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h1", "m11 7-3 5h4l-3 5", "M22 11L22 13") }
    val BatteryFull: ImageVector by lazy { fluxIcon("battery-full", false, "M10 10v4", "M14 10v4", "M22 14v-4", "M6 10v4", "M4 6h12a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2Z") }
    val BatteryWarning: ImageVector by lazy { fluxIcon("battery-warning", false, "M4 6h12a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2Z", "M22 14v-4", "M10 9v3.2", "M10 15.2h.01") }
    val Bell: ImageVector by lazy { fluxIcon("bell", false, "M10.268 21a2 2 0 0 0 3.464 0", "M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326") }
    val BellOff: ImageVector by lazy { fluxIcon("bell-off", false, "M10.268 21a2 2 0 0 0 3.464 0", "M17 17H4a1 1 0 0 1-.74-1.673C4.59 13.956 6 12.499 6 8a6 6 0 0 1 .258-1.742", "m2 2 20 20", "M8.668 3.01A6 6 0 0 1 18 8c0 2.687.77 4.653 1.707 6.05") }
    val BellRing: ImageVector by lazy { fluxIcon("bell-ring", false, "M10.268 21a2 2 0 0 0 3.464 0", "M22 8c0-2.3-.8-4.3-2-6", "M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326", "M4 2C2.8 3.7 2 5.7 2 8") }
    val Bluetooth: ImageVector by lazy { fluxIcon("bluetooth", false, "m7 7 10 10-5 5V2l5 5L7 17") }
    val Bookmark: ImageVector by lazy { fluxIcon("bookmark", false, "M17 3a2 2 0 0 1 2 2v15a1 1 0 0 1-1.496.868l-4.512-2.578a2 2 0 0 0-1.984 0l-4.512 2.578A1 1 0 0 1 5 20V5a2 2 0 0 1 2-2z") }
    val Box: ImageVector by lazy { fluxIcon("box", false, "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z", "m3.3 7 8.7 5 8.7-5", "M12 22V12") }
    val Braces: ImageVector by lazy { fluxIcon("braces", false, "M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1", "M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1") }
    val Calendar: ImageVector by lazy { fluxIcon("calendar", false, "M8 2v4", "M16 2v4", "M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M3 10h18") }
    val Camera: ImageVector by lazy { fluxIcon("camera", false, "M13.997 4a2 2 0 0 1 1.76 1.05l.486.9A2 2 0 0 0 18.003 7H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h1.997a2 2 0 0 0 1.759-1.048l.489-.904A2 2 0 0 1 10.004 4z", "M9 13a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z") }
    val Check: ImageVector by lazy { fluxIcon("check", false, "M20 6 9 17l-5-5") }
    val CheckCheck: ImageVector by lazy { fluxIcon("check-check", false, "M18 6 7 17l-5-5", "m22 10-7.5 7.5L13 16") }
    val CheckCircle: ImageVector by lazy { fluxIcon("check-circle", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "m9 12 2 2 4-4") }
    val CheckboxCheck: ImageVector by lazy { fluxIcon("checkbox-check", false, "M20 6 9 17l-5-5") }
    val CheckboxMinus: ImageVector by lazy { fluxIcon("checkbox-minus", false, "M5 12h14") }
    val ChevronDown: ImageVector by lazy { fluxIcon("chevron-down", false, "m6 9 6 6 6-6") }
    val ChevronDownSm: ImageVector by lazy { fluxIcon("chevron-down-sm", false, "m6 9 6 6 6-6") }
    val ChevronLeft: ImageVector by lazy { fluxIcon("chevron-left", false, "m15 18-6-6 6-6") }
    val ChevronRight: ImageVector by lazy { fluxIcon("chevron-right", false, "m9 18 6-6-6-6") }
    val ChevronUp: ImageVector by lazy { fluxIcon("chevron-up", false, "m18 15-6-6-6 6") }
    val ChevronsUpDown: ImageVector by lazy { fluxIcon("chevrons-up-down", false, "m7 15 5 5 5-5", "m7 9 5-5 5 5") }
    val Circle: ImageVector by lazy { fluxIcon("circle", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z") }
    val CircleAlert: ImageVector by lazy { fluxIcon("circle-alert", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M12 8L12 12", "M12 16L12.01 16") }
    val CircleArrowDown: ImageVector by lazy { fluxIcon("circle-arrow-down", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M12 8v8", "m8 12 4 4 4-4") }
    val CircleCheck: ImageVector by lazy { fluxIcon("circle-check", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "m16 9-5.5 5.5L8 12") }
    val CircleDot: ImageVector by lazy { fluxIcon("circle-dot", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z") }
    val CircleHelp: ImageVector by lazy { fluxIcon("circle-help", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3", "M12 17h.01") }
    val CirclePause: ImageVector by lazy { fluxIcon("circle-pause", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M10 15L10 9", "M14 15L14 9") }
    val CircleUser: ImageVector by lazy { fluxIcon("circle-user", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M9 10a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z", "M7 20.662V19a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v1.662") }
    val CircleX: ImageVector by lazy { fluxIcon("circle-x", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "m15 9-6 6", "m9 9 6 6") }
    val Clipboard: ImageVector by lazy { fluxIcon("clipboard", false, "M9 2h6a1 1 0 0 1 1 1v2a1 1 0 0 1 -1 1h-6a1 1 0 0 1 -1 -1v-2a1 1 0 0 1 1 -1Z", "M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2") }
    val ClipboardPaste: ImageVector by lazy { fluxIcon("clipboard-paste", false, "M11 14h10", "M16 4h2a2 2 0 0 1 2 2v1.344", "m17 18 4-4-4-4", "M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 1.793-1.113", "M9 2h6a1 1 0 0 1 1 1v2a1 1 0 0 1 -1 1h-6a1 1 0 0 1 -1 -1v-2a1 1 0 0 1 1 -1Z") }
    val Clock: ImageVector by lazy { fluxIcon("clock", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M12 6v6l4 2") }
    val Cloud: ImageVector by lazy { fluxIcon("cloud", false, "M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z") }
    val CloudAlert: ImageVector by lazy { fluxIcon("cloud-alert", false, "M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z", "M12 9.5v3.2", "M12 15.6h.01") }
    val CloudCheck: ImageVector by lazy { fluxIcon("cloud-check", false, "M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z", "m9.5 13.5 2 2 3.5-3.5") }
    val CloudDownload: ImageVector by lazy { fluxIcon("cloud-download", false, "M12 13v8l-4-4", "m12 21 4-4", "M4.393 15.269A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.436 8.284") }
    val CloudOff: ImageVector by lazy { fluxIcon("cloud-off", false, "m2 2 20 20", "M5.782 5.782A7 7 0 0 0 9 19h8.5a4.5 4.5 0 0 0 1.307-.193", "M21.532 16.5A4.5 4.5 0 0 0 17.5 10h-1.79A7.008 7.008 0 0 0 10 5.07") }
    val CloudSync: ImageVector by lazy { fluxIcon("cloud-sync", false, "M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z", "M12 16v-5", "m9.8 13 2.2-2.2 2.2 2.2") }
    val Code: ImageVector by lazy { fluxIcon("code", false, "m16 18 6-6-6-6", "m8 6-6 6 6 6") }
    val Columns3: ImageVector by lazy { fluxIcon("columns-3", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M9 3v18", "M15 3v18") }
    val Cookie: ImageVector by lazy { fluxIcon("cookie", false, "M12 2a10 10 0 1 0 10 10 4 4 0 0 1-5-5 4 4 0 0 1-5-5", "M8.5 8.5v.01", "M16 15.5v.01", "M12 12v.01", "M11 17v.01", "M7 14v.01") }
    val Copy: ImageVector by lazy { fluxIcon("copy", false, "M10 8h10a2 2 0 0 1 2 2v10a2 2 0 0 1 -2 2h-10a2 2 0 0 1 -2 -2v-10a2 2 0 0 1 2 -2Z", "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2") }
    val CornerDownLeft: ImageVector by lazy { fluxIcon("corner-down-left", false, "M9 10L4 15L9 20", "M20 4v7a4 4 0 0 1-4 4H4") }
    val Cpu: ImageVector by lazy { fluxIcon("cpu", false, "M12 20v2", "M12 2v2", "M17 20v2", "M17 2v2", "M2 12h2", "M2 17h2", "M2 7h2", "M20 12h2", "M20 17h2", "M20 7h2", "M7 20v2", "M7 2v2", "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-12a2 2 0 0 1 2 -2Z", "M9 8h6a1 1 0 0 1 1 1v6a1 1 0 0 1 -1 1h-6a1 1 0 0 1 -1 -1v-6a1 1 0 0 1 1 -1Z") }
    val Crown: ImageVector by lazy { fluxIcon("crown", false, "M11.562 3.266a.5.5 0 0 1 .876 0L15.39 8.87a1 1 0 0 0 1.516.294L21.183 5.5a.5.5 0 0 1 .798.519l-2.834 10.246a1 1 0 0 1-.956.734H5.81a1 1 0 0 1-.957-.734L2.02 6.02a.5.5 0 0 1 .798-.519l4.276 3.664a1 1 0 0 0 1.516-.294z", "M5 21h14") }
    val Database: ImageVector by lazy { fluxIcon("database", false, "M3 5a9 3 0 1 0 18 0a9 3 0 1 0 -18 0Z", "M3 5V19A9 3 0 0 0 21 19V5", "M3 12A9 3 0 0 0 21 12") }
    val Disc: ImageVector by lazy { fluxIcon("disc", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M10 12a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z") }
    val Disc3: ImageVector by lazy { fluxIcon("disc-3", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M6 12c0-1.7.7-3.2 1.8-4.2", "M10 12a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z", "M18 12c0 1.7-.7 3.2-1.8 4.2") }
    val Download: ImageVector by lazy { fluxIcon("download", false, "M12 15V3", "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4", "m7 10 5 5 5-5") }
    val Ellipsis: ImageVector by lazy { fluxIcon("ellipsis", false, "M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M18 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M4 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z") }
    val EllipsisVertical: ImageVector by lazy { fluxIcon("ellipsis-vertical", false, "M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M11 5a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M11 19a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z") }
    val ExternalLink: ImageVector by lazy { fluxIcon("external-link", false, "M15 3h6v6", "M10 14 21 3", "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6") }
    val Eye: ImageVector by lazy { fluxIcon("eye", false, "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0", "M9 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z") }
    val EyeOff: ImageVector by lazy { fluxIcon("eye-off", false, "M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49", "M14.084 14.158a3 3 0 0 1-4.242-4.242", "M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143", "m2 2 20 20") }
    val File: ImageVector by lazy { fluxIcon("file", false, "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z", "M14 2v5a1 1 0 0 0 1 1h5") }
    val FileArchive: ImageVector by lazy { fluxIcon("file-archive", false, "M13.659 22H18a2 2 0 0 0 2-2V8a2.4 2.4 0 0 0-.706-1.706l-3.588-3.588A2.4 2.4 0 0 0 14 2H6a2 2 0 0 0-2 2v11.5", "M14 2v5a1 1 0 0 0 1 1h5", "M8 12v-1", "M8 18v-2", "M8 7V6", "M6 20a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z") }
    val FileDown: ImageVector by lazy { fluxIcon("file-down", false, "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z", "M14 2v4a2 2 0 0 0 2 2h4", "M12 18v-6", "m9 15 3 3 3-3") }
    val FileImage: ImageVector by lazy { fluxIcon("file-image", false, "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z", "M14 2v5a1 1 0 0 0 1 1h5", "M8 12a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z", "m20 17-1.296-1.296a2.41 2.41 0 0 0-3.408 0L9 22") }
    val FileMusic: ImageVector by lazy { fluxIcon("file-music", false, "M11.65 22H18a2 2 0 0 0 2-2V8a2.4 2.4 0 0 0-.706-1.706l-3.588-3.588A2.4 2.4 0 0 0 14 2H6a2 2 0 0 0-2 2v10.35", "M14 2v5a1 1 0 0 0 1 1h5", "M8 20v-7l3 1.474", "M4 20a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z") }
    val FilePlay: ImageVector by lazy { fluxIcon("file-play", false, "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z", "M14 2v5a1 1 0 0 0 1 1h5", "M15.033 13.44a.647.647 0 0 1 0 1.12l-4.065 2.352a.645.645 0 0 1-.968-.56v-4.704a.645.645 0 0 1 .967-.56z") }
    val FileText: ImageVector by lazy { fluxIcon("file-text", false, "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z", "M14 2v5a1 1 0 0 0 1 1h5", "M10 9H8", "M16 13H8", "M16 17H8") }
    val FileUp: ImageVector by lazy { fluxIcon("file-up", false, "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z", "M14 2v4a2 2 0 0 0 2 2h4", "M12 12v6", "m15 15-3-3-3 3") }
    val FileWarning: ImageVector by lazy { fluxIcon("file-warning", false, "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z", "M14 2v4a2 2 0 0 0 2 2h4", "M12 9v4", "M12 17h.01") }
    val Film: ImageVector by lazy { fluxIcon("film", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M7 3v18", "M3 7.5h4", "M3 12h18", "M3 16.5h4", "M17 3v18", "M17 7.5h4", "M17 16.5h4") }
    val Flashlight: ImageVector by lazy { fluxIcon("flashlight", false, "M18 6c0 2-2 2-2 4v10a2 2 0 0 1-2 2h-4a2 2 0 0 1-2-2V10c0-2-2-2-2-4V2h12z", "M6 6L18 6", "M12 12L12 12") }
    val Folder: ImageVector by lazy { fluxIcon("folder", false, "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z") }
    val FolderOpen: ImageVector by lazy { fluxIcon("folder-open", false, "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2") }
    val FolderPlus: ImageVector by lazy { fluxIcon("folder-plus", false, "M12 10v6", "M9 13h6", "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z") }
    val Folders: ImageVector by lazy { fluxIcon("folders", false, "M20 5a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h2.5a1.5 1.5 0 0 1 1.2.6l.6.8a1.5 1.5 0 0 0 1.2.6z", "M3 8.268a2 2 0 0 0-1 1.738V19a2 2 0 0 0 2 2h11a2 2 0 0 0 1.732-1") }
    val Font: ImageVector by lazy { fluxIcon("font", false, "m4 20 6-16 6 16", "M6.5 14h7", "M3 20h3", "M14 20h4", "M18 9h3") }
    val Gamepad: ImageVector by lazy { fluxIcon("gamepad", false, "M6 11L10 11", "M8 9L8 13", "M15 12L15.01 12", "M18 10L18.01 10", "M17.32 5H6.68a4 4 0 0 0-3.978 3.59C2.604 9.416 2 14.456 2 16a3 3 0 0 0 3 3c1 0 1.5-.5 2-1l1.414-1.414A2 2 0 0 1 9.828 16h4.344a2 2 0 0 1 1.414.586L17 18c.5.5 1 1 2 1a3 3 0 0 0 3-3c0-1.545-.604-6.584-.685-7.258A4 4 0 0 0 17.32 5z") }
    val Gamepad2: ImageVector by lazy { fluxIcon("gamepad-2", false, "M6 11L10 11", "M8 9L8 13", "M15 12L15.01 12", "M18 10L18.01 10", "M17.32 5H6.68a4 4 0 0 0-3.978 3.59c-.006.052-.01.101-.017.152C2.604 9.416 2 14.456 2 16a3 3 0 0 0 3 3c1 0 1.5-.5 2-1l1.414-1.414A2 2 0 0 1 9.828 16h4.344a2 2 0 0 1 1.414.586L17 18c.5.5 1 1 2 1a3 3 0 0 0 3-3c0-1.545-.604-6.584-.685-7.258-.007-.05-.011-.1-.017-.151A4 4 0 0 0 17.32 5z") }
    val Gauge: ImageVector by lazy { fluxIcon("gauge", false, "m12 14 4-4", "M3.34 19a10 10 0 1 1 17.32 0") }
    val Globe: ImageVector by lazy { fluxIcon("globe", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20", "M2 12h20") }
    val Grip: ImageVector by lazy { fluxIcon("grip", false, "M7.8 6a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0Z", "M13.8 6a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0Z", "M7.8 12a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0Z", "M13.8 12a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0Z", "M7.8 18a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0Z", "M13.8 18a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0Z") }
    val GripVertical: ImageVector by lazy { fluxIcon("grip-vertical", false, "M8 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M8 5a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M8 19a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M14 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M14 5a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z", "M14 19a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z") }
    val Group: ImageVector by lazy { fluxIcon("group", false, "M3 7V5c0-1.1.9-2 2-2h2", "M17 3h2c1.1 0 2 .9 2 2v2", "M21 17v2c0 1.1-.9 2-2 2h-2", "M7 21H5c-1.1 0-2-.9-2-2v-2", "M8 7h5a1 1 0 0 1 1 1v3a1 1 0 0 1 -1 1h-5a1 1 0 0 1 -1 -1v-3a1 1 0 0 1 1 -1Z", "M11 12h5a1 1 0 0 1 1 1v3a1 1 0 0 1 -1 1h-5a1 1 0 0 1 -1 -1v-3a1 1 0 0 1 1 -1Z") }
    val HardDrive: ImageVector by lazy { fluxIcon("hard-drive", false, "M10 16h.01", "M2.212 11.577a2 2 0 0 0-.212.896V18a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-5.527a2 2 0 0 0-.212-.896L18.55 5.11A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z", "M21.946 12.013H2.054", "M6 16h.01") }
    val Hash: ImageVector by lazy { fluxIcon("hash", false, "M4 9L20 9", "M4 15L20 15", "M10 3L8 21", "M16 3L14 21") }
    val Heart: ImageVector by lazy { fluxIcon("heart", false, "M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z") }
    val History: ImageVector by lazy { fluxIcon("history", false, "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8", "M3 3v5h5", "M12 7v5l4 2") }
    val Hourglass: ImageVector by lazy { fluxIcon("hourglass", false, "M5 22h14", "M5 2h14", "M17 22v-4.172a2 2 0 0 0-.586-1.414L12 12l-4.414 4.414A2 2 0 0 0 7 17.828V22", "M7 2v4.172a2 2 0 0 0 .586 1.414L12 12l4.414-4.414A2 2 0 0 0 17 6.172V2") }
    val House: ImageVector by lazy { fluxIcon("house", false, "M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8", "M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z") }
    val Image: ImageVector by lazy { fluxIcon("image", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M7 9a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z", "m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21") }
    val Inbox: ImageVector by lazy { fluxIcon("inbox", false, "M22 12L16 12L14 15L10 15L8 12L2 12", "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z") }
    val Info: ImageVector by lazy { fluxIcon("info", false, "M2 12a10 10 0 1 0 20 0a10 10 0 1 0 -20 0Z", "M12 16v-4", "M12 8h.01") }
    val Key: ImageVector by lazy { fluxIcon("key", false, "m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4", "m21 2-9.6 9.6", "M2 15.5a5.5 5.5 0 1 0 11 0a5.5 5.5 0 1 0 -11 0Z") }
    val KeyRound: ImageVector by lazy { fluxIcon("key-round", true, "M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z", "M16 7.5a0.5 0.5 0 1 0 1 0a0.5 0.5 0 1 0 -1 0Z") }
    val Laptop: ImageVector by lazy { fluxIcon("laptop", false, "M20 16V7a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9m16 0H4m16 0 1.28 2.55a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45L4 16") }
    val Layers: ImageVector by lazy { fluxIcon("layers", false, "M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z", "M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12", "M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17") }
    val Library: ImageVector by lazy { fluxIcon("library", false, "m16 6 4 14", "M12 6v14", "M8 8v12", "M4 4v16") }
    val Link: ImageVector by lazy { fluxIcon("link", false, "M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71", "M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71") }
    val List: ImageVector by lazy { fluxIcon("list", false, "M3 12h.01", "M3 18h.01", "M3 6h.01", "M8 12h13", "M8 18h13", "M8 6h13") }
    val ListChecks: ImageVector by lazy { fluxIcon("list-checks", false, "m3 17 2 2 4-4", "m3 7 2 2 4-4", "M13 6h8", "M13 12h8", "M13 18h8") }
    val ListFilter: ImageVector by lazy { fluxIcon("list-filter", false, "M2 5h20", "M6 12h12", "M9 19h6") }
    val Lock: ImageVector by lazy { fluxIcon("lock", false, "M5 11h14a2 2 0 0 1 2 2v7a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-7a2 2 0 0 1 2 -2Z", "M7 11V7a5 5 0 0 1 10 0v4") }
    val LogOut: ImageVector by lazy { fluxIcon("log-out", false, "m16 17 5-5-5-5", "M21 12H9", "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4") }
    val Magnet: ImageVector by lazy { fluxIcon("magnet", false, "m12 15 4 4", "M2.352 10.648a1.205 1.205 0 0 0 0 1.704l2.296 2.296a1.205 1.205 0 0 0 1.704 0l6.029-6.029a1 1 0 1 1 3 3l-6.029 6.029a1.205 1.205 0 0 0 0 1.704l2.296 2.296a1.205 1.205 0 0 0 1.704 0l6.365-6.367A1 1 0 0 0 8.716 4.282z", "m5 8 4 4") }
    val Mail: ImageVector by lazy { fluxIcon("mail", false, "m22 7-8.991 5.727a2 2 0 0 1-2.009 0L2 7", "M4 4h16a2 2 0 0 1 2 2v12a2 2 0 0 1 -2 2h-16a2 2 0 0 1 -2 -2v-12a2 2 0 0 1 2 -2Z") }
    val MapPin: ImageVector by lazy { fluxIcon("map-pin", false, "M20 10c0 4.993-5.539 10.193-7.399 11.799a1 1 0 0 1-1.202 0C9.539 20.193 4 14.993 4 10a8 8 0 0 1 16 0", "M9 10a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z") }
    val Menu: ImageVector by lazy { fluxIcon("menu", false, "M4 12L20 12", "M4 6L20 6", "M4 18L20 18") }
    val MessageCircle: ImageVector by lazy { fluxIcon("message-circle", false, "M7.9 20A9 9 0 1 0 4 16.1L2 22Z") }
    val Minus: ImageVector by lazy { fluxIcon("minus", false, "M5 12h14") }
    val Monitor: ImageVector by lazy { fluxIcon("monitor", false, "M4 3h16a2 2 0 0 1 2 2v10a2 2 0 0 1 -2 2h-16a2 2 0 0 1 -2 -2v-10a2 2 0 0 1 2 -2Z", "M8 21L16 21", "M12 17L12 21") }
    val Moon: ImageVector by lazy { fluxIcon("moon", false, "M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401") }
    val MousePointerClick: ImageVector by lazy { fluxIcon("mouse-pointer-click", false, "M14 4.1 12 6", "m5.1 8-2.9-.8", "m6 12-1.9 2", "M7.2 2.2 8 5.1", "M9.037 9.69a.498.498 0 0 1 .653-.653l11 4.5a.5.5 0 0 1-.074.949l-4.349 1.041a1 1 0 0 0-.74.739l-1.04 4.35a.5.5 0 0 1-.95.074z") }
    val Music: ImageVector by lazy { fluxIcon("music", false, "M9 18V5l12-2v13", "M3 18a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z", "M15 16a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z") }
    val Network: ImageVector by lazy { fluxIcon("network", false, "M17 16h4a1 1 0 0 1 1 1v4a1 1 0 0 1 -1 1h-4a1 1 0 0 1 -1 -1v-4a1 1 0 0 1 1 -1Z", "M3 16h4a1 1 0 0 1 1 1v4a1 1 0 0 1 -1 1h-4a1 1 0 0 1 -1 -1v-4a1 1 0 0 1 1 -1Z", "M10 2h4a1 1 0 0 1 1 1v4a1 1 0 0 1 -1 1h-4a1 1 0 0 1 -1 -1v-4a1 1 0 0 1 1 -1Z", "M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3", "M12 12V8") }
    val Package: ImageVector by lazy { fluxIcon("package", false, "M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z", "M12 22V12", "M3.29 7L12 12L20.71 7", "m7.5 4.27 9 5.15") }
    val Palette: ImageVector by lazy { fluxIcon("palette", true, "M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z", "M13 6.5a0.5 0.5 0 1 0 1 0a0.5 0.5 0 1 0 -1 0Z", "M17 10.5a0.5 0.5 0 1 0 1 0a0.5 0.5 0 1 0 -1 0Z", "M6 12.5a0.5 0.5 0 1 0 1 0a0.5 0.5 0 1 0 -1 0Z", "M8 7.5a0.5 0.5 0 1 0 1 0a0.5 0.5 0 1 0 -1 0Z") }
    val PanelBottom: ImageVector by lazy { fluxIcon("panel-bottom", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M3 15h18") }
    val PanelRight: ImageVector by lazy { fluxIcon("panel-right", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M15 3v18") }
    val Pause: ImageVector by lazy { fluxIcon("pause", false, "M15 3h3a1 1 0 0 1 1 1v16a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-16a1 1 0 0 1 1 -1Z", "M6 3h3a1 1 0 0 1 1 1v16a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-16a1 1 0 0 1 1 -1Z") }
    val Pen: ImageVector by lazy { fluxIcon("pen", false, "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z") }
    val Play: ImageVector by lazy { fluxIcon("play", false, "M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z") }
    val Plug: ImageVector by lazy { fluxIcon("plug", false, "M12 22v-5", "M9 8V2", "M15 8V2", "M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z") }
    val Plus: ImageVector by lazy { fluxIcon("plus", false, "M5 12h14", "M12 5v14") }
    val Power: ImageVector by lazy { fluxIcon("power", false, "M12 2v10", "M18.4 6.6a9 9 0 1 1-12.77.04") }
    val PowerOff: ImageVector by lazy { fluxIcon("power-off", false, "M18.36 6.64A9 9 0 0 1 20.77 15", "M6.16 6.16a9 9 0 1 0 12.68 12.68", "M12 2v4", "m2 2 20 20") }
    val Printer: ImageVector by lazy { fluxIcon("printer", false, "M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2", "M6 9V3a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v6", "M7 14h10a1 1 0 0 1 1 1v6a1 1 0 0 1 -1 1h-10a1 1 0 0 1 -1 -1v-6a1 1 0 0 1 1 -1Z") }
    val Puzzle: ImageVector by lazy { fluxIcon("puzzle", false, "M15.39 4.39a1 1 0 0 0 1.68-.474 2.5 2.5 0 1 1 3.014 3.015 1 1 0 0 0-.474 1.68l1.683 1.682a2.414 2.414 0 0 1 0 3.414L19.61 15.39a1 1 0 0 1-1.68-.474 2.5 2.5 0 1 0-3.014 3.015 1 1 0 0 1 .474 1.68l-1.683 1.682a2.414 2.414 0 0 1-3.414 0L8.61 19.61a1 1 0 0 0-1.68.474 2.5 2.5 0 1 1-3.014-3.015 1 1 0 0 0 .474-1.68l-1.683-1.682a2.414 2.414 0 0 1 0-3.414L4.39 8.61a1 1 0 0 1 1.68.474 2.5 2.5 0 1 0 3.014-3.015 1 1 0 0 1-.474-1.68l1.683-1.682a2.414 2.414 0 0 1 3.414 0z") }
    val QrCode: ImageVector by lazy { fluxIcon("qr-code", false, "M4 3h3a1 1 0 0 1 1 1v3a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-3a1 1 0 0 1 1 -1Z", "M17 3h3a1 1 0 0 1 1 1v3a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-3a1 1 0 0 1 1 -1Z", "M4 16h3a1 1 0 0 1 1 1v3a1 1 0 0 1 -1 1h-3a1 1 0 0 1 -1 -1v-3a1 1 0 0 1 1 -1Z", "M21 16h-3a2 2 0 0 0-2 2v3", "M21 21v.01", "M12 7v3a2 2 0 0 1-2 2H7", "M3 12h.01", "M12 3h.01", "M12 16v.01", "M16 12h1", "M21 12v.01", "M12 21v-1") }
    val RefreshCw: ImageVector by lazy { fluxIcon("refresh-cw", false, "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8", "M21 3v5h-5", "M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16", "M8 16H3v5") }
    val RotateCcw: ImageVector by lazy { fluxIcon("rotate-ccw", false, "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8", "M3 3v5h5") }
    val RotateCw: ImageVector by lazy { fluxIcon("rotate-cw", false, "M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8", "M21 3v5h-5") }
    val Rows3: ImageVector by lazy { fluxIcon("rows-3", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "M21 9H3", "M21 15H3") }
    val Rss: ImageVector by lazy { fluxIcon("rss", false, "M4 11a9 9 0 0 1 9 9", "M4 4a16 16 0 0 1 16 16", "M4 19a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z") }
    val Save: ImageVector by lazy { fluxIcon("save", false, "M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z", "M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7", "M7 3v4a1 1 0 0 0 1 1h7") }
    val Scale: ImageVector by lazy { fluxIcon("scale", false, "m16 16 3-8 3 8c-.87.65-1.92 1-3 1s-2.13-.35-3-1Z", "m2 16 3-8 3 8c-.87.65-1.92 1-3 1s-2.13-.35-3-1Z", "M7 21h10", "M12 3v18", "M3 7h2c2 0 5-1 7-2 2 1 5 2 7 2h2") }
    val Scan: ImageVector by lazy { fluxIcon("scan", false, "M3 7V5a2 2 0 0 1 2-2h2", "M17 3h2a2 2 0 0 1 2 2v2", "M21 17v2a2 2 0 0 1-2 2h-2", "M7 21H5a2 2 0 0 1-2-2v-2") }
    val ScrollText: ImageVector by lazy { fluxIcon("scroll-text", false, "M15 12h-5", "M15 8h-5", "M19 17V5a2 2 0 0 0-2-2H4", "M8 21h12a2 2 0 0 0 2-2v-1a1 1 0 0 0-1-1H11a1 1 0 0 0-1 1v1a2 2 0 1 1-4 0V5a2 2 0 1 0-4 0v2a1 1 0 0 0 1 1h3") }
    val Search: ImageVector by lazy { fluxIcon("search", false, "m21 21-4.34-4.34", "M3 11a8 8 0 1 0 16 0a8 8 0 1 0 -16 0Z") }
    val Send: ImageVector by lazy { fluxIcon("send", false, "M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z", "m21.854 2.147-10.94 10.939") }
    val Server: ImageVector by lazy { fluxIcon("server", false, "M4 2h16a2 2 0 0 1 2 2v4a2 2 0 0 1 -2 2h-16a2 2 0 0 1 -2 -2v-4a2 2 0 0 1 2 -2Z", "M4 14h16a2 2 0 0 1 2 2v4a2 2 0 0 1 -2 2h-16a2 2 0 0 1 -2 -2v-4a2 2 0 0 1 2 -2Z", "M6 6L6.01 6", "M6 18L6.01 18") }
    val Settings: ImageVector by lazy { fluxIcon("settings", false, "M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915", "M9 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z") }
    val Share2: ImageVector by lazy { fluxIcon("share-2", false, "M15 5a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z", "M3 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z", "M15 19a3 3 0 1 0 6 0a3 3 0 1 0 -6 0Z", "M8.59 13.51L15.42 17.49", "M15.41 6.51L8.59 10.49") }
    val Shield: ImageVector by lazy { fluxIcon("shield", false, "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z") }
    val ShieldAlert: ImageVector by lazy { fluxIcon("shield-alert", false, "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z", "M12 8v4", "M12 16h.01") }
    val ShieldCheck: ImageVector by lazy { fluxIcon("shield-check", false, "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z", "m9 12 2 2 4-4") }
    val Signal: ImageVector by lazy { fluxIcon("signal", false, "M2 20h.01", "M7 20v-4", "M12 20v-8", "M17 20V8", "M22 4v16") }
    val SlidersHorizontal: ImageVector by lazy { fluxIcon("sliders-horizontal", false, "M10 5H3", "M12 19H3", "M14 3v4", "M16 17v4", "M21 12h-9", "M21 19h-5", "M21 5h-7", "M8 10v4", "M8 12H3") }
    val SlidersVertical: ImageVector by lazy { fluxIcon("sliders-vertical", false, "M4 21L4 14", "M4 10L4 3", "M12 21L12 12", "M12 8L12 3", "M20 21L20 16", "M20 12L20 3", "M2 14L6 14", "M10 8L14 8", "M18 16L22 16") }
    val Smartphone: ImageVector by lazy { fluxIcon("smartphone", false, "M7 2h10a2 2 0 0 1 2 2v16a2 2 0 0 1 -2 2h-10a2 2 0 0 1 -2 -2v-16a2 2 0 0 1 2 -2Z", "M12 18h.01") }
    val SortAsc: ImageVector by lazy { fluxIcon("sort-asc", false, "m3 8 4-4 4 4", "M7 4v16", "M11 12h4", "M11 16h7", "M11 20h10") }
    val SortDesc: ImageVector by lazy { fluxIcon("sort-desc", false, "m3 16 4 4 4-4", "M7 20V4", "M11 4h10", "M11 8h7", "M11 12h4") }
    val Sparkles: ImageVector by lazy { fluxIcon("sparkles", false, "M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z") }
    val Square: ImageVector by lazy { fluxIcon("square", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z") }
    val SquareCheck: ImageVector by lazy { fluxIcon("square-check", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "m9 12 2 2 4-4") }
    val SquarePen: ImageVector by lazy { fluxIcon("square-pen", false, "M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7", "M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z") }
    val SquareTerminal: ImageVector by lazy { fluxIcon("square-terminal", false, "m7 11 2-2-2-2", "M11 13h4", "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z") }
    val Stethoscope: ImageVector by lazy { fluxIcon("stethoscope", false, "M11 2v2", "M5 2v2", "M5 3H4a2 2 0 0 0-2 2v4a6 6 0 0 0 12 0V5a2 2 0 0 0-2-2h-1", "M8 15a6 6 0 0 0 12 0v-3", "M18 10a2 2 0 1 0 4 0a2 2 0 1 0 -4 0Z") }
    val Store: ImageVector by lazy { fluxIcon("store", false, "m2 7 4.41-4.41A2 2 0 0 1 7.83 2h8.34a2 2 0 0 1 1.42.59L22 7", "M4 12v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8", "M15 22v-4a2 2 0 0 0-2-2h-2a2 2 0 0 0-2 2v4", "M2 7h20") }
    val Subtitles: ImageVector by lazy { fluxIcon("subtitles", false, "M5 5h14a2 2 0 0 1 2 2v10a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-10a2 2 0 0 1 2 -2Z", "M7 15h4M15 15h2M7 11h2M13 11h4") }
    val Sun: ImageVector by lazy { fluxIcon("sun", false, "M8 12a4 4 0 1 0 8 0a4 4 0 1 0 -8 0Z", "M12 2v2", "M12 20v2", "m4.93 4.93 1.41 1.41", "m17.66 17.66 1.41 1.41", "M2 12h2", "M20 12h2", "m6.34 17.66-1.41 1.41", "m19.07 4.93-1.41 1.41") }
    val Tablet: ImageVector by lazy { fluxIcon("tablet", false, "M6 2h12a2 2 0 0 1 2 2v16a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-16a2 2 0 0 1 2 -2Z", "M12 18L12.01 18") }
    val Tag: ImageVector by lazy { fluxIcon("tag", true, "M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z", "M7 7.5a0.5 0.5 0 1 0 1 0a0.5 0.5 0 1 0 -1 0Z") }
    val Terminal: ImageVector by lazy { fluxIcon("terminal", false, "M4 17L10 11L4 5", "M12 19L20 19") }
    val Timer: ImageVector by lazy { fluxIcon("timer", false, "M10 2L14 2", "M12 14L15 11", "M4 14a8 8 0 1 0 16 0a8 8 0 1 0 -16 0Z") }
    val Trash: ImageVector by lazy { fluxIcon("trash", false, "M3 6h18", "M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6", "M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2") }
    val Trash2: ImageVector by lazy { fluxIcon("trash-2", false, "M10 11v6", "M14 11v6", "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6", "M3 6h18", "M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2") }
    val TriangleAlert: ImageVector by lazy { fluxIcon("triangle-alert", false, "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3", "M12 9v4", "M12 17h.01") }
    val Type: ImageVector by lazy { fluxIcon("type", false, "M12 4v16", "M4 7V5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2", "M9 20h6") }
    val Undo2: ImageVector by lazy { fluxIcon("undo-2", false, "M9 14 4 9l5-5", "M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11") }
    val Upload: ImageVector by lazy { fluxIcon("upload", false, "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4", "M17 8L12 3L7 8", "M12 3L12 15") }
    val User: ImageVector by lazy { fluxIcon("user", false, "M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2", "M8 7a4 4 0 1 0 8 0a4 4 0 1 0 -8 0Z") }
    val UserPlus: ImageVector by lazy { fluxIcon("user-plus", false, "M2 21a8 8 0 0 1 13.292-6", "M5 8a5 5 0 1 0 10 0a5 5 0 1 0 -10 0Z", "M19 16v6", "M22 19h-6") }
    val Users: ImageVector by lazy { fluxIcon("users", false, "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2", "M5 7a4 4 0 1 0 8 0a4 4 0 1 0 -8 0Z", "M22 21v-2a4 4 0 0 0-3-3.87", "M16 3.13a4 4 0 0 1 0 7.75") }
    val Wallpaper: ImageVector by lazy { fluxIcon("wallpaper", false, "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2Z", "m3 15 4-4 4 4 3-3 7 7", "M14.5 8a1.5 1.5 0 1 0 3 0a1.5 1.5 0 1 0 -3 0Z") }
    val Webhook: ImageVector by lazy { fluxIcon("webhook", false, "M18 16.98h-5.99c-1.1 0-1.95.94-2.48 1.9A4 4 0 0 1 2 17c.01-.7.2-1.4.57-2", "m6 17 3.13-5.78c.53-.97.1-2.18-.5-3.1a4 4 0 1 1 6.89-4.06", "m12 6 3.13 5.73C15.66 12.7 16.9 13 18 13a4 4 0 0 1 0 8") }
    val Wifi: ImageVector by lazy { fluxIcon("wifi", false, "M12 20h.01", "M2 8.82a15 15 0 0 1 20 0", "M5 12.859a10 10 0 0 1 14 0", "M8.5 16.429a5 5 0 0 1 7 0") }
    val WifiOff: ImageVector by lazy { fluxIcon("wifi-off", false, "M12 20h.01", "M8.5 16.429a5 5 0 0 1 7 0", "M5 12.859a10 10 0 0 1 5.17-2.69", "M19 12.859a10 10 0 0 0-2.007-1.523", "M2 8.82a15 15 0 0 1 4.177-2.643", "M22 8.82a15 15 0 0 0-11.288-3.764", "m2 2 20 20") }
    val X: ImageVector by lazy { fluxIcon("x", false, "M18 6 6 18", "m6 6 12 12") }
    val Zap: ImageVector by lazy { fluxIcon("zap", false, "M15.914 4a1.5 1.5 0 00-2.474-1.561l-9 9A1.5 1.5 0 005.5 14h4.002a.5.5 0 01.471.666L8.086 20a1.5 1.5 0 002.475 1.56l9-9A1.5 1.5 0 0018.5 10h-3.997a.5.5 0 01-.472-.667z") }
    val ZapOff: ImageVector by lazy { fluxIcon("zap-off", false, "M10.513 4.856 13.12 2.17a.5.5 0 0 1 .86.46l-1.377 4.317", "M15.656 10H20a1 1 0 0 1 .78 1.63l-1.72 1.773", "M16.273 16.273 10.88 21.83a.5.5 0 0 1-.86-.46L10 14H4a1 1 0 0 1-.78-1.63l4.507-4.643", "m2 2 20 20") }
}
