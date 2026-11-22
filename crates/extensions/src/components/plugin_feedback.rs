use std::sync::Arc;

use fluxdown_ui_components::{ControlExt as _, dialog_scroll_body};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::Button,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    switch::Switch,
    v_flex,
};

use crate::ExtensionsPort;

pub(super) fn open(
    identity: String,
    version: String,
    loaded: Option<bool>,
    translator: Translator,
    port: Arc<dyn ExtensionsPort>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = translator.text("plugFeedbackTitle").to_owned();
    let body = cx.new(|cx| {
        let subject = cx.new(|cx| InputState::new(window, cx));
        let description = cx.new(|cx| TextareaState::new(window, cx));
        let mut form = Feedback {
            identity,
            version,
            translator,
            port,
            subject,
            description,
            repository: None,
            busy: false,
            message: None,
            issue: None,
            loaded: loaded == Some(true),
            diagnostics: false,
            audit_pending: false,
            audit_counts: None,
        };
        if loaded.is_some() {
            form.resolve(cx);
        } else {
            form.message = Some("plugFeedbackUnavailable");
        }
        form
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let body = body.clone();
        dialog
            .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(480.))
            .content(move |root, _, cx| {
                root.min_h_0().child(dialog_scroll_body(
                    "plugin-feedback",
                    None,
                    body.clone(),
                    cx,
                ))
            })
    });
}

struct Feedback {
    identity: String,
    version: String,
    translator: Translator,
    port: Arc<dyn ExtensionsPort>,
    subject: Entity<InputState>,
    description: Entity<TextareaState>,
    repository: Option<String>,
    busy: bool,
    message: Option<&'static str>,
    issue: Option<u64>,
    loaded: bool,
    diagnostics: bool,
    audit_pending: bool,
    audit_counts: Option<(usize, usize, usize)>,
}

impl Feedback {
    fn resolve(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        let future = self.port.feedback(self.identity.clone(), None);
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |form, cx| {
                form.busy = false;
                form.repository = result
                    .as_ref()
                    .ok()
                    .and_then(|value| value["repository"].as_str())
                    .map(str::to_owned);
                if form.repository.is_none() {
                    form.message = Some(
                        match result
                            .as_ref()
                            .ok()
                            .and_then(|value| value["status"].as_u64())
                        {
                            Some(404) => "plugFeedbackUnavailable",
                            Some(429) => "feedbackRateLimited",
                            _ => "plugFeedbackFailed",
                        },
                    );
                }
                cx.notify();
            }) else {
                return;
            };
        })
        .detach();
        cx.notify();
    }

    fn set_diagnostics(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.diagnostics = enabled;
        if enabled && !self.audit_pending && self.audit_counts.is_none() {
            self.audit_pending = true;
            let future =
                crate::controller::list_plugin_audit(&self.port, Some(&self.identity), None, 20);
            cx.spawn(async move |this, cx| {
                let counts = future.await.ok().and_then(|value| {
                    let entries = value["entries"].as_array()?;
                    Some(entries.iter().take(20).fold(
                        (0, 0, 0),
                        |(total, errors, denied), entry| {
                            let outcome = entry["outcome"].as_str();
                            (
                                total + 1,
                                errors + usize::from(outcome == Some("error")),
                                denied + usize::from(outcome == Some("denied")),
                            )
                        },
                    ))
                });
                let Ok(()) = this.update(cx, |form, cx| {
                    form.audit_counts = counts;
                    form.audit_pending = false;
                    cx.notify();
                }) else {
                    return;
                };
            })
            .detach();
        }
        cx.notify();
    }

    fn environment(&self) -> String {
        let os = std::env::consts::OS;
        if self.diagnostics {
            let status = if self.loaded { "loaded" } else { "load failed" };
            let arch = std::env::consts::ARCH;
            if let Some((total, errors, denied)) = self.audit_counts {
                format!(
                    "{os}; {arch}; plugin: {status}; recent audit ({total}): {errors} errors, {denied} denied"
                )
            } else {
                format!("{os}; {arch}; plugin: {status}")
            }
        } else {
            os.to_owned()
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(repository) = self.repository.as_deref() else {
            return;
        };
        if self.busy || (self.diagnostics && self.audit_pending) || self.issue.is_some() {
            return;
        }
        let subject = self.subject.read(cx).value();
        let subject = subject.trim();
        let description = self.description.read(cx).value();
        let description = description.trim();
        if subject.is_empty() {
            self.message = Some("feedbackTitleLabel");
            cx.notify();
            return;
        }
        if description.is_empty() {
            self.message = Some("plugFeedbackDescription");
            cx.notify();
            return;
        }
        self.busy = true;
        self.message = None;
        let report = serde_json::json!({
            "type": "bug", "source": "app",
            "title": subject, "description": description,
            "pluginIdentity": self.identity, "pluginVersion": self.version,
            "appVersion": fluxdown_protocol::APP_VERSION, "environment": self.environment(),
            "repository": repository,
        });
        let future = self.port.feedback(self.identity.clone(), Some(report));
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |form, cx| {
                form.busy = false;
                let result = result.ok();
                form.issue = result
                    .as_ref()
                    .and_then(|value| value["issueNumber"].as_u64());
                if form.issue.is_none() {
                    form.message = Some(
                        match result.as_ref().and_then(|value| value["status"].as_u64()) {
                            Some(409) => "plugFeedbackRouteChanged",
                            Some(404) => "plugFeedbackUnavailable",
                            Some(429) => "feedbackRateLimited",
                            _ => "plugFeedbackFailed",
                        },
                    );
                    if matches!(
                        result.as_ref().and_then(|value| value["status"].as_u64()),
                        Some(404 | 409)
                    ) {
                        form.repository = None;
                        form.resolve(cx);
                    }
                }
                cx.notify();
            }) else {
                return;
            };
        })
        .detach();
        cx.notify();
    }
}

impl Render for Feedback {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = |key| self.translator.text(key).to_owned();
        let mut body = v_flex().w_full().gap(active_theme(cx).tokens().spacing.sm);
        if let Some(issue) = self.issue {
            return body.child(
                self.translator
                    .text_with("plugFeedbackSent", &[("number", &issue.to_string())]),
            );
        }
        if let Some(repository) = &self.repository {
            body = body
                .child(format!("{}: {repository}", text("plugFeedbackDestination")))
                .child(format!(
                    "{} · {} · FluxDown {} · {}",
                    self.identity,
                    self.version,
                    fluxdown_protocol::APP_VERSION,
                    self.environment()
                ))
                .child(text("plugFeedbackPublic"))
                .child(text("feedbackTitleLabel"))
                .child(
                    Input::new(&self.subject)
                        .control(cx)
                        .w_full()
                        .disabled(self.busy),
                )
                .child(text("plugFeedbackDescription"))
                .child(
                    Textarea::new(&self.description)
                        .w_full()
                        .h(px(120.))
                        .disabled(self.busy),
                )
                .child(
                    h_flex()
                        .gap(active_theme(cx).tokens().spacing.sm)
                        .child(
                            Switch::new("plugin-feedback-diagnostics")
                                .checked(self.diagnostics)
                                .disabled(self.busy)
                                .on_click(cx.listener(|form, checked: &bool, _, cx| {
                                    form.set_diagnostics(*checked, cx);
                                })),
                        )
                        .child(text("plugFeedbackDiagnostics")),
                )
                .when(
                    self.diagnostics && !self.audit_pending && self.audit_counts.is_none(),
                    |body| body.child(text("plugFeedbackAuditUnavailable")),
                )
                .child(
                    Button::new("plugin-feedback-send")
                        .control(cx)
                        .label(text("plugFeedbackSend"))
                        .disabled(self.busy || (self.diagnostics && self.audit_pending))
                        .on_click(cx.listener(|form, _, _, cx| form.submit(cx))),
                );
        }
        if let Some(message) = self.message {
            body = body.child(div().child(text(message)));
        }
        if self.repository.is_none() && !self.busy {
            body = body.child(
                Button::new("plugin-feedback-fallback")
                    .outline()
                    .control(cx)
                    .label(text("plugFeedbackFallback"))
                    .on_click(|_, _, cx| cx.open_url("https://fluxdown.com/feedback")),
            );
        }
        body
    }
}
