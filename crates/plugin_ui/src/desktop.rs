//! 桌面脚本（gpui-shell）的加载、每插件 runtime 与 `fluxdown` 宿主模块。
//!
//! 两种承载方式：
//! - 独立窗口：[`load_desktop_view`] 只返回脚本视图，由 app 组装为
//!   `Root(ShellRoot::new(AuxiliaryWindowView(view)))`——gpui-shell 的 `ShellRoot::find`
//!   只认窗口根自身或 `Root` 的直接内层是 `ShellRoot`，脚本的对话框 / sheet / toast 才可用。
//! - 嵌入（活动栏页 / 设置 / 任务详情）：[`desktop_view`] 返回包住脚本视图的
//!   `DesktopViewHost`，**没有 `ShellRoot`**，脚本内的对话框 / sheet / toast 浮层 API 不可用，
//!   应改用 `fluxdown.toast`。

use std::{rc::Rc, sync::Arc};

use fluxdown_protocol::{NAV_BUILTIN_TARGETS, plugin_route_id};
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div,
};
use gpui_component::{WindowExt as _, alert::Alert, notification::Notification};
use gpui_shell::{
    HostError, HostModule, HostValue, ScriptView, ShellRuntime, policy::Policy, with_current_app,
};
use serde_json::{Map, Value, json};

use crate::{
    PluginScriptHost, PluginUiError, PluginUiPort, PluginWindowSpec, ViewContainer,
    audit::{self, AuditEntry},
    grant, host_value,
};

/// 一个插件的独立 runtime 及其存活视图数。
pub(crate) struct RuntimeEntry {
    runtime: Rc<ShellRuntime>,
    views: usize,
}

/// 加载独立窗口的桌面脚本视图。失败时 app 以 [`load_failure_view`] 代替。
pub fn load_desktop_view(
    spec: &PluginWindowSpec,
    container: ViewContainer,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<ScriptView>, PluginUiError> {
    load(spec, None, container, window, cx)
}

/// 嵌入式桌面脚本视图（活动栏页 / 设置 / 任务详情）。`task_id` 经 `fluxdown.taskId()`
/// 暴露给脚本。加载失败返回错误面板。
///
/// 没有 `ShellRoot`：脚本内的对话框 / sheet / toast 浮层 API 不可用，改用 `fluxdown.toast`。
pub fn desktop_view(
    spec: PluginWindowSpec,
    task_id: Option<String>,
    container: ViewContainer,
    window: &mut Window,
    cx: &mut App,
) -> AnyView {
    match load(&spec, task_id, container, window, cx) {
        Ok(view) => cx.new(|_| DesktopViewHost { view }).into(),
        Err(error) => load_failure_view(&error, cx),
    }
}

/// 承载加载失败信息的面板。
pub fn load_failure_view(error: &PluginUiError, cx: &mut App) -> AnyView {
    log::warn!("{error}");
    let title = cx
        .try_global::<PluginScriptHost>()
        .and_then(|host| host.translator.clone())
        .map(|translator| {
            translator
                .read(cx)
                .text("pluginWindowLoadFailed")
                .to_owned()
        })
        .unwrap_or_else(|| "Plugin interface failed to load".to_owned());
    cx.new(|_| LoadFailure {
        title,
        message: error.to_string(),
    })
    .into()
}

/// 包住脚本视图的嵌入式宿主实体；脚本视图的释放（审计 unload、runtime 计数）挂在视图实体上。
struct DesktopViewHost {
    view: Entity<ScriptView>,
}

impl Render for DesktopViewHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.view.clone())
    }
}

pub(crate) struct LoadFailure {
    pub(crate) title: String,
    pub(crate) message: String,
}

impl Render for LoadFailure {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(
            Alert::error("plugin-window-load-failure", self.message.clone())
                .title(self.title.clone()),
        )
    }
}

fn load(
    spec: &PluginWindowSpec,
    task_id: Option<String>,
    container: ViewContainer,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<ScriptView>, PluginUiError> {
    let result = load_inner(spec, task_id, container, window, cx);
    let summary = grant::summary(&spec.entry.network_hosts);
    let target = format!("{}/{}", spec.view_id, spec.entry.entry);
    match &result {
        Ok(_) => audit::record(
            cx,
            AuditEntry::new(&spec.identity, "desktop.load", target, "ok", summary),
        ),
        Err(error) => {
            release_if_unused(cx, &spec.identity);
            audit::record(
                cx,
                AuditEntry::new(
                    &spec.identity,
                    "desktop.load",
                    target,
                    "error",
                    format!("{summary}; {error}"),
                ),
            );
        }
    }
    result
}

fn load_inner(
    spec: &PluginWindowSpec,
    task_id: Option<String>,
    container: ViewContainer,
    window: &mut Window,
    cx: &mut App,
) -> Result<Entity<ScriptView>, PluginUiError> {
    let load_error = |message: String| PluginUiError::Load {
        identity: spec.identity.clone(),
        message,
    };
    let Some(host) = cx.try_global::<PluginScriptHost>() else {
        return Err(PluginUiError::Runtime(
            "plugin UI host is not initialized".to_owned(),
        ));
    };
    let port = host.port.clone();
    let data_dir = grant::data_dir(&host.data_root, &spec.identity);
    let runtime = runtime(cx, &spec.identity)?;
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        // 目录不可写只影响 localStorage 落盘，界面仍可运行。
        log::warn!(
            "plugin UI data dir unavailable for {}: {}: {error}",
            spec.identity,
            data_dir.display()
        );
    }
    let policy = Policy::new()
        .with_application(&spec.identity)
        .with_capabilities(grant::full_trust(&spec.entry.network_hosts))
        .with_data_dir(data_dir.clone())
        .with_storage_path(data_dir.join("store.json"))
        .with_host_module(fluxdown_module(&spec.identity, task_id, container, port))
        .map_err(|error| load_error(error.message().to_owned()))?;
    // gpui-shell 在加载 / 实例化时把「当前默认策略」冻结进视图：逐插件替换默认策略，
    // 授权与 HostModule 就随该视图走，后续加载的其他插件不受影响。
    gpui_shell::policy::set_default(policy);
    let application = runtime
        .load_application(&spec.package_dir, &spec.entry.entry)
        .map_err(|error| load_error(format!("{error:#}")))?;
    let view = runtime
        .mount_application(&application, window, cx)
        .map_err(|error| load_error(format!("{error:#}")))?;
    track(cx, &spec.identity, &view);
    Ok(view)
}

/// 记入存活视图表与 runtime 计数；视图释放时记审计、归还计数（归零即丢弃 runtime）。
fn track(cx: &mut App, identity: &str, view: &Entity<ScriptView>) {
    if let Some(host) = crate::host_mut(cx) {
        if let Some(entry) = host.runtimes.get_mut(identity) {
            entry.views += 1;
        }
        host.views.push(view.downgrade());
    }
    let identity = identity.to_owned();
    cx.observe_release(view, move |_, cx| {
        if let Some(host) = crate::host_mut(cx) {
            if let Some(entry) = host.runtimes.get_mut(&identity) {
                entry.views = entry.views.saturating_sub(1);
            }
            host.views.retain(|view| view.upgrade().is_some());
        }
        release_if_unused(cx, &identity);
        audit::record(
            cx,
            AuditEntry::new(&identity, "desktop.unload", "", "ok", ""),
        );
    })
    .detach();
}

/// 取（必要时创建）该插件独立的 gpui-shell runtime。
fn runtime(cx: &mut App, identity: &str) -> Result<Rc<ShellRuntime>, PluginUiError> {
    let host = cx.global::<PluginScriptHost>();
    if let Some(entry) = host.runtimes.get(identity) {
        return Ok(entry.runtime.clone());
    }
    let components = host.components.clone();
    let runtime = ShellRuntime::new_isolated_with_components(components)
        .map_err(|error| PluginUiError::Runtime(format!("{error:#}")))?;
    // 插件包是已安装产物，不是被编辑的源码：不往包目录写 `gpui-kit.d.ts` / `jsconfig.json`。
    runtime.set_editor_files(false);
    if let Some(host) = crate::host_mut(cx) {
        host.runtimes.insert(
            identity.to_owned(),
            RuntimeEntry {
                runtime: runtime.clone(),
                views: 0,
            },
        );
    }
    Ok(runtime)
}

/// 该插件没有存活视图时丢弃它的 runtime。
fn release_if_unused(cx: &mut App, identity: &str) {
    if let Some(host) = crate::host_mut(cx)
        && host
            .runtimes
            .get(identity)
            .is_some_and(|entry| entry.views == 0)
    {
        host.runtimes.remove(identity);
    }
}

/// 脚本侧审计记录（在脚本调用作用域内取当前 `App`）。
fn audit_script(
    identity: &str,
    action: &'static str,
    target: &str,
    outcome: &'static str,
    detail: &str,
) {
    let entry = AuditEntry::new(identity, action, target, outcome, detail);
    if with_current_app(|cx| audit::record(cx, entry)).is_none() {
        log::debug!("plugin audit skipped outside script scope: {identity} {action}");
    }
}

const FLUXDOWN_DECLARATIONS: &str = "\
/** 当前插件的 identity（`author@name`）。 */
export function identity(): string;
/** 打开该视图时传入的任务 id（任务详情 tab）；其他位置为 `null`。 */
export function taskId(): string | null;
/** 调用 agent `/rpc`：只放行 `daemon.*` / `agent.*`（其余在调用处同步抛错）；RPC 失败以错误详情（JSON）reject。 */
export function call(method: string, params?: unknown): Promise<unknown>;
/** 调用本插件后台 `main` 里 `flux.rpc.handle(name, …)` 注册的处理器（经 `daemon.plugin.invoke`）。 */
export function invoke(name: string, args?: unknown): Promise<unknown>;
/** FluxDown 当前完整已解析主题：`tokens` 为 registry 路径 → 颜色（`#rrggbbaa`）/ 数值 / 字符串 / 阴影数组；主题变化时 `revision` 递增且视图会被重绘。 */
export function theme(): { revision: number; tokens: Record<string, string | number | unknown[]> };
/** 当前界面语言（如 `zh-CN`、`en`）。 */
export function locale(): string;
/** 在当前活动窗口弹出一条通知（嵌入式视图没有 `ShellRoot`，请用它代替 `window.push_toast`）。 */
export function toast(level: \"info\" | \"success\" | \"warning\" | \"error\", text: string): void;
/** 关闭承载本视图的容器（登录弹窗、设置弹窗或独立窗口）并返回 `true`；活动栏页 / 任务详情 tab 等无法关闭的嵌入页返回 `false`。关闭在当前事件处理结束后执行。 */
export function close(): boolean;
/** 切到 FluxDown 主界面的某个页面并把主窗口带到前台：`\"downloads\" | \"rss\" | \"webhooks\"` 为内置页，`{ view }` 为本插件 `location` 为 `activity` 的视图 id。目标形状非法或页面不存在时同步抛错。 */
export function navigate(target: \"downloads\" | \"rss\" | \"webhooks\" | { view: string }): void;";

/// 与原生移动端通用通道同规则（`native/mobile` `HostSession.call`）。
pub(crate) fn is_allowed_method(method: &str) -> bool {
    method.starts_with("daemon.") || method.starts_with("agent.")
}

/// 插件可 import 的 `fluxdown` 模块（每个视图一份，携带其 identity 与任务 id）。
pub(crate) fn fluxdown_module(
    identity: &str,
    task_id: Option<String>,
    container: ViewContainer,
    port: Arc<dyn PluginUiPort>,
) -> HostModule {
    let id_identity = identity.to_owned();
    let id_call = identity.to_owned();
    let id_invoke = identity.to_owned();
    let id_navigate = identity.to_owned();
    let invoke_port = port.clone();
    HostModule::new("fluxdown")
        .declarations(FLUXDOWN_DECLARATIONS)
        .function("identity", move |_| {
            Ok(HostValue::from(id_identity.clone()))
        })
        .function("taskId", move |_| {
            Ok(task_id.clone().map_or(HostValue::Null, HostValue::from))
        })
        .async_function("call", move |arguments| {
            let method = arguments.string(0)?.to_owned();
            if !is_allowed_method(&method) {
                audit_script(&id_call, "desktop.call", &method, "denied", "");
                return Err(HostError::new(format!(
                    "fluxdown.call: `{method}` is not allowed; only daemon.* and agent.* methods"
                )));
            }
            audit_script(&id_call, "desktop.call", &method, "ok", "");
            let params = arguments.get(1).map_or(Value::Null, host_value::host_to_json);
            Ok(rpc_future(port.call(method, params)))
        })
        .async_function("invoke", move |arguments| {
            let name = arguments.string(0)?.to_owned();
            audit_script(&id_invoke, "desktop.invoke", &name, "ok", "");
            let args = arguments.get(1).map_or(Value::Null, host_value::host_to_json);
            let params = json!({ "identity": id_invoke, "name": name, "args": args });
            Ok(rpc_future(
                invoke_port.call("daemon.plugin.invoke".to_owned(), params),
            ))
        })
        .function("theme", |_| {
            let snapshot = with_current_app(|cx| {
                if !cx.has_global::<fluxdown_ui_theme::FluxThemeState>() {
                    return None;
                }
                let revision = cx
                    .try_global::<PluginScriptHost>()
                    .map_or(0, |host| host.theme_revision);
                let tokens: Map<String, Value> =
                    fluxdown_ui_theme::resolved_token_map(cx).into_iter().collect();
                Some((revision, tokens))
            })
            .flatten();
            let Some((revision, tokens)) = snapshot else {
                return Err(HostError::new("fluxdown.theme: theme is not available"));
            };
            Ok(HostValue::Object(vec![
                ("revision".to_owned(), HostValue::Number(f64::from(revision))),
                (
                    "tokens".to_owned(),
                    host_value::json_to_host(Value::Object(tokens)),
                ),
            ]))
        })
        .function("locale", |_| {
            let locale = with_current_app(|cx| {
                cx.try_global::<PluginScriptHost>()
                    .and_then(|host| host.translator.clone())
                    .map(|translator| translator.read(cx).locale().to_owned())
            })
            .flatten()
            .unwrap_or_else(fluxdown_ui_i18n::system_locale);
            Ok(HostValue::from(locale))
        })
        .function("toast", |arguments| {
            let level = arguments.string(0)?.to_owned();
            let text = arguments.string(1)?.to_owned();
            let note = match level.as_str() {
                "info" => Notification::info(text),
                "success" => Notification::success(text),
                "warning" => Notification::warning(text),
                "error" => Notification::error(text),
                other => {
                    return Err(HostError::new(format!(
                        "fluxdown.toast: unknown level `{other}`; expected info, success, warning or error"
                    )));
                }
            };
            let queued = with_current_app(|cx| {
                cx.defer(move |cx| {
                    let Some(handle) = cx.active_window() else {
                        return;
                    };
                    if let Err(error) = handle.update(cx, |_, window, cx| {
                        window.push_notification(note, cx);
                    }) {
                        log::warn!("plugin toast dropped: {error}");
                    }
                });
            });
            if queued.is_none() {
                return Err(HostError::new("fluxdown.toast: no active application scope"));
            }
            Ok(HostValue::Null)
        })
        .function("close", move |_| {
            let closed = with_current_app(|cx| container.close(cx));
            closed
                .map(HostValue::Bool)
                .ok_or_else(|| HostError::new("fluxdown.close: no active application scope"))
        })
        .function("navigate", move |arguments| {
            let route = navigation_route(&id_navigate, arguments.get(0))?;
            match with_current_app(|cx| crate::navigate(cx, &id_navigate, &route)) {
                Some(Ok(())) => Ok(HostValue::Null),
                Some(Err(message)) => Err(HostError::new(format!("fluxdown.navigate: {message}"))),
                None => Err(HostError::new(
                    "fluxdown.navigate: no active application scope",
                )),
            }
        })
}

async fn rpc_future(call: crate::PortFuture<Value>) -> Result<HostValue, HostError> {
    call.await.map(host_value::json_to_host).map_err(|error| {
        HostError::new(
            serde_json::to_string(&error).unwrap_or_else(|_| format!("{:?}", error.code)),
        )
    })
}

/// `fluxdown.navigate(target)` 的 target → 路由 id：内置页 id 原样，`{ view }` 解析为本插件的
/// 活动栏路由。形状非法同步报错；页面是否存在由跳转器校验。
fn navigation_route(identity: &str, target: Option<&HostValue>) -> Result<String, HostError> {
    let unknown = || {
        let json = target.map_or(Value::Null, host_value::host_to_json);
        HostError::new(format!("fluxdown.navigate: unknown target {json}"))
    };
    match target {
        Some(HostValue::Str(id)) if NAV_BUILTIN_TARGETS.contains(&id.as_str()) => Ok(id.clone()),
        Some(HostValue::Object(fields)) => fields
            .iter()
            .find_map(|(key, value)| match value {
                HostValue::Str(view) if key == "view" && !view.is_empty() => Some(view),
                _ => None,
            })
            .map(|view| plugin_route_id(identity, view))
            .ok_or_else(unknown),
        _ => Err(unknown()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(error: &HostError) -> String {
        error.message().to_owned()
    }

    #[test]
    fn builtin_targets_pass_through_and_views_resolve_to_plugin_routes() {
        for id in NAV_BUILTIN_TARGETS {
            let target = HostValue::Str(id.to_owned());
            assert_eq!(
                navigation_route("a@b", Some(&target)).ok().as_deref(),
                Some(id)
            );
        }
        let view = HostValue::Object(vec![("view".into(), HostValue::Str("dash".into()))]);
        assert_eq!(
            navigation_route("a@b", Some(&view)).ok().as_deref(),
            Some("plugin:a@b:dash")
        );
    }

    #[test]
    fn malformed_targets_are_rejected_with_their_json() {
        let cases = [
            (Some(HostValue::Str("settings".into())), "\"settings\""),
            (Some(HostValue::Number(3.0)), "3"),
            (
                Some(HostValue::Object(vec![(
                    "view".into(),
                    HostValue::Str(String::new()),
                )])),
                "{\"view\":\"\"}",
            ),
            (None, "null"),
        ];
        for (target, json) in cases {
            let Err(error) = navigation_route("a@b", target.as_ref()) else {
                panic!("target should be rejected: {json}");
            };
            assert_eq!(
                message(&error),
                format!("fluxdown.navigate: unknown target {json}")
            );
        }
    }
}
