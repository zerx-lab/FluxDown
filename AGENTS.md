# FluxDown — 项目知识库

**应用名称**: FluxDown（类迅雷多协议下载工具）
**技术栈**: Flutter (GUI) + Rust (下载引擎) + WXT 浏览器扩展
**FFI 框架**: [Rinf 8.9](https://rinf.cunarist.org)（Dart↔Rust 信号通信，bincode 序列化）

## 命令速查

```bash
# 开发运行
flutter run -d windows              # 运行桌面应用,禁止运行这个命令
rinf gen                             # 修改 Rust 信号后必须执行，生成 Dart 绑定

# 构建与检查
cargo build                          # 构建 Rust 后端
cargo clippy                         # Rust lint（deny 级别，见下方规则）
flutter analyze                      # Dart 静态分析
flutter build windows                # 构建 Windows 发行版

# 测试
flutter test                         # 全部 Dart 测试
flutter test test/widget_test.dart   # 运行单个测试文件
cargo test -p hub                    # 运行 Rust 单元测试（segment_advisor 模块有测试）
cargo test -p hub -- segment_advisor # 运行特定 Rust 测试模块

# 依赖
flutter pub get                      # Dart 依赖安装
cargo install rinf_cli               # Rinf CLI（首次安装）

# 浏览器扩展（fluxDown/ 目录下）
npm run dev                          # 开发模式（Chrome）
npm run build                        # 构建生产版
```

## 项目结构

```
x_down/
├── lib/                               # Flutter 前端
│   ├── main.dart                      # 应用入口（多窗口分发、初始化流程）
│   └── src/
│       ├── models/                    # 数据模型与状态管理
│       │   ├── download_task.dart     # DownloadTask/TaskStatus/FileCategory/SegmentData
│       │   ├── download_controller.dart # 核心状态枢纽（ChangeNotifier）
│       │   └── settings_provider.dart # 全局配置状态
│       ├── pages/                     # 页面
│       │   ├── home_page.dart         # 主页面布局（Sidebar+Header+TaskList+DetailPanel）
│       │   └── settings_page.dart     # 设置页面
│       ├── services/                  # 服务层
│       │   ├── external_download_service.dart  # 浏览器扩展下载请求处理
│       │   └── tray_service.dart      # 系统托盘
│       ├── theme/                     # 主题系统
│       │   ├── app_theme.dart         # ShadThemeData 构建/缓存
│       │   ├── app_colors.dart        # 主题感知色板 AppColors.of(context)
│       │   └── theme_provider.dart    # 主题模式/配色持久化
│       ├── widgets/                   # UI 组件
│       │   ├── header_bar.dart        # 顶部工具栏 + 窗口控制
│       │   ├── sidebar.dart           # 左侧文件类型导航
│       │   ├── task_tab_bar.dart      # 状态筛选 Tab
│       │   ├── task_list.dart         # 任务列表容器
│       │   ├── task_list_item.dart    # 单个任务行 + 右键菜单
│       │   ├── detail_panel.dart      # 详情面板 + IDM 分片可视化（CustomPainter）
│       │   ├── new_download_dialog.dart  # 新建下载对话框
│       │   ├── context_menu.dart      # 通用 Overlay 右键菜单
│       │   ├── status_bar.dart        # 底部状态栏
│       │   └── title_drag_area.dart   # 自定义标题栏拖拽区域
│       ├── windows/
│       │   └── quick_download_window.dart  # 浏览器扩展快速下载确认子窗口
│       └── bindings/                  # ⚠️ 自动生成 — 勿手动编辑
├── native/hub/                        # Rust 下载引擎 crate
│   └── src/
│       ├── lib.rs                     # 入口（tokio current_thread runtime）
│       ├── signals/mod.rs             # 信号结构体定义（DartSignal/RustSignal）
│       ├── actors/
│       │   ├── mod.rs                 # create_actors() 入口
│       │   └── download_actor.rs      # 核心事件循环（tokio::select!）
│       ├── download_manager.rs        # 并发管理/任务生命周期/进度报告
│       ├── downloader.rs              # HTTP/HTTPS 下载引擎（分片/断点续传）
│       ├── ftp_downloader.rs          # FTP 下载引擎（suppaftp 同步 API）
│       ├── db.rs                      # SQLite 数据层（tasks/task_segments/config 三表）
│       ├── speed_limiter.rs           # Token bucket 全局速度限制器
│       ├── segment_advisor.rs         # 动态分段计算（文件大小+CPU+带宽）
│       └── native_messaging.rs        # 本地 HTTP 服务器（localhost:19527）
├── fluxDown/                          # WXT 浏览器扩展（Chrome MV3）
│   ├── entrypoints/
│   │   ├── background.ts             # Service Worker（下载拦截/右键菜单）
│   │   └── popup/                     # Popup UI（状态/设置/统计）
│   └── utils/
│       ├── native-messaging.ts        # HTTP 通信（fetch → localhost:19527）
│       └── settings.ts               # 扩展设置管理（拦截模式/扩展名/域名）
├── Cargo.toml                         # Rust workspace（resolver = "3"）
└── pubspec.yaml                       # Flutter 依赖
```

## 架构概览

```
[Dart UI (shadcn_ui)] ←Rinf FFI→ [download_actor (tokio::select! 事件循环)]
                                          │
                          ┌───────────────┼──────────────────┐
                    [DownloadManager]    [Db]          [native_messaging]
                     │          │      (SQLite)       (HTTP :19527)
               [downloader]  [ftp_downloader]              ↑
               (HTTP/HTTPS)     (FTP)              [WXT 浏览器扩展]
                     │
            [SpeedLimiter] + [segment_advisor]
```

**信号协议**: `DartSignal`(Dart→Rust), `RustSignal`(Rust→Dart), `SignalPiece`(嵌套类型)
**状态管理**: ChangeNotifier（DownloadController / SettingsProvider / ThemeProvider）
**并发模型**: 每个下载 spawn 独立 tokio task，CancellationToken 控制生命周期

## 代码风格与规范

### Rust 端

- **Edition**: 2024，Clippy deny 级别: `unwrap_used`, `expect_used`, `wildcard_imports`
- **错误处理**: 必须用 `?` 或 `match`，禁止 `.unwrap()` / `.expect()`（编译失败）
- **导入**: 禁止 `use foo::*`，必须显式导入每个符号
- **异步**: 始终用 async 非阻塞；同步阻塞操作用 `tokio::task::spawn_blocking`
- **错误类型**: 使用 `thiserror` 派生 `DownloadError` 枚举
- **命名**: snake_case 函数/变量，PascalCase 类型，SCREAMING_SNAKE_CASE 常量
- **Crate 名**: `hub` 不可更改（Rinf 硬编码依赖）
- **FTP**: 使用 `suppaftp` 同步 API + `spawn_blocking` + mpsc channel（因异步 FTP 与 tokio 冲突）

### Dart/Flutter 端

- **Lint**: `flutter_lints` 推荐规则集（analysis_options.yaml）
- **UI 框架**: 全程使用 **shadcn_ui**，禁止原生 Material/Cupertino 组件
- **字体**: MiSans 自定义字体族
- **统一导入**: `import 'package:shadcn_ui/shadcn_ui.dart';`（含 LucideIcons、flutter_animate）
- **根组件**: 使用 `ShadApp`（或手动组合 `ShadTheme` + `WidgetsApp`），禁止 `MaterialApp`
- **主题访问**: `ShadTheme.of(context)`，禁止 `Theme.of(context)`
- **对话框**: `showShadDialog()`，禁止 `showDialog()`
- **图标**: `LucideIcons.xxx`
- **颜色**: 通过 `AppColors.of(context)` 获取主题感知色板
- **配色方案**: Slate/Zinc/Blue/Gray/Green/Neutral/Orange/Red/Rose/Stone/Violet/Yellow
- **状态管理**: ChangeNotifier + ListenableBuilder，无 Provider/Riverpod/Bloc
- **文件命名**: snake_case.dart

### 浏览器扩展（fluxDown/）

- **框架**: WXT 0.20+，TypeScript
- **通信方式**: HTTP fetch → localhost:19527（非 Native Messaging 协议）
- **存储**: chrome.storage.sync（设置）+ chrome.storage.local（统计/主题）

## 禁止事项（Anti-Patterns）

| 禁止 | 原因 |
|------|------|
| 编辑 `lib/src/bindings/**` | 自动生成，`rinf gen` 会覆盖 |
| Rust `.unwrap()` / `.expect()` | Clippy deny，编译失败 |
| Rust `use foo::*` | Clippy deny，编译失败 |
| 改 crate name `hub` | Rinf 框架硬编码此名称 |
| async 中阻塞 I/O | tokio current_thread runtime 会死锁 |
| `MaterialApp` / `showDialog()` / `Theme.of()` | 全程 shadcn_ui 体系 |
| Material/Cupertino 原生组件 | 统一使用 shadcn_ui 组件 |

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
- 参考 `downloader.rs`（HTTP）和 `ftp_downloader.rs`（FTP）的对称设计模式
- 新模块在 `lib.rs` 中声明 `mod xxx;`
- DB 操作统一通过 `db.rs` 的 `Db` 结构体，所有 rusqlite 调用在 `spawn_blocking` 中
