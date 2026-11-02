//! 主窗口状态栏右下角的更新按钮，交互对齐 Zed 更新按钮：agent 在后台自行检查并下载新版本，
//! 下载完成后点「重启以更新」即安装并重启。整个过程不弹窗、不抢焦点。平时只显示强调色图标，
//! 鼠标悬浮时展开文字与 ×。
//!
//! 状态全部来自 [`SettingsStore`]（agent 推送的 `UpdateStatusDto` + 偏好），本视图只做映射与
//! 转发动作。可操作的状态（有可用更新 / 待重启 / 失败）可以暂时隐藏；隐藏只在本次界面进程内
//! 有效，状态变化（如下载完成、换了版本）后按钮重新出现。只能手动升级与失败时点击交给宿主打开
//! 更新窗口（说明、更新日志与重试都在那里）。

use std::rc::Rc;

use fluxdown_protocol::{UpdateFailure, UpdatePhase, UpdateStatusDto, update_prompt};
use fluxdown_ui_components::{FluxIcon, tabular_numbers};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, Context, Entity, Global, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Icon, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
};

use crate::store::SettingsStore;
use crate::update_prompt::install_in_flight;
use crate::update_view::{download_percent, failure_key};

/// 宿主动作：打开更新窗口。
pub type OpenUpdatePrompt = Rc<dyn Fn(&mut App)>;

/// 顶栏更新按钮的状态；`None`（见 [`update_badge`]）表示不显示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateBadge {
    /// 有新版本但没有在后台下载（关闭了自动检查、取消过下载，或只能手动升级）。
    Available {
        version: String,
        manual: bool,
    },
    Downloading {
        version: String,
        percent: u8,
    },
    /// 已要求安装：下载完成后替换程序并重启。
    Installing {
        version: String,
    },
    /// 更新包已下载并校验，点击即安装并重启。
    Ready {
        version: String,
    },
    Failed {
        version: String,
        failure: Option<UpdateFailure>,
    },
}

/// 用户刚发起、agent 尚未回报新阶段的请求。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateRequests {
    pub download: bool,
    pub install: bool,
}

/// 点击按钮的效果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateBadgeAction {
    Download,
    Install,
    OpenPrompt,
}

impl UpdateBadge {
    /// 点击效果；进行中的状态（下载中 / 安装中）不可点击。
    #[must_use]
    pub fn action(&self) -> Option<UpdateBadgeAction> {
        match self {
            Self::Available { manual: false, .. } => Some(UpdateBadgeAction::Download),
            Self::Available { manual: true, .. } | Self::Failed { .. } => {
                Some(UpdateBadgeAction::OpenPrompt)
            }
            Self::Ready { .. } => Some(UpdateBadgeAction::Install),
            Self::Downloading { .. } | Self::Installing { .. } => None,
        }
    }

    /// 可以暂时隐藏：只有等待用户决定的状态。
    #[must_use]
    pub fn dismissible(&self) -> bool {
        self.action().is_some()
    }
}

/// 更新状态 → 顶栏按钮。没有新版本、后台检查失败（此时 `has_update` 为假）时不显示；被
/// 「跳过此版本」的版本只在仍未下载时隐藏——用户自己下载 / 安装了它照常显示进度。
#[must_use]
pub fn update_badge(
    status: &UpdateStatusDto,
    skipped: Option<&str>,
    requests: UpdateRequests,
) -> Option<UpdateBadge> {
    if !status.has_update || status.latest_version.is_empty() {
        return None;
    }
    let version = status.latest_version.clone();
    if requests.install || install_in_flight(status) {
        return Some(UpdateBadge::Installing { version });
    }
    match status.phase {
        // 周期复查期间沿用上一轮结果，按钮不闪烁。
        UpdatePhase::Available | UpdatePhase::Checking => {
            if requests.download {
                return Some(UpdateBadge::Downloading {
                    version,
                    percent: 0,
                });
            }
            (skipped != Some(version.as_str())).then(|| UpdateBadge::Available {
                manual: status.manual_reason.is_some(),
                version,
            })
        }
        UpdatePhase::Downloading => Some(UpdateBadge::Downloading {
            percent: download_percent(status),
            version,
        }),
        UpdatePhase::Ready => Some(UpdateBadge::Ready { version }),
        UpdatePhase::Failed => Some(UpdateBadge::Failed {
            version,
            failure: status.failure,
        }),
        UpdatePhase::Idle | UpdatePhase::UpToDate | UpdatePhase::Installing => None,
    }
}

/// 本次界面进程内被隐藏的按钮状态（主窗口重建后仍然有效）。
#[derive(Default)]
struct DismissedBadge(Option<UpdateBadge>);

impl Global for DismissedBadge {}

pub struct UpdateButton {
    store: Entity<SettingsStore>,
    translator: Entity<Translator>,
    open_prompt: OpenUpdatePrompt,
    /// 悬浮时展开文字与 × ；平时只显示图标。
    hovered: bool,
}

impl UpdateButton {
    pub fn new(
        translator: Entity<Translator>,
        store: Entity<SettingsStore>,
        open_prompt: OpenUpdatePrompt,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self {
            store,
            translator,
            open_prompt,
            hovered: false,
        }
    }

    fn current(&self, cx: &App) -> Option<UpdateBadge> {
        let store = self.store.read(cx);
        let requests = UpdateRequests {
            download: store.in_flight("updateDownload"),
            install: store.is_busy("updateInstall"),
        };
        update_badge(
            store.update_status(),
            update_prompt::skipped_version(store.preferences()),
            requests,
        )
        .filter(|badge| {
            cx.try_global::<DismissedBadge>()
                .and_then(|dismissed| dismissed.0.as_ref())
                != Some(badge)
        })
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.current(cx).as_ref().and_then(UpdateBadge::action) else {
            return;
        };
        match action {
            UpdateBadgeAction::Download => {
                self.store.update(cx, |store, cx| store.download_update(cx));
            }
            UpdateBadgeAction::Install => {
                self.store.update(cx, |store, cx| store.install_update(cx));
            }
            UpdateBadgeAction::OpenPrompt => (self.open_prompt)(cx),
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        let badge = self.current(cx).filter(UpdateBadge::dismissible);
        if badge.is_some() {
            cx.set_global(DismissedBadge(badge));
            cx.notify();
        }
    }

    /// 标签、提示与图标；`Ready` 用强调色，`Failed` 用警示色。
    fn presentation(
        badge: &UpdateBadge,
        translator: &Translator,
    ) -> (SharedString, SharedString, Option<FluxIcon>) {
        let text = |key: &str| SharedString::from(translator.text(key).to_owned());
        let with_version = |key: &str, version: &str| {
            SharedString::from(translator.text_with(key, &[("v", version)]))
        };
        match badge {
            UpdateBadge::Available { version, .. } => (
                text("updateTitleAvailable"),
                with_version("newVersionFound", version),
                Some(FluxIcon::CircleArrowDown),
            ),
            UpdateBadge::Downloading { version, percent } => {
                let percent = percent.to_string();
                (
                    SharedString::from(
                        translator.text_with("updateTitleDownloading", &[("percent", &percent)]),
                    ),
                    SharedString::from(translator.text_with(
                        "updateDownloadingProgress",
                        &[("v", version), ("percent", &percent)],
                    )),
                    Some(FluxIcon::Download),
                )
            }
            UpdateBadge::Installing { .. } => (
                text("updateTitleInstalling"),
                text("updateInstalling"),
                None,
            ),
            UpdateBadge::Ready { version } => (
                text("updateTitleReady"),
                with_version("updateReadyToast", version),
                Some(FluxIcon::RotateCw),
            ),
            UpdateBadge::Failed { failure, .. } => (
                text("updateTitleFailed"),
                text(failure_key(*failure)),
                Some(FluxIcon::TriangleAlert),
            ),
        }
    }
}

impl Render for UpdateButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(badge) = self.current(cx) else {
            self.hovered = false;
            return div().into_any_element();
        };
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let radius = theme.components().button_radius;
        let control_height = theme.density().status_control;
        let icon_size = extended.icon.sm;
        let translator = self.translator.read(cx);
        let (label, tooltip, icon) = Self::presentation(&badge, translator);
        let dismiss_label = SharedString::from(translator.text("close").to_owned());
        // 强调色（蓝）表达「有更新」；失败用警示色。
        let color = match badge {
            UpdateBadge::Failed { .. } => extended.colors.warning,
            _ => tokens.colors.primary,
        };
        let clickable = badge.action().is_some();
        let expanded = self.hovered;

        let leading: AnyElement = match icon {
            Some(icon) => Icon::new(icon)
                .size(icon_size)
                .text_color(color)
                .into_any_element(),
            None => Spinner::new()
                .with_size(Size::Size(icon_size))
                .into_any_element(),
        };
        let content = h_flex()
            .items_center()
            .gap(tokens.spacing.xxs)
            .whitespace_nowrap()
            .text_size(extended.caption.size)
            .line_height(extended.caption.line_height)
            .font_features(tabular_numbers())
            .text_color(color)
            .child(leading)
            .when(expanded, |this| this.child(label));
        let main = Button::new("status-update")
            .ghost()
            .with_size(Size::XSmall)
            .h(control_height)
            .px(tokens.spacing.xs)
            .tooltip(tooltip)
            .disabled(!clickable)
            .child(content)
            .on_click(cx.listener(|this, _, _, cx| this.activate(cx)));
        let dismiss = (expanded && badge.dismissible()).then(|| {
            Button::new("status-update-dismiss")
                .ghost()
                .with_size(Size::XSmall)
                .h(control_height)
                .text_color(color)
                .icon(Icon::new(FluxIcon::X).size(icon_size))
                .tooltip(dismiss_label)
                .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)))
        });

        h_flex()
            .id("status-update-badge")
            .flex_none()
            .items_center()
            .rounded(radius)
            .bg(color.opacity(0.12))
            .overflow_hidden()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.hovered != *hovered {
                    this.hovered = *hovered;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(main)
            .children(dismiss)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::UpdateManualReason;

    use super::*;

    fn status(phase: UpdatePhase) -> UpdateStatusDto {
        UpdateStatusDto {
            phase,
            latest_version: "2.0.0".to_owned(),
            has_update: true,
            ..UpdateStatusDto::default()
        }
    }

    fn badge(status: &UpdateStatusDto) -> Option<UpdateBadge> {
        update_badge(status, None, UpdateRequests::default())
    }

    #[test]
    fn hidden_without_a_new_version() {
        for phase in [UpdatePhase::Idle, UpdatePhase::UpToDate] {
            assert_eq!(badge(&status(phase)), None, "{phase:?}");
        }
        // 后台检查失败（没有已知新版本）不打扰用户。
        let mut check_failed = status(UpdatePhase::Failed);
        check_failed.has_update = false;
        assert_eq!(badge(&check_failed), None);
    }

    #[test]
    fn background_download_then_restart() {
        let mut downloading = status(UpdatePhase::Downloading);
        downloading.asset_size = 200;
        downloading.downloaded_bytes = 90;
        let progress = badge(&downloading);
        assert_eq!(
            progress,
            Some(UpdateBadge::Downloading {
                version: "2.0.0".to_owned(),
                percent: 45
            })
        );
        assert_eq!(progress.and_then(|b| b.action()), None, "not clickable");

        let ready = badge(&status(UpdatePhase::Ready));
        assert_eq!(
            ready.as_ref().and_then(UpdateBadge::action),
            Some(UpdateBadgeAction::Install)
        );
        assert!(ready.is_some_and(|b| b.dismissible()));
    }

    #[test]
    fn install_request_shows_installing_until_restart() {
        let requested = UpdateRequests {
            install: true,
            ..UpdateRequests::default()
        };
        let installing = Some(UpdateBadge::Installing {
            version: "2.0.0".to_owned(),
        });
        assert_eq!(
            update_badge(&status(UpdatePhase::Ready), None, requested),
            installing
        );
        assert_eq!(badge(&status(UpdatePhase::Installing)), installing);
        let mut pending = status(UpdatePhase::Downloading);
        pending.install_pending = true;
        assert_eq!(badge(&pending), installing);
    }

    #[test]
    fn available_downloads_or_opens_manual_instructions() {
        let available = badge(&status(UpdatePhase::Available));
        assert_eq!(
            available.and_then(|b| b.action()),
            Some(UpdateBadgeAction::Download)
        );
        let requested = UpdateRequests {
            download: true,
            ..UpdateRequests::default()
        };
        assert!(matches!(
            update_badge(&status(UpdatePhase::Available), None, requested),
            Some(UpdateBadge::Downloading { percent: 0, .. })
        ));

        let mut manual = status(UpdatePhase::Available);
        manual.manual_reason = Some(UpdateManualReason::NotWritable);
        assert_eq!(
            badge(&manual).and_then(|b| b.action()),
            Some(UpdateBadgeAction::OpenPrompt)
        );
        let mut failed = status(UpdatePhase::Failed);
        failed.failure = Some(UpdateFailure::Network);
        assert_eq!(
            badge(&failed).and_then(|b| b.action()),
            Some(UpdateBadgeAction::OpenPrompt)
        );
    }

    #[test]
    fn periodic_recheck_keeps_the_button() {
        assert!(matches!(
            badge(&status(UpdatePhase::Checking)),
            Some(UpdateBadge::Available { .. })
        ));
    }

    #[test]
    fn skipped_version_hides_only_while_not_downloaded() {
        let skip = |phase| update_badge(&status(phase), Some("2.0.0"), UpdateRequests::default());
        assert_eq!(skip(UpdatePhase::Available), None);
        assert!(skip(UpdatePhase::Ready).is_some());
        let mut newer = status(UpdatePhase::Available);
        newer.latest_version = "2.0.1".to_owned();
        assert!(update_badge(&newer, Some("2.0.0"), UpdateRequests::default()).is_some());
    }
}
