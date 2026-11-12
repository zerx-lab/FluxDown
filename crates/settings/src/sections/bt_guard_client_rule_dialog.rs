//! 客户端名字规则新增 / 编辑对话框：字段、匹配方式、匹配内容、动作、生效场景、封禁时长（仅 ban）、备注。
//!
//! 保存经 [`mutate_rules`] 基于最新列表 upsert，编辑框随即关闭。

use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, choice_chip, dialog_scroll_body, dialog_title, field_error,
    form, form_field,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    WindowExt as _,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
};

use super::bt_guard::label_or_raw;
use super::bt_guard_client_rules::{confirm_delete, field_label, mutate_rules, read_rules};
use crate::model::client_rules::{
    self, ACTIONS, ClientRule, DEFAULT_BAN_MINUTES, FIELDS, MAX_BAN_MINUTES, OPS, WHENS,
};
use crate::store::SettingsStore;
use crate::ui::{danger_ghost_button, dialog_footer};

pub(crate) struct ClientRuleDialog {
    store: Entity<SettingsStore>,
    translator: Translator,
    /// 编辑中的规则；新增时 id 为空，保存时按最新列表分配。
    draft: ClientRule,
    editing: bool,
    value: Entity<InputState>,
    minutes: Entity<InputState>,
    reason: Entity<InputState>,
    value_error: Option<SharedString>,
}

/// 打开新增（`existing = None`）或编辑对话框。
pub(crate) fn open(
    store: Entity<SettingsStore>,
    translator: Translator,
    existing: Option<ClientRule>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(
        translator
            .text(if existing.is_some() {
                "btGuardClientRuleEdit"
            } else {
                "btGuardClientRuleAdd"
            })
            .to_owned(),
    );
    let view = cx.new(|cx| ClientRuleDialog::new(store, translator, existing, window, cx));
    let value = view.read(cx).value.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(520.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    value.update(cx, |input, cx| input.focus(window, cx));
}

impl ClientRuleDialog {
    fn new(
        store: Entity<SettingsStore>,
        translator: Translator,
        existing: Option<ClientRule>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing = existing.is_some();
        let draft = existing.unwrap_or_else(|| ClientRule::draft(String::new()));
        let placeholder = SharedString::from(
            translator
                .text("btGuardClientRuleValuePlaceholder")
                .to_owned(),
        );
        let initial_value = draft.value.clone();
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(initial_value)
        });
        let initial_minutes = draft.ban_minutes.to_string();
        let minutes = cx.new(|cx| InputState::new(window, cx).default_value(initial_minutes));
        let initial_reason = draft.reason.clone();
        let reason = cx.new(|cx| InputState::new(window, cx).default_value(initial_reason));
        cx.subscribe_in(
            &value,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.value_error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            },
        )
        .detach();
        Self {
            store,
            translator,
            draft,
            editing,
            value,
            minutes,
            reason,
            value_error: None,
        }
    }

    fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.translator.text(key).to_owned())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut rule = self.draft.clone();
        rule.value = self.value.read(cx).value().to_string();
        rule.reason = self.reason.read(cx).value().to_string();
        rule.ban_minutes = self
            .minutes
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .unwrap_or(DEFAULT_BAN_MINUTES)
            .min(MAX_BAN_MINUTES);
        let rule = match client_rules::normalize(rule) {
            Ok(rule) => rule,
            Err(error) => {
                self.value_error = Some(self.t(error.message_key()));
                cx.notify();
                return;
            }
        };
        self.store.update(cx, |store, cx| {
            mutate_rules(
                store,
                move |list| {
                    let mut rule = rule.clone();
                    if rule.id.is_empty() {
                        rule.id = client_rules::next_id(&list);
                    }
                    client_rules::upsert(list, rule)
                },
                cx,
            );
        });
        window.close_dialog(cx);
    }

    fn chips(
        &self,
        group: &'static str,
        options: &'static [&'static str],
        selected: &str,
        label: impl Fn(&Translator, &str) -> SharedString,
        set: fn(&mut ClientRule, &'static str),
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let tokens = active_theme(cx).tokens().clone();
        h_flex()
            .flex_wrap()
            .gap(tokens.spacing.sm)
            .children(options.iter().map(|option| {
                let option: &'static str = option;
                choice_chip(
                    SharedString::from(format!("bt-guard-client-rule-{group}-{option}")),
                    label(&self.translator, option),
                    selected == option,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    set(&mut this.draft, option);
                    cx.notify();
                }))
            }))
    }
}

impl Render for ClientRuleDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let field = self.draft.field.clone();
        let op = self.draft.op.clone();
        let action = self.draft.action.clone();
        let when = self.draft.when.clone();
        let fields = self.chips(
            "field",
            &FIELDS,
            &field,
            field_label,
            |r, v| r.field = v.to_owned(),
            cx,
        );
        let ops = self.chips(
            "op",
            &OPS,
            &op,
            |t, v| label_or_raw(t, "btGuardClientRuleOp", v),
            |r, v| r.op = v.to_owned(),
            cx,
        );
        let actions = self.chips(
            "action",
            &ACTIONS,
            &action,
            |t, v| label_or_raw(t, "btGuardClientRuleAction", v),
            |r, v| r.action = v.to_owned(),
            cx,
        );
        let whens = self.chips(
            "when",
            &WHENS,
            &when,
            |t, v| label_or_raw(t, "btGuardClientRuleWhen", v),
            |r, v| r.when = v.to_owned(),
            cx,
        );
        let matcher = gpui_component::v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(fields)
            .child(ops)
            .child(
                Input::new(&self.value)
                    .control(cx)
                    .w_full()
                    .font_family(tokens.typography.mono.clone()),
            );
        let body = form(cx)
            .child(
                form_field(self.t("btGuardClientRuleValue"), matcher, None, cx)
                    .when_some(self.value_error.clone(), |field, error| {
                        field.child(field_error(error, cx))
                    }),
            )
            .child(form_field(
                self.t("btGuardClientRuleAction"),
                actions,
                None,
                cx,
            ))
            .child(form_field(self.t("btGuardClientRuleWhen"), whens, None, cx))
            .when(action == "ban", |form| {
                form.child(form_field(
                    self.t("btGuardClientRuleBanMinutes"),
                    Input::new(&self.minutes).control(cx).w(px(160.)),
                    None,
                    cx,
                ))
            })
            .child(form_field(
                self.t("btGuardClientRuleReason"),
                Input::new(&self.reason).control(cx).w_full(),
                None,
                cx,
            ));
        let delete = self.editing.then(|| {
            let store = self.store.clone();
            let translator = self.translator.clone();
            let id = self.draft.id.clone();
            danger_ghost_button(
                "bt-guard-client-rule-dialog-delete",
                self.t("btGuardClientRuleDelete"),
                cx,
            )
            .on_click(move |_, window, cx| {
                let current = read_rules(store.read(cx))
                    .into_iter()
                    .find(|rule| rule.id == id);
                if let Some(rule) = current {
                    confirm_delete(store.clone(), &translator, &rule, true, window, cx);
                } else {
                    window.close_dialog(cx);
                }
            })
            .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body(
                "bt-guard-client-rule-body",
                Some(px(480.)),
                body,
                cx,
            ))
            .child(
                dialog_footer(
                    delete,
                    [
                        Button::new("bt-guard-client-rule-cancel")
                            .outline()
                            .label(self.t("cancel"))
                            .control(cx)
                            .on_click(|_, window, cx| window.close_dialog(cx))
                            .into_any_element(),
                        fluxdown_ui_components::button(
                            "bt-guard-client-rule-save",
                            self.t("confirm"),
                            ButtonVariant::Primary,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.submit(window, cx);
                        }))
                        .into_any_element(),
                    ],
                    cx,
                )
                .pt(tokens.spacing.sm),
            )
    }
}
