//! 插件系统 —— 可选、可失败的下载任务中间层。
//!
//! 插件能力拆成两个正交平面：
//! 1. **Resolver 平面**：`resolve(url, ctx) -> Option<ResolveResult>`，惰性执行于每次
//!    实际发起下载协议判定之前、且 off-actor（见 [`crate::download_manager`] 的
//!    off-actor resolve 插桩）；命中后失败 fail-closed（进 status=4，绝不静默把网页
//!    HTML 当视频保存），并暴露「忽略插件重试」逃生舱。
//! 2. **通知平面**：onStart/onDone/onError/onMetaProbed/onCancel **全部 fire-and-forget**
//!    （失败仅记日志、超时、`try_acquire` 不阻塞，绝不影响任务状态）。
//! 3. **订阅平面**：manifest 声明 `subscriptions` 后，宿主调用插件的
//!    `globalThis.subscribe(ctx)`，返回值进入公共订阅调度；插件不直接碰订阅表或建任务。
//!
//! 本模块仅在 `plugins` feature 开启时编译（desktop/server），mobile 关闭以免背 JS
//! 引擎债。`DownloadManager` 对 `plugin_manager` 字段注入一个 no-op `PluginManager`
//! 使得下载主链路在 feature 关时零行为变化。
//!
//! # 模块职责
//! - [`manifest`]：`PluginManifest`/`SettingField` + 手写校验器 + `url_glob_match`。
//! - [`semver`]：engine-local `parse_semver`/`satisfies_min`（三段版本比较）。
//! - [`runtime`]：抽象层 —— `ScriptRuntime`/`PluginBridge` trait + 跨 JS 边界结构体，
//!   本文件禁止出现任何 rquickjs 类型，未来可换 deno_core。
//! - [`quickjs`]：v1 唯一的短调用实现 `QuickJsScriptRuntime`，rquickjs 类型仅存在于本文件与
//!   [`quickjs_host`]。
//! - [`quickjs_host`]：常驻宿主（`main`）的 QuickJS actor。
//! - [`manager`]（含 `host_manager` 子模块）：宿主侧——会话、激活 / 回收、渲染节流、命令与 RPC。
//! - [`bridge`]：`EngineBridge` 实现 `PluginBridge`（网络出口守卫 + flux.* API）。
//! - [`call_egress`]：每次插件调用的出口计划与网络观测（站点规则 / 自带代理 / 全局模式 /
//!   Auto 换路），解析与下载同出口的源头。
//! - [`manager`]：`PluginManager`（Arc 共享，插件装载/启停/resolve/notify）。

mod archive_rules;
pub mod audit;
pub mod bridge;
pub mod call_egress;
pub mod consent;
#[cfg(test)]
mod consent_tests;
pub mod dependencies;
pub mod install;
pub mod manager;
pub mod manifest;
pub mod market;
mod package_revision;
pub mod page_scripts;
pub mod quickjs;
pub mod quickjs_host;
pub mod runtime;
pub mod semver;
pub mod sniff_pack;
mod tool_path;
pub use manager::routing;
pub use manager::{DisabledReason, LoadedPlugin, PluginInfo, PluginLoadStatus, PluginManager};
pub use manifest::{PluginManifest, SettingField, SettingType, SettingWidget, SubscriptionDecl};
pub use market::{MarketClient, MarketEntry, MarketError, MarketIndex};
pub use runtime::{
    AuthRequest, AuthResult, CancelReason, ExecutionBudget, HostCall, HostEvents, HostFuture,
    HostReply, ManifestItem, ManifestVariant, PluginBridge, PluginError, PluginEvent, PluginHost,
    PluginLogLevel, PluginTaskPort, ResolveManifest, ResolveRequest, ResolveResult, ResolveVariant,
    ScriptRuntime, StartedTasks, SubscriptionRequest,
};
