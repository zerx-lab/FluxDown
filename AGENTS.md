# FluxDown — 项目知识库

**应用名称**: FluxDown（多协议下载管理器，IDM 免费替代品）
**官网**: https://fluxdown.zerx.dev
**技术栈**: Flutter (GUI) + Rust (下载引擎) + WXT 浏览器扩展
**FFI 框架**: [Rinf 8.9](https://rinf.cunarist.org)（Dart↔Rust 信号通信，bincode 序列化）

## 产品定位

> **"Downloads, Supercharged."**（下载，全面加速。）

- **核心价值主张**: Rust 驱动，10x 下载速度，永久免费，零广告，零追踪，无需账号
- **目标用户**: 需要高速下载的用户、IDM 付费用户替代、关注隐私的用户、多协议需求专业用户
- **与 IDM 对比优势**: 完全免费、现代技术栈（Rust + Flutter）、本地优先架构、零追踪零广告
- **平台支持**: Windows（已发布）；macOS/Linux/Web/移动端（规划中）
- **SEO 描述**: "A blazing fast, multi-protocol download manager with browser extension. Powered by Rust engine with HTTP/HTTPS/FTP support, intelligent segmentation, and speed optimization."

## 命令速查

```bash
# 开发运行
# flutter run -d windows            # ⚠️ 禁止运行此命令
rinf gen                             # 修改 Rust 信号后必须执行，生成 Dart 绑定

# 构建与检查
cargo build                          # 构建 Rust 后端
cargo clippy                         # Rust lint（deny 级别，见下方规则）
flutter analyze                      # Dart 静态分析
flutter build windows                # 构建 Windows 发行版

# 测试
flutter test                         # 全部 Dart 测试
flutter test test/widget_test.dart   # 运行单个 Dart 测试文件
cargo test -p fluxdown_engine        # 运行下载引擎全部单元测试（native/engine，下载协议/分段/DB 等核心逻辑）
cargo test -p hub                    # 运行 hub 适配层全部单元测试（native/hub，Rinf FFI/信号桥接）
cargo test -p fluxdown_api           # 运行本机 API 服务全部测试（native/api，axum HTTP API/aria2 兼容）
cargo test -p fluxdown_server        # 运行 headless 服务器全部测试（native/server，WS/actor/扩展路由）
cargo test -p fluxdown_cli           # 运行命令行客户端测试（native/cli，格式化/退出码/尺寸解析 doctest）
cargo test -p fluxdown_engine -- segment_advisor # 运行特定 Rust 测试模块
cargo test -p fluxdown_engine -- test_name       # 运行单个 Rust 测试函数
PG_TEST_URL=postgres://postgres:pw@localhost/postgres cargo test -p fluxdown_engine -- --ignored pg_smoke  # Postgres 后端冒烟（需本地 pg）
cargo run -p fluxdown_api --example gen_openapi > website/public/openapi.json  # 改动 API 后重新生成 OpenAPI 规范（官网 /api-docs 渲染）

# Web 服务器（headless，native/server）
cargo run -p fluxdown_server         # 启动服务器（默认 0.0.0.0:17800；环境变量：FLUXDOWN_BIND / FLUXDOWN_DATA_DIR / FLUXDOWN_DATABASE_URL / FLUXDOWN_WEBROOT）

# 命令行客户端（native/cli，二进制名 fluxdown）
cargo build -p fluxdown_cli          # 构建 CLI（target/debug/fluxdown）
cargo run -p fluxdown_cli -- ping    # 探活；子命令 add/list/status/pause/resume/rm/pause-all/resume-all/queue/watch/info
# 环境变量：FLUXDOWN_URL（默认 http://127.0.0.1:17800）/ FLUXDOWN_TOKEN（管理 API token）
cargo run -p fluxdown_cli -- add <url> --local  # B 模式：内嵌引擎独立下载，不依赖 App/server（仅 add 支持 --local）
# Web 前端（web/ 目录下，React 19 + TanStack + Tailwind v4，包管理器 bun）
bun run dev                          # 开发服务器 localhost:5173（/api 代理到 localhost:17800）
bun run build                        # 构建到 web/dist（FLUXDOWN_WEBROOT=web/dist 由服务器托管）
bun run lint                         # oxlint

# 依赖
flutter pub get                      # Dart 依赖安装
cargo install rinf_cli               # Rinf CLI（首次安装）

# 浏览器扩展（fluxDown/ 目录下）
npm run dev                          # 开发模式（Chrome）
npm run dev:firefox                  # 开发模式（Firefox）
npm run build                        # 构建生产版
npm run zip                          # 打包上架

# 官网（website/ 目录下，Astro + React）
npm run dev                          # 本地开发服务器 localhost:4321
npm run build                        # 构建生产版到 dist/
npm run preview                      # 预览构建结果

# 发布版本（推送 v* tag 触发 GitHub Actions，release notes 由 git-cliff 从 Conventional Commits 生成）
git tag -a v0.x.x -m "v0.x.x" && git push origin v0.x.x

# 图标生成（修改 assets/logo/fluxdown_logo.svg 后执行）
bun scripts/gen_icons.ts               # 全平台图标一键生成（50 个文件，覆盖所有平台）
```

## 项目结构

```
x_down/
├── lib/                               # Flutter 前端（Dart SDK ^3.10.8）
│   ├── main.dart                      # 应用入口（多窗口分发、初始化流程）
│   └── src/
│       ├── models/                    # 数据模型与状态管理
│       │   ├── download_task.dart     # 任务模型（状态枚举/文件类型/分段数据）
│       │   ├── download_controller.dart  # 核心控制器（桥接 Rust 信号和 Flutter UI）
│       │   ├── download_queue.dart    # 命名队列模型
│       │   └── settings_provider.dart # 全局设置（30+ 配置项）
│       ├── pages/                     # 页面
│       │   ├── home_page.dart         # 主页面（三栏布局：侧边栏+列表+详情）
│       │   └── settings_page.dart     # 设置页面（6个分类：通用/外观/下载/BT/代理/关于）
│       ├── i18n/                      # 国际化
│       │   ├── locale_provider.dart   # 语言切换与持久化
│       │   └── translations.dart      # 中英双语翻译字符串
│       ├── services/                  # 服务层
│       │   ├── external_download_service.dart  # 监听浏览器扩展/RPC/API 请求（Rinf 信号），首选独立小窗，回退主窗口内对话框
│       │   ├── popup_window_service.dart       # 独立快速下载小窗（主引擎侧：fluxdown/popup_host 通道 + 载荷组装 + 结果回填）
│       │   ├── quick_download_submitter.dart   # 表单结果统一提交器（解析多行 URL / 记录偏好 / 发送下载信号）
│       │   ├── hls_quality_service.dart        # HLS 画质选择服务
│       │   ├── tray_service.dart               # 系统托盘
│       │   ├── notification_service.dart       # 下载完成通知（800ms 防抖合批，Win: Win32 悬浮窗 / Linux/mac: 系统通知）
│       │   ├── update_service.dart             # 自动更新（GitHub Releases）
│       │   ├── feedback_service.dart           # 反馈提交（GitHub Issues）
│       │   ├── log_service.dart                # 日志管理（2MB 分卷，总量默认 10MB 超量清理，保留 7 天）
│       │   ├── open_folder.dart                # 打开文件夹（跨平台）
│       │   └── win32_toast/                    # Windows 悬浮通知窗（纯 Win32 GDI，主屏右下角）
│       ├── theme/                     # 主题
│       │   ├── app_theme.dart         # 浅色/深色主题构建
│       │   ├── app_colors.dart        # 主题感知色板（AppColors.of(context)）
│       │   └── theme_provider.dart    # 主题切换+持久化（SharedPreferences）
│       ├── widgets/                   # UI 组件（见下方详细清单）
│       ├── mobile/                    # 移动端（Android/iOS）UI 层，复用 models/i18n/theme/bindings
│       │   ├── mobile_app.dart        # 移动端根组件（无桌面服务；保留 HLS/BT 选择服务）
│       │   ├── mobile_shell.dart      # 双屏壳（任务列表/设置）+ 悬浮玻璃 Dock
│       │   ├── mobile_ui.dart         # 设计 Token/玻璃弹层/Chip/进度条/分段格子映射纯函数
│       │   ├── screens/               # mobile_tasks_screen（顶栏+Tab+卡片+FAB）/ mobile_settings_screen
│       │   ├── pages/                 # mobile_task_detail_page（分段可视化+速度曲线+操作）
│       │   └── sheets/                # 筛选 / 新建下载 / 任务动作 三个底部弹层
│       └── bindings/                  # ⚠️ 自动生成 — 勿手动编辑
├── native/engine/                     # `fluxdown_engine` crate（edition 2024，零 FFI 依赖）
│   └── src/
│       ├── lib.rs                     # `Engine` facade（唯一构造入口）+ `EngineConfig`/`EngineError`/`NoopSink`/`NoopSelection`
│       ├── events.rs                  # `EngineEvent`（进度/分段拆分/队列变化等）+ `EventSink` trait
│       ├── selection.rs               # `SelectionOutcome`/`HostSelection` trait（HLS 画质/BT 文件选择）
│       ├── model.rs                   # 引擎领域类型（TaskInfo/QueueInfo/SegmentDetail/BtFileEntry/…，不带 rinf derive）
│       ├── download_manager.rs        # 并发管理/任务生命周期/进度报告（`progress_reporter`）
│       ├── downloader.rs              # HTTP/HTTPS 下载引擎（分片/断点续传/`RequestSpec`/`build_request`）
│       ├── ftp_downloader.rs          # FTP 下载引擎（suppaftp 同步 API）
│       ├── bt_downloader.rs           # BitTorrent 引擎（librqbit）
│       ├── hls_downloader.rs          # HLS 下载引擎（M3U8/多码率/AES解密）
│       ├── dash_downloader.rs         # DASH 下载引擎（MPD，基础支持）
│       ├── segment_coordinator.rs     # 动态分段协调（主动拆分/抢救慢速分段）
│       ├── meta_prober.rs             # 文件元数据探测（HEAD/Range:0-0）
│       ├── proxy_config.rs            # 代理配置（无/系统/手动，读 Windows 注册表）
│       ├── db.rs                      # 持久化层（sqlx Any 池：SQLite/PostgreSQL 双后端，$N 占位符统一 SQL）
│       ├── data_dir.rs                # 数据目录解析（`resolve_data_dir(Option<&Path>)`）
│       ├── logger.rs                  # 全局文件日志（`log_info!`/`log_error!`，`#[macro_export]`）
│       ├── speed_limiter.rs           # Token bucket 全局速度限制器
│       ├── segment_advisor.rs         # 动态分段计算（文件大小+CPU+带宽）
│       └── tracker_subscription.rs    # BT tracker 订阅列表抓取/去重
│   ├── examples/headless_download.rs  # CLI 式同进程直接调用证明（不依赖 hub/rinf）
│   └── tests/                         # realtest.rs / corruption_test.rs（迁移自 hub，确定性/真实网络回归）
├── native/api/                        # `fluxdown_api` crate（本机 HTTP API，axum，零 rinf 依赖）
│   └── src/
│       ├── types.rs                   # wire JSON 契约（TaskDto/CreateTaskRequest/DownloadRequest，camelCase）
│       ├── routes.rs                  # 路径常量（/api/v1/*，server 与 Rust 客户端共用）
│       ├── service.rs                 # `ApiHost` trait —— 宿主能力契约（桌面 App / 未来 server 各自实现）
│       ├── server.rs                  # axum 服务器（/ping、脚本接管、aria2 兼容、管理 API；仅 127.0.0.1）
│       ├── jsonrpc.rs                 # aria2 JSON-RPC 兼容层（36 方法全覆盖派发：addUri/addTorrent/tell*/pause/remove/get(change)GlobalOption/…）
│       ├── aria2.rs                   # aria2 纯映射层（GID=UUID去连字符前16hex+前缀反查/status 映射/选项↔config 映射/字段拼装/错误文案）
│       ├── takeover.rs                # 脚本接管批量请求解析
│       └── auth.rs                    # 鉴权（常量时间比较/Client 头门禁/管理 API 强制 token）
├── native/hub/                        # Rinf FFI 适配层 crate（`hub`，edition 2024，crate 名不可改）
│   └── src/
│       ├── lib.rs                     # 入口（tokio current_thread runtime）
│       ├── signals/mod.rs             # 信号结构体定义（DartSignal/RustSignal/SignalPiece，不可动——Dart 绑定契约）
│       ├── actors/download_actor.rs   # 核心事件循环（tokio::select!），构造 `fluxdown_engine::Engine` 并转发调用
│       ├── rinf_sink.rs               # `EventSink` 实现：`EngineEvent` → 具体 `RustSignal` 发送
│       ├── rinf_selection.rs          # `HostSelection` 实现：HLS/BT 选择请求 → `RustSignal` + oneshot 等待表
│       ├── signal_bridge.rs           # `engine::model::*` ↔ `signals::*` 的 `From` 转换
│       ├── protocol_registry.rs       # fluxdown:// 自定义协议注册（Windows）
│       ├── file_association.rs        # .torrent 文件关联注册（Windows）
│       ├── native_messaging.rs        # Windows: Named Pipe `\\.\pipe\fluxdown`；Linux: Unix socket 服务端
│       ├── api_host.rs                # `fluxdown_api::ApiHost` 实现（读直查 Db，写经 ApiCommand+oneshot 进 actor）
│       ├── nmh_registry.rs            # NMH 清单注册（Linux: 写入 Chrome/Firefox NMH JSON）
│       ├── reveal_file.rs             # 在文件管理器中定位文件/打开目录
│       └── updater.rs                 # 自动更新器（GitHub Releases API）
├── native/nmh/                        # Native Messaging Host（Linux/macOS 平台）
│   └── src/main.rs                    # 独立二进制：stdin/stdout ↔ Unix socket 桥接 + 启动 app
├── native/server/                     # `fluxdown_server` crate（headless Web 服务器，axum，零 rinf 依赖）
│   └── src/
│       ├── main.rs                    # 组装：Engine + actor + WS hub + api_router 合并扩展路由 + SPA 托管（ServeDir fallback）
│       ├── config.rs                  # 环境变量（FLUXDOWN_BIND/DATA_DIR/DATABASE_URL/WEBROOT）+ token 首次生成
│       ├── actor.rs                   # ActorCmd 命令循环（独占 Engine；ApplyConfig live-apply 镜像桌面 SaveConfig）
│       ├── ws_hub.rs                  # WsHub broadcast + EngineEventSink（EngineEvent→WS JSON）+ WsHostSelection（HLS/BT 经 WS 往返）
│       ├── host.rs                    # `ApiHost` 实现（读直查 Db，写经 ActorCmd+oneshot；submit_external 直接建任务，无确认框）
│       ├── wire.rs                    # WS/扩展 REST 的 wire JSON 契约（WsServerMsg/WsClientMsg，camelCase）
│       └── routes_ext.rs              # 扩展路由（/ws、/config、队列 CRUD、/tasks/{id}/file 流式取回、/fs/list、/proxy/test、/stats、合并版 openapi.json + Scalar /docs）
├── web/                               # Web SPA（React 19 + TanStack Router/Query/Table/Virtual + Tailwind v4 + Radix，bun）
│   └── src/
│       ├── design.css                 # 移植自 design/web/styles.css（像素级依据）
│       ├── lib/                       # api.ts（typed REST）/ ws.ts（可重连 WS + live store）/ auth/format/theme
│       ├── routes/                    # login / tasks（三栏主界面）/ settings
│       └── components/                # tasks 组件 + dialogs（新建下载/HLS 画质/BT 文件选择）
├── fluxDown/                          # WXT 浏览器扩展（Chrome MV3, TypeScript）
├── website/                           # 官网（Astro + React，部署到 Vercel）
│   └── src/
│       ├── pages/index.astro          # 主页（Hero/Features/Extension/Download 区块）
│       ├── pages/faq.astro            # FAQ 页面（8个常见问题，中英双语）
│       ├── pages/changelog.astro      # 更新日志（GitHub Releases 自动加载）
│       ├── pages/feedback.astro       # 反馈页面
│       ├── pages/vote.astro           # 社区投票页面
│       ├── pages/qq-group.astro       # QQ 群页面（群号：832143651）
│       ├── pages/announcements.astro  # 公告页面
│       └── pages/api/                 # API 路由（feedback/issues/release/vote/subscribe/changelog）
├── scripts/
│   ├── send_notify.py             # 通知推送（邮件/钉钉等）
│   └── gen_icons.ts               # 全平台图标生成（bun scripts/gen_icons.ts）
├── Cargo.toml                         # Rust workspace（resolver = "3"）
└── pubspec.yaml                       # Flutter 依赖
```

## 架构概览

```
[Dart UI (shadcn_ui)] ←Rinf FFI→ [download_actor (tokio::select! 事件循环, hub crate)]
                                          │ 构造 fluxdown_engine::Engine
                          ┌───────────────┼──────────────────────────┐
                    [RinfEventSink]  [RinfHostSelection]      [native_messaging]
                   (EventSink impl) (HostSelection impl)  Windows: Named Pipe
                          │                │                Linux: Unix socket
                          └───────┬────────┘                       ↑
                          [fluxdown_engine::Engine]           [fluxdown_nmh 进程]
                     │        │        │        │             (stdin/stdout NMH)
              [DownloadManager]      [Db]                            ↑
                     │        (sqlx Any: SQLite/PG)           [WXT 浏览器扩展]
              ┌──────┼──────┬───────┐
           [HTTP] [FTP]   [BT]    [HLS/DASH]
                     │
            [SpeedLimiter] + [segment_advisor]
                        + [segment_coordinator]
```

**crate 边界**: `fluxdown_engine`（`native/engine`）零 rinf/Dart 依赖，通过 `EventSink`/`HostSelection`
两个 trait 与宿主解耦；`hub`（`native/hub`）是 Rinf FFI 适配层，只做信号收发与类型转换
（`rinf_sink.rs`/`rinf_selection.rs`/`signal_bridge.rs`），不含下载协议逻辑。

**状态管理**: ChangeNotifier + ListenableBuilder（无 Provider/Riverpod/Bloc）
**并发模型**: 每个下载 spawn 独立 tokio task，CancellationToken 控制生命周期
**状态码**: 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing

## UI 组件完整清单

### 页面

| 文件 | 功能描述 |
|------|---------|
| `pages/home_page.dart` | 主页面。三栏布局（侧边栏 180-320px / 任务列表 / 详情面板 240-420px），全局快捷键（Ctrl+F/A/Esc/Del），Boost 优先下载 Banner |
| `pages/settings_page.dart` | 设置页面。侧边栏导航 6 个分类：通用（开机启动/关闭到托盘/torrent关联）、外观（语言/主题/颜色）、下载（目录/线程/并发/速度/UA/队列）、BT（自定义 Tracker）、代理（无/系统/手动 + 代理测试）、关于（版本更新） |

### 核心布局组件

| 文件 | 功能描述 |
|------|---------|
| `widgets/sidebar.dart` | 侧边栏。Logo、文件类型筛选器（视频/音频/文档/图片/压缩包/其他）、状态筛选器、命名队列列表（增删改查）、反馈按钮 |
| `widgets/header_bar.dart` | 顶部栏。搜索框（Ctrl+F）、批量操作（管理模式/全选/暂停/删除）、全局暂停/恢复、新建下载、设置、窗口控制 |
| `widgets/task_tab_bar.dart` | 任务状态 Tab（全部/下载中/已完成/已暂停/错误），显示各状态计数 |
| `widgets/task_list.dart` | 任务列表。虚拟化滚动，时间分组（今天/昨天/本周/本月/更早），分组折叠/展开，右键菜单 |
| `widgets/task_list_item.dart` | 任务列表项。文件图标、文件名/大小/速度/进度、协议标识（HTTP/FTP/BT）、进度条、多选复选框、操作按钮、Boost 标识 |
| `widgets/detail_panel.dart` | 详情面板。5个Tab：常规（文件信息/URL/路径/进度/速度/ETA）、分段（IDM风格可视化+动态拆分动画）、队列（移动任务）、日志、高级（Checksum/代理） |
| `widgets/status_bar.dart` | 底部状态栏。全局下载速度、活跃任务数/总任务数、速度限制显示 |
| `widgets/title_drag_area.dart` | 自定义标题栏拖拽区域 |

### 对话框组件

| 文件 | 功能描述 |
|------|---------|
| `widgets/new_download_dialog.dart` | 新建下载。URL（多行批量）、文件名、保存目录、线程数、队列、Cookies、代理、UA、Checksum |
|`widgets/quick_download_dialog.dart`|快速下载对话框（主窗口内回退路径 + 悬浮球拖链入口；表单主体复用 quick_download_form）|
|`widgets/quick_download_form.dart`|快速下载共享表单（URL/目录/线程/重命名/队列 + 高级选项：任务代理/UA/Cookie 预填可编辑/哈希校验 + 动作按钮）。经 QuickDownloadFormHost 抽象隔离全局单例，主窗口对话框与独立小窗共用|
| `widgets/hls_quality_dialog.dart` | HLS 画质选择。M3U8 多码率选择，显示带宽/分辨率 |
| `widgets/update_changelog_dialog.dart` | 版本更新对话框。Markdown 渲染更新日志，立即更新/稍后提醒 |
| `widgets/feedback_dialog.dart` | 反馈对话框。提交到 GitHub Issues |
| `widgets/context_menu.dart` | 右键菜单。暂停/恢复/取消/删除/删除+文件、打开文件/文件夹、复制URL、Boost优先 |
| `widgets/dir_picker_field.dart` | 文件夹选择器（系统文件对话框） |

## 数据模型

### 任务状态枚举（8种）
`pending`(0) / `downloading`(1) / `paused`(2) / `completed`(3) / `error`(4) / `resuming` / `preparing`(5)

### 文件类型分类（7种）
`all` / `video`(15种扩展名) / `audio`(10种) / `document`(14种) / `image`(13种) / `archive`(13种) / `other`

### 时间分组（5种）
`today` / `yesterday` / `thisWeek` / `thisMonth` / `older`

### 数据库（db.rs，sqlx `Any` 池：SQLite / PostgreSQL 双后端）

```sql
-- 任务表
CREATE TABLE tasks (
    id TEXT PRIMARY KEY,              -- UUID
    url TEXT NOT NULL,
    file_name TEXT NOT NULL,
    save_dir TEXT NOT NULL,
    status INTEGER NOT NULL DEFAULT 0,  -- 0-5 状态码
    total_bytes INTEGER NOT NULL DEFAULT 0,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    segments INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,           -- Unix 时间戳（秒）
    error_message TEXT NOT NULL DEFAULT '',
    proxy_url TEXT NOT NULL DEFAULT '',
    queue_id TEXT NOT NULL DEFAULT '',
    checksum TEXT NOT NULL DEFAULT ''   -- 格式：algo=hexhash
);

-- 分段表
CREATE TABLE task_segments (
    -- 复合主键 (task_id, segment_index)；旧桌面库遗留的 id AUTOINCREMENT 列不再读取
    task_id TEXT NOT NULL,
    segment_index INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, segment_index),
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

-- 配置表（30+ 配置项）
CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- BT 文件表
CREATE TABLE torrent_files (
    task_id TEXT PRIMARY KEY,
    file_bytes BLOB NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

-- 队列表
CREATE TABLE queues (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    speed_limit_kbps INTEGER NOT NULL DEFAULT 0,
    max_concurrent INTEGER NOT NULL DEFAULT 0,
    default_save_dir TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    default_segments INTEGER NOT NULL DEFAULT 0,
    default_user_agent TEXT NOT NULL DEFAULT ''
);
```

**数据库特性**: sqlx `Any` 连接池（URL scheme 选后端：`sqlite:`/`postgres:`）、`$N` 占位符统一 SQL、SQLite 侧 WAL + 外键、pg 侧字节列 BIGINT、Schema 迁移（幂等 ADD COLUMN）、5s 批量持久化。`Engine::new` 为 async，`EngineConfig.database_url` 为 `None` 时用数据目录下 SQLite 文件

## 下载协议支持

| 协议 | 实现文件 | 特性 |
|------|---------|------|
| HTTP/HTTPS | `downloader.rs` | 多线程、断点续传、Cookie、代理、Checksum、Accept-Encoding:identity |
| FTP | `ftp_downloader.rs` | 多线程（独立连接）、REST断点续传、代理（SOCKS4/5/HTTP）、用户名密码 |
| BitTorrent | `bt_downloader.rs` | Magnet链接、.torrent文件、DHT、UPnP、自定义Tracker（25个，亚洲优先）、断点续传 |
| HLS | `hls_downloader.rs` | M3U8解析、多码率选择、AES-128-CBC解密、分段下载合并、重试3次 |
| DASH | `dash_downloader.rs` | MPD格式，基础支持 |

## Rust 核心模块详解

> 以下模块均已迁移到 `native/engine`（`fluxdown_engine` crate）。`native/hub/src/logger.rs`
> 是转发 `pub use fluxdown_engine::logger::*;` 的 shim，保留 `crate::logger::*` 路径供
> hub 内 App-shell 专属文件（`native_messaging.rs`/`api_host.rs`/`updater.rs`/…）零改动继续使用。

### segment_advisor.rs — 动态分段计算
- 文件 < 1MB → 1线程；1-10MB → 4；10-100MB → 8；100MB-1GB → 16；> 1GB → 32
- CPU 核心数上限：`num_cpus::get() * 2`

### segment_coordinator.rs — 动态分段协调
- **主动拆分（Proactive）**: 检测慢速分段 → 拆分为两段加速
- **抢救拆分（Reactive）**: 分段卡住 → 拆分并行
- 拆分原子性：子分段插入 + 父分段缩小，单事务提交
- 通过 `EventSink::emit(EngineEvent::SegmentSplit{..})` 上报，hub 的 `RinfEventSink` 转发为
  `SegmentSplitEvent` 信号触发 Dart 端拆分动画

### speed_limiter.rs — Token Bucket 限速
- 参数：`rate`（字节/秒）、`burst`（=rate，突发缓冲）
- API：`consume(bytes)` 异步等待令牌

### download_manager.rs — 任务生命周期
- 并发控制（`maxConcurrentTasks`）
- 协议分发（HTTP/FTP/BT/HLS/DASH）
- 速度平滑（EMA，α=0.3）
- WAL Checkpoint（所有任务空闲时执行）
- 队列管理（全局默认队列 + 命名队列独立配置）
- 通过 `Arc<dyn EventSink>`/`Arc<dyn HostSelection>` 与宿主解耦（由 `Engine::new` 注入）

### proxy_config.rs — 代理配置
- 模式：`None` / `System`（Windows 注册表）/ `Manual`
- 类型：HTTP / HTTPS / SOCKS4 / SOCKS5
- 读取注册表路径：`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`

### meta_prober.rs — 元数据探测
- HEAD 请求 → GET Range:0-0 降级 → 文件名解析（URL / Content-Disposition）
- 检测 Accept-Ranges 支持

### logger.rs（`native/engine/src/logger.rs`）— 全局文件日志
- 与 Dart 端 `LogService` 写入同一目录、同一文件（`fluxdown_YYYY-MM-DD.log`）
- 启动时自动清理 7 天前的日志文件
- 提供 `#[macro_export]` 的 `log_info!` / `log_error!` 宏（`$crate` 前缀保证跨 crate 调用正确解析），用法同 `format!()`
- hub 侧使用前需在文件顶部 `use crate::logger::log_info;`（经 `native/hub/src/logger.rs` shim 转发）

## 本机 API 服务（native/api，`fluxdown_api` crate）

一个端口（默认 17800，仅监听 127.0.0.1）、一个 axum 服务器，三组按配置独立启停的路由；
`local_server_*` 配置变更时 actor 热重启监听（优雅停机 + 重绑，无需重启应用）。

|路由组|端点|开关|鉴权|
|---|---|---|---|
|探活|`GET /ping`|总开关|无|
|脚本接管|`POST /download`、`/download/batch`|`local_server_takeover_enabled`|`X-FluxDown-Client` 头 + 可选 token|
|aria2 兼容|`POST /jsonrpc`（36 方法全覆盖：addUri/addTorrent/tellStatus·Active·Waiting·Stopped/pause·unpause·remove(+force/All)/getFiles·getUris·getOption/get·changeGlobalOption/getGlobalStat/purge·removeDownloadResult/getVersion·getSessionInfo/multicall·listMethods·listNotifications；getPeers·getServers 返空、saveSession·changeOption 降级 OK、addMetalink·changePosition·changeUri·shutdown 明确拒绝 code:1。GID=task_id UUID 去连字符前 16 hex，支持前缀反查；业务错误统一 aria2 风格 code:1；multicall 信封免鉴权、子调用各自带 token）|`local_server_jsonrpc_enabled`|可选 token（`X-FluxDown-Token` 头或 `params[0]="token:xxx"`）|
|管理 API|`GET /api/v1/info`、`GET/POST /api/v1/tasks`、`GET/DELETE /api/v1/tasks/{id}`、`PUT /api/v1/tasks/{id}/pause\|continue`、`PUT /api/v1/tasks/pause\|continue`、`GET /api/v1/queues`|`local_server_api_enabled`|**强制** token（`Authorization: Bearer` 或 `X-FluxDown-Token`）|
|MCP|`POST /mcp`（initialize / tools/list / tools/call / ping；9 个下载管理工具）|`local_server_mcp_enabled`|**强制** token（`Authorization: Bearer` 或 `X-FluxDown-Token`，与管理 API 共用）|
|API 文档|`GET /api/v1/openapi.json`（OpenAPI 3.1）|`local_server_api_enabled`|无（纯接口描述，不含数据）|

**架构**：`fluxdown_api` 零 rinf 依赖，只定义 wire 契约（`types.rs`，camelCase JSON）+ 路径常量
（`routes.rs`）+ 宿主契约（`service.rs` 的 `ApiHost` trait）+ axum 服务器（`server.rs`）。
桌面 App 在 `native/hub/src/api_host.rs` 实现 `ApiHost`：读操作直查 `Db`（Clone），
写操作打包 `ApiCommand` + oneshot 经 mpsc 进 `download_actor` 事件循环串行执行。
未来 headless server / 手机端复用同一 crate，只需另写一个 `ApiHost` 实现；
MCP server 等 Rust 客户端直接 import `types` + `routes`。

**语义区分**：浏览器脚本接管入口 → 外部下载流程（弹快速下载确认框）；aria2 `addUri`/`addTorrent`
与管理 API `POST /api/v1/tasks` → 直接创建任务并返回真实 ID/GID（aria2 客户端与自动化客户端
预期同步建任务语义，无弹框）。

**OpenAPI 文档**：spec 由 utoipa 从 handler 注解（`#[utoipa::path]`）与 `ToSchema` 派生
（`openapi.rs`，含漂移守卫测试——路由常量与注解不同步会跑挂）。改动 API 后执行
`cargo run -p fluxdown_api --example gen_openapi > website/public/openapi.json` 重新生成，
官网 `/api-docs` 页用 Scalar（CDN）渲染该文件。

**MCP（Model Context Protocol）**：`native/api/src/mcp.rs` 是与 `jsonrpc.rs` 同构的薄派发层
（JSON-RPC 2.0 over 单 `POST /mcp`），全走 `ApiHost` trait，零新依赖。采用 Streamable HTTP
无状态子集：请求返回 `application/json`，通知（无 `id`）返回 `202`，不维护 `Mcp-Session-Id`。
鉴权复用 `check_management_auth`（Bearer / `X-FluxDown-Token`，规范允许内部部署用静态 token
代替 OAuth 2.1）。暴露 9 个工具（`download_add`/`download_list`/`download_get`/`download_pause`/
`download_resume`/`download_pause_all`/`download_resume_all`/`download_remove`/`queue_list`），
直接映射 `ApiHost` 方法。桌面 App 与 headless server 经同一 `register_core` 自动获得 `/mcp`；
AI 客户端（Claude Desktop/Cursor/Cline）配置 `{"url":".../mcp","headers":{"Authorization":"Bearer <token>"}}` 即可接入。

## 命令行客户端（native/cli，`fluxdown_cli` crate）

aria2c 风格 CLI，二进制名 `fluxdown`。**A 模式（typed HTTP client）已实现**：复用
`fluxdown_api` 的 `routes`（路径常量）+ `types`（`TaskDto`/`CreateTaskRequest`/…，为客户端补齐
了 serde 双向 derive）+ `auth::TOKEN_HEADER`，与运行中的 App / headless server 通信，地址/JSON
永不漂移。**B 模式（`add --local` 内嵌 `fluxdown_engine::Engine` 独立下载）已实现**：不连运行
中的 App/server，在本进程构造引擎（`NoopSink`/`NoopSelection`）→ 创建任务 → 阻塞等待至终态 →
退出（`native/cli/src/local.rs`，结构照 `examples/headless_download.rs` 的直接 `&mut Engine` 顺序
调用，不搭 actor）。仅 `add` 支持 `--local`；其余命令仍走 A 模式（连 App/server）。

**命令集**：`ping`（无鉴权探活）/ `info` / `add`(别名 `get`) / `list`(别名 `ls`) /
`status`(别名 `stat`) / `pause` / `resume` / `rm`(`--delete-files`) / `pause-all` / `resume-all` /
`queue` / `watch`（ANSI 清屏轮询进度直至终态）。

**约定**：token 经 `--token` 或 `FLUXDOWN_TOKEN`，服务地址经 `--url` 或 `FLUXDOWN_URL`
（默认 `http://127.0.0.1:17800`）；`--json` 输出脚本友好 JSON；`add` 支持多 URL + `-i`
输入文件（每行一 URL，`#` 注释，`-` 读 stdin）+ `-d/-o/-s/--proxy/-U/--referrer/--cookies/--queue/--checksum`。
aria2 风格退出码：0 成功 / 1 未知 / 2 超时 / 3 未找到 / 5 网络 / 7 中断未完成（`--local` Ctrl-C）/ 24 鉴权 / 32 参数非法。
`K/M/G/T` 尺寸后缀按 1024 进制解析（`format::parse_size`）。HTTP client `.no_proxy()` 直连本地
回环，不受系统代理干扰。放弃 Metalink/XML-RPC/saveSession（SQLite 已覆盖）。

**B 模式（`--local`）约定**：与 App/server **共享同一数据目录/SQLite**（安装模式；Windows 便携模式下
CLI 独立二进制可能解析到不同目录、不共享），下载任务对 App 可见。落盘目录优先级：`-d` > 共享库
`default_save_dir` 配置 > 当前工作目录。无墙钟超时（跑到完成或 Ctrl-C；Ctrl-C → 暂停任务 + 退出码 7，
续传经 App/server）。HLS 取最高码率、BT/magnet 下全部文件（无 UI 交互，`NoopSelection`）。
**并发告警**：勿在 App/server 活跃或另一 `--local` 运行时对**同一输出文件**并发下载（DB 层 WAL 安全，
仅同一目标文件并写才可能损坏）。web 侧独立可用不依赖本 CLI —— 运行 `fluxdown_server` 托管即可。

## 浏览器扩展（fluxDown/）

### 通信架构
全平台统一走 Native Messaging Host（NMH）协议，扩展与 app 间通过 IPC 通信：

- **Windows**: 扩展 → NMH（stdin/stdout）→ `fluxdown_nmh.exe` → Named Pipe `\\.\pipe\fluxdown`
- **Linux/macOS**: 扩展 → NMH（stdin/stdout）→ `fluxdown_nmh` → Unix socket `$XDG_RUNTIME_DIR/fluxdown.sock`

消息协议（4字节 LE 长度前缀 + JSON）：
- `{"action":"ping","msg_id":N}` → `{"success":true,"message":"pong","msg_id":N}`
- `{"action":"download","msg_id":N,...}` → `{"success":true,"message":"download accepted","msg_id":N}`
- `{"action":"warmup","msg_id":N}` → `{"success":true,"message":"warmed","msg_id":N}`（NMH 本地应答不转发：确保 App 已拉起+管道已连接；扩展在下载流程入口 fire-and-forget 发送，让 App 冷启动与 cookie 收集并行）

NMH 连接策略：ping 只探测不拉起 App;其余 action 未连接时 auto-launch App 并以固定 50ms 间隔轮询管道(上限 10s);写失败(App 重启后陈旧管道)进程内重连+重发一次(写失败=内核未收帧,重发安全;读失败不重发,防重复任务)。

NMH 注册：
- NMH 清单：`~/.config/google-chrome/NativeMessagingHosts/com.fluxdown.nmh.json`（Linux）
- NMH 二进制：`target/debug/fluxdown_nmh`（workspace target/ 目录）
- App 启动时自动调用 `nmh_registry::register()` 注册清单

### 下载拦截三层防线
1. **第一层** `webRequest.onHeadersReceived`: 缓存响应元数据，检测 Content-Disposition/Content-Type
2. **第二层** `downloads.onDeterminingFilename`: 主拦截（Chrome MV3 专属），`suggest({cancel:true})` 干净取消
3. **第三层** `downloads.onCreated + onChanged`: 兜底拦截，Firefox 唯一路径（300ms 等待元数据填充）

### 资源嗅探
- 检测：视频/音频、HLS（application/vnd.apple.mpegurl）、大文件（>1MB）、下载附件
- 存储：按 tabId 分组，浮动面板展示
- Badge：图标右上角数字显示资源数量

### 其他特性
- **Alt+Click 绕过**: 写入 bypassTokens（15秒有效），放行浏览器直接下载
- **右键菜单**: "Send to FluxDown"
- **统计**: 每日 sent/failed 计数，跨天自动重置
- **存储**: chrome.storage.sync（设置）+ chrome.storage.local（统计/主题）

## 设置项完整列表（settings_provider.dart）

| 分类 | 配置项 | 说明 |
|------|-------|------|
| 下载 | `defaultSaveDir` | 默认保存目录 |
| 下载 | `defaultSegments` | 默认线程数 |
| 下载 | `maxConcurrentTasks` | 最大并发数 |
| 下载 | `speedLimitBytes` | 全局速度限制（字节/秒） |
| 下载 | `globalUserAgent` | 全局 User-Agent（预设：Chrome/Firefox/Edge/Safari/百度网盘） |
| 下载 | `defaultQueueId` | 默认队列 |
| 行为 | `autoResumeOnStart` | 启动时自动恢复 |
| 行为 | `closeToTray` | 关闭到系统托盘 |
| 行为 | `autoStartup` | 开机启动 |
| 行为 | `autoCheckUpdate` | 自动检查更新 |
| 行为 | `silentDownloadEnabled` | 免打扰下载：外部下载请求不弹确认框，直接按默认设置创建任务（默认关） |
| BT | `btEnableDht` | 启用 DHT |
| BT | `btEnableUpnp` | 启用 UPnP |
| BT | `btPortStart/End` | 端口范围 |
| BT | `btCustomTrackers` | 自定义 Tracker 列表 |
| 代理 | `proxyMode` | 代理模式（None/System/Manual） |
| 代理 | `proxyType` | 代理类型（HTTP/HTTPS/SOCKS4/SOCKS5） |
| 代理 | `proxyHost/Port` | 代理地址 |
| 代理 | `proxyUsername/Password` | 代理认证 |
| 代理 | `proxyNoList` | 排除列表 |
| 文件关联 | `torrentAssocPrompted` | 是否已提示过 torrent 关联 |
| 文件关联 | `torrentAssociated` | 是否已关联 .torrent 文件 |
| API 服务 | `local_server_enabled` | 本机 API 服务总开关（默认开） |
| API 服务 | `local_server_port` | 监听端口（默认 17800，仅 127.0.0.1） |
| API 服务 | `local_server_token` | 访问令牌（管理 API 强制要求非空） |
| API 服务 | `local_server_takeover_enabled` | 浏览器脚本接管子开关（默认开） |
| API 服务 | `local_server_jsonrpc_enabled` | aria2 RPC 兼容子开关（默认开） |
| API 服务 | `local_server_api_enabled` | 管理 API 子开关（默认关，开启时 Dart 侧自动生成 token） |
| API 服务 | `local_server_mcp_enabled` | MCP 端点子开关（默认关；headless server 默认开）。与管理 API 共用 token |

## 主题系统

- **主题模式**: 亮色 / 深色 / 跟随系统
- **预设色彩方案（13套）**: Zinc（默认）/ Slate / Stone / Gray / Neutral / Red / Rose / Orange / Green / Blue / Yellow / Violet / Custom
- **字体**: MiSans 自定义字体族
- **色板层级**:
  - 背景: `bg` / `surface1` / `surface2`
  - 文字: `textPrimary` / `textSecondary` / `textMuted`
  - 交互: `border` / `hoverBg` / `accentBg`
  - 语义: `accent` / `destructive` / `warning` / `success`

## 服务层说明

| 服务 | 职责 |
|------|------|
| `external_download_service.dart` | 监听 Rust 发来的 ExternalDownloadRequest 信号：免打扰直建任务 → 首选独立小窗（PopupWindowService）→ 回退主窗口内快速下载对话框 |
| `popup_window_service.dart` | 外部唤起独立小窗（主引擎侧）。原生窗口承载**第二 Flutter 引擎**（entrypoint 参数 `--quick-popup`，零插件、不初始化 Rust），懒创建常驻复用；载荷（主题 tokens/语言/队列/默认目录）JSON 注入，结果经原生中继回主引擎发信号。显示时序为 reveal 握手：show 只投递载荷（窗口保持隐藏），弹窗 Dart 首帧就绪后经 `reveal(height)` 一次到位「设高+显示」，原生 3s 兜底定时器保证极端情况下窗口仍弹出；小窗可见期间新请求经 `append` 合入现有表单（append 模式，原生返回 false 时主引擎自愈失步的可见标志）；另有 15min pending watchdog 兜底复位。原生宿主：windows/runner/popup_window_host.cpp、macos/Runner/PopupWindowHost.swift、linux/popup_window_host.cc |
| `quick_download_submitter.dart` | 快速下载表单结果统一提交器：解析 aria2 风格多行条目、记录上次目录/线程数、发 ConfirmExternalDownload/BatchCreateTask |
| `hls_quality_service.dart` | 监听 HLS 画质信号，弹窗让用户选择码率 |
| `tray_service.dart` | 系统托盘图标+菜单（多语言），菜单项：显示窗口/新建下载/暂停恢复/退出 |
| `notification_service.dart` | 下载完成通知。800ms 防抖合批（3s 最长等待），Windows → Win32ToastWindow 主显示器右下角悬浮窗（无论主窗口可见性），Linux/macOS → 系统通知带"打开文件夹/打开文件"动作按钮（Linux D-Bus actions / macOS UNNotificationCategory；Wayland 禁止全局坐标定位，D-Bus 通知是唯一正确做法） |
| `update_service.dart` | GitHub Releases 检查，启动后 5s 静默检查，弹窗展示 changelog |
| `feedback_service.dart` | POST GitHub Issues API 提交反馈（含 OS/版本/语言系统信息） |
| `log_service.dart` | 按日期写入 `fluxdown_YYYY-MM-DD.log`，启动时清理 7 天前日志，提供 `logInfo()`/`logError()` 全局函数 |
| `open_folder.dart` | 跨平台打开文件夹（调用系统文件管理器） |
| `win32_toast/win32_toast_window.dart` | Win32 悬浮通知窗。卡片由 Flutter 主引擎离屏光栅化（`toast_card_renderer.dart`，与 App 同主题/字体），经 UpdateLayeredWindow（per-pixel alpha）贴入分层窗口；窗口侧 DefWindowProcW 原生指针 + Dart Timer 驱动（零 Dart 原生回调，无 Isolate 竞态），SPI_GETWORKAREA 定位主屏工作区右下角，串行播放+批次合并，hover 4 变体预渲染 |

## 官网（website/）

**技术栈**: Astro 5.17+ + React 19 + TypeScript + Tailwind CSS 4，部署到 Vercel
**多语言**: 中英双语（i18n 支持）

### 页面结构
- `/` — 主页（Hero / Features / Extension / Download / Announcements）
- `/faq` — 8个常见问题（中英双语）
- `/changelog` — 更新日志（GitHub Releases 自动加载，支持复制 Markdown/纯文本）
- `/feedback` — 反馈页面
- `/vote` — 社区投票（选择社区平台：微信群/QQ群/公众号）
- `/qq-group` — QQ 群（群号：832143651）
- `/announcements` — 公告页面
- `/privacy` — 隐私政策
- `/terms` — 服务条款
- `/docs/{en,zh}/...` — 产品文档（Content Collections,全量预渲染,bento 索引页 + 三栏正文页）

### 文档系统（/docs)
- **内容源**: `website/src/content/docs/{en,zh}/**/*.md`(纯 Markdown,禁 MDX/HTML);schema 见 `src/content.config.ts`(title 必填,section/order/sourceHash 可选)
- **路由**: `/docs/{lang}/{slug}/` 全部 `prerender = true`;`/docs/` 唯一 SSR 页,按 cookie(`fluxdown-locale`,仅用户主动切换语言时由 `saveLocale()` 写入)或 Accept-Language 302
- **回退机制**: zh 缺译仍生成 zh URL,渲染 en 内容 + 未翻译横幅 + `noindex`,不参与 hreflang/sitemap(判定单源 `src/lib/docs-fallback.ts`,页面与 astro.config sitemap filter 共享)
- **译文过期**: zh frontmatter `sourceHash` = en 正文 sha256 前 12 位(`npm run docs:hash <zh文件>` 自动写入;`--check-all` 全量检查);不匹配时页面显示过期横幅
- **社区贡献**: 每页"编辑此页"深链 GitHub 网页编辑器(自动 Fork→PR);页脚反馈表单 → `POST /api/feedback`(type=docs);CI `.github/workflows/website-ci.yml`(build + `docs:lint` 安全检查:拒 javascript:/data: 链接与外链图片);PR 模板 `.github/PULL_REQUEST_TEMPLATE/docs.md`
- **代码高亮**: Shiki 双主题(github-light/dark,`defaultColor:false`),global.css 中锚定 `html.light` 的桥接 CSS 决定实际配色

### API 路由（/api/）
- `POST /api/feedback` — 提交反馈
- `GET /api/release` — 获取最新 GitHub Release
- `GET /api/changelog` — 更新日志获取
- `GET/POST /api/vote` — 社区投票
- `POST /api/subscribe` — 订阅平台上线通知
- `GET /api/issues/[number]` — 获取 GitHub Issue
- `GET /api/issues/[number]/comments` — 获取 Issue 评论
- `GET /api/download/*` — 下载相关子路由
- `POST /api/webhooks/github` — GitHub Webhook

### 官网 8 大功能特性文案（供 AI 代码生成参考产品语言）
1. Rust-Powered Engine — 基于 Rust 和 Tokio，零开销抽象，内存安全，最大吞吐量
2. Smart Segmentation — IDM 风格智能分段，运行时动态拆分，空闲线程接管慢速分段
3. Multi-Protocol — HTTP/HTTPS/FTP/BitTorrent，每种协议专属优化引擎
4. Speed Control — Token bucket 全局限速，后台下载不影响浏览
5. Resume Anywhere — SQLite 全量断点续传，安全关机不丢进度
6. Browser Integration — Chrome/Firefox 三层拦截引擎，自动检测 HLS/DASH 流媒体
7. Beautiful Interface — 深浅主题 + 12套配色 + 可调节面板响应式布局
8. Clean & Private — 零广告/零追踪/无账号，本地优先架构，数据完全本地

## 代码风格与规范

### Rust 端

- **Edition**: 2024，Clippy deny 级别: `unwrap_used`, `expect_used`, `wildcard_imports`
- **错误处理**: 必须用 `?` 或 `match`，禁止 `.unwrap()` / `.expect()`（编译失败）
- **导入**: 禁止 `use foo::*`，必须显式导入每个符号
- **错误类型**: 使用 `thiserror` 派生 `DownloadError` 枚举
- **异步**: 始终用 async 非阻塞；同步阻塞操作用 `tokio::task::spawn_blocking`
- **命名**: snake_case 函数/变量，PascalCase 类型，SCREAMING_SNAKE_CASE 常量
- **日志**: `rinf::debug_print!("[module] message, key=value")` 输出到 Dart 控制台
- **注释**: 公开 API 用 `///` 文档注释，内部用 `//`
- **Crate 名**: `hub` 不可更改（Rinf 硬编码依赖）
- **FTP**: 使用 `suppaftp` 同步 API + `spawn_blocking` + mpsc channel
- **BT**: 使用 `librqbit`，内含 `block_on`，必须在 `spawn_blocking` 中调用
- **重试**: 指数退避，MAX_RETRIES=3, base=2s, `2^attempt` 倍增
- **Panic 恢复**: `AssertUnwindSafe` + `catch_unwind()` 捕获 task panic

### Dart/Flutter 端

- **SDK**: `^3.10.8`，Lint: `flutter_lints ^6.0.0`
- **UI 框架**: 全程使用 **shadcn_ui ^0.45.2**，禁止原生 Material/Cupertino 组件
- **字体**: MiSans 自定义字体族
- **统一导入**: `import 'package:shadcn_ui/shadcn_ui.dart';`（含 LucideIcons、flutter_animate）
- **导入顺序**: dart: → package:flutter/ → 第三方包（字母序）→ 相对导入
- **根组件**: 使用 `ShadApp`（或 `ShadTheme` + `WidgetsApp`），禁止 `MaterialApp`
- **主题访问**: `ShadTheme.of(context)`，禁止 `Theme.of(context)`
- **对话框**: `showShadDialog()`，禁止 `showDialog()`
- **图标**: `LucideIcons.xxx`
- **颜色**: 通过 `AppColors.of(context)` 获取主题感知色板
- **状态管理**: ChangeNotifier + ListenableBuilder，`_safeNotifyListeners()` 防已释放调用
- **模型**: 不可变数据类 + `copyWith()` 模式，枚举扩展 getter
- **命名**: PascalCase 类/枚举，camelCase 函数/变量，`_` 前缀私有成员，snake_case.dart 文件名
- **日志**: `const _tag = 'ModuleName';` 用于日志标签

### 浏览器扩展（fluxDown/）

- **框架**: WXT 0.20+，TypeScript（strict: true, target: ESNext）
- **通信方式**: Native Messaging Host（NMH）协议，stdin/stdout 与 IPC 通信（Windows Named Pipe / Linux Unix socket）
- **存储**: chrome.storage.sync（设置）+ chrome.storage.local（统计/主题）

## 日志系统

Dart 和 Rust 两端写入同一目录、同一日志文件，统一格式。

| 项目 | 说明 |
|------|------|
| 目录 | Linux `~/.local/share/fluxdown/logs/`，Windows exe 同级 `logs/` |
| 文件名 | `fluxdown_YYYY-MM-DD.log`（按日期自动分割），单文件超 2MB 分卷为 `fluxdown_YYYY-MM-DD.N.log` |
| 清理 | 两端启动时各自清理 7 天前的 `fluxdown_*.log`；总大小超上限（设置项 `log_max_size_mb`，默认 10MB）时按（日期, 分卷）从最旧删除 |
| 格式 | `HH:MM:SS.mmm [Tag] message` |

### Dart 端用法

```dart
import '../services/log_service.dart';

const _tag = 'MyModule';

logInfo(_tag, 'something happened: $value');
logError(_tag, 'failed', error, stackTrace);
```

### Rust 端用法

```rust
use crate::logger::log_info;

log_info!("[my-module] something happened: {}", value);
log_error!("[my-module] failed: {}", e);  // 立即刷盘
```

> **注意**: Rust 2024 edition 不支持 `#[macro_use]`，每个文件需显式 `use crate::logger::log_info;`。

### 日志导出

设置页「关于」分类中有导出按钮，打包为 ZIP 压缩包（纯 Dart 标准库实现，零外部依赖）：

```dart
// 导出为 zip（返回打包的文件数量）
final count = await LogService.instance.exportLogs('/path/to/fluxdown_logs.zip');
final sizeBytes = LogService.instance.logDirSizeBytes;
final fileCount = LogService.instance.logFileCount;
```
## 强制规则
- 禁止新增 dependency，需要时先在 PR/对话里说明理由并等确认
- 禁止 `unsafe`，除非显式批准
- 禁止 `unwrap()` / `expect()` 在非测试代码中出现；用 `?` + `thiserror`
- 公开 API 必须有 doc comment + 至少一个 doctest 或 example
- 改动前先跑 `cargo check -p <crate>`（不是整个 workspace）
- 提交前必须通过：`cargo fmt --check && cargo clippy -- -D warnings`
- 验证编译时优先 `cargo check -p <crate> --lib`
- 跑测试时优先 `cargo nextest run -p <crate> <filter>`，不要 `cargo test --workspace`
- 使用cargo管理依赖，禁止直接编辑`Cargo.toml`进行版本管理
- 禁止估算任务工作时间，不能因为时长而去过度分割工作
- 测试 provider 兼容性时调用 `provider-contract-test` skill

## 代码风格
- 优先复用项目已有的 trait / error 类型，不要平行造轮子
- 单文件超过 600 行考虑拆分；单函数超过 80 行需要说明

## 查文档优先级
1. `cargo path <crate>` 看本地源码（最权威）
2. `cargo doc --open` 或 docs.rs
3. 最后才是 web 搜索

## Rust 编码触发规则
写或改 `.rs` 文件前，先判断本次改动是否涉及以下任一项：
- 新增/修改 public API、trait、error 类型
- 写 unsafe / FFI / 性能关键路径
- 新增 crate 或调整 workspace 结构
- 写文档注释（doc comment）

若**命中任一项**,必须先读 `rust-router` skill
若仅是改变量名、调格式、加日志等局部改动，可跳过。

## 关键开发流程

### 添加新的 Dart ↔ Rust 信号
1. 在 `native/hub/src/signals/mod.rs` 定义结构体（标注 `DartSignal`/`RustSignal`/`SignalPiece`）
2. 运行 `rinf gen` 生成 Dart 绑定
3. Rust 端在 `download_actor.rs` 的 `tokio::select!` 中添加监听分支
4. Dart 端通过 `XxxSignal.rustSignalStream` 监听或 `.sendSignalToRust()` 发送

### 添加新页面/功能
1. 在 `lib/src/` 对应目录创建文件（pages/widgets/models/services）
2. 状态管理用 ChangeNotifier，通过 ListenableBuilder 绑定 UI
3. 使用 shadcn_ui 组件，颜色通过 `AppColors.of(context)` 获取

### Rust 模块开发
- 下载协议/引擎逻辑（HTTP/FTP/BT/HLS/DASH/分段协调/DB/…）新模块加入 `native/engine`（`fluxdown_engine` crate），在 `native/engine/src/lib.rs` 中声明 `pub mod xxx;`
- 参考 `downloader.rs`（HTTP）和 `ftp_downloader.rs`（FTP）的对称设计模式
- 需要上报事件给宿主 → 用 `EventSink::emit`（不要引入新的 rinf/Dart 依赖）；需要宿主介入决策（如弹窗选择）→ 用 `HostSelection`
- DB 操作统一通过 `native/engine/src/db.rs` 的 `Db` 结构体（sqlx 原生 async，占位符一律 `$N`，两后端共用同一份 SQL；仅 DDL 与 `wal_checkpoint` 按后端分支）
- App-shell 专属逻辑（文件关联/协议注册/NMH/更新器/…）留在 `native/hub`，在 `native/hub/src/lib.rs` 中声明 `mod xxx;`

### 发布新版本
1. 创建并推送 annotated tag：`git tag -a v0.x.x -m "v0.x.x" && git push origin v0.x.x`
2. GitHub Actions（`.github/workflows/release.yml`）自动构建全平台产物，git-cliff 从 Conventional Commits 生成 Release Notes
3. Release Notes 经 Claude 翻译为中英双语（`<!-- fluxdown:lang:zh -->` / `<!-- fluxdown:lang:en -->` 标记分块），官网 changelog 页按站点语言展示对应区块；翻译失败时回退原始 cliff 输出
