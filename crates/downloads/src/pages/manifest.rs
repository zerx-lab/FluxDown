//! Embedded manifest picker for the existing new-download window.

mod controls;
mod rows;

use std::rc::Rc;

use fluxdown_protocol::{CreateGroupRequest, CreateTaskRequest, ResolvePreviewResponse};
use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, field_hint, field_label, form, form_field, input_with_action,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Div, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::Button,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::{
    model::{
        format_bytes,
        manifest::{ManifestSelection, ManifestStat, default_group_name},
    },
    pages::new_download::NewDownloadQueue,
};

type ManifestSubmit = Rc<dyn Fn(CreateGroupRequest, &mut Window, &mut App)>;
type ManifestBack = Rc<dyn Fn(&mut Window, &mut App)>;

/// Owns only the picker; returning and submitting leave window lifetime to its host.
pub(crate) struct ManifestView {
    translator: Entity<Translator>,
    selection: ManifestSelection,
    /// Identity of the plugin that produced the manifest; copied into the group request.
    plugin_id: String,
    archive_metadata: Option<fluxdown_protocol::daemon::ArchiveMetadataDto>,
    category_route: Option<fluxdown_protocol::CategoryRouteDto>,
    offered_save_dir: String,
    base: CreateTaskRequest,
    queues: Vec<NewDownloadQueue>,
    group_name: Entity<InputState>,
    save_dir: Entity<InputState>,
    search: Entity<InputState>,
    picking: bool,
    submitting: bool,
    /// 当前（或最近一次）提交走「稍后下载」：在途时只让被点击的那一侧按钮转圈。
    submitting_paused: bool,
    on_submit: ManifestSubmit,
    on_back: ManifestBack,
}

impl ManifestView {
    /// Advanced request settings are inherited intact from `base`; Back lets the
    /// caller expose the original form rather than introducing another editor.
    #[allow(
        clippy::too_many_arguments,
        reason = "embeds a manifest with its existing form context and callbacks"
    )]
    pub(crate) fn new(
        translator: Entity<Translator>,
        manifest: ResolvePreviewResponse,
        base: CreateTaskRequest,
        queues: Vec<NewDownloadQueue>,
        category_route: Option<fluxdown_protocol::CategoryRouteDto>,
        offered_save_dir: String,
        on_submit: ManifestSubmit,
        on_back: ManifestBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = default_group_name(&manifest.name, &base.url);
        let name_hint = translator
            .read(cx)
            .text("manifestGroupNamePlaceholder")
            .to_owned();
        let dir_hint = translator.read(cx).text("selectSaveDir").to_owned();
        let search_hint = translator
            .read(cx)
            .text("manifestSearchPlaceholder")
            .to_owned();
        let group_name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(name)
                .placeholder(name_hint)
        });
        let save_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(base.save_dir.clone())
                .placeholder(dir_hint)
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(search_hint));
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.selection.set_search(input.read(cx).value().as_str());
                cx.notify();
            }
        })
        .detach();
        for input in [&group_name, &save_dir] {
            cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
        }
        search.update(cx, |input, cx| input.focus(window, cx));
        Self {
            translator,
            selection: ManifestSelection::new(manifest.items),
            plugin_id: manifest.plugin_id,
            archive_metadata: manifest.archive_metadata,
            base,
            queues,
            category_route,
            offered_save_dir,
            group_name,
            save_dir,
            search,
            picking: false,
            submitting: false,
            submitting_paused: false,
            on_submit,
            on_back,
        }
    }

    /// The host clears this after a failed request so the same selection can retry.
    pub(crate) fn set_submitting(&mut self, submitting: bool, cx: &mut Context<Self>) {
        self.submitting = submitting;
        cx.notify();
    }

    fn t(&self, key: &str, cx: &App) -> SharedString {
        self.translator.read(cx).text(key).to_owned().into()
    }
    fn text_with(&self, key: &str, arguments: &[(&str, &str)], cx: &App) -> SharedString {
        self.translator.read(cx).text_with(key, arguments).into()
    }
    fn summary(&self, stat: ManifestStat, selected: bool, cx: &App) -> SharedString {
        if selected && stat.count == 0 {
            return self.t("manifestNoSelection", cx);
        }
        let translator = self.translator.read(cx);
        let count = stat.count.to_string();
        let size = format_bytes(stat.size);
        let mut summary = translator.text_with(
            if selected {
                "manifestSelectedSummary"
            } else {
                "manifestSummary"
            },
            &[("count", &count), ("size", &size)],
        );
        if stat.unknown > 0 {
            summary.push(' ');
            summary.push_str(&translator.text_with(
                "manifestUnknownSizeNote",
                &[("count", &stat.unknown.to_string())],
            ));
        }
        summary.into()
    }
    fn submit(
        &mut self,
        paused: bool,
        queue: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.submitting || self.picking {
            return;
        }
        let default_queue = if paused && !self.base.start_paused {
            fluxdown_protocol::LATER_QUEUE_ID
        } else {
            &self.base.queue_id
        };
        let queue = queue.as_deref().unwrap_or(default_queue);
        let submitted_save_dir = self.save_dir.read(cx).value().to_string();
        let category_route = self
            .category_route
            .clone()
            .filter(|route| route.follows(&submitted_save_dir, &self.offered_save_dir));
        let save_dir = category_route.as_ref().map_or_else(
            || submitted_save_dir.clone(),
            |route| route.base_dir(&submitted_save_dir, &self.offered_save_dir),
        );
        let Some(mut request) = self.selection.build_request(
            &self.base,
            self.group_name.read(cx).value().as_str(),
            &save_dir,
            queue,
            &self.plugin_id,
            paused,
        ) else {
            return;
        };
        request.archive_metadata = self.archive_metadata.clone();
        request.category_route = category_route;
        self.submitting_paused = paused;
        self.set_submitting(true, cx);
        (self.on_submit)(request, window, cx);
    }
    fn pick_save_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking || self.submitting {
            return;
        }
        self.picking = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.t("selectSaveDir", cx)),
        });
        cx.spawn_in(window, async move |this, cx| {
            let (path, failed) = match receiver.await {
                Ok(Ok(Some(paths))) => {
                    (paths.first().map(|path| path.display().to_string()), false)
                }
                Ok(Ok(None)) => (None, false),
                Ok(Err(_)) => (None, true),
                Err(_) => (None, true),
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking = false;
                if let Some(path) = path {
                    this.save_dir
                        .update(cx, |input, cx| input.set_value(path, window, cx));
                }
                if failed {
                    window.push_notification(
                        Notification::error(this.t("filePickerErrorNative", cx)),
                        cx,
                    );
                }
                cx.notify();
            }) else {
                // The form or its window has gone away; stop the picker callback.
                return;
            };
        })
        .detach();
    }
    fn render_fields(&self, cx: &mut Context<Self>) -> Div {
        form(cx)
            .child(form_field(
                self.t("manifestGroupNameTooltip", cx),
                Input::new(&self.group_name)
                    .control(cx)
                    .w_full()
                    .disabled(self.submitting),
                None,
                cx,
            ))
            .child(form_field(
                self.t("saveDir", cx),
                input_with_action(
                    Input::new(&self.save_dir)
                        .control(cx)
                        .w_full()
                        .disabled(self.submitting),
                    Button::new("manifest-browse")
                        .outline()
                        .icon(FluxIcon::FolderOpen)
                        .label(self.t("browse", cx))
                        .control(cx)
                        .disabled(self.picking || self.submitting)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.pick_save_dir(window, cx)
                        })),
                    cx,
                ),
                None,
                cx,
            ))
    }
}

impl Render for ManifestView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let body = v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.lg)
            .py(tokens.spacing.md)
            .child(field_label(self.t("manifestDialogTitle", cx), cx))
            .child(field_hint(
                self.summary(self.selection.total_stat(), false, cx),
                cx,
            ))
            .child(self.render_fields(cx))
            .child(self.render_toolbar(cx))
            .child(self.render_breadcrumb(cx))
            .child(self.render_rows(cx));
        v_flex()
            .size_full()
            .bg(tokens.colors.surface)
            .child(div().flex_1().min_h_0().child(body.overflow_y_scrollbar()))
            .child(self.render_footer(cx))
    }
}
