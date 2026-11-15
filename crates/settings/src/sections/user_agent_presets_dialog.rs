use fluxdown_protocol::user_agent_presets::{PREF_KEY, UserAgentPreset, from_preference};
use fluxdown_ui_components::{
    ControlExt as _, dialog_scroll_body, dialog_title, field_error, form, form_field,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    v_flex,
};
use serde_json::json;

use crate::{
    store::SettingsStore,
    ui::{danger_ghost_button, dialog_footer, meta_text},
};

pub(crate) fn open(
    store: Entity<SettingsStore>,
    translator: Translator,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text("userAgentManagePresets").to_owned());
    let view = cx.new(|_| PresetList {
        store: store.clone(),
        translator: translator.clone(),
    });
    view.update(cx, |_, cx| {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(680.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
}

struct PresetList {
    store: Entity<SettingsStore>,
    translator: Translator,
}

impl PresetList {
    fn t(&self, key: &str) -> SharedString {
        self.translator.text(key).into()
    }
}

impl Render for PresetList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens();
        let presets = from_preference(self.store.read(cx).pref(PREF_KEY));
        let disabled =
            self.store.read(cx).is_read_only() || self.store.read(cx).preference_pending(PREF_KEY);
        let add_store = self.store.clone();
        let add_translator = self.translator.clone();
        let mut body = v_flex().w_full().gap(tokens.spacing.sm);
        for (key, value) in super::user_agent::UA_PRESETS {
            let label_key = match *key {
                "chrome" => "userAgentPresetChrome",
                "firefox" => "userAgentPresetFirefox",
                "edge" => "userAgentPresetEdge",
                _ => "userAgentPresetSafari",
            };
            body = body.child(
                v_flex()
                    .w_full()
                    .p(tokens.spacing.sm)
                    .child(div().child(self.t(label_key)))
                    .child(meta_text(cx).truncate().child(*value)),
            );
        }
        for (index, preset) in presets.into_iter().enumerate() {
            let edit_store = self.store.clone();
            let edit_translator = self.translator.clone();
            let edit_item = preset.clone();
            let delete_name = preset.name.clone();
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .w_full()
                    .p(tokens.spacing.sm)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(preset.name))
                            .child(meta_text(cx).truncate().child(preset.value)),
                    )
                    .child(
                        Button::new(("ua-preset-edit", index))
                            .outline()
                            .label(self.t("edit"))
                            .control(cx)
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
                        danger_ghost_button(("ua-preset-delete", index), self.t("delete"), cx)
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                let mut items = from_preference(this.store.read(cx).pref(PREF_KEY));
                                if items
                                    .get(index)
                                    .is_some_and(|item| item.name == delete_name)
                                {
                                    items.remove(index);
                                    this.store.update(cx, |store, cx| {
                                        store.clear_feedback(cx);
                                        store.set_pref(PREF_KEY, json!(items), cx);
                                    });
                                }
                            })),
                    ),
            );
        }
        let add = Button::new("ua-preset-add")
            .label(self.t("userAgentAddPreset"))
            .control(cx)
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                open_editor(add_store.clone(), add_translator.clone(), None, window, cx)
            });
        let write_error = self
            .store
            .read(cx)
            .last_error()
            .map(|error| self.t(error.kind.i18n_key()));
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .when_some(write_error, |view, error| {
                view.child(field_error(error, cx))
            })
            .child(dialog_scroll_body(
                "ua-presets-list",
                Some(gpui::px(460.)),
                body,
                cx,
            ))
            .child(dialog_footer(None, [add.into_any_element()], cx))
    }
}

fn open_editor(
    store: Entity<SettingsStore>,
    translator: Translator,
    existing: Option<UserAgentPreset>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text("userAgentManagePresets").to_owned());
    let store_for_editor = store.clone();
    let translator_for_editor = translator.clone();
    let view = cx.new(|cx| {
        PresetEditor::new(
            store_for_editor,
            translator_for_editor,
            existing,
            window,
            cx,
        )
    });
    view.update(cx, |_, cx| {
        cx.observe_in(&store, window, move |editor, store, window, cx| {
            let state = store.read(cx);
            if !editor.saving || state.preference_pending(PREF_KEY) {
                return;
            }
            if let Some(error) = state.last_error() {
                editor.saving = false;
                editor.error = Some(editor.translator.text(error.kind.i18n_key()).into());
            } else if state.pref(PREF_KEY) == editor.expected.as_ref() {
                editor.saving = false;
                editor.saved = true;
                window.close_dialog(cx);
            }
            cx.notify();
        })
        .detach();
    });
    let focus = view.read(cx).name.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(520.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    focus.update(cx, |input, cx| input.focus(window, cx));
}

struct PresetEditor {
    store: Entity<SettingsStore>,
    translator: Translator,
    existing: Option<UserAgentPreset>,
    name: Entity<InputState>,
    value: Entity<InputState>,
    error: Option<SharedString>,
    saving: bool,
    saved: bool,
    expected: Option<serde_json::Value>,
}

impl PresetEditor {
    fn new(
        store: Entity<SettingsStore>,
        translator: Translator,
        existing: Option<UserAgentPreset>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                existing
                    .as_ref()
                    .map_or_else(String::new, |item| item.name.clone()),
            )
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                existing
                    .as_ref()
                    .map_or_else(String::new, |item| item.value.clone()),
            )
        });
        Self {
            store,
            translator,
            existing,
            name,
            value,
            error: None,
            saving: false,
            saved: false,
            expected: None,
        }
    }

    fn t(&self, key: &str) -> SharedString {
        self.translator.text(key).into()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.saved {
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        let value = self.value.read(cx).value().trim().to_owned();
        let has_controls = value
            .bytes()
            .any(|byte| (byte < 32 && byte != b'\t') || byte == 127);
        if name.is_empty() || value.is_empty() || has_controls {
            self.error = Some(self.t("userAgentPresetInvalid"));
            cx.notify();
            return;
        }
        if super::user_agent::UA_PRESETS
            .iter()
            .any(|(_, builtin)| *builtin == value)
        {
            self.error = Some(self.t("userAgentPresetExists"));
            cx.notify();
            return;
        }
        let mut items = from_preference(self.store.read(cx).pref(PREF_KEY));
        if items.iter().any(|item| {
            item.value == value
                && self
                    .existing
                    .as_ref()
                    .is_none_or(|old| old.value != item.value)
        }) {
            self.error = Some(self.t("userAgentPresetExists"));
            cx.notify();
            return;
        }
        let preset = UserAgentPreset { name, value };
        if let Some(old) = &self.existing {
            if let Some(item) = items.iter_mut().find(|item| item.value == old.value) {
                *item = preset;
            }
        } else {
            items.push(preset);
        }
        let desired = json!(items);
        self.expected = Some(desired.clone());
        self.saving = true;
        self.error = None;
        self.store.update(cx, |store, cx| {
            store.clear_feedback(cx);
            store.set_pref(PREF_KEY, desired.clone(), cx);
        });
        let store = self.store.read(cx);
        if let Some(error) = store.last_error() {
            self.saving = false;
            self.error = Some(self.t(error.kind.i18n_key()));
        } else if !store.preference_pending(PREF_KEY) && store.pref(PREF_KEY) == Some(&desired) {
            self.saving = false;
            self.saved = true;
            window.close_dialog(cx);
        }
        cx.notify();
    }
}

impl Render for PresetEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens();
        let body = form(cx)
            .child(form_field(
                self.t("userAgentPresetName"),
                Input::new(&self.name)
                    .control(cx)
                    .disabled(self.saving)
                    .w_full(),
                None,
                cx,
            ))
            .child(form_field(
                self.t("userAgentPresetValue"),
                Input::new(&self.value)
                    .control(cx)
                    .disabled(self.saving)
                    .w_full(),
                None,
                cx,
            ));
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(tokens.spacing.md)
            .child(body)
            .when_some(self.error.clone(), |this, error| {
                this.child(field_error(error, cx))
            })
            .child(dialog_footer(
                None,
                [
                    Button::new("ua-preset-editor-cancel")
                        .outline()
                        .label(self.t("cancel"))
                        .control(cx)
                        .on_click(|_, window, cx| window.close_dialog(cx))
                        .into_any_element(),
                    Button::new("ua-preset-save")
                        .label(self.t("confirm"))
                        .primary()
                        .control(cx)
                        .disabled(self.saving || self.saved)
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)),
                        )
                        .into_any_element(),
                ],
                cx,
            ))
    }
}
