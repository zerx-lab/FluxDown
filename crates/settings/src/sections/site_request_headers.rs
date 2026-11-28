//! Host-local defaults for request headers on exact HTTP(S) download origins.

use std::collections::BTreeMap;

use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, DialogIntent, button, dialog_scroll_body, dialog_title,
    field_error, form, form_field, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
};
use gpui_component::{
    WindowExt as _, h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    v_flex,
};
use serde::{Deserialize, Serialize};

use super::SectionContext;
use crate::store::SettingsStore;
use crate::ui::{
    SettingsRow, SettingsSection, body_text, dialog_footer, row_button, row_danger_button,
};

const KEY: &str = "site_request_headers";

#[derive(Clone, Deserialize, Serialize)]
struct Rule {
    origin: String,
    headers: BTreeMap<String, String>,
}

fn parse_list(raw: &str) -> Result<Vec<Rule>, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw)
}

fn draft(origin: &str, referer: &str, extra: &str) -> Option<Rule> {
    let mut headers = BTreeMap::new();
    if !referer.trim().is_empty() {
        headers.insert("Referer".to_owned(), referer.trim().to_owned());
    }
    for line in extra.lines().filter(|line| !line.trim().is_empty()) {
        let (name, value) = line.split_once(':')?;
        let name = name.trim();
        if name.is_empty()
            || name.eq_ignore_ascii_case("referer")
            || headers
                .keys()
                .any(|header| header.eq_ignore_ascii_case(name))
        {
            return None;
        }
        headers.insert(name.to_owned(), value.trim().to_owned());
    }
    let origin = origin.trim();
    (!origin.is_empty() && !headers.is_empty()).then(|| Rule {
        origin: origin.to_owned(),
        headers,
    })
}

fn apply_edit(rules: &mut Vec<Rule>, original: Option<&str>, draft: Option<&Rule>) -> bool {
    match (original, draft) {
        (None, Some(draft)) => rules.push(draft.clone()),
        (Some(origin), draft) => {
            let Some(index) = rules.iter().position(|rule| rule.origin == origin) else {
                return false;
            };
            if let Some(draft) = draft {
                rules[index] = draft.clone();
            } else {
                rules.remove(index);
            }
        }
        (None, None) => return false,
    }
    true
}

fn patch(
    store: &Entity<SettingsStore>,
    original: Option<String>,
    draft: Option<Rule>,
    cx: &mut App,
) {
    store.update(cx, |store, cx| {
        store.mutate_daemon(
            KEY,
            move |raw| {
                let mut rules = parse_list(raw).ok()?;
                if !apply_edit(&mut rules, original.as_deref(), draft.as_ref()) {
                    return None;
                }
                if rules.is_empty() {
                    Some(String::new())
                } else {
                    serde_json::to_string(&rules).ok()
                }
            },
            cx,
        );
    });
}

pub(super) fn group(ctx: &SectionContext, cx: &App) -> Option<SettingsSection> {
    ctx.store.read(cx).daemon_value(KEY)?;
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let add = ctx.t("siteRequestHeadersAdd");
    let invalid = ctx.t("siteRequestHeadersInvalid");
    Some(
        SettingsSection::new()
            .title(ctx.t("siteRequestHeadersTitle"))
            .subtitle(ctx.t("siteRequestHeadersDesc"))
            .row(
                SettingsRow::custom(move |disabled, _, _, cx| {
                    let tokens = active_theme(cx).tokens();
                    let state = store.read(cx);
                    let Some(raw) = state.daemon_value(KEY) else {
                        return div().into_any_element();
                    };
                    let list = parse_list(raw);
                    let disabled = disabled || !state.daemon_connected() || list.is_err();
                    let mut column = v_flex().w_full().gap(tokens.spacing.xxs);
                    match list {
                        Ok(list) => {
                            for (index, rule) in list.into_iter().enumerate() {
                                column = column.child(rule_row(
                                    index,
                                    rule,
                                    &store,
                                    &translator,
                                    disabled,
                                    cx,
                                ));
                            }
                        }
                        Err(_) => column = column.child(field_error(invalid.clone(), cx)),
                    }
                    let add_store = store.clone();
                    let add_translator = translator.clone();
                    column
                        .child(
                            h_flex().w_full().pt(tokens.spacing.sm).justify_end().child(
                                button("site-headers-add", add.clone(), ButtonVariant::Primary, cx)
                                    .disabled(disabled)
                                    .on_click(move |_, window, cx| {
                                        Editor::open(
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
                })
                .keywords([
                    ctx.t("siteRequestHeadersTitle"),
                    ctx.t("siteRequestHeadersDesc"),
                    ctx.t("siteRequestHeadersOrigin"),
                    ctx.t("siteRequestHeadersReferer"),
                    ctx.t("siteRequestHeadersExtra"),
                ]),
            ),
    )
}

fn text(translator: &Translator, key: &str) -> SharedString {
    SharedString::from(translator.text(key).to_owned())
}

fn rule_row(
    index: usize,
    rule: Rule,
    store: &Entity<SettingsStore>,
    translator: &Translator,
    disabled: bool,
    cx: &App,
) -> Div {
    let tokens = active_theme(cx).tokens();
    let origin = SharedString::from(rule.origin.clone());
    let origin_tip = origin.clone();
    let edit_store = store.clone();
    let edit_translator = translator.clone();
    let delete_store = store.clone();
    let delete_translator = translator.clone();
    h_flex()
        .w_full()
        .gap(tokens.spacing.sm)
        .py(tokens.spacing.xxs)
        .child(
            body_text(cx)
                .id(("site-headers-origin", index))
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(tokens.typography.mono.clone())
                .tooltip(move |window, cx| tooltip_view(origin_tip.clone(), window, cx))
                .child(origin.clone()),
        )
        .child(
            row_button(
                ("site-headers-edit", index),
                text(translator, "menuEdit"),
                ButtonVariant::Ghost,
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                Editor::open(
                    edit_store.clone(),
                    edit_translator.clone(),
                    Some(&rule),
                    window,
                    cx,
                );
            }),
        )
        .child(
            row_danger_button(
                ("site-headers-delete", index),
                text(translator, "delete"),
                cx,
            )
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                confirm_delete(
                    delete_store.clone(),
                    &delete_translator,
                    origin.clone(),
                    window,
                    cx,
                );
            }),
        )
}

fn confirm_delete(
    store: Entity<SettingsStore>,
    translator: &Translator,
    origin: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    let cancel = text(translator, "cancel");
    let delete = text(translator, "delete");
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let store = store.clone();
        let original = origin.to_string();
        alert
            .title(dialog_title(delete.clone(), cx))
            .description(origin.clone())
            .footer(fluxdown_ui_components::dialog_footer(
                Some(cancel.clone()),
                delete.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, _, cx| {
                patch(&store, Some(original.clone()), None, cx);
                true
            })
    });
}

struct Editor {
    store: Entity<SettingsStore>,
    translator: Translator,
    original: Option<String>,
    origin: Entity<InputState>,
    referer: Entity<InputState>,
    extra: Entity<TextareaState>,
    invalid: bool,
}

impl Editor {
    fn open(
        store: Entity<SettingsStore>,
        translator: Translator,
        existing: Option<&Rule>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let title = text(
            &translator,
            if existing.is_some() {
                "siteRequestHeadersTitle"
            } else {
                "siteRequestHeadersAdd"
            },
        );
        let (mut referer_value, mut extra_value) = (String::new(), String::new());
        if let Some(rule) = existing {
            for (name, value) in &rule.headers {
                if name.eq_ignore_ascii_case("referer") {
                    referer_value.clone_from(value);
                } else {
                    if !extra_value.is_empty() {
                        extra_value.push('\n');
                    }
                    extra_value.push_str(name);
                    extra_value.push_str(": ");
                    extra_value.push_str(value);
                }
            }
        }
        let original = existing.map(|rule| rule.origin.clone());
        let origin = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://files.example.net")
                .default_value(original.clone().unwrap_or_default())
        });
        let referer = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://www.example.com/")
                .default_value(referer_value)
        });
        let extra = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 6)
                .default_value(extra_value)
        });
        let view = cx.new(|_| Self {
            store,
            translator,
            original,
            origin: origin.clone(),
            referer,
            extra,
            invalid: false,
        });
        window.open_dialog(cx, move |dialog, _, cx| {
            let view = view.clone();
            dialog
                .title(dialog_title(title.clone(), cx))
                .w(active_theme(cx).text_extent(520.))
                .content(move |content, _, _| content.min_h_0().child(view.clone()))
        });
        origin.update(cx, |input, cx| input.focus(window, cx));
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.store.read(cx).daemon_connected() {
            return;
        }
        let Some(draft) = draft(
            &self.origin.read(cx).value(),
            &self.referer.read(cx).value(),
            &self.extra.read(cx).value(),
        ) else {
            self.invalid = true;
            cx.notify();
            return;
        };
        patch(&self.store, self.original.clone(), Some(draft), cx);
        window.close_dialog(cx);
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens();
        let t = |key| text(&self.translator, key);
        let body = form(cx)
            .child(form_field(
                t("siteRequestHeadersOrigin"),
                Input::new(&self.origin)
                    .control(cx)
                    .w_full()
                    .font_family(tokens.typography.mono.clone()),
                None,
                cx,
            ))
            .child(form_field(
                t("siteRequestHeadersReferer"),
                Input::new(&self.referer)
                    .control(cx)
                    .w_full()
                    .font_family(tokens.typography.mono.clone()),
                None,
                cx,
            ))
            .child(form_field(
                t("siteRequestHeadersExtra"),
                Textarea::new(&self.extra)
                    .w_full()
                    .font_family(tokens.typography.mono.clone()),
                Some(t("siteRequestHeadersExtraHint")),
                cx,
            ))
            .children(
                self.invalid
                    .then(|| field_error(t("siteRequestHeadersInvalid"), cx)),
            );
        let cancel = button(
            "site-headers-cancel",
            t("cancel"),
            ButtonVariant::Secondary,
            cx,
        )
        .on_click(|_, window, cx| window.close_dialog(cx));
        let save = button(
            "site-headers-save",
            t("confirm"),
            ButtonVariant::Primary,
            cx,
        )
        .disabled(!self.store.read(cx).daemon_connected())
        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)));
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body("site-headers-form", None, body, cx))
            .child(dialog_footer(
                None,
                [cancel.into_any_element(), save.into_any_element()],
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_edit, draft, parse_list};

    #[test]
    fn site_request_headers_draft_separates_target_from_referer() -> Result<(), &'static str> {
        let rule = draft(
            " https://例子.example:8443 ",
            "https://cdn.example/ref?contains=a:b",
            "X-Hint: two  spaces:with colon\r\n\r\nX-Flag:",
        )
        .ok_or("a complete draft should parse")?;
        assert_eq!(rule.origin, "https://例子.example:8443");
        assert_eq!(
            rule.headers.get("Referer").map(String::as_str),
            Some("https://cdn.example/ref?contains=a:b")
        );
        assert_eq!(
            rule.headers.get("X-Hint").map(String::as_str),
            Some("two  spaces:with colon")
        );
        assert_eq!(rule.headers.get("X-Flag").map(String::as_str), Some(""));
        Ok(())
    }

    #[test]
    fn site_request_headers_draft_rejects_ambiguous_or_missing_fields() {
        for extra in ["no separator", "X-Key: a\nx-key: b", "referer: a", ": a"] {
            assert!(draft("https://target.example", "https://ref.example/", extra).is_none());
        }
        assert!(draft("", "", "X-Key: value").is_none());
        assert!(draft("https://target.example", "", "").is_none());
    }

    #[test]
    fn site_request_headers_edits_keep_unrelated_entries_and_do_not_resurrect_deleted_rules() {
        let mut rules = parse_list(
            r#"[{"origin":"https://other.example","headers":{"X-Key":"other"}},
                {"origin":"https://old.example","headers":{"X-Key":"old"}}]"#,
        )
        .unwrap();
        let replacement = draft("https://new.example", "", "X-Key: new");
        assert!(apply_edit(
            &mut rules,
            Some("https://old.example"),
            replacement.as_ref(),
        ));
        assert_eq!(rules.len(), 2);
        assert_eq!(
            rules.first().map(|rule| rule.origin.as_str()),
            Some("https://other.example")
        );
        assert_eq!(
            rules.last().map(|rule| rule.origin.as_str()),
            Some("https://new.example")
        );
        assert!(!apply_edit(
            &mut rules,
            Some("https://old.example"),
            replacement.as_ref(),
        ));
        assert!(apply_edit(&mut rules, Some("https://new.example"), None));
        assert_eq!(rules.len(), 1);
    }
}
