//! 主窗口状态栏右下角的版本条目（chrome 布局条目 [`VERSION_WIDGET_ID`]）：平时显示当前版本号，
//! 有新版本时就地变成更新入口——更新提示与版本号是同一个控件，不再另挂更新按钮。
//!
//! 更新流程对齐 Zed 更新按钮：agent 在后台自行检查并下载新版本，条目依次显示「→ v新版本」
//! 「下载进度」「重启以更新」，整个过程不弹窗、不抢焦点。单击弹出菜单：当前状态对应的更新动作、
//! 检查更新、复制诊断信息（用于问题反馈）、打开日志目录、关于。
//!
//! 状态全部来自 [`SettingsStore`]（agent 推送的 `UpdateStatusDto` + 偏好），本视图只做映射；
//! 需要窗口 / 系统能力的动作经 [`VersionHost`] 交给宿主。被用户在布局里隐藏时，宿主经
//! [`VersionButton::needs_attention`] 在有更新状态时临时显示它，更新提示不会因此丢失。

use std::rc::Rc;

use fluxdown_protocol::{
    APP_VERSION, PROTOCOL_VERSION, UpdateFailure, UpdatePhase, UpdateStatusDto, update_prompt,
};
use fluxdown_ui_components::{FluxIcon, tabular_numbers};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Icon, Sizable as _, Size, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    spinner::Spinner,
    v_flex,
};

use crate::store::SettingsStore;
use crate::update_prompt::install_in_flight;
use crate::update_view::{download_percent, failure_key, status_line};

/// chrome 布局条目 id（与 Web `web/src/shell/chromeCatalog.ts` 一致）。
pub const VERSION_WIDGET_ID: &str = "appVersion";
/// 编辑布局时的占位 / 拖拽预览文案键。
pub const VERSION_WIDGET_LABEL_KEY: &str = "chromeWidgetVersion";

/// 宿主动作。
pub type VersionHostAction = Rc<dyn Fn(&mut App)>;

/// 版本菜单里需要宿主能力的动作。
#[derive(Clone)]
pub struct VersionHost {
    /// 打开更新窗口（只能手动升级 / 失败时的说明、更新日志与重试）。
    pub open_update_prompt: VersionHostAction,
    /// 立即检查更新并提示结果（有新版本即打开更新窗口）。
    pub check_update: VersionHostAction,
    pub open_logs_folder: VersionHostAction,
    /// 打开设置「关于」页。
    pub open_about: VersionHostAction,
}

/// 版本条目的更新状态；`None`（见 [`update_badge`]）表示只显示版本号。
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

/// 菜单里的更新动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateBadgeAction {
    Download,
    Install,
    OpenPrompt,
}

impl UpdateBadge {
    /// 更新动作；进行中的状态（下载中 / 安装中）没有。
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
}

/// 更新状态 → 版本条目的更新形态。没有新版本、后台检查失败（此时 `has_update` 为假）时为
/// `None`；被「跳过此版本」的版本只在仍未下载时不提示——用户自己下载 / 安装了它照常显示进度。
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
        // 周期复查期间沿用上一轮结果，条目不闪烁。
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

/// 「复制诊断信息」的纯文本，可直接贴进问题反馈。只含版本、渠道、协议、平台、安装形态与界面
/// 语言——绝不含路径、账户、设备名或下载地址。格式与 Web `web/src/lib/diagnosticInfo.ts` 一致。
#[must_use]
pub fn diagnostic_text(status: &UpdateStatusDto, locale: &str) -> String {
    let mut text = format!("FluxDown v{APP_VERSION}");
    if !status.channel.is_empty() {
        text.push_str(&format!(" ({})", status.channel));
    }
    text.push('\n');
    // 界面与 agent 版本不一致（如更新只完成了一半）时单独列出，这正是反馈最需要的信息。
    if !status.current_version.is_empty() && status.current_version != APP_VERSION {
        text.push_str(&format!("Service: v{}\n", status.current_version));
    }
    text.push_str(&format!("Protocol: {PROTOCOL_VERSION}\n"));
    text.push_str(&format!(
        "Platform: {} {} ({})\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        install_kind_name(status),
    ));
    text.push_str("Client: desktop\n");
    text.push_str(&format!("Locale: {locale}"));
    if status.has_update && !status.latest_version.is_empty() {
        text.push_str(&format!("\nLatest: v{}", status.latest_version));
    }
    text
}

/// 安装形态的 wire 名（camelCase，与 Web 侧一致）。
fn install_kind_name(status: &UpdateStatusDto) -> String {
    match serde_json::to_value(status.install_kind) {
        Ok(serde_json::Value::String(name)) => name,
        // 单元枚举恒序列化为字符串；其它结果按未知处理。
        Ok(_) | Err(_) => "unknown".to_owned(),
    }
}

/// 决定外观与菜单内容的状态快照；只在它变化时重绘，shell 观察本视图决定是否临时显示。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct VersionState {
    badge: Option<UpdateBadge>,
    checking: bool,
}

pub struct VersionButton {
    store: Entity<SettingsStore>,
    translator: Entity<Translator>,
    host: VersionHost,
    state: VersionState,
    /// 菜单相对条目的锚点：随所在区域变化（状态栏向上弹，顶栏向下弹）。
    anchor: Anchor,
}

impl VersionButton {
    pub fn new(
        translator: Entity<Translator>,
        store: Entity<SettingsStore>,
        host: VersionHost,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        cx.observe(&store, |this, _, cx| {
            let state = this.compute(cx);
            if state != this.state {
                this.state = state;
                cx.notify();
            }
        })
        .detach();
        let mut this = Self {
            store,
            translator,
            host,
            state: VersionState::default(),
            anchor: Anchor::BottomRight,
        };
        this.state = this.compute(cx);
        this
    }

    /// 有更新状态（新版本 / 下载中 / 待重启 / 失败）：被布局隐藏时宿主据此临时显示。
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.state.badge.is_some()
    }

    /// 按摆放位置设置菜单锚点；未变化时不写，避免渲染期无条件改写实体。
    pub fn set_anchor(&mut self, anchor: Anchor, cx: &mut Context<Self>) {
        if self.anchor != anchor {
            self.anchor = anchor;
            cx.notify();
        }
    }

    fn compute(&self, cx: &App) -> VersionState {
        let store = self.store.read(cx);
        let status = store.update_status();
        let requests = UpdateRequests {
            download: store.in_flight("updateDownload"),
            install: store.is_busy("updateInstall"),
        };
        VersionState {
            badge: update_badge(
                status,
                update_prompt::skipped_version(store.preferences()),
                requests,
            ),
            checking: status.phase == UpdatePhase::Checking,
        }
    }

    /// 条目外观：前置图标（`None` = 转圈）、版本号后缀、悬浮提示。
    fn presentation(
        &self,
        status: &UpdateStatusDto,
        translator: &Translator,
    ) -> (Option<Option<FluxIcon>>, Option<SharedString>, SharedString) {
        let text = |key: &str| translator.text(key).to_owned();
        let with_version = |key: &str, version: &str| translator.text_with(key, &[("v", version)]);
        let title = format!("FluxDown v{APP_VERSION}");
        let Some(badge) = &self.state.badge else {
            let line = status_line(status).map(|line| line.text(translator));
            let tooltip = match line {
                Some(line) => format!("{title} · {line}"),
                None => title,
            };
            let leading = self.state.checking.then_some(None);
            return (leading, None, SharedString::from(tooltip));
        };
        let (icon, suffix, tooltip) = match badge {
            UpdateBadge::Available { version, .. } => (
                Some(FluxIcon::CircleArrowDown),
                format!("→ v{version}"),
                with_version("newVersionFound", version),
            ),
            UpdateBadge::Downloading { version, percent } => {
                let percent = percent.to_string();
                (
                    Some(FluxIcon::Download),
                    format!("→ v{version} · {percent}%"),
                    translator.text_with(
                        "updateDownloadingProgress",
                        &[("v", version), ("percent", &percent)],
                    ),
                )
            }
            UpdateBadge::Installing { .. } => (
                None,
                text("updateTitleInstalling"),
                text("updateInstalling"),
            ),
            UpdateBadge::Ready { version } => (
                Some(FluxIcon::RotateCw),
                text("updateTitleReady"),
                with_version("updateReadyToast", version),
            ),
            UpdateBadge::Failed { failure, .. } => (
                Some(FluxIcon::TriangleAlert),
                text("updateTitleFailed"),
                text(failure_key(*failure)),
            ),
        };
        (
            Some(icon),
            Some(SharedString::from(suffix)),
            SharedString::from(tooltip),
        )
    }
}

/// 菜单：标题（版本 + 渠道 / 状态）→ 更新动作 → 检查更新、复制诊断信息、日志、关于。
fn version_menu(menu: PopupMenu, entity: &Entity<VersionButton>, cx: &App) -> PopupMenu {
    let this = entity.read(cx);
    let translator = this.translator.read(cx);
    let text = |key: &str| SharedString::from(translator.text(key).to_owned());
    let status = this.store.read(cx).update_status();
    let channel_key = if status.channel == "frontier" {
        "updateChannelFrontier"
    } else {
        "updateChannelStable"
    };
    let mut subtitle = translator.text(channel_key).to_owned();
    if let Some(line) = status_line(status) {
        subtitle.push_str(" · ");
        subtitle.push_str(&line.text(translator));
    }
    let title = SharedString::from(format!("FluxDown v{APP_VERSION}"));
    let subtitle = SharedString::from(subtitle);
    let mut menu = menu
        .max_w(active_theme(cx).density().status_control * 14.)
        .item(
            PopupMenuItem::element(move |_, cx| {
                let theme = active_theme(cx);
                let caption = &theme.extended().caption;
                v_flex()
                    .w_full()
                    .min_w_0()
                    .child(div().font_features(tabular_numbers()).child(title.clone()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(caption.size)
                            .line_height(caption.line_height)
                            .text_color(theme.tokens().colors.muted_foreground)
                            .child(subtitle.clone()),
                    )
            })
            .disabled(true),
        )
        .separator();

    let store = this.store.clone();
    let open_prompt = this.host.open_update_prompt.clone();
    if let Some(badge) = &this.state.badge
        && let Some(action) = badge.action()
    {
        let (label, icon) = match (action, badge) {
            (UpdateBadgeAction::Download, UpdateBadge::Available { version, .. }) => (
                SharedString::from(
                    translator.text_with("downloadUpdateVersion", &[("v", version)]),
                ),
                FluxIcon::Download,
            ),
            (UpdateBadgeAction::Install, _) => (text("updateTitleReady"), FluxIcon::RotateCw),
            _ => (text("updateViewDetails"), FluxIcon::CircleArrowDown),
        };
        let action_store = store.clone();
        menu = menu.item(PopupMenuItem::new(label).icon(icon).on_click(
            move |_, _, cx| match action {
                UpdateBadgeAction::Download => {
                    action_store.update(cx, |store, cx| store.download_update(cx));
                }
                UpdateBadgeAction::Install => {
                    action_store.update(cx, |store, cx| store.install_update(cx));
                }
                UpdateBadgeAction::OpenPrompt => open_prompt(cx),
            },
        ));
        // 「跳过此版本」与更新窗口同一偏好：该版本不再自动下载，条目回到只显示版本号。
        if let UpdateBadge::Available { version, .. } = badge {
            let version = version.clone();
            menu = menu.item(
                PopupMenuItem::new(text("skipThisVersion"))
                    .icon(FluxIcon::X)
                    .on_click(move |_, _, cx| {
                        let version = serde_json::Value::String(version.clone());
                        store.update(cx, |store, cx| {
                            store.set_pref(update_prompt::SKIPPED_VERSION_KEY, version, cx);
                        });
                    }),
            );
        }
        menu = menu.separator();
    }

    let check = this.host.check_update.clone();
    let logs = this.host.open_logs_folder.clone();
    let about = this.host.open_about.clone();
    let copy_store = this.store.clone();
    let copy_translator = this.translator.clone();
    menu.item(
        PopupMenuItem::new(text("checkUpdate"))
            .icon(FluxIcon::RotateCw)
            .disabled(this.state.checking)
            .on_click(move |_, _, cx| check(cx)),
    )
    .item(
        PopupMenuItem::new(text("copyDiagnosticInfo"))
            .icon(FluxIcon::Copy)
            .on_click(move |_, window, cx| {
                let translator = copy_translator.read(cx).clone();
                copy_diagnostics(&copy_store, &translator, window, cx);
            }),
    )
    .item(
        PopupMenuItem::new(text("menuOpenLogsFolder"))
            .icon(FluxIcon::FolderOpen)
            .on_click(move |_, _, cx| logs(cx)),
    )
    .item(
        PopupMenuItem::new(text("menuAbout"))
            .icon(FluxIcon::Info)
            .on_click(move |_, _, cx| about(cx)),
    )
}

/// 复制诊断信息并提示（版本菜单与设置「关于」页共用）。
pub(crate) fn copy_diagnostics(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    window: &mut Window,
    cx: &mut App,
) {
    let text = diagnostic_text(store.read(cx).update_status(), translator.locale());
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    let copied = translator.text("diagnosticInfoCopied").to_owned();
    window.push_notification(Notification::success(copied), cx);
}

impl Render for VersionButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let radius = theme.components().button_radius;
        let control_height = theme.density().status_control;
        let icon_size = extended.icon.sm;
        let translator = self.translator.read(cx);
        let status = self.store.read(cx).update_status();
        let (leading, suffix, tooltip) = self.presentation(status, translator);
        // 有更新用强调色（蓝），失败用警示色；只有版本号时与其它状态栏读数同为次要文字色。
        let color = match &self.state.badge {
            None => tokens.colors.muted_foreground,
            Some(UpdateBadge::Failed { .. }) => extended.colors.warning,
            Some(_) => tokens.colors.primary,
        };
        let highlighted = self.state.badge.is_some();
        let leading: Option<AnyElement> = leading.map(|icon| match icon {
            Some(icon) => Icon::new(icon)
                .size(icon_size)
                .text_color(color)
                .into_any_element(),
            None => Spinner::new()
                .with_size(Size::Size(icon_size))
                .into_any_element(),
        });
        let content = h_flex()
            .items_center()
            .gap(tokens.spacing.xxs)
            .whitespace_nowrap()
            .text_size(extended.caption.size)
            .line_height(extended.caption.line_height)
            .font_features(tabular_numbers())
            .text_color(color)
            .children(leading)
            .child(SharedString::from(format!("v{APP_VERSION}")))
            .children(suffix);
        let view = cx.entity();
        let anchor = self.anchor;
        let trigger = Button::new("status-app-version")
            .ghost()
            .with_size(Size::XSmall)
            .h(control_height)
            .px(tokens.spacing.xs)
            .tooltip(tooltip)
            .child(content)
            .dropdown_menu_with_anchor(anchor, move |menu, window, cx| {
                let mut state = view.read(cx).state.clone();
                // 只在状态快照变化时重建已打开菜单。
                cx.observe_in(&view, window, move |menu, entity, window, cx| {
                    let current = entity.read(cx).state.clone();
                    if state != current {
                        state = current;
                        menu.rebuild(window, cx, |menu, _, cx| version_menu(menu, &entity, cx));
                    }
                })
                .detach();
                version_menu(menu, &view, cx)
            });

        h_flex()
            .id("status-app-version-badge")
            .flex_none()
            .items_center()
            .rounded(radius)
            .when(highlighted, |this| this.bg(color.opacity(0.12)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(trigger)
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{UpdateInstallKind, UpdateManualReason};

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
    fn only_version_without_a_new_version() {
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
        assert_eq!(progress.and_then(|b| b.action()), None, "no update action");

        let ready = badge(&status(UpdatePhase::Ready));
        assert_eq!(
            ready.as_ref().and_then(UpdateBadge::action),
            Some(UpdateBadgeAction::Install)
        );
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
    fn periodic_recheck_keeps_the_update_state() {
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

    #[test]
    fn diagnostic_text_reports_versions_and_platform_only() {
        let mut current = UpdateStatusDto {
            current_version: APP_VERSION.to_owned(),
            channel: "stable".to_owned(),
            install_kind: UpdateInstallKind::MacosApp,
            ..UpdateStatusDto::default()
        };
        let text = diagnostic_text(&current, "zh");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("FluxDown v{APP_VERSION} (stable)"));
        assert_eq!(lines[1], format!("Protocol: {PROTOCOL_VERSION}"));
        assert_eq!(
            lines[2],
            format!(
                "Platform: {} {} (macosApp)",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
        assert_eq!(&lines[3..], ["Client: desktop", "Locale: zh"]);

        // 界面与服务版本不一致、已知新版本时都要写出来。
        current.current_version = "0.0.1".to_owned();
        current.has_update = true;
        current.latest_version = "9.9.9".to_owned();
        let text = diagnostic_text(&current, "en");
        assert!(text.contains("\nService: v0.0.1\n"), "{text}");
        assert!(text.ends_with("\nLatest: v9.9.9"), "{text}");
    }
}
