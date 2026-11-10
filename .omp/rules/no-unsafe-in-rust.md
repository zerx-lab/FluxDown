---
description: Rust 默认安全；仅允许经审查、无语义等价安全替代的平台 FFI 边界
condition:
  - '\bunsafe\s*\{'
  - '\bunsafe\s+(fn|impl|trait|extern)\b'
  - '\bunsafe\s*\('
globs:
  - native/**/*.rs
  - crates/**/*.rs
  - scripts/**/*.rs
repeatMode: after-gap
repeatGap: 2
---

项目维护的 Rust 源码（含测试、构建脚本与开发工具）默认安全。`unsafe` 块、函数、impl、extern 和属性都必须审查；文件出现在下文不等于整文件豁免。

## 先排除语义等价的安全实现

1. 优先 std：`std::env::home_dir`、文件元数据、Windows `MetadataExt::file_attributes`、`CStr::from_bytes_until_nul`、`CommandExt::creation_flags` 等。本地拥有的字节缓冲不得再用裸指针重建切片或字符串。
2. 复用当前 crate **已有直接依赖及启用的 feature**：`directories`、`fs2`、`tokio::fs`、`objc2` 的安全 downcast、`windows` 的类型化 COM RAII。传递依赖不是直接依赖；新增依赖仍须用户确认，不为去 unsafe 私自扩充依赖或改变 headless feature 边界。
3. 安全替代必须保留实际语义：物理分配与 `set_len` 不等价；核对锁定依赖实现、错误码、符号链接、路径存在性、整数宽度、并发写与权限。不得为减少关键字引入错误或无谓分配、复制、系统调用。
4. 所有权、生命周期和线程归属优先由类型表达。禁止 `transmute` / `from_raw_parts`；纯 Rust 数据处理不能借 FFI 例外使用 unsafe。

### 已核实的 fs2 0.4.3 限制

- Linux `allocate` 使用 `posix_fallocate`，其错误码不能用 `last_os_error` 取得；glibc 仿真与当前只写句柄、并发扩容不等价。保留直接 `fallocate`，不改为写零仿真或全量 `set_len`。
- Apple 空间查询须保留 64 位 `statfs`；fs2 的 `statvfs` 使用 32 位可用块数，后置转 u64 不能修复大卷截断。其他 Unix 当前用饱和乘法，fs2 的普通乘法也不能无条件替换。
- Windows fs2 空间查询并非 `GetDiskFreeSpaceExW`，路径及容量语义不同；`allocate` 还混合分配查询、EOF 修改与错误，不可直接替换既有后备分配策略。依赖升级后重新核对，不把这些限制当永久豁免。

## 必要平台边界的准入条件

只有无语义等价安全替代的平台操作才允许，且全部满足：

- 用目标平台 `cfg` 门控；桌面特有依赖同时尊重 feature 边界。
- 每个 unsafe 块、unsafe impl 和 ABI 属性都有**紧邻的 `// SAFETY:` 注释块**；多行注释合法，必须解释实际前置条件，而非只写“FFI”。unsafe 函数另以 `# Safety` 文档声明调用契约；extern 声明说明 ABI 来源。
- 块内仅一项必要不安全操作；参数准备、分支和错误处理在块外。除调用外，仅允许同一边界必要的外部常量读取、有效回调指针短期借用、已成功初始化的 FFI 输出读取。它们各自需要生命周期/初始化证明，不能假称 `assume_init` 是安全 API。
- Objective-C 协议 impl/方法属性必须核对 selector、ABI、回调线程和完成回调恰好一次；不得借此新增 `unsafe impl Send/Sync`。
- 对外提供安全业务函数并消费失败；不得暴露可接管任意裸指针的 safe 构造器，也不得以 `pub unsafe fn` 把义务转嫁给调用者。私有边界辅助函数若确需 unsafe，必须声明完整契约并在调用处证明。
- 拥有引用与借用引用分开、字符串与数组等不同资源类型分开；借用不能逃逸所有者生命周期，不能从借用复制出第二个 owner。仅判空或改成 NonNull 不足以证明安全。
- 拥有的 CF/COM/GDI/PIDL 资源立即进入 RAII；线程受限守卫必须 !Send/!Sync，相关对象先于 apartment 释放。初始化失败必须停止依赖它的操作，不允许继续执行后再记录“不可用”。
- 所有返回状态都处理，包括未标记 must_use 的 C 整数/BOOL；按 API 契约区分直接错误码与 last_os_error。Drop 清理失败记录而不 panic，禁止把错误哨兵当作属性或尺寸。
- 复用现有绑定；手写 extern 只声明实际调用的符号。

## 已审查的边界坐标

|位置|必要语义；不是目录级自由授权|
|---|---|
|`native/engine/src/segment_coordinator.rs`|Linux fallocate / fadvise，Windows 非 sparse 后备物理分配；不能改变并发扩容或错误策略|
|`native/engine/src/disk_space.rs`|64 位/饱和空间计算与 Windows 目录语义|
|`native/engine/src/bt_sparse.rs`、`bt_downloader.rs`|置 sparse / HIDDEN 属性；读取属性用 std|
|`native/agent/src/platform/macos_cf.rs`|CF/LaunchServices 单一所有权边界；调用方不得操作裸 CF 引用|
|`native/agent/src/platform/`|系统图标、Shell 定位/关联变更、环境变量变更广播（`WM_SETTINGCHANGE`）、COM 生命周期；每项遵守上述约束|
|`native/agent/src/notification/`|macOS UserNotifications ABI 与 Windows WinRT apartment；已有安全接口优先|
|`native/agent/src/platform/elevation.rs`|Windows UAC 降权：`OpenProcessToken` + `GetTokenInformation(TokenElevationType)`（令牌句柄 RAII）只读本进程提权类型；经 `IShellWindows`→桌面视图→`IShellDispatch2::ShellExecute`（`windows` 类型化 COM，STA 守卫先于接口释放）以资源管理器令牌拉起本程序 / 同级桌面。只对拆分令牌的完整一半降权，新实例确认接手前旧进程不退出|
|`native/agent/src/platform/hotkey/`|全局快捷键：Windows `RegisterHotKey` + `WH_KEYBOARD_LL`（专用消息循环线程，钩子只观察不吞键、仅在有双击绑定时安装）；macOS Carbon `RegisterEventHotKey` + `NSEvent` 全局监听 + `AXIsProcessTrustedWithOptions`（主线程）。Linux X11 / Wayland 用 x11rb / ashpd 安全 API，零 unsafe|
|`native/agent/src/platform/app_icon/`|应用图标：Windows 跨进程 `WM_SETICON`（只认类名 `GpuiFast::Window` + 进程映像 = 同目录桌面程序，HICON 由 agent RAII 持有）、`.lnk` `IconLocation` 改写（`windows` 类型化 COM，目标 fail-closed）、`fluxdown-desktop.exe` 图标资源改写（`Begin/Update/EndUpdateResourceW` 只作用于同目录临时副本，句柄 RAII、失败丢弃；读资源用纯 Rust 有界 PE 解析，不碰 OS 映射；校验后 `MoveFileExW` 原子替换，从不改名运行中的 exe）；macOS `NSWorkspace.setIcon` 为安全 API；Linux X11 `_NET_WM_ICON` 用 x11rb 安全 API，零 unsafe|
|`crates/app/src/app_icon.rs`（**`crates/*` 唯一例外**，用户 2026-10-30 决定）|macOS `NSApplication::setApplicationIconImage(Some(..))` 单个调用（objc2 因参数可为 `None` 标为 `unsafe fn`；Cmd-Tab / 强制退出窗口只读它，Dock tile 安全 API 覆盖不到）；主线程由 `MainThreadMarker` 证明、参数恒为 `Some`。不得扩到其他调用|

`native/{api,protocol,daemon,server,mobile}`、`crates/*`（GPUI，上表 `crates/app/src/app_icon.rs` 单点例外除外）、`native/nmh`、`scripts/desktop-dev` 的项目源码保持零 unsafe；冻结 server 不改动。测试不自动豁免：先用安全 API，确需独立平台 oracle 才保留最小 FFI 并检查失败。

## 防止回归

- workspace deny `unsafe_op_in_unsafe_fn` 与 `clippy::undocumented_unsafe_blocks`；NMH 以 `forbid(unsafe_code)` 锁定安全实现。不得 allow/命令行关闭这些检查来消音。
- 源码扫描不覆盖第三方实现及宏展开。`native/mobile`（`fluxdown_mobile`）的 UniFFI 导出宏是独立审计边界：手写源码仍零 unsafe，宏生成的 FFI 胶水不计入“零 unsafe”声明；若 workspace lint 命中宏展开，只允许在该 crate 针对具体生成 lint 局部放行并注明边界，不得全局关闭。也不要另写第二套 ABI。
- 验证实际目标与 feature：macOS 编译通过不证明 Windows cfg 已编译；没有目标运行环境时明确报告限制。规则准入变更同步根 AGENTS.md、RULES.md 与 WATCHDOG.md，避免相互矛盾。
