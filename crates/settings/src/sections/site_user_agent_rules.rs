//! Download-host User-Agent rules, in first-match order.

mod editor;

use std::rc::Rc;

use fluxdown_protocol::{DaemonConfigSnapshot, RpcErrorData, method};
use fluxdown_ui_components::{
    BusyHold, ButtonVariant, DialogIntent, FluxIcon, button, dialog_title, field_error,
    icon_button, loading_icon_button, tooltip_view, truncated_text,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, Context, Div, Entity, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, prelude::FluentBuilder as _,
};
use gpui_component::{Icon, WindowExt as _, h_flex, v_flex};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::SectionContext;
use crate::store::{SettingsErrorKind, SettingsStore};
use crate::ui::{SettingsRow, body_text, meta_text};

const KEY: &str = "site_user_agent_rules";
const MAX_RULES: usize = 200;
const INVALID: &str = "siteUserAgentInvalid";
const CONFLICT: &str = "localServiceConflict";
const FAILED: &str = "localServiceActionFailed";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rule {
    domain: String,
    include_subdomains: bool,
    user_agent: String,
}

fn parse(raw: &str) -> Result<Vec<Rule>, &'static str> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw).map_err(|_| INVALID)
}

enum Change {
    Add(Rule),
    Edit(usize, Rule),
    Delete(usize),
    Move(usize, usize),
}

impl Change {
    fn apply(
        self,
        mut current: Vec<Rule>,
        base: &[Rule],
    ) -> Result<Option<Vec<Rule>>, &'static str> {
        // With no persistent row ID, an index is meaningful only against the same ordered list.
        if !matches!(&self, Self::Add(_)) && current.as_slice() != base {
            return Err(CONFLICT);
        }
        match self {
            Self::Add(rule) => {
                if current.len() >= MAX_RULES {
                    return Err(CONFLICT);
                }
                current.push(rule);
            }
            Self::Edit(index, rule) => {
                let item = current.get_mut(index).ok_or(CONFLICT)?;
                if *item == rule {
                    return Ok(None);
                }
                *item = rule;
            }
            Self::Delete(index) => {
                if index >= current.len() {
                    return Err(CONFLICT);
                }
                current.remove(index);
            }
            Self::Move(from, to) => {
                let pair = current.get(from).zip(current.get(to)).ok_or(CONFLICT)?;
                if pair.0 == pair.1 {
                    return Ok(None);
                }
                current.swap(from, to);
            }
        }
        Ok(Some(current))
    }
}

fn rpc_error_key(error: RpcErrorData) -> &'static str {
    match SettingsErrorKind::from_rpc(&error) {
        SettingsErrorKind::InvalidArgument => INVALID,
        kind => kind.i18n_key(),
    }
}

fn decode_snapshot(value: serde_json::Value) -> Result<DaemonConfigSnapshot, &'static str> {
    serde_json::from_value(value).map_err(|_| FAILED)
}

/// Read, compare, and patch using the same daemon revision. An unrelated settings value is never
/// sent back, and the host remains responsible for validating and normalizing the rule values.
fn save_change<T: 'static>(
    store: Entity<SettingsStore>,
    base: Rc<Vec<Rule>>,
    change: Change,
    window: &mut Window,
    cx: &mut Context<T>,
    done: impl FnOnce(&mut T, Result<(), &'static str>, &mut Window, &mut Context<T>) + 'static,
) {
    let get = store
        .read(cx)
        .raw_call(method::DAEMON_CONFIG_GET, json!({}));
    cx.spawn_in(window, async move |this, cx| {
        let mut latest = None;
        let result = async {
            let snapshot = decode_snapshot(get.await.map_err(rpc_error_key)?)?;
            let revision = snapshot.revision;
            let current = parse(snapshot.values.get(KEY).map_or("", String::as_str));
            latest = Some(snapshot);
            let Some(next) = change.apply(current?, &base)? else {
                return Ok(());
            };
            let raw = serde_json::to_string(&next).map_err(|_| FAILED)?;
            let patch = cx
                .update(|_, cx| {
                    store.read(cx).raw_call(
                        method::DAEMON_CONFIG_PATCH,
                        json!({ "expectedRevision": revision, "values": { (KEY): raw } }),
                    )
                })
                .map_err(|_| "localServiceDisconnected")?;
            latest = Some(decode_snapshot(patch.await.map_err(rpc_error_key)?)?);
            Ok(())
        }
        .await;
        if let Some(snapshot) = latest {
            let Ok(()) = cx.update(|_, cx| {
                store.update(cx, |store, cx| store.absorb_daemon_snapshot(snapshot, cx));
            }) else {
                return;
            };
        }
        let Ok(()) = this.update_in(cx, |this, window, cx| done(this, result, window, cx)) else {
            return;
        };
    })
    .detach();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowAction {
    Up,
    Down,
    Delete,
}

#[derive(Default)]
struct ListWrites {
    pending: bool,
    action: Option<(usize, RowAction)>,
    hold: BusyHold,
    error: Option<&'static str>,
}

impl ListWrites {
    fn submit(
        &mut self,
        store: Entity<SettingsStore>,
        rules: Rc<Vec<Rule>>,
        index: usize,
        action: RowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending || !store.read(cx).daemon_connected() {
            return;
        }
        let change = match action {
            RowAction::Up => Change::Move(index, index.saturating_sub(1)),
            RowAction::Down => Change::Move(index, index + 1),
            RowAction::Delete => Change::Delete(index),
        };
        self.pending = true;
        self.action = Some((index, action));
        self.error = None;
        self.hold.start(cx);
        cx.notify();
        save_change(
            store,
            rules,
            change,
            window,
            cx,
            move |state, result, _, cx| {
                state.pending = false;
                if result.is_ok() {
                    state.action = match action {
                        RowAction::Up => Some((index.saturating_sub(1), action)),
                        RowAction::Down => Some((index + 1, action)),
                        RowAction::Delete => None,
                    };
                }
                state.error = result.err();
                cx.notify();
            },
        );
    }
}

pub(super) fn row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let title = ctx.t("siteUserAgentRules");
    let desc = ctx.t("siteUserAgentRulesDesc");
    let render_title = title.clone();
    let render_desc = desc.clone();
    let mut row = SettingsRow::custom(move |disabled, key, window, cx| {
        let writes = window.use_keyed_state(
            SharedString::from(format!("{key}-site-ua-writes")),
            cx,
            |_, _| ListWrites::default(),
        );
        let list = parse(&store.read(cx).daemon_str(KEY));
        let disabled = disabled || !store.read(cx).daemon_connected() || writes.read(cx).pending;
        let spacing = active_theme(cx).tokens().spacing;
        let header = v_flex()
            .min_w_0()
            .gap(spacing.xxs)
            .child(body_text(cx).child(render_title.clone()))
            .child(meta_text(cx).child(render_desc.clone()));
        let column = v_flex().w_full().min_w_0().gap(spacing.sm).child(header);
        let rules = match list {
            Ok(rules) => Rc::new(rules),
            Err(key) => return column.child(field_error(translator.text(key), cx)),
        };
        let add_rules = rules.clone();
        let add_store = store.clone();
        let add_translator = translator.clone();
        column
            .children(rules.iter().enumerate().map(|(index, rule)| {
                RuleRow {
                    index,
                    rules: &rules,
                    store: &store,
                    writes: &writes,
                    translator: &translator,
                    disabled,
                }
                .render(rule, cx)
            }))
            .when_some(writes.read(cx).error, |column, key| {
                column.child(field_error(translator.text(key), cx))
            })
            .child(
                h_flex().justify_end().child(
                    button(
                        "site-ua-add",
                        translator.text("siteUserAgentAdd"),
                        ButtonVariant::Primary,
                        cx,
                    )
                    .disabled(disabled || rules.len() >= MAX_RULES)
                    .on_click(move |_, window, cx| {
                        editor::open(
                            add_store.clone(),
                            add_translator.clone(),
                            add_rules.clone(),
                            None,
                            window,
                            cx,
                        );
                    }),
                ),
            )
    });
    row.title = title;
    row.description(desc)
}

struct RuleRow<'a> {
    index: usize,
    rules: &'a Rc<Vec<Rule>>,
    store: &'a Entity<SettingsStore>,
    writes: &'a Entity<ListWrites>,
    translator: &'a Translator,
    disabled: bool,
}

impl RuleRow<'_> {
    fn render(&self, rule: &Rule, cx: &App) -> gpui::Stateful<Div> {
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let index = self.index;
        let store = self.store.clone();
        let translator = self.translator.clone();
        let rules = self.rules.clone();
        let edit = SharedString::from(translator.text("siteUserAgentEdit"));
        let edit_tip = edit.clone();
        h_flex()
            .id(("site-ua-rule", index))
            .w_full()
            .items_center()
            .gap(spacing.xs)
            .py(spacing.xxs)
            .child(self.summary(rule, cx))
            .child(self.action(RowAction::Up, cx))
            .child(self.action(RowAction::Down, cx))
            .child(
                icon_button(
                    "site-ua-edit",
                    edit,
                    Icon::new(FluxIcon::Pen).size(theme.extended().icon.sm),
                    ButtonVariant::Ghost,
                    cx,
                )
                .disabled(self.disabled)
                .tooltip(move |window, cx| tooltip_view(edit_tip.clone(), window, cx))
                .on_click(move |_, window, cx| {
                    editor::open(
                        store.clone(),
                        translator.clone(),
                        rules.clone(),
                        Some(index),
                        window,
                        cx,
                    );
                }),
            )
            .child(self.action(RowAction::Delete, cx))
    }

    fn summary(&self, rule: &Rule, cx: &App) -> Div {
        let site = if rule.include_subdomains {
            format!(
                "{} · {}",
                rule.domain,
                self.translator.text("siteUserAgentSubdomains"),
            )
        } else {
            rule.domain.clone()
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .child(
                body_text(cx)
                    .truncate()
                    .child(truncated_text("site-ua-domain", site)),
            )
            .child(
                meta_text(cx)
                    .truncate()
                    .child(truncated_text("site-ua-value", rule.user_agent.clone())),
            )
    }

    fn action(&self, action: RowAction, cx: &App) -> fluxdown_ui_components::Button {
        let index = self.index;
        let (id, key, icon, at_edge) = match action {
            RowAction::Up => (
                "site-ua-up",
                "siteUserAgentMoveUp",
                FluxIcon::ArrowUp,
                index == 0,
            ),
            RowAction::Down => (
                "site-ua-down",
                "siteUserAgentMoveDown",
                FluxIcon::ArrowDown,
                index + 1 == self.rules.len(),
            ),
            RowAction::Delete => ("site-ua-delete", "delete", FluxIcon::Trash2, false),
        };
        let store = self.store.clone();
        let rules = self.rules.clone();
        let writes = self.writes.clone();
        let state = writes.read(cx);
        let loading =
            state.action == Some((index, action)) && (state.pending || state.hold.is_held());
        let label = SharedString::from(self.translator.text(key));
        let tip = label.clone();
        let cancel = (action == RowAction::Delete)
            .then(|| SharedString::from(self.translator.text("cancel")));
        loading_icon_button(
            id,
            label.clone(),
            Icon::new(icon).size(active_theme(cx).extended().icon.sm),
            ButtonVariant::Ghost,
            loading,
            cx,
        )
        .disabled(self.disabled || at_edge || loading)
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        .on_click(move |_, window, cx| {
            let submit = {
                let (writes, store, rules) = (writes.clone(), store.clone(), rules.clone());
                move |window: &mut Window, cx: &mut App| {
                    writes.update(cx, |state, cx| {
                        state.submit(store.clone(), rules.clone(), index, action, window, cx);
                    });
                }
            };
            if let (Some(cancel), Some(rule)) = (&cancel, rules.get(index)) {
                let (label, cancel) = (label.clone(), cancel.clone());
                let domain = SharedString::from(rule.domain.clone());
                window.open_alert_dialog(cx, move |alert, _, cx| {
                    let submit = submit.clone();
                    alert
                        .title(dialog_title(label.clone(), cx))
                        .description(domain.clone())
                        .footer(fluxdown_ui_components::dialog_footer(
                            Some(cancel.clone()),
                            label.clone(),
                            DialogIntent::Destructive,
                            cx,
                        ))
                        .on_ok(move |_, window, cx| {
                            submit(window, cx);
                            true
                        })
                });
            } else {
                submit(window, cx);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CONFLICT, Change, Rule, parse};

    fn rule(domain: &str) -> Rule {
        Rule {
            domain: domain.to_owned(),
            user_agent: "Example/1.0".to_owned(),
            include_subdomains: false,
        }
    }

    #[test]
    fn site_user_agent_rules_use_the_host_wire_shape() {
        let raw =
            r#"[{"domain":"example.com","includeSubdomains":true,"userAgent":"Example/1.0"}]"#;
        let rules = parse(raw).unwrap();
        assert_eq!(serde_json::to_string(&rules).unwrap(), raw);
        assert!(parse("").unwrap().is_empty());
        assert!(parse(r#"[{"domain":"example.com","userAgent":"Example/1.0"}]"#).is_err());
    }

    #[test]
    fn site_user_agent_rules_do_not_replay_an_index_on_a_changed_list() {
        let base = vec![rule("a.example"), rule("b.example")];
        let mut current = base.clone();
        current.insert(0, rule("c.example"));
        assert_eq!(
            Change::Delete(0).apply(current.clone(), &base),
            Err(CONFLICT)
        );
        assert_eq!(
            Change::Edit(1, rule("d.example")).apply(current.clone(), &base),
            Err(CONFLICT)
        );
        assert_eq!(
            Change::Move(0, 1).apply(current.clone(), &base),
            Err(CONFLICT)
        );
        let next = Change::Add(rule("d.example"))
            .apply(current.clone(), &base)
            .unwrap()
            .unwrap();
        current.push(rule("d.example"));
        assert_eq!(next, current);
    }

    #[test]
    fn site_user_agent_rules_reorder_and_edit_the_selected_occurrence() {
        let first = rule("example.com");
        let second = rule("cdn.example.com");
        let base = vec![first.clone(), first.clone(), second.clone()];
        let next = Change::Move(1, 2)
            .apply(base.clone(), &base)
            .unwrap()
            .unwrap();
        assert_eq!(next, vec![first.clone(), second.clone(), first.clone()]);
        let replacement = rule("other.example.com");
        let next = Change::Edit(1, replacement.clone())
            .apply(base.clone(), &base)
            .unwrap()
            .unwrap();
        assert_eq!(next, vec![first, replacement, second]);
    }
}
