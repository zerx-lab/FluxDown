use std::collections::{HashMap, HashSet};

use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, dialog_title, field_error, form, form_field,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};

use super::SectionContext;
use crate::{
    store::SettingsStore,
    ui::{
        SettingsRow, SettingsSection, body_text, dialog_footer, meta_text, row_button,
        row_danger_button,
    },
};

const PREF: &str = "download.capture_device_rules";

fn extension(value: &str) -> Option<&str> {
    let value = value.trim();
    let value = value.strip_prefix('.').unwrap_or(value);
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(value)
}

fn rule(line: &str) -> Option<(&str, &str)> {
    let (suffix, target) = line.split_once('=')?;
    let target = target.trim();
    let (kind, id) = target.split_once(':')?;
    (matches!(kind, "cloud" | "link") && !id.is_empty()).then_some((extension(suffix)?, target))
}

fn rules(store: &SettingsStore) -> &str {
    store
        .pref(PREF)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

#[derive(Clone)]
struct Existing {
    index: usize,
    line: String,
}

fn edit_rules(raw: &str, existing: Option<&Existing>, replacement: Option<&str>) -> Option<String> {
    let mut lines = raw.lines().collect::<Vec<_>>();
    if let Some(existing) = existing {
        if lines.get(existing.index) != Some(&existing.line.as_str()) {
            return None;
        }
        if let Some(replacement) = replacement {
            *lines.get_mut(existing.index)? = replacement;
        } else {
            lines.remove(existing.index);
        }
    } else {
        lines.push(replacement?);
    }
    Some(lines.join("\n"))
}

struct RuleDevice {
    target: String,
    label: String,
    attached: bool,
}

fn targets(store: &SettingsStore) -> Vec<RuleDevice> {
    let (cloud, links) = store.capture_devices();
    let current = store
        .session()
        .map(|session| session.device.device_id.as_str());
    let mut cloud_ids = HashSet::new();
    let mut choices = Vec::with_capacity(cloud.len() + links.len());
    for device in cloud {
        let id = device.device_id.as_str();
        if !device.is_current && Some(id) != current && !id.is_empty() && cloud_ids.insert(id) {
            choices.push((format!("cloud:{id}"), id, device.name.trim(), false));
        }
    }
    let mut link_ids = HashSet::new();
    for device in links {
        let id = device.fingerprint.as_str();
        if device.revoked || id.is_empty() || !link_ids.insert(id) {
            continue;
        }
        let attached = device
            .cloud_device_id
            .as_deref()
            .is_some_and(|id| cloud_ids.contains(id));
        choices.push((format!("link:{id}"), id, device.name.trim(), attached));
    }
    let mut counts = HashMap::new();
    for (_, _, name, _) in &choices {
        *counts.entry(*name).or_insert(0) += 1;
    }
    choices
        .into_iter()
        .map(|(target, id, name, attached)| RuleDevice {
            target,
            label: bounded_label(name, id, counts.get(name).copied().unwrap_or(0) > 1),
            attached,
        })
        .collect()
}

fn bounded_label(name: &str, id: &str, duplicate: bool) -> String {
    let mut chars = name.chars();
    let mut label: String = chars.by_ref().take(32).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    if label.is_empty() || duplicate {
        if !label.is_empty() {
            label.push_str(" · ");
        }
        let start = id.char_indices().rev().nth(5).map_or(0, |(index, _)| index);
        if let Some(code) = id.get(start..) {
            label.push_str(code);
        }
    }
    label
}

pub(crate) fn group(ctx: &SectionContext) -> SettingsSection {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    SettingsSection::new()
        .title(ctx.t("captureDeviceRules"))
        .subtitle(ctx.t("captureDeviceRulesDesc"))
        .row(
            SettingsRow::custom(move |disabled, _, _, cx: &mut App| {
                let gap = active_theme(cx).tokens().spacing.sm;
                let state = store.read(cx);
                let choices = targets(state);
                let disabled = disabled || state.is_read_only() || state.preference_pending(PREF);
                let mut column = v_flex().w_full().gap(gap);
                for (index, line) in rules(state).lines().enumerate() {
                    let Some((suffix, target)) = rule(line) else {
                        continue;
                    };
                    let label = choices
                        .iter()
                        .find(|choice| choice.target == target)
                        .map_or_else(
                            || translator.text("deviceOffline").to_owned(),
                            |choice| choice.label.clone(),
                        );
                    let existing = Existing {
                        index,
                        line: line.to_owned(),
                    };
                    let edit_item = existing.clone();
                    let edit_store = store.clone();
                    let edit_translator = translator.clone();
                    let delete_store = store.clone();
                    column = column.child(
                        h_flex()
                            .w_full()
                            .gap(gap)
                            .items_center()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(body_text(cx).truncate().child(format!(".{suffix}")))
                                    .child(meta_text(cx).truncate().child(label)),
                            )
                            .child(
                                row_button(
                                    ("capture-rule-edit", index),
                                    translator.text("captureDeviceRuleEdit").to_owned(),
                                    ButtonVariant::Secondary,
                                    cx,
                                )
                                .disabled(disabled)
                                .on_click(move |_, window, cx| {
                                    open_editor(
                                        edit_store.clone(),
                                        edit_translator.clone(),
                                        Some(edit_item.clone()),
                                        window,
                                        cx,
                                    )
                                }),
                            )
                            .child(
                                row_danger_button(
                                    ("capture-rule-delete", index),
                                    translator.text("delete").to_owned(),
                                    cx,
                                )
                                .disabled(disabled)
                                .on_click(move |_, _, cx| {
                                    delete_store.update(cx, |store, cx| {
                                        if let Some(raw) =
                                            edit_rules(rules(store), Some(&existing), None)
                                        {
                                            store.set_pref_str(PREF, raw, cx);
                                        }
                                    });
                                }),
                            ),
                    );
                }
                let add_store = store.clone();
                let add_translator = translator.clone();
                column
                    .when(choices.is_empty(), |column| {
                        column.child(
                            meta_text(cx)
                                .child(translator.text("captureDeviceRuleNoDevices").to_owned()),
                        )
                    })
                    .child(
                        h_flex().justify_end().child(
                            row_button(
                                "capture-rule-add",
                                translator.text("captureDeviceRuleAdd").to_owned(),
                                ButtonVariant::Primary,
                                cx,
                            )
                            .disabled(disabled || choices.is_empty())
                            .on_click(move |_, window, cx| {
                                open_editor(
                                    add_store.clone(),
                                    add_translator.clone(),
                                    None,
                                    window,
                                    cx,
                                )
                            }),
                        ),
                    )
                    .into_any_element()
            })
            .keywords([
                ctx.t("captureDeviceRules"),
                ctx.t("captureDeviceRuleExtension"),
            ]),
        )
}

struct Editor {
    store: Entity<SettingsStore>,
    translator: Translator,
    existing: Option<Existing>,
    suffix: Entity<InputState>,
    target: Option<String>,
}

fn open_editor(
    store: Entity<SettingsStore>,
    translator: Translator,
    existing: Option<Existing>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text(if existing.is_some() {
        "captureDeviceRuleEdit"
    } else {
        "captureDeviceRuleAdd"
    }));
    let view = cx.new(|cx| {
        let (suffix, target) = existing
            .as_ref()
            .and_then(|existing| rule(&existing.line))
            .map_or(("", None), |(suffix, target)| {
                (suffix, Some(target.to_owned()))
            });
        let suffix = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(suffix.to_owned())
                .placeholder("apk")
        });
        cx.subscribe(&suffix, |_, _, _: &InputEvent, cx| cx.notify())
            .detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Editor {
            store,
            translator,
            existing,
            suffix,
            target,
        }
    });
    let input = view.read(cx).suffix.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(480.))
            .content(move |content, _, _| content.child(view.clone()))
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = |key| SharedString::from(self.translator.text(key));
        let state = self.store.read(cx);
        let current = self.target.clone();
        let choices = targets(state)
            .into_iter()
            .filter(|choice| !choice.attached || current.as_deref() == Some(choice.target.as_str()))
            .collect::<Vec<_>>();
        let label = current
            .as_ref()
            .and_then(|target| choices.iter().find(|choice| &choice.target == target))
            .map_or_else(
                || text("captureDeviceRuleTarget"),
                |choice| choice.label.clone().into(),
            );
        let disabled = state.is_read_only() || state.preference_pending(PREF);
        let target_available = current
            .as_ref()
            .is_some_and(|target| choices.iter().any(|choice| &choice.target == target));
        let suffix = self.suffix.read(cx).value();
        let valid = extension(&suffix).is_some();
        let show_error = !suffix.trim().is_empty() && !valid;
        let view = cx.weak_entity();
        let theme = active_theme(cx);
        let menu_max_h = (theme.density().nav_row * 8.).min(window.viewport_size().height * 0.5);
        let gap = theme.tokens().spacing.md;
        let no_devices = choices.is_empty();
        let picker = Button::new("capture-rule-target")
            .outline()
            .control(cx)
            .w_full()
            .label(label)
            .dropdown_caret(true)
            .disabled(disabled || no_devices)
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                choices
                    .iter()
                    .fold(menu.scrollable(true).max_h(menu_max_h), |menu, choice| {
                        let checked = current.as_ref() == Some(&choice.target);
                        let id = choice.target.clone();
                        let view = view.clone();
                        menu.item(
                            PopupMenuItem::new(choice.label.clone())
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    if let Some(view) = view.upgrade() {
                                        view.update(cx, |this, cx| {
                                            this.target = Some(id.clone());
                                            cx.notify();
                                        });
                                    }
                                }),
                        )
                    })
            });
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(gap)
            .child(
                form(cx)
                    .child(form_field(
                        text("captureDeviceRuleExtension"),
                        Input::new(&self.suffix)
                            .control(cx)
                            .w_full()
                            .disabled(disabled),
                        None,
                        cx,
                    ))
                    .when(show_error, |form| {
                        form.child(field_error(text("captureDeviceRuleExtensionInvalid"), cx))
                    })
                    .child(form_field(
                        text("captureDeviceRuleTarget"),
                        picker,
                        no_devices.then(|| text("captureDeviceRuleNoDevices")),
                        cx,
                    )),
            )
            .child(dialog_footer(
                None,
                [
                    Button::new("capture-rule-cancel")
                        .outline()
                        .control(cx)
                        .label(text("cancel"))
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("capture-rule-save")
                        .primary()
                        .control(cx)
                        .label(text("confirm"))
                        .disabled(disabled || !valid || !target_available)
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)),
                        )
                        .into_any_element(),
                ],
                cx,
            ))
    }
}

impl Editor {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suffix = self.suffix.read(cx).value();
        let Some(suffix) = extension(&suffix) else {
            return;
        };
        let Some(target) = self.target.as_deref() else {
            return;
        };
        let state = self.store.read(cx);
        if state.is_read_only()
            || state.preference_pending(PREF)
            || !targets(state).iter().any(|choice| choice.target == target)
        {
            return;
        }
        let replacement = format!("{}={target}", suffix.to_ascii_lowercase());
        if let Some(raw) = edit_rules(rules(state), self.existing.as_ref(), Some(&replacement)) {
            self.store
                .update(cx, |store, cx| store.set_pref_str(PREF, raw, cx));
        }
        window.close_dialog(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{Existing, bounded_label, edit_rules, rule};

    #[test]
    fn rules_are_edited_in_place_without_reordering_or_dropping_other_lines() {
        let raw = "invalid\n .APK = cloud:one \napk=link:two";
        let existing = Existing {
            index: 1,
            line: " .APK = cloud:one ".to_owned(),
        };
        assert_eq!(rule(&existing.line), Some(("APK", "cloud:one")));
        assert_eq!(rule("..apk=cloud:one"), None);
        assert_eq!(rule("exe=link:"), None);
        assert_eq!(
            edit_rules(raw, Some(&existing), Some("apk=cloud:three")).as_deref(),
            Some("invalid\napk=cloud:three\napk=link:two")
        );
        assert_eq!(
            edit_rules(raw, Some(&existing), None).as_deref(),
            Some("invalid\napk=link:two")
        );
        assert_eq!(edit_rules("apk=link:two", Some(&existing), None), None);
    }

    #[test]
    fn device_labels_are_bounded_and_distinguish_empty_or_duplicate_names() {
        assert_eq!(bounded_label("", "abc123456", false), "123456");
        assert_eq!(bounded_label("同名电脑", "first", true), "同名电脑 · first");
        let name = "行💻".repeat(40);
        assert_eq!(bounded_label(&name, "id", false).chars().count(), 33);
    }
}
