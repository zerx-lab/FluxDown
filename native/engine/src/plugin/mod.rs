//! 插件系统 —— 可选、可失败的下载任务中间层。
//!
//! 插件能力拆成两个正交平面：
//! 1. **Resolver 平面**：`resolve(url, ctx) -> Option<ResolveResult>`，惰性执行于每次
//!    实际发起下载协议判定之前、且 off-actor（见 [`crate::download_manager`] 的
//!    off-actor resolve 插桩）；命中后失败 fail-closed（进 status=4，绝不静默把网页
//!    HTML 当视频保存），并暴露「忽略插件重试」逃生舱。
//! 2. **通知平面**：onStart/onDone/onError/onMetaProbed **全部 fire-and-forget**
//!    （失败仅记日志、超时、`try_acquire` 不阻塞，绝不影响任务状态）。
//!
//! 本模块仅在 `plugins` feature 开启时编译（desktop/server），mobile 关闭以免背 JS
//! 引擎债。`DownloadManager` 对 `plugin_manager` 字段注入一个 no-op `PluginManager`
//! 使得下载主链路在 feature 关时零行为变化。
//!
//! # 模块职责
//! - [`manifest`]：`PluginManifest`/`SettingField` + 手写校验器 + `url_glob_match`。
//! - [`semver`]：engine-local `parse_semver`/`satisfies_min`（复刻 hub updater 语义）。
//! - [`runtime`]：抽象层 —— `ScriptRuntime`/`PluginBridge` trait + 跨 JS 边界结构体，
//!   本文件禁止出现任何 rquickjs 类型，未来可换 deno_core。
//! - [`quickjs`]：v1 唯一实现 `QuickJsScriptRuntime`，rquickjs 类型仅存在于本文件。
//! - [`bridge`]：`EngineBridge` 实现 `PluginBridge`（网络出口守卫 + flux.* API）。
//! - [`manager`]：`PluginManager`（Arc 共享，插件装载/启停/resolve/notify）。

pub mod bridge;
pub mod dependencies;
pub mod install;
pub mod manager;
pub mod manifest;
pub mod market;
pub mod quickjs;
pub mod runtime;
pub mod semver;
pub use manager::{DisabledReason, LoadedPlugin, PluginInfo, PluginManager};
pub use manifest::{PluginManifest, SettingField, SettingType, SettingWidget};
pub use market::{MarketClient, MarketEntry, MarketError, MarketIndex};
pub use runtime::{
    ExecutionBudget, ManifestItem, ManifestVariant, PluginBridge, PluginError, PluginEvent,
    PluginLogLevel, ResolveManifest, ResolveRequest, ResolveResult, ResolveVariant, ScriptRuntime,
};
