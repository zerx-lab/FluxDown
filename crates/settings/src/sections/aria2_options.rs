//! Advanced aria2-style config editor: draft validation and revision-checked apply.

use fluxdown_protocol::{
    DaemonConfigSnapshot, RpcErrorData, aria2_options::Aria2OptionsResponse, method,
};
use fluxdown_ui_components::{BusyHold, ButtonVariant, button, loading_button};
use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, ParentElement, SharedString, Styled,
    Subscription, Window, div,
};
use gpui_component::{
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    v_flex,
};
use serde_json::{Value, json};

use super::SectionContext;
use crate::{
    store::SettingsStore,
    ui::{SettingsRow, body_text, meta_text},
};

const RPC_ARIA2_OPTIONS: &str = method::DAEMON_CONFIG_ARIA2_OPTIONS;

type Aria2OptionsDto = Aria2OptionsResponse;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BusyAction {
    Apply,
    Reload,
}
struct Aria2Editor {
    store: Entity<SettingsStore>,
    input: Entity<TextareaState>,
    _input_subscription: Subscription,
    strings: Strings,
    open: bool,
    loading: bool,
    applying: bool,
    revision: Option<u64>,
    defaults: String,
    error: Option<String>,
    status: Option<String>,
    busy_hold: BusyHold,
    busy_action: Option<BusyAction>,
}

#[derive(Clone)]
struct Strings {
    title: SharedString,
    description: SharedString,
    open: SharedString,
    close: SharedString,
    apply: SharedString,
    defaults: SharedString,
    export: SharedString,
    reload: SharedString,
    saved: SharedString,
    loading: SharedString,
    priority: SharedString,
    restart: SharedString,
    disconnected: SharedString,
    conflict: SharedString,
    invalid: SharedString,
    failed: SharedString,
    copied: SharedString,
    busy: SharedString,
    open_hint: SharedString,
}

impl Strings {
    fn new(ctx: &SectionContext) -> Self {
        let t = |key| ctx.t(key);
        Self {
            title: t("aria2AdvancedTitle"),
            description: t("aria2AdvancedDescription"),
            open: t("aria2AdvancedOpen"),
            close: t("aria2AdvancedClose"),
            apply: t("aria2AdvancedApply"),
            defaults: t("aria2AdvancedDefaults"),
            export: t("aria2AdvancedExport"),
            reload: t("aria2AdvancedReload"),
            saved: t("aria2AdvancedSaved"),
            loading: t("aria2AdvancedLoading"),
            priority: t("aria2AdvancedPriority"),
            restart: t("aria2AdvancedRestart"),
            disconnected: t("localServiceDisconnected"),
            conflict: t("localServiceConflict"),
            invalid: t("aria2AdvancedInvalid"),
            failed: t("localServiceActionFailed"),
            copied: t("aria2AdvancedExported"),
            busy: t("aria2AdvancedBusy"),
            open_hint: t("aria2AdvancedOpenHint"),
        }
    }
}

pub(crate) fn row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let strings = Strings::new(ctx);
    let search_title = strings.title.clone();
    SettingsRow::custom(move |_, row_key, window, cx| {
        let editor = window.use_keyed_state(
            SharedString::from(format!("{row_key}-aria2-options")),
            cx,
            {
                let store = store.clone();
                let strings = strings.clone();
                move |window, cx| {
                    let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(12, 12));
                    let _input_subscription =
                        cx.subscribe(&input, |_: &mut Aria2Editor, _, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Change) {
                                cx.notify();
                            }
                        });
                    Aria2Editor {
                        store,
                        input,
                        _input_subscription,
                        strings,
                        open: false,
                        loading: false,
                        applying: false,
                        revision: None,
                        defaults: String::new(),
                        error: None,
                        busy_hold: BusyHold::default(),
                        busy_action: None,
                        status: None,
                    }
                }
            },
        );
        editor.read(cx).render(&editor, cx)
    })
    .keywords([search_title])
}

impl Aria2Editor {
    fn connected(&self, cx: &App) -> bool {
        self.store.read(cx).daemon_connected()
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = !self.open;
        self.error = None;
        self.status = None;
        if self.open && self.revision.is_none() {
            self.load(true, window, cx);
        }
        cx.notify();
    }

    fn load(&mut self, replace_draft: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.applying {
            return;
        }
        if !self.connected(cx) {
            self.error = Some(self.strings.disconnected.to_string());
            cx.notify();
            return;
        }
        self.loading = true;
        self.error = None;
        self.status = None;
        let future = self.store.read(cx).raw_call(RPC_ARIA2_OPTIONS, json!({}));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(decode_options);
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(options) => {
                        if let Some(error) = options.error {
                            this.error = Some(error);
                        } else {
                            this.revision = Some(options.revision);
                            this.defaults = options.defaults;
                            if replace_draft {
                                this.input.update(cx, |input, cx| {
                                    input.set_value(options.text, window, cx);
                                });
                            }
                            this.status = Some(this.strings.open_hint.to_string());
                        }
                    }
                    Err(error) => this.show_rpc_error(&error),
                }
                cx.notify();
            }) else {
                return;
            };
        })
        .detach();
    }

    fn reset_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(self.defaults.clone(), window, cx);
        });
        self.error = None;
        self.status = None;
        cx.notify();
    }
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.busy_hold.start(cx);
        self.busy_action = Some(BusyAction::Reload);
        self.load(true, window, cx);
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let draft = self.input.read(cx).value().to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(draft));
        self.status = Some(self.strings.copied.to_string());
        self.error = None;
        cx.notify();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.applying {
            return;
        }
        let Some(revision) = self.revision else {
            self.error = Some(self.strings.failed.to_string());
            cx.notify();
            return;
        };
        if !self.connected(cx) {
            self.error = Some(self.strings.disconnected.to_string());
            cx.notify();
            return;
        }
        let text = self.input.read(cx).value().to_string();
        let future = self
            .store
            .read(cx)
            .raw_call(RPC_ARIA2_OPTIONS, json!({ "text": text }));
        self.busy_hold.start(cx);
        self.busy_action = Some(BusyAction::Apply);
        self.applying = true;
        self.error = None;
        self.status = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(decode_options);
            let Ok(()) = this.update_in(cx, |this, window, cx| match result {
                Ok(preview) => {
                    if let Some(error) = preview.error.as_deref() {
                        this.applying = false;
                        this.error = Some(error.to_owned());
                        cx.notify();
                    } else {
                        this.patch(preview, revision, window, cx);
                    }
                }
                Err(error) => {
                    this.applying = false;
                    this.show_rpc_error(&error);
                    cx.notify();
                }
            }) else {
                return;
            };
        })
        .detach();
    }

    fn patch(
        &mut self,
        preview: Aria2OptionsDto,
        expected_revision: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let params = json!({
            "expectedRevision": expected_revision,
            "values": preview.values,
        });
        let future = self
            .store
            .read(cx)
            .raw_call(method::DAEMON_CONFIG_PATCH, params);
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<DaemonConfigSnapshot>(value).map_err(|_| {
                    RpcErrorData::new(fluxdown_protocol::ApplicationErrorCode::Internal, false)
                })
            });
            let Ok(()) = this.update_in(cx, |this, window, cx| match result {
                Ok(snapshot) => {
                    this.revision = Some(snapshot.revision);
                    store.update(cx, |store, cx| store.absorb_daemon_snapshot(snapshot, cx));
                    this.reload_after_apply(window, cx);
                }
                Err(error) => {
                    this.applying = false;
                    this.show_rpc_error(&error);
                    cx.notify();
                }
            }) else {
                return;
            };
        })
        .detach();
    }

    fn reload_after_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let future = self.store.read(cx).raw_call(RPC_ARIA2_OPTIONS, json!({}));
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(decode_options);
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.applying = false;
                match result {
                    Ok(options) => {
                        if let Some(error) = options.error {
                            this.error = Some(error);
                        } else {
                            this.revision = Some(options.revision);
                            this.defaults = options.defaults;
                            this.input.update(cx, |input, cx| {
                                input.set_value(options.text, window, cx);
                            });
                            this.status = Some(this.strings.saved.to_string());
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!(
                            "{}: {}",
                            this.strings.saved,
                            rpc_error_text(&this.strings, &error)
                        ));
                    }
                }
                cx.notify();
            }) else {
                return;
            };
        })
        .detach();
    }

    fn show_rpc_error(&mut self, error: &RpcErrorData) {
        self.error = Some(rpc_error_text(&self.strings, error));
    }

    fn render(&self, entity: &Entity<Self>, cx: &App) -> gpui::Div {
        let theme = fluxdown_ui_theme::active_theme(cx);
        let tokens = theme.tokens();
        let connected = self.connected(cx);
        let busy = self.loading || self.applying || self.busy_hold.is_held();
        let strings = self.strings.clone();
        let open_label = if self.open {
            strings.close.clone()
        } else {
            strings.open.clone()
        };
        let mut content = v_flex().w_full().gap(tokens.spacing.md);
        content = content.child(body_text(cx).child(strings.title.clone()));
        content = content.child(body_text(cx).child(strings.description.clone()));
        content = content.child(meta_text(cx).child(strings.priority.clone()));
        content = content.child(meta_text(cx).child(strings.restart.clone()));
        if !connected {
            content = content.child(meta_text(cx).child(strings.disconnected.clone()));
        } else if busy {
            content = content.child(meta_text(cx).child(strings.busy.clone()));
        }
        content = content.child(
            h_flex().gap(tokens.spacing.sm).child(
                button(
                    "aria2-options-toggle",
                    open_label,
                    ButtonVariant::Secondary,
                    cx,
                )
                .disabled(busy || !connected)
                .on_click({
                    let entity = entity.clone();
                    move |_, window, cx| entity.update(cx, |this, cx| this.toggle(window, cx))
                }),
            ),
        );
        if self.open {
            if self.loading {
                content = content.child(meta_text(cx).child(strings.loading.clone()));
            }
            content = content.child(
                Textarea::new(&self.input)
                    .w_full()
                    .disabled(busy || !connected)
                    .aria_label(strings.title.clone()),
            );
            content = content.child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .flex_wrap()
                    .child(
                        loading_button(
                            "aria2-options-apply",
                            strings.apply.clone(),
                            ButtonVariant::Primary,
                            self.busy_action == Some(BusyAction::Apply) && self.busy_hold.is_held(),
                            cx,
                        )
                        .disabled(
                            !connected
                                || self.loading
                                || self.revision.is_none()
                                || self.applying
                                || self.busy_hold.is_held(),
                        )
                        .on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                entity.update(cx, |this, cx| this.apply(window, cx))
                            }
                        }),
                    )
                    .child(
                        button(
                            "aria2-options-defaults",
                            strings.defaults.clone(),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(busy || self.defaults.is_empty())
                        .on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                entity.update(cx, |this, cx| this.reset_defaults(window, cx))
                            }
                        }),
                    )
                    .child(
                        button(
                            "aria2-options-export",
                            strings.export.clone(),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(busy)
                        .on_click({
                            let entity = entity.clone();
                            move |_, _, cx| entity.update(cx, |this, cx| this.export(cx))
                        }),
                    )
                    .child(
                        loading_button(
                            "aria2-options-reload",
                            strings.reload.clone(),
                            ButtonVariant::Secondary,
                            self.busy_action == Some(BusyAction::Reload)
                                && self.busy_hold.is_held(),
                            cx,
                        )
                        .disabled(!connected || self.applying || self.loading)
                        .on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                entity.update(cx, |this, cx| this.reload(window, cx))
                            }
                        }),
                    ),
            );
            if let Some(error) = &self.error {
                content = content.child(
                    meta_text(cx)
                        .text_color(
                            fluxdown_ui_theme::active_theme(cx)
                                .tokens()
                                .colors
                                .destructive,
                        )
                        .child(error.clone()),
                );
            }
            if let Some(status) = &self.status {
                content = content.child(meta_text(cx).child(status.clone()));
            }
        }
        div().w_full().child(content)
    }
}

fn decode_options(value: Value) -> Result<Aria2OptionsDto, RpcErrorData> {
    serde_json::from_value(value)
        .map_err(|_| RpcErrorData::new(fluxdown_protocol::ApplicationErrorCode::Internal, false))
}

fn rpc_error_text(strings: &Strings, error: &RpcErrorData) -> String {
    match error.code {
        fluxdown_protocol::ApplicationErrorCode::Unavailable => strings.disconnected.to_string(),
        fluxdown_protocol::ApplicationErrorCode::Conflict => strings.conflict.to_string(),
        fluxdown_protocol::ApplicationErrorCode::InvalidArgument => {
            let field = error.field.as_deref().unwrap_or("");
            if field.is_empty() {
                strings.invalid.to_string()
            } else {
                format!("{} ({field})", strings.invalid)
            }
        }
        _ => strings.failed.to_string(),
    }
}
