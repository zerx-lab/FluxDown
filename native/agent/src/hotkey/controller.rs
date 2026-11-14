//! 全局快捷键控制器：偏好 `desktop.shortcuts.global` → 绑定表 → 平台后端；后端回报 → 合并进
//! `ShellStatusDto.global_shortcuts`；触发 → 外壳动作（[`TrayAction`]）。
//!
//! 绑定 id 按「世代」分配：每次下发新表就从单调计数器取一段新的连续 id，后端对旧表的迟到触发 /
//! 回报因 id 落在当前区段之外而被丢弃，不会错绑到新表的动作上。控制器只在外壳控制循环里被轮询，
//! 空闲时没有任何定时器与磁盘 IO。

use std::sync::Arc;

use fluxdown_protocol::AgentEvent;
use fluxdown_protocol::shortcuts::{
    GLOBAL_SHORTCUTS_PREF_KEY, GlobalShortcutBackend, GlobalShortcutState, GlobalShortcutStatusDto,
    GlobalShortcutsStatusDto, ShortcutScope, ShortcutTrigger, resolve_shortcuts, shortcut_spec,
};
use fluxdown_ui_i18n::{I18nCatalog, Translator};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{HotkeyBinding, HotkeyOutcome, HotkeyPort, HotkeySink, MainThreadHost};
use crate::event_hub::AgentEventHub;
use crate::shell::TrayAction;

/// 后端 → 控制循环的消息。
enum SinkEvent {
    Fired(u32),
    Report(GlobalShortcutBackend, Vec<HotkeyOutcome>),
}

/// 把后端回调转成通道消息：回调可能发生在任意线程，不能触碰控制器状态。
struct ChannelSink(mpsc::UnboundedSender<SinkEvent>);

impl HotkeySink for ChannelSink {
    fn fired(&self, id: u32) {
        if self.0.send(SinkEvent::Fired(id)).is_err() {
            tracing::debug!(id, "hotkey controller closed before hotkey activation");
        }
    }

    fn report(&self, backend: GlobalShortcutBackend, outcomes: Vec<HotkeyOutcome>) {
        if self.0.send(SinkEvent::Report(backend, outcomes)).is_err() {
            tracing::debug!("hotkey controller closed before backend report");
        }
    }
}

/// 控制循环需要处理的结果。
pub(crate) enum HotkeyEvent {
    /// 某全局快捷键被触发。
    Fire(TrayAction),
    /// 后端回报了新的注册状态（待 [`HotkeyController::sync`] 发布）。
    Reported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Planned {
    action: &'static str,
    trigger: ShortcutTrigger,
}

/// 偏好解析后的绑定计划（与 id 无关，可直接比较）。
#[derive(Debug, Default, Eq, PartialEq)]
struct Plan {
    /// 下发给后端的绑定，顺序即 id 偏移。
    active: Vec<Planned>,
    /// 与更靠前的动作绑定了同一触发器：不下发，状态里标记失败。附带占用它的动作 id。
    shadowed: Vec<(Planned, &'static str)>,
}

impl Plan {
    fn resolve(overrides: Option<&Value>) -> Self {
        let mut plan = Self::default();
        for resolved in resolve_shortcuts(ShortcutScope::Global, overrides) {
            for trigger in resolved.triggers {
                let owner = plan
                    .active
                    .iter()
                    .find(|planned| planned.trigger == trigger)
                    .map(|planned| planned.action);
                let planned = Planned {
                    action: resolved.spec.id,
                    trigger,
                };
                match owner {
                    Some(owner) => plan.shadowed.push((planned, owner)),
                    None => plan.active.push(planned),
                }
            }
        }
        plan
    }

    /// 以 `base` 为起点给 `active` 分配 id；`describe` 给出动作的展示名。
    fn bindings(&self, base: u32, describe: impl Fn(&str) -> String) -> Vec<HotkeyBinding> {
        self.active
            .iter()
            .enumerate()
            .map(|(index, planned)| HotkeyBinding {
                id: id_at(base, index),
                trigger: planned.trigger.clone(),
                description: describe(planned.action),
            })
            .collect()
    }

    /// 当前世代内 id 对应的动作；不在区段内（旧世代 / 未知）返回 `None`。
    fn action_of(&self, base: u32, id: u32) -> Option<&'static str> {
        let index = usize::try_from(id.wrapping_sub(base)).ok()?;
        self.active.get(index).map(|planned| planned.action)
    }

    /// 回报的全部 id 是否都属于当前世代（空回报恒属于）。
    fn covers(&self, base: u32, outcomes: &[HotkeyOutcome]) -> bool {
        outcomes
            .iter()
            .all(|outcome| self.action_of(base, outcome.id).is_some())
    }
}

fn id_at(base: u32, index: usize) -> u32 {
    base.wrapping_add(u32::try_from(index).unwrap_or(u32::MAX))
}

/// 全局动作 id → 外壳动作。
fn tray_action(action: &str) -> Option<TrayAction> {
    match action {
        "show_window" => Some(TrayAction::ShowWindow),
        "open_settings" => Some(TrayAction::ShowSettings),
        "new_download" => Some(TrayAction::NewDownload),
        "pause_all" => Some(TrayAction::PauseAll),
        "resume_all" => Some(TrayAction::ResumeAll),
        _ => None,
    }
}

struct Report {
    backend: GlobalShortcutBackend,
    outcomes: Vec<HotkeyOutcome>,
}

/// 由计划与后端回报合并出对外状态。
fn build_status(plan: &Plan, base: u32, report: &Report) -> GlobalShortcutsStatusDto {
    if report.backend == GlobalShortcutBackend::Unavailable {
        return GlobalShortcutsStatusDto::default();
    }
    let active = plan
        .active
        .iter()
        .enumerate()
        .filter_map(|(index, planned)| {
            let id = id_at(base, index);
            let outcome = report.outcomes.iter().find(|outcome| outcome.id == id)?;
            Some(GlobalShortcutStatusDto {
                action: planned.action.to_owned(),
                trigger: planned.trigger.to_string(),
                state: outcome.state,
                detail: outcome.detail.clone(),
            })
        });
    let shadowed = plan
        .shadowed
        .iter()
        .map(|(planned, owner)| GlobalShortcutStatusDto {
            action: planned.action.to_owned(),
            trigger: planned.trigger.to_string(),
            state: GlobalShortcutState::Failed,
            detail: Some(format!("trigger already bound to `{owner}`")),
        });
    GlobalShortcutsStatusDto {
        backend: report.backend,
        entries: active.chain(shadowed).collect(),
    }
}

pub(crate) struct HotkeyController {
    /// `None`：没有可用后端（无桌面会话 / 平台后端启动失败），状态恒为默认（不可用）。
    port: Option<Box<dyn HotkeyPort>>,
    events: mpsc::UnboundedReceiver<SinkEvent>,
    /// 持有一份发送端，通道不会因后端释放回调而关闭。
    _sink: Arc<ChannelSink>,
    /// 最近一次读到的原始偏好值。
    overrides: Option<Value>,
    /// 是否已经下发过首张表（首张表即使为空也要下发，让后端回报自身类型）。
    applied: bool,
    plan: Plan,
    /// 当前世代的首个 id。
    base: u32,
    /// 下一世代的首个 id。
    next_id: u32,
    /// 已下发但尚未收到对应回报：此时不发布，避免状态在新旧表之间闪烁。
    awaiting: bool,
    report: Option<Report>,
    /// 动作展示名（portal 描述）；内嵌文案加载失败时退回动作 id。
    translator: Option<Translator>,
    dirty: bool,
}

impl HotkeyController {
    /// 启动平台后端。`desktop_session = false`（无桌面会话）时不启动。
    /// 必须在 agent Tokio 运行时上下文内调用。
    pub(crate) fn start(
        desktop_session: bool,
        main_thread: Option<Arc<dyn MainThreadHost>>,
    ) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        let sink = Arc::new(ChannelSink(tx));
        let port = if desktop_session {
            let backend_sink: Arc<dyn HotkeySink> = sink.clone();
            match crate::platform::hotkey::start(backend_sink, main_thread.as_ref()) {
                Ok(port) => Some(port),
                Err(detail) => {
                    tracing::warn!(detail, "global hotkey backend unavailable");
                    None
                }
            }
        } else {
            None
        };
        let translator = port
            .as_ref()
            .and_then(|_| match I18nCatalog::load_embedded() {
                Ok(catalog) => Some(catalog.translator(&fluxdown_ui_i18n::system_locale())),
                Err(error) => {
                    tracing::warn!(error = %error, "hotkey translations unavailable");
                    None
                }
            });
        Self {
            port,
            events,
            _sink: sink,
            overrides: None,
            applied: false,
            plan: Plan::default(),
            base: 0,
            next_id: 0,
            awaiting: false,
            report: None,
            translator,
            dirty: true,
        }
    }

    /// 等待下一个后端消息；触发时返回对应动作，过期 / 未知 id 的触发被丢弃并继续等待。
    pub(crate) async fn recv(&mut self) -> HotkeyEvent {
        loop {
            let Some(event) = self.events.recv().await else {
                return std::future::pending().await;
            };
            match event {
                SinkEvent::Fired(id) => {
                    let action = self.plan.action_of(self.base, id).and_then(tray_action);
                    match action {
                        Some(action) => return HotkeyEvent::Fire(action),
                        None => tracing::debug!(id, "ignored stale or unknown hotkey activation"),
                    }
                }
                SinkEvent::Report(backend, outcomes) => {
                    if self.plan.covers(self.base, &outcomes) && self.applied {
                        self.report = Some(Report { backend, outcomes });
                        self.awaiting = false;
                        self.dirty = true;
                        return HotkeyEvent::Reported;
                    }
                    tracing::debug!("ignored stale hotkey backend report");
                }
            }
        }
    }

    /// 偏好变化 → 重新下发绑定表；状态变化 → 发布 `ShellChanged`。
    pub(crate) fn sync(&mut self, hub: &AgentEventHub) {
        if self.port.is_some() {
            let changed = hub.inspect(|snapshot| {
                let values = &snapshot.preferences.values;
                let value = values.get(GLOBAL_SHORTCUTS_PREF_KEY);
                (!self.applied || value != self.overrides.as_ref()).then(|| {
                    let locale = values
                        .get("general.locale")
                        .and_then(Value::as_str)
                        .filter(|locale| *locale != "system")
                        .map(str::to_owned);
                    (value.cloned(), locale)
                })
            });
            if let Some((overrides, locale)) = changed {
                if let Some(translator) = self.translator.as_mut() {
                    translator.set_locale(&locale.unwrap_or_else(fluxdown_ui_i18n::system_locale));
                }
                self.overrides = overrides;
                self.apply_plan(Plan::resolve(self.overrides.as_ref()));
            }
        }
        self.publish(hub);
    }

    fn apply_plan(&mut self, plan: Plan) {
        let Some(port) = self.port.as_ref() else {
            return;
        };
        let active_changed = !self.applied || plan.active != self.plan.active;
        if plan.shadowed != self.plan.shadowed {
            self.dirty = true;
        }
        self.plan = plan;
        if !active_changed {
            return;
        }
        self.base = self.next_id;
        self.next_id = id_at(self.base, self.plan.active.len());
        self.applied = true;
        self.awaiting = true;
        let translator = self.translator.as_ref();
        port.apply(self.plan.bindings(self.base, |action| {
            match (translator, shortcut_spec(ShortcutScope::Global, action)) {
                (Some(translator), Some(spec)) => translator.text(spec.label_key).to_owned(),
                _ => action.to_owned(),
            }
        }));
    }

    fn publish(&mut self, hub: &AgentEventHub) {
        if !self.dirty || self.awaiting {
            return;
        }
        self.dirty = false;
        let status = match &self.report {
            Some(report) => build_status(&self.plan, self.base, report),
            None => GlobalShortcutsStatusDto::default(),
        };
        let shell = hub.inspect(|snapshot| {
            (snapshot.shell.global_shortcuts != status).then(|| snapshot.shell.clone())
        });
        if let Some(mut shell) = shell {
            shell.global_shortcuts = status;
            hub.publish(AgentEvent::ShellChanged(shell));
        }
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::shortcuts::SHORTCUT_SPECS;
    use serde_json::json;

    use super::*;

    fn outcome(id: u32, state: GlobalShortcutState) -> HotkeyOutcome {
        HotkeyOutcome {
            id,
            state,
            detail: None,
        }
    }

    #[test]
    fn every_global_action_maps_to_a_shell_action() {
        for spec in SHORTCUT_SPECS
            .iter()
            .filter(|spec| spec.scope == ShortcutScope::Global)
        {
            assert!(
                tray_action(spec.id).is_some(),
                "{} has no shell action",
                spec.id
            );
        }
    }

    #[test]
    fn defaults_bind_nothing_and_overrides_flatten_in_catalog_order() {
        assert!(Plan::resolve(None).active.is_empty());
        let plan = Plan::resolve(Some(&json!({
            "pause_all": ["ctrl-alt-p", "double:ctrl"],
            "show_window": ["ctrl-alt-d"],
        })));
        let actual: Vec<(&str, String)> = plan
            .active
            .iter()
            .map(|planned| (planned.action, planned.trigger.to_string()))
            .collect();
        assert_eq!(
            actual,
            [
                ("show_window", "ctrl-alt-d".to_owned()),
                ("pause_all", "ctrl-alt-p".to_owned()),
                ("pause_all", "double:ctrl".to_owned()),
            ]
        );
        let ids: Vec<u32> = plan
            .bindings(10, str::to_owned)
            .iter()
            .map(|binding| binding.id)
            .collect();
        assert_eq!(ids, [10, 11, 12]);
    }

    #[test]
    fn duplicate_trigger_is_shadowed_by_the_earlier_action() {
        let plan = Plan::resolve(Some(&json!({
            "show_window": ["ctrl-alt-d"],
            "resume_all": ["ctrl-alt-d"],
        })));
        assert_eq!(plan.active.len(), 1);
        assert_eq!(plan.shadowed.len(), 1);
        assert_eq!(plan.shadowed[0].0.action, "resume_all");
        assert_eq!(plan.shadowed[0].1, "show_window");
    }

    #[test]
    fn ids_outside_the_current_generation_are_stale() {
        let plan = Plan::resolve(Some(&json!({ "show_window": ["ctrl-alt-d"] })));
        assert_eq!(plan.action_of(5, 5), Some("show_window"));
        assert_eq!(plan.action_of(5, 4), None);
        assert_eq!(plan.action_of(5, 6), None);
        assert_eq!(plan.action_of(u32::MAX, u32::MAX), Some("show_window"));
        assert_eq!(plan.action_of(u32::MAX, 0), None);
        assert!(plan.covers(5, &[outcome(5, GlobalShortcutState::Active)]));
        assert!(!plan.covers(5, &[outcome(1, GlobalShortcutState::Active)]));
        assert!(plan.covers(5, &[]));
    }

    #[test]
    fn status_merges_outcomes_and_marks_shadowed_entries_failed() {
        let plan = Plan::resolve(Some(&json!({
            "show_window": ["ctrl-alt-d"],
            "pause_all": ["double:ctrl"],
            "resume_all": ["ctrl-alt-d"],
        })));
        let report = Report {
            backend: GlobalShortcutBackend::MacOs,
            outcomes: vec![
                outcome(7, GlobalShortcutState::InUse),
                outcome(8, GlobalShortcutState::PermissionRequired),
            ],
        };
        let status = build_status(&plan, 7, &report);
        assert_eq!(status.backend, GlobalShortcutBackend::MacOs);
        let summary: Vec<(&str, &str, GlobalShortcutState)> = status
            .entries
            .iter()
            .map(|entry| (entry.action.as_str(), entry.trigger.as_str(), entry.state))
            .collect();
        assert_eq!(
            summary,
            [
                ("show_window", "ctrl-alt-d", GlobalShortcutState::InUse),
                (
                    "pause_all",
                    "double:ctrl",
                    GlobalShortcutState::PermissionRequired
                ),
                ("resume_all", "ctrl-alt-d", GlobalShortcutState::Failed),
            ]
        );
        assert!(status.entries[2].detail.is_some());
    }

    #[test]
    fn unavailable_backend_hides_all_entries() {
        let plan = Plan::resolve(Some(&json!({ "show_window": ["ctrl-alt-d"] })));
        let report = Report {
            backend: GlobalShortcutBackend::Unavailable,
            outcomes: vec![outcome(0, GlobalShortcutState::Active)],
        };
        assert_eq!(
            build_status(&plan, 0, &report),
            GlobalShortcutsStatusDto::default()
        );
    }
}
