use std::{rc::Rc, time::Duration};

use fluxdown_protocol::{
    ApplicationErrorCode, CreateGroupRequest, CreateTaskRequest, ResolvePreviewResponse,
    RpcErrorData,
};
use fluxdown_ui_components::ControlExt as _;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement as _, Styled as _, Window,
};
use gpui_component::{WindowExt as _, button::Button, notification::Notification, v_flex};

use super::NewDownloadView;
use crate::{
    controller::{DownloadsCommand, DownloadsResult},
    model::new_download::BuiltRequests,
    pages::manifest::ManifestView,
    strings::error_text,
    submission::{NewDownloadSubmission, SubmissionHooks, run_submission},
};

const PREVIEW_TIMEOUT: Duration = Duration::from_secs(90);

impl NewDownloadView {
    pub(super) fn start_preview(
        &mut self,
        request: CreateTaskRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let generation = self.preview_gate.begin();
        let transaction_id = self
            .captures
            .iter()
            .find(|capture| capture.url == request.url)
            .map(|capture| capture.transaction_id.clone());
        let future = self.port.execute(DownloadsCommand::ResolvePreview {
            request: Box::new(request.clone()),
            transaction_id: transaction_id.clone(),
        });
        self.preview_request = Some((request, transaction_id));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.finish_preview(generation, result, window, cx)
            }) else {
                // 窗口关闭：预解析只读，不再创建任务。
                return;
            };
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(PREVIEW_TIMEOUT).await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.finish_preview(
                    generation,
                    Err(RpcErrorData::new(ApplicationErrorCode::Timeout, true)),
                    window,
                    cx,
                )
            }) else {
                return;
            };
        })
        .detach();
    }

    fn finish_preview(
        &mut self,
        generation: u64,
        result: Result<DownloadsResult, RpcErrorData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.preview_gate.finish(generation) {
            return;
        }
        let Some((request, transaction_id)) = self.preview_request.take() else {
            return;
        };
        let manifest = match result {
            Ok(DownloadsResult::Preview(manifest))
                if manifest.error.is_empty() && !manifest.items.is_empty() =>
            {
                Some(*manifest)
            }
            Ok(DownloadsResult::Preview(manifest)) if manifest.error.is_empty() => None,
            Ok(_) => {
                window.push_notification(
                    Notification::warning(error_text(
                        self.translator.read(cx),
                        &RpcErrorData::new(ApplicationErrorCode::Internal, false),
                    )),
                    cx,
                );
                None
            }
            Err(error) => {
                window.push_notification(
                    Notification::warning(error_text(self.translator.read(cx), &error)),
                    cx,
                );
                None
            }
        };
        match manifest {
            Some(manifest) => self.show_manifest(manifest, request, transaction_id, window, cx),
            None => self.submit_requests(BuiltRequests::from_single(request), window, cx),
        }
    }

    fn show_manifest(
        &mut self,
        manifest: ResolvePreviewResponse,
        request: CreateTaskRequest,
        transaction_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let context = request.clone();
        let submit = Rc::new(move |group, window: &mut Window, cx: &mut App| {
            let Ok(()) = weak.update(cx, |this, cx| {
                this.submit_group(group, context.clone(), transaction_id.clone(), window, cx);
            }) else {
                // 表单释放后不提交。
                return;
            };
        });
        let weak = cx.entity().downgrade();
        let back = Rc::new(move |_window: &mut Window, cx: &mut App| {
            let Ok(()) = weak.update(cx, |this, cx| {
                this.manifest = None;
                cx.notify();
            }) else {
                return;
            };
        });
        self.manifest = Some(cx.new(|cx| {
            ManifestView::new(
                self.translator.clone(),
                manifest,
                request.clone(),
                self.context.queues.clone(),
                self.context
                    .category_route
                    .clone()
                    .filter(|_| self.routes_by_category(cx) && !self.is_remote()),
                self.context.save_dir.clone(),
                submit,
                back,
                window,
                cx,
            )
        }));
        cx.notify();
    }

    fn submit_group(
        &mut self,
        request: CreateGroupRequest,
        context: CreateTaskRequest,
        transaction_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.manifest.is_none() {
            return;
        }
        self.record_save_dir_history(&request.save_dir, cx);
        let port = self.port.clone();
        let timer = cx.background_executor().clone();
        let submission = NewDownloadSubmission::Group {
            request: Box::new(request),
            context: Box::new(context),
            transaction_id: transaction_id.clone(),
            last_category_mode: crate::model::save_dir_category::category_mode_preference(
                &self.category_mode,
            ),
        };
        cx.spawn_in(window, async move |this, cx| {
            let report = run_submission(
                submission,
                move |command| port.execute(command),
                SubmissionHooks::new(move |delay| timer.timer(delay)),
            )
            .await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if report.failed() {
                    if let Some(manifest) = &this.manifest {
                        manifest.update(cx, |view, cx| view.set_submitting(false, cx));
                    }
                    window.push_notification(
                        Notification::error(report.notice(this.translator.read(cx)).message),
                        cx,
                    );
                } else {
                    if let Some(id) = &transaction_id {
                        this.captures
                            .retain(|capture| &capture.transaction_id != id);
                    }
                    window.remove_window();
                }
            }) else {
                // agent 已接管建组；关窗不重复提交，也不恢复已成功消费的捕获。
                return;
            };
        })
        .detach();
    }

    pub(super) fn render_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        v_flex()
            .size_full()
            .bg(tokens.colors.surface)
            .items_center()
            .justify_center()
            .gap(tokens.spacing.lg)
            .child(
                self.translator
                    .read(cx)
                    .text("manifestResolvingLabel")
                    .to_owned(),
            )
            .child(
                Button::new("manifest-resolving-cancel")
                    .outline()
                    .control(cx)
                    .label(
                        self.translator
                            .read(cx)
                            .text("manifestResolvingCancel")
                            .to_owned(),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.preview_gate.cancel();
                        this.preview_request = None;
                        cx.notify();
                    })),
            )
    }
}
