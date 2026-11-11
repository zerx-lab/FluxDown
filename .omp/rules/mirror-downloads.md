---
description: GPUI 下载页纯模型（新建/菜单/排序/任务组/来源构成/设备/做种等）改动须同步 Web 与移动端镜像
condition: '[\s\S]'
scope: "tool:edit, tool:write"
globs:
  - '**/crates/downloads/src/model/{new_download,aria2c,save_dir_history,speed_limit_recent,dispatch,mod,view_prefs,row_order,group_tree,directory_tree,source_composition,devices,file_rescan,bt_trackers,file_hash}.rs'
  - '**/crates/downloads/src/components/{task_table,task_table_groups,task_table_directories}.rs'
  - '**/crates/downloads/src/pages/{task_edit,task_verify,task_detail,task_detail_ed2k}.rs'
  - '**/crates/downloads/tests/fixtures/**'
  - '**/web/src/pages/downloads/**'
  - '**/web/src/lib/{saveDirHistory,speedLimitRecent,rescanThrottle,fileHash}.ts'
  - '**/mobile/Android/core/src/main/java/com/fluxdown/core/model/Models.kt'
  - '**/mobile/Android/core/src/main/java/com/fluxdown/core/protocol/Devices.kt'
  - '**/mobile/Android/app/src/main/java/com/fluxdown/app/feature/downloads/DownloadsModel.kt'
  - '**/mobile/Android/app/src/main/java/com/fluxdown/app/actions/TaskActions.kt'
  - '**/mobile/FluxDown/FluxKit/Sources/FluxDomain/Model/Models.swift'
  - '**/mobile/FluxDown/FluxKit/Sources/FluxDomain/Protocol/Devices.swift'
  - '**/mobile/FluxDown/FluxDown/Features/Downloads/DownloadsModel.swift'
  - '**/mobile/FluxDown/FluxDown/Actions/TaskMenu.swift'
  - '**/fluxDown/utils/android-intent{,.test}.ts'
  - '**/mobile/Android/app/src/main/java/com/fluxdown/app/ExternalDownloadActivity.kt'
  - '**/mobile/Android/core/src/main/java/com/fluxdown/core/capture/ExternalDownload.kt'
interruptMode: never
---

你正在改动下载页纯模型（`crates/downloads` ↔ Web / 移动端）的一侧。下列条目里，标题是事实源一侧、「→」后是必须同回合同步的镜像端：改任一侧都要把另一侧过一遍；有共享夹具的先改夹具用例，再改两侧实现，两侧测试都要跑（`/verify` 第 3 步）。只改了一侧就交付 = 不完整。

## `crates/downloads/src/model/new_download.rs::capture_entry`（捕获名与 URL 末段百分号解码后相同则省略 `out=`）

→ `web/src/pages/downloads/dialogs/model.ts::captureEntry`：同一判定逐条对齐（`a%20b.zip` 与 `a b.zip` 视为相同），两端新建框显示一致

## `crates/downloads/src/model/new_download.rs::captured_referrer`（新建框「来源页」框自动带入：仅单条且带 http(s) 来源页的外部捕获才带入，批量留空由 agent 让各捕获沿用自己的来源页；框只有一份回填记录（`NewDownloadView::sync_referrer`）：aria2c 命令的 `--referer` / `Referer` 头（`RequestHints.referrer`，不再进自定义请求头）非空就用它，否则用捕获来源页；回填走 `aria2c::plan_field_fill`，用户填过的不覆盖；`DraftOptions.referrer` 填了整批覆盖，捕获确认时 agent `merge_confirmed_request` / `merge_group_request`：表单空取捕获值，表单非空且与捕获不同时同时去掉捕获的 `Referer` 请求头（引擎优先取请求头里的 Referer），未改 / 留空保留捕获头）

→ `web/src/pages/downloads/dialogs/model.ts::capturedReferrer` + `NewDownloadDialog.tsx`（`planFieldFill`，`parsed.hints.referrer || capturedReferrer`）+ `aria2c.ts`（`RequestHints.referrer`）；清单预解析 / 建组（`ManifestBaseOptions.referrer`）同带；移动端（`mobileReferrer`）本来就有来源页字段

## `crates/downloads/src/model/save_dir_history.rs`（新建下载保存目录历史：设备本地偏好 `download.save_dir_history` 字符串数组，最新在前 / 去重 / 封顶 10；分类目录记忆 `download.category_save_dir_history` = `{分类 id: 同规则数组}`，只在表单对应分类（指定分类 / 自动命中）且目录被手改时记，下拉先列该分类记忆再列全局历史；`download.save_dir_history_enabled` 缺省开，关闭即两者都不列出不记录；只记本机提交）

→ `web/src/lib/saveDirHistory.ts` 逐条对齐，Rust 与 `saveDirHistory.test.ts` 共用 `crates/downloads/tests/fixtures/save_dir_history_cases.json`；两端设置「保存位置」分组同位（开关 + 清空 = 两个键都写 null）；移动端无历史下拉，不镜像

## `crates/downloads/src/model/speed_limit_recent.rs`（状态栏限速最近自定义值：设备本地偏好 `download.speed_limit_recent_kbps` / `download.upload_limit_recent_kbps` 整数数组 KB/s，最新在前 / 去重 / 封顶 3，丢弃 ≤0 与内置预设 1/5/10/50 MB/s，非法项丢弃）

→ `web/src/lib/speedLimitRecent.ts` 逐条对齐，Rust 与 `speedLimitRecent.test.ts` 共用 `crates/downloads/tests/fixtures/speed_limit_recent_cases.json`

## `crates/downloads/src/model/new_download.rs::parse_entries` / `parse_input` + `model/aria2c.rs`（aria2c 命令切词 / 续行合并 / 镜像 URI 只取首个、`-Z` 拆分 / 带值选项表 / 头部回填 `plan_field_fill`）

→ `web/src/pages/downloads/dialogs/model.ts::parseEntries` + `aria2c.ts`（`parseCommand` / `joinContinuations` / `planFieldFill`）逐条对齐，`aria2c.test.ts` 覆盖同组行为；TXT 导入两端都用「原文保留、按 URL 去重追加」（`append_entries` ↔ `appendEntries`）

## `crates/downloads/src/components/task_table.rs::TaskMenuFacts::{can_rename, can_change_url, can_change_segments, can_verify_integrity}`（`task_menu_facts_for` 为右键 / 应用菜单与详情常规页按钮共用事实源；应用菜单项不置灰，`on_rename_selected` 等入口按同一事实源再判）+ `pages/task_edit.rs`（`open_change_url_dialog` / `open_change_segments_dialog`，下载页与 `task_detail.rs` 常规页共用；失败任务换链 / 改线程数成功后串行 Resume）+ `pages/task_verify.rs`（BT 完整性校验结果通知：损坏文件列前 3 个、其余折叠计数，`conflict` = 已在校验）

→ `web/src/pages/downloads/model/taskEdit.ts::{canRename, canChangeUrl, canChangeSegments, canVerifyIntegrity, parseTaskSegments}` + `table/menus.ts` 与 `detail/GeneralTab.tsx` 两处入口 + `dialogs/TaskEditDialogs.tsx`（`resumeIfFailed`）+ `model/actions.ts::verifyTaskIntegrity`；换链源仅 http(s)/ftp（对齐引擎 `change_task_url`）；重命名 = 本地非 BT 且非下载 / 准备中（对齐引擎 `rename_task`），Android `actions/TaskActions.kt` 与 iOS `Actions/TaskMenu.swift` 同条件（`!status.isActive && protocol != Bt`）

## `crates/downloads/src/components/task_table.rs` 的 BT 选片菜单纯模型（`piece_picking_*`）（「选片方式」`btPiecePicking` 分组，用户 2026-11-06 决定：下载顺序二选一 = 单选「最稀有优先」`btRarestFirst`（`btSequential=false`，默认）/「按顺序下载」，全部所选一致才点亮对应项、混选两项都不点亮，点选逐任务只对状态不同的任务发 `sequential`；「先下载首尾文件块」是可叠加的独立复选框，全部已开才打勾，点击设为 `!全部已开` 只发 `firstLastPiecesFirst`。所选全部为本机未完成 BT 任务才显示；位于单任务编辑项之后、「重新下载」之前；能嵌套的端用子菜单，GPUI 表格菜单 / Android `FluxMenu` 用不可点击标题 + 子行；单选画空圆 / 圆点、复选画空框 / 勾选框，两态都画）

→ Web `web/src/pages/downloads/model/taskEdit.ts::{canSetPiecePicking, pieceOrder, pieceOrderTargets, pieceSequentialParams, pieceOptionAllOn, pieceEndsParams}` + `table/menus.ts` + `model/actions.ts::{setPieceOrder, setFirstLastPieces}`；Android `actions/TaskActions.kt::piecePickingMenu`（单任务菜单 + 多选长按菜单）；iOS `FluxDomain/Protocol/Tasks.swift::PiecePickingMenu` + `Actions/TaskMenu.swift` / `Features/Downloads/DownloadsListScreen.swift`（只在 `FULL_FLAVOR` 编入）。GPUI `is_local` 排除的是云端远程任务行；移动端没有该行类型，**不用** `isLocalHost`（那只表示文件在这台手机上，会错藏正在控制的远端 PC）。移动端按 URL 小写判断 `magnet:` / `torrent-file://`，未完成才显示；批量切换逐任务捕获错误、全部尝试后再 toast 一次。各端测试覆盖同一组显示 / 顺序 / 勾选用例

## `crates/downloads/src/model/dispatch.rs::remote_action_applies`（远程命令适用状态）

→ `web/src/pages/downloads/model/batchPlan.ts::remoteCan`；未知状态一律不可控制

## `crates/downloads/src/model/mod.rs::DownloadTaskView::{is_seeding, can_resume_seeding, is_resume_all_seed}` + `DownloadStatusFilter::Seeding`（做种交互对齐 qBittorrent：做种中 = 完成且 `seeding_status` ∈ {1, 8}，侧栏「做种中」紧随「已完成」且同时计入后者；可恢复做种 = 完成且 2..=7 且文件在；仅本机任务；做种行可暂停、可恢复行可继续，其余完成态继续仍禁止 #935；引擎侧见 `.omp/knowledge/engine.md`「做种交互」）

→ Web `web/src/pages/downloads/model/task.ts::{isSeeding, canResumeSeeding}` + `filters.ts`、Android `core/.../model/Models.kt`（`Task.isSeeding` / `canResumeSeeding` / `allTasksPaused`）+ `feature/downloads/DownloadsModel.kt::StatusFolder.Seeding`、iOS `FluxDomain/Model/Models.swift`（`isSeeding` / `canResumeSeeding` / `isAllPaused`）+ `Features/Downloads/DownloadsModel.swift::StatusFolder.seeding`；各端测试覆盖同一组成员 / 资格用例

## `crates/downloads/src/model/view_prefs.rs::compare`（智能排序档位 / 平局 / 自然序 / 表头三档）+ `model/row_order.rs`（行顺序保持期）

→ `web/src/pages/downloads/model/viewPrefs.ts::compareViews` + `rowOrder.ts`（保持期判定）+ `rowEngine.ts`（增量行引擎，与 GPUI `task_table.rs` 的变更日志重放同构；逐条对齐，`viewPrefs.test.ts` / `rowOrder.test.ts` / `rowEngine.test.ts` 覆盖同一组行为）

## `crates/downloads/src/model/group_tree.rs`（任务组文件夹行纯模型：成员计数 / 聚合状态、`member_dir`、`flatten` 目录树、`group_sort_view` 排序代理）+ `components/task_table_groups.rs`（归组并入顶层排序、展开、整组选择）

→ `web/src/pages/downloads/model/groupTree.ts` + `groupRows.ts`：逐条对齐，纯模型两端测试共用 `crates/downloads/tests/fixtures/group_tree_cases.json`（改规则先改用例）；聚合只覆盖当前筛选 + 搜索范围内的成员，组命令与停靠面板组概览按全组

## `crates/downloads/src/model/directory_tree.rs`（相对默认下载根的嵌套目录分组、单分支压缩、后代计数与稳定折叠键）

→ Web `web/src/pages/downloads/model/directoryTree.ts`，共用 `crates/downloads/tests/fixtures/directory_tree_cases.json`。先筛选再建树，非根目录无直属任务且只有一个子目录才压缩，已折叠节点不跨过；任务组按组根整体纳入，云端按设备隔离，根外 / 未知目录分别展示。GPUI `task_table_directories.rs` / Web `groupRows.ts` 组合目录与任务组缩进；保存目录字段变化须使 `model/store.rs::same_view_fields` / Web `rowEngine.ts` 的视图失效，默认目录配置与 runtime fallback 变化同样重算。移动端尚无该分组入口，不镜像。

## `crates/downloads/src/components/task_table.rs` 宽松密度列表视图（`relaxed_meta` 副标题；宽松不合并列、状态列同舒适，行高 = max(64, 舒适)）

→ `web/src/pages/downloads/table/text.ts::relaxedMeta` + `table/cells.tsx` / `rowMetrics.ts`：副标题字段与顺序逐条对齐（`text.test.ts`）

## `crates/downloads/src/model/source_composition.rs`（详情常规页「来源构成」区块：P2P 判定 / 超额按比例缩放 / 行纳入规则 / 百分比格式；常规页信息列 300 + 来源区块 360 按可用宽度折行）

→ `web/src/pages/downloads/model/sourceComposition.ts` + `detail/GeneralTab.tsx` / `SourcesSection.tsx`（逐条对齐，`sourceComposition.test.ts` 复用同一组用例）；数据源 `tasks.src_{cdn,proxy,nic}_bytes` 只记加速路径，源站 = 已下载 − 三者之和，进度复位时同步清零

## `crates/downloads/src/model/devices.rs::other_devices`（云设备 + 直连设备合并：`cloud_device_id` 命中云名册即挂接为一条、在线取任一、直连在线优先走直连下发、revoked 排除、合并后同名消歧；侧栏云条目可选中筛选，独立直连条目排在其后、点击打开新建下载并预选该设备、右键下发 / 重命名 / 解除配对；设备区默认可见性按其他设备总数含独立直连）

→ Web `web/src/pages/downloads/model/devices.ts` + `dialogs/target.ts` + `sidebar/Sidebar.tsx`、iOS `FluxKit/Sources/FluxDomain/Protocol/Devices.swift`、Android `mobile/Android/core/.../protocol/Devices.kt` 逐条对齐，各端测试覆盖同一组行为；直连设备不注册 FluxCloud、不计入云设备数 / 在线名额

## 文件跟踪重扫节流（`crates/downloads/src/model/file_rescan.rs::RescanThrottle`）

→ `web/src/lib/rescanThrottle.ts`（`RescanThrottle`）：逐条对齐 10s 冷却 / 尾沿排队 / 合并 / 尾沿后重计冷却；测试复用同组用例（`rescanThrottle.test.ts`）

## `crates/downloads/src/model/file_hash.rs`（详情「文件哈希」行：算法展示名 `hash_algorithm_label`、与用户校验和比对 `file_hash_matches_checksum`）

→ `web/src/lib/fileHash.ts::{hashAlgorithmLabel, fileHashMatchesChecksum}` 逐条对齐（算法名去 `-` 小写归一、校验和只剥同算法 `<algo>:` 前缀、hex 忽略大小写；任一侧空 / 不可解析 / 算法不同 = 不比较），两端测试覆盖同一组用例；设置页算法下拉的四个技术名（`crates/settings/src/sections/download.rs` 本地映射）同此

## `crates/downloads/src/model/bt_trackers.rs`（详情 Tracker 页：状态筛选标签 + 计数、表头三态排序，DHT 伪行固定不参与；稳定排序、缺省值垫底）

→ `web/src/pages/downloads/detail/btStatus.ts::{trackerCounts, trackerView, nextTrackerSort}`：逐条对齐，两端测试共用 `crates/downloads/tests/fixtures/bt_tracker_view_cases.json`（改规则先改用例）；两端列表都虚拟化（GPUI `ListState` 可变行高、Web `useVirtualizer` + `measureElement`）

## `crates/downloads/src/pages/task_detail_ed2k.rs`（详情「来源 / 网络」页签的纯函数：源状态 → 文案、冷却 / 下次找源是否在未来、「同时连接来源数」选项与当前值归一、LowID 源地址展示）

→ `web/src/pages/downloads/detail/ed2kStatus.ts` 逐条对齐（两端单测覆盖同一组边界：segments 为 0 / 9 / 负数、`retryAt` 已过期、`queueRank` 缺省、LowID 源无 `addr`）；HighID / LowID 与 UPnP 文案映射在 Web 另由设置页共用（`web/src/lib/ed2kNetwork.ts`）。两端都不重排 `peers`（服务端已按活跃度排序），渲染封顶 200 行；UPnP / Kad 开关写同步键 `ed2k.enable_upnp` / `ed2k.enable_kad`，「同时连接来源数」写 `daemon.task.setSegments`（当前值取 `Ed2kStatusDto.segments`，不是任务行）

## 扩展 → Android App 的外部唤起载荷（`fluxDown/utils/android-intent.ts`：包名 `ANDROID_PACKAGE`、extra 键 `ANDROID_EXTRA`、Intent URI 编码；`fluxdown://download?url=&filename=&cookies=&referrer=&headers=` 协议参数同理）

→ `mobile/Android/app/.../ExternalDownloadActivity.kt`（`EXTRA_*` 常量）+ `mobile/Android/core/.../capture/ExternalDownload.kt::ExternalIntake.fromView`：键名逐字一致，两侧测试（`android-intent.test.ts` / `ExternalDownloadTest`）都跑；改包名同步 `mobile/Android/app/build.gradle.kts::applicationId`
