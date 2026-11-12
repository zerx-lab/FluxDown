//! BT 防吸血「客户端规则」分组：本机名字规则列表（编辑 / 删除 / 调序 / 添加）与云端规则只读列表。
//!
//! 本机规则存 `bt_guard_client_rules`，写入经 [`SettingsStore::mutate_daemon`] 基于最新值重算；
//! 云端规则来自 `daemon.bt.guard.state` 的 `cloudRules`。

use fluxdown_protocol::BtGuardClientRuleDto;
use fluxdown_ui_components::{
    ButtonVariant, DialogIntent, FluxIcon, button, dialog_title, icon_button, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, Context, Div, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{Icon, WindowExt as _, h_flex, v_flex};

use super::SectionContext;
use super::bt_guard::{action_chip, ensure_state, label_or_raw};
use super::bt_guard_client_rule_dialog;
use crate::model::client_rules::{self, CLIENT_RULES_KEY, ClientRule};
use crate::store::SettingsStore;
use crate::ui::{
    SettingsRow, SettingsSection, empty_state, meta_text, row_button, row_danger_button,
};

pub(crate) fn read_rules(store: &SettingsStore) -> Vec<ClientRule> {
    client_rules::parse(&store.daemon_str(CLIENT_RULES_KEY))
}

/// 读-改-写本机规则；`edit` 返回 `None` 表示无需写入。
pub(crate) fn mutate_rules(
    store: &mut SettingsStore,
    edit: impl Fn(Vec<ClientRule>) -> Option<Vec<ClientRule>> + 'static,
    cx: &mut Context<SettingsStore>,
) {
    store.mutate_daemon(
        CLIENT_RULES_KEY,
        move |raw| edit(client_rules::parse(raw)).and_then(|list| client_rules::encode(&list)),
        cx,
    );
}

pub(crate) fn field_label(translator: &Translator, field: &str) -> SharedString {
    match field {
        "peerIdPrefix" => {
            SharedString::from(translator.text("btGuardClientRuleFieldPeerId").to_owned())
        }
        "clientName" => SharedString::from(
            translator
                .text("btGuardClientRuleFieldClientName")
                .to_owned(),
        ),
        other => SharedString::from(other.to_owned()),
    }
}

/// 规则的一行展示数据（本机与云端共用）。
struct RuleView<'a> {
    field: &'a str,
    op: &'a str,
    value: &'a str,
    action: &'a str,
    when: &'a str,
    ban_minutes: u32,
    reason: &'a str,
}

impl<'a> From<&'a ClientRule> for RuleView<'a> {
    fn from(rule: &'a ClientRule) -> Self {
        Self {
            field: &rule.field,
            op: &rule.op,
            value: &rule.value,
            action: &rule.action,
            when: &rule.when,
            ban_minutes: rule.ban_minutes,
            reason: &rule.reason,
        }
    }
}

impl<'a> From<&'a BtGuardClientRuleDto> for RuleView<'a> {
    fn from(rule: &'a BtGuardClientRuleDto) -> Self {
        Self {
            field: &rule.field,
            op: &rule.op,
            value: &rule.value,
            action: &rule.action,
            when: &rule.when,
            ban_minutes: rule.ban_minutes,
            reason: &rule.reason,
        }
    }
}

/// 规则摘要：`字段 · 匹配方式`、匹配内容、动作、生效场景（+ ban 时长）、备注。
fn rule_summary(rule: &RuleView<'_>, translator: &Translator, muted: bool, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let head = format!(
        "{} · {}",
        field_label(translator, rule.field),
        label_or_raw(translator, "btGuardClientRuleOp", rule.op)
    );
    let mut when = label_or_raw(translator, "btGuardClientRuleWhen", rule.when).to_string();
    if rule.action == "ban" {
        when = format!(
            "{when} · {}",
            translator.text_with("etaMinutes", &[("n", &rule.ban_minutes.to_string())])
        );
    }
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(tokens.spacing.xxs)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(meta_text(cx).flex_none().child(SharedString::from(head)))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .font_family(tokens.typography.mono.clone())
                        .when(muted, |text| text.text_color(extended.colors.text_tertiary))
                        .child(SharedString::from(rule.value.to_owned())),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap(tokens.spacing.xs)
                .child(action_chip(
                    rule.action,
                    label_or_raw(translator, "btGuardClientRuleAction", rule.action),
                    cx,
                ))
                .child(meta_text(cx).flex_none().child(SharedString::from(when)))
                .when(!rule.reason.is_empty(), |line| {
                    line.child(
                        meta_text(cx)
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(rule.reason.to_owned())),
                    )
                }),
        )
}

/// 「客户端规则」分组；`active` 为假（总开关关或名字规则 off）时整组置灰。
pub(crate) fn section(ctx: &SectionContext, active: bool) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardClientRules"))
        .subtitle(ctx.t("btGuardClientRulesDesc"))
        .row(local_list_row(ctx).disabled(!active))
        .row(cloud_list_row(ctx).disabled(!active))
}

fn local_list_row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let empty = ctx.t("btGuardClientRulesEmpty");
    let desc = ctx.t("btGuardClientRulesDesc");
    let add = ctx.t("btGuardClientRuleAdd");
    SettingsRow::custom(
        move |disabled: bool, _: &SharedString, _: &mut Window, cx: &mut App| {
            let tokens = active_theme(cx).tokens().clone();
            let (list, connected) = {
                let state = store.read(cx);
                (read_rules(state), state.daemon_connected())
            };
            let disabled = disabled || !connected;
            let mut column = v_flex().w_full().gap(tokens.spacing.xxs);
            if list.is_empty() {
                column = column.child(empty_state(
                    FluxIcon::Shield,
                    empty.clone(),
                    desc.clone(),
                    cx,
                ));
            }
            let last = list.len().saturating_sub(1);
            for (index, rule) in list.iter().enumerate() {
                column = column.child(local_rule_row(
                    rule,
                    index == 0,
                    index == last,
                    &store,
                    &translator,
                    disabled,
                    cx,
                ));
            }
            let full = list.len() >= client_rules::MAX_RULES;
            let add_store = store.clone();
            let add_translator = translator.clone();
            column
                .child(
                    h_flex().w_full().pt(tokens.spacing.sm).justify_end().child(
                        button(
                            "bt-guard-client-rule-add",
                            add.clone(),
                            ButtonVariant::Primary,
                            cx,
                        )
                        .disabled(disabled || full)
                        .on_click(move |_, window, cx| {
                            bt_guard_client_rule_dialog::open(
                                add_store.clone(),
                                add_translator.clone(),
                                None,
                                window,
                                cx,
                            );
                        }),
                    ),
                )
                .into_any_element()
        },
    )
    .keywords([
        ctx.t("btGuardClientRules"),
        ctx.t("btGuardClientRuleFieldClientName"),
        ctx.t("btGuardClientRuleFieldPeerId"),
        ctx.t("btGuardClientRuleActionBan"),
    ])
}

fn local_rule_row(
    rule: &ClientRule,
    first: bool,
    last: bool,
    store: &Entity<SettingsStore>,
    translator: &Translator,
    disabled: bool,
    cx: &App,
) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let id = rule.id.clone();
    let move_button = |up: bool| {
        let (key, icon, suffix) = if up {
            ("moveUpAction", FluxIcon::ArrowUp, "up")
        } else {
            ("moveDownAction", FluxIcon::ArrowDown, "down")
        };
        let label = SharedString::from(translator.text(key).to_owned());
        let tip = label.clone();
        let (store, id) = (store.clone(), id.clone());
        icon_button(
            SharedString::from(format!("bt-guard-client-rule-{suffix}-{id}")),
            label,
            Icon::new(icon).size(extended.icon.sm),
            ButtonVariant::Ghost,
            cx,
        )
        .disabled(disabled || if up { first } else { last })
        .tooltip(move |window, cx| tooltip_view(tip.clone(), window, cx))
        .on_click(move |_, _, cx| {
            let id = id.clone();
            store.update(cx, |store, cx| {
                mutate_rules(store, move |list| client_rules::shift(list, &id, up), cx);
            });
        })
    };
    let edit_store = store.clone();
    let edit_translator = translator.clone();
    let edit_rule = rule.clone();
    let delete_store = store.clone();
    let delete_translator = translator.clone();
    let delete_rule = rule.clone();
    h_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.sm)
        .px(tokens.spacing.sm)
        .py(tokens.spacing.xs)
        .rounded(tokens.radius.md)
        .hover(|style| style.bg(extended.colors.row_hover))
        .child(rule_summary(
            &RuleView::from(rule),
            translator,
            disabled,
            cx,
        ))
        .child(move_button(true))
        .child(move_button(false))
        .child(
            row_button(
                SharedString::from(format!("bt-guard-client-rule-edit-{id}")),
                SharedString::from(translator.text("btGuardClientRuleEdit").to_owned()),
                ButtonVariant::Secondary,
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                bt_guard_client_rule_dialog::open(
                    edit_store.clone(),
                    edit_translator.clone(),
                    Some(edit_rule.clone()),
                    window,
                    cx,
                );
            }),
        )
        .child(
            row_danger_button(
                SharedString::from(format!("bt-guard-client-rule-delete-{id}")),
                SharedString::from(translator.text("btGuardClientRuleDelete").to_owned()),
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                confirm_delete(
                    delete_store.clone(),
                    &delete_translator,
                    &delete_rule,
                    false,
                    window,
                    cx,
                );
            }),
        )
}

/// 删除确认；`close_all` 为真时（从编辑对话框触发）连同编辑框一起关闭。
pub(crate) fn confirm_delete(
    store: Entity<SettingsStore>,
    translator: &Translator,
    rule: &ClientRule,
    close_all: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let id = rule.id.clone();
    let title = SharedString::from(translator.text("btGuardClientRuleDelete").to_owned());
    let description = SharedString::from(format!(
        "{} · {}",
        field_label(translator, &rule.field),
        rule.value
    ));
    let cancel = SharedString::from(translator.text("cancel").to_owned());
    let confirm = SharedString::from(translator.text("delete").to_owned());
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let store = store.clone();
        let id = id.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .description(description.clone())
            .footer(fluxdown_ui_components::dialog_footer(
                Some(cancel.clone()),
                confirm.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let id = id.clone();
                store.update(cx, |store, cx| {
                    mutate_rules(store, move |list| client_rules::remove(list, &id), cx);
                });
                if close_all {
                    // 确认框与编辑框一起关掉；返回 false 避免基座再弹出一层。
                    window.close_all_dialogs(cx);
                    return false;
                }
                true
            })
    });
}

/// 云端规则列表每次展开的条数。
const CLOUD_RULES_PAGE: usize = 10;

fn cloud_list_row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let title = ctx.t("btGuardCloudRules");
    let desc = ctx.t("btGuardCloudRulesDesc");
    let inactive = ctx.t("btGuardCloudRulesInactive");
    let empty = ctx.t("btGuardClientRulesEmpty");
    let loading = ctx.t("btGuardLoading");
    SettingsRow::custom(
        move |disabled: bool, key: &SharedString, window: &mut Window, cx: &mut App| {
            ensure_state(&store, cx);
            // 云端规则只读且可达上百条：先显示一页，其余按需展开。
            let shown = window.use_keyed_state(
                SharedString::from(format!("{key}-cloud-rules-shown")),
                cx,
                |_, _| CLOUD_RULES_PAGE,
            );
            let theme = active_theme(cx);
            let tokens = theme.tokens().clone();
            let warning = theme.extended().colors.warning;
            let (state, cloud_enabled) = {
                let store = store.read(cx);
                (
                    store.bt_guard_state().cloned(),
                    store.daemon_bool("bt_guard_cloud_enabled"),
                )
            };
            let mut column = v_flex()
                .w_full()
                .gap(tokens.spacing.xxs)
                .child(
                    meta_text(cx)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title.clone()),
                )
                .child(meta_text(cx).child(desc.clone()));
            let Some(state) = state else {
                return column
                    .child(meta_text(cx).child(loading.clone()))
                    .into_any_element();
            };
            if !cloud_enabled || state.cloud.stale {
                column = column.child(meta_text(cx).text_color(warning).child(inactive.clone()));
            }
            if state.cloud_rules.is_empty() {
                column = column.child(meta_text(cx).child(empty.clone()));
            }
            let muted = disabled || !cloud_enabled || state.cloud.stale;
            let total = state.cloud_rules.len();
            let visible = (*shown.read(cx)).min(total);
            for rule in state.cloud_rules.iter().take(visible) {
                column = column.child(
                    h_flex()
                        .w_full()
                        .px(tokens.spacing.sm)
                        .py(tokens.spacing.xs)
                        .child(rule_summary(&RuleView::from(rule), &translator, muted, cx)),
                );
            }
            if visible < total {
                let label = SharedString::from(translator.text_with(
                    "btGuardLoadMore",
                    &[
                        ("shown", &visible.to_string()),
                        ("total", &total.to_string()),
                    ],
                ));
                column = column.child(
                    h_flex().w_full().justify_end().child(
                        row_button(
                            "bt-guard-cloud-rules-more",
                            label,
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .on_click(move |_, _, cx| {
                            shown.update(cx, |count, cx| {
                                *count = count.saturating_add(CLOUD_RULES_PAGE);
                                cx.notify();
                            });
                        }),
                    ),
                );
            }
            column.into_any_element()
        },
    )
    .keywords([ctx.t("btGuardCloudRules")])
}
