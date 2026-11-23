//! `fluxdown-agent` 的一次性系统打开模式：桌面界面进程在用户点击时拉起一个短命 agent，
//! 由它执行与常驻 agent 相同的打开逻辑（`agent.platform.openTask` / `revealTask` 同一实现），
//! 结果经退出码回报后立即退出。
//!
//! Windows 的前台授权受调用进程及其启动者影响；由用户正在操作的界面拉起短命 agent，
//! 避免常驻后台 agent 发起打开导致窗口落在后面。最终前台状态仍由系统焦点策略决定。
//!
//! 参数（不含 argv[0]）固定为「标志 + 位置参数」，标志必须是第一个参数：
//! - [`OPEN_TASK_ARG`] `<保存目录> <文件名>`：用系统默认程序打开任务产物；
//! - [`REVEAL_TASK_ARG`] `<保存目录> <文件名>`：在文件管理器中定位任务（文件名可为空）；
//! - [`OPEN_DIR_ARG`] `<目录>`：用系统默认的目录处理程序打开已存在的目录；
//! - [`INSTALL_BROWSER_EXTENSION_ARG`] `<chrome|edge|firefox>`：为该浏览器安装 FluxDown 扩展
//!   （`agent.platform.installBrowserExtension` 同一实现）；成功时退出码区分是否登记了外部扩展。

pub const OPEN_TASK_ARG: &str = "--open-task";
pub const REVEAL_TASK_ARG: &str = "--reveal-task";
pub const OPEN_DIR_ARG: &str = "--open-dir";
pub const INSTALL_BROWSER_EXTENSION_ARG: &str = "--install-browser-extension";

/// 已交给系统打开。
pub const EXIT_OK: i32 = 0;
/// 安装浏览器扩展：只打开了商店 / 安装页（本平台不支持登记外部扩展）。
pub const EXIT_OK_STORE_ONLY: i32 = 10;
/// 系统调用失败。
pub const EXIT_FAILED: i32 = 1;
/// 参数不合法（不会落入常驻启动）。
pub const EXIT_USAGE: i32 = 2;
/// 目标（任务产物 / 保存目录 / 目录）不在磁盘上。
pub const EXIT_NOT_FOUND: i32 = 3;
/// 当前平台不支持由 agent 打开路径。
pub const EXIT_UNSUPPORTED: i32 = 4;
