use std::{rc::Rc, sync::Arc};

use fluxdown_protocol::user_agent_presets::{PREF_KEY, UserAgentPreset};
use fluxdown_ui_components::{ControlExt as _, dialog_title, field_error, form, form_field};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
};
use serde_json::json;

use crate::{
    controller::{DownloadsCommand, DownloadsPort},
    model::new_download::UA_PRESETS,
    strings::error_text,
};

pub(crate) type OnPresetSaved = Rc<dyn Fn(UserAgentPreset, &mut Window, &mut App)>;

pub(crate) fn open_save_dialog(
    value: String,
    presets: Vec<UserAgentPreset>,
    translator: Translator,
    port: Arc<dyn DownloadsPort>,
    on_saved: OnPresetSaved,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text("userAgentSavePreset").to_owned());
    let view =
        cx.new(|cx| SavePresetDialog::new(value, presets, translator, port, on_saved, window, cx));
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

struct SavePresetDialog {
    name: Entity<InputState>,
    value: String,
    presets: Vec<UserAgentPreset>,
    translator: Translator,
    port: Arc<dyn DownloadsPort>,
    on_saved: OnPresetSaved,
    error: Option<SharedString>,
    saving: bool,
}

impl SavePresetDialog {
    fn new(
        value: String,
        presets: Vec<UserAgentPreset>,
        translator: Translator,
        port: Arc<dyn DownloadsPort>,
        on_saved: OnPresetSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        Self {
            name,
            value,
            presets,
            translator,
            port,
            on_saved,
            error: None,
            saving: false,
        }
    }

    fn t(&self, key: &str) -> SharedString {
        self.translator.text(key).into()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        let value = self.value.trim().to_owned();
        let invalid = name.is_empty()
            || value.is_empty()
            || value
                .bytes()
                .any(|byte| (byte < 32 && byte != b'\t') || byte == 127);
        let exists = self.presets.iter().any(|preset| preset.value == value)
            || UA_PRESETS.iter().any(|(_, built_in)| *built_in == value);
        if invalid || exists {
            self.error = Some(self.t(if exists {
                "userAgentPresetExists"
            } else {
                "userAgentPresetInvalid"
            }));
            cx.notify();
            return;
        }
        let preset = UserAgentPreset { name, value };
        let mut next = self.presets.clone();
        next.push(preset.clone());
        let port = self.port.clone();
        let on_saved = self.on_saved.clone();
        let handle = window.window_handle();
        let entity = cx.entity();
        let future = port.execute(DownloadsCommand::SetLocalPreference {
            key: PREF_KEY,
            value: json!(next),
        });
        self.saving = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |_, cx| {
            let result = future.await;
            let Ok(()) = handle.update(cx, |_, window, cx| {
                entity.update(cx, |this, cx| {
                    this.saving = false;
                    match result {
                        Ok(_) => {
                            (on_saved)(preset, window, cx);
                            window.close_dialog(cx);
                        }
                        Err(error) => {
                            this.error =
                                Some(SharedString::from(error_text(&this.translator, &error)));
                            cx.notify();
                        }
                    }
                });
            }) else {
                return;
            };
        })
        .detach();
    }
}

impl Render for SavePresetDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens();
        let body = form(cx)
            .child(form_field(
                self.t("userAgentPresetName"),
                Input::new(&self.name).control(cx).w_full(),
                None,
                cx,
            ))
            .child(form_field(
                self.t("userAgentPresetValue"),
                div()
                    .w_full()
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .text_size(tokens.typography.sm.size)
                    .child(self.value.clone()),
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
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("ua-preset-save-cancel")
                            .outline()
                            .label(self.t("cancel"))
                            .control(cx)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("ua-preset-save-confirm")
                            .primary()
                            .label(self.t("confirm"))
                            .control(cx)
                            .disabled(self.saving)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.save(window, cx)
                            })),
                    ),
            )
    }
}
