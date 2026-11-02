//! 更新窗口内容：版本信息、状态行、更新说明，以及「跳过此版本 / 稍后再说 / 立即更新」三个选择。
//!
//! 只在用户主动时打开（菜单「检查更新…」、顶栏更新按钮的手动升级 / 失败状态），从不自动弹出。
//! 状态全部来自 app 常驻的 [`SettingsStore`]（agent 推送的 `UpdateStatusDto` + 偏好），视图只
//! 渲染与转发动作；用户作出不需要窗口继续存在的选择时发出 [`UpdatePromptEvent::Close`]。
//! 点「立即更新」后窗口保持打开，显示下载 / 安装进度直到服务重启。

use fluxdown_protocol::{UpdatePhase, UpdateStatusDto, update_prompt};
use fluxdown_ui_components::{ButtonVariant, button, loading_button};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div,
};
use gpui_component::{h_flex, scroll::ScrollableElement as _, v_flex};
use serde_json::Value;

use crate::sections::about::release_notes;
use crate::store::SettingsStore;
use crate::ui::meta_text;
use crate::update_view;

/// 窗口建议尺寸（逻辑像素）：固定大小，更新说明在窗口内滚动。
pub const UPDATE_PROMPT_WINDOW_WIDTH: f32 = 520.;
pub const UPDATE_PROMPT_WINDOW_HEIGHT: f32 = 460.;

/// 视图对宿主的事件。
pub enum UpdatePromptEvent {
    /// 关闭承载窗口（已跳过 / 稍后再说 / 转去官网手动下载）。
    Close,
}

/// 用户已要求安装且尚未完成（含 RPC 在途）。
#[must_use]
pub fn install_in_flight(status: &UpdateStatusDto) -> bool {
    match status.phase {
        UpdatePhase::Installing => true,
        UpdatePhase::Downloading => status.install_pending,
        _ => false,
    }
}

pub struct UpdatePromptView {
    store: Entity<SettingsStore>,
    translator: Entity<Translator>,
}

impl EventEmitter<UpdatePromptEvent> for UpdatePromptView {}

impl UpdatePromptView {
    pub fn new(
        translator: Entity<Translator>,
        store: Entity<SettingsStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self { store, translator }
    }

    fn installing(&self, cx: &Context<Self>) -> bool {
        let store = self.store.read(cx);
        store.is_busy("updateInstall") || install_in_flight(store.update_status())
    }

    /// 「跳过此版本」：记下该版本（设备本地），此后只有更高版本才再提醒。
    fn skip(&mut self, cx: &mut Context<Self>) {
        let version = self.store.read(cx).update_status().latest_version.clone();
        if !version.is_empty() {
            self.store.update(cx, |store, cx| {
                store.set_pref(
                    update_prompt::SKIPPED_VERSION_KEY,
                    Value::String(version),
                    cx,
                );
            });
        }
        cx.emit(UpdatePromptEvent::Close);
    }

    /// 「立即更新」：可一键更新时交给 agent 下载并安装（窗口留着显示进度）；只能手动升级时
    /// 打开官网下载地址并关窗。
    fn update_now(&mut self, cx: &mut Context<Self>) {
        let status = self.store.read(cx).update_status().clone();
        if update_view::can_install(&status) {
            self.store.update(cx, |store, cx| store.install_update(cx));
        } else if let Some(url) = update_view::manual_url(&status) {
            cx.open_url(url);
            cx.emit(UpdatePromptEvent::Close);
        }
    }
}

impl Render for UpdatePromptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let translator = self.translator.read(cx).clone();
        let status = self.store.read(cx).update_status().clone();
        let installing = self.installing(cx);

        let title = translator.text_with("newVersionFound", &[("v", &status.latest_version)]);
        let current = format!(
            "{}: v{}",
            translator.text("currentVersion"),
            status.current_version
        );
        // 「有新版本」本身就是标题；其余阶段（下载中 / 待重启 / 失败…）与手动升级说明逐行展示。
        let status_line = update_view::status_line(&status)
            .filter(|_| status.phase != UpdatePhase::Available)
            .map(|line| line.text(&translator));
        let failed = status.phase == UpdatePhase::Failed;
        let manual_line = update_view::manual_line(&status).map(|line| line.text(&translator));

        let notes = (!status.notes.is_empty())
            .then(|| release_notes(status.notes.clone(), translator.locale(), cx));

        let manual = update_view::manual_url(&status).is_some();
        let can_act = update_view::can_install(&status) || manual;
        let primary_label = if manual {
            translator.text("updateFailedOpenSite")
        } else {
            translator.text("updateNow")
        };

        let header = v_flex()
            .flex_none()
            .gap(tokens.spacing.xs)
            .p(tokens.spacing.md)
            .child(
                div()
                    .text_size(extended.title.size)
                    .line_height(extended.title.line_height)
                    .font_weight(extended.title.weight)
                    .child(SharedString::from(title)),
            )
            .child(meta_text(cx).child(SharedString::from(current)))
            .children(status_line.map(|line| {
                let text = meta_text(cx).child(SharedString::from(line));
                if failed {
                    text.text_color(tokens.colors.destructive)
                } else {
                    text
                }
            }))
            .children(manual_line.map(|line| meta_text(cx).child(SharedString::from(line))));

        let body = v_flex()
            .id("update-prompt-notes")
            .flex_1()
            .min_h_0()
            .w_full()
            .px(tokens.spacing.md)
            .pb(tokens.spacing.md)
            .overflow_y_scrollbar()
            .children(notes);

        let footer = h_flex()
            .flex_none()
            .justify_between()
            .items_center()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.md)
            .py(tokens.spacing.sm)
            .bg(extended.colors.chrome)
            .border_t(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(
                button(
                    "update-prompt-skip",
                    SharedString::from(translator.text("skipThisVersion").to_owned()),
                    ButtonVariant::Ghost,
                    cx,
                )
                .disabled(installing)
                .on_click(cx.listener(|this, _, _, cx| this.skip(cx))),
            )
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .items_center()
                    .child(
                        button(
                            "update-prompt-later",
                            SharedString::from(translator.text("updateLater").to_owned()),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(installing)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(UpdatePromptEvent::Close))),
                    )
                    .child(
                        loading_button(
                            "update-prompt-now",
                            SharedString::from(primary_label.to_owned()),
                            ButtonVariant::Primary,
                            installing,
                            cx,
                        )
                        .disabled(installing || !can_act)
                        .on_click(cx.listener(|this, _, _, cx| this.update_now(cx))),
                    ),
            );

        v_flex()
            .size_full()
            .min_h_0()
            .bg(tokens.colors.surface)
            .text_color(tokens.colors.foreground)
            .child(header)
            .child(body)
            .child(footer)
    }
}
