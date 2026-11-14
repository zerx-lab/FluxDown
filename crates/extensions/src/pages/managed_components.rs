//! 受管组件子页：每个组件（ffmpeg / yt-dlp）一张卡片 —— 生效状态、系统 PATH、
//! 手动路径、托管安装（版本列表 / 安装 / 更新 / 卸载 / 下载进度）。

use fluxdown_protocol::{
    ApplicationErrorCode, ComponentKind, ComponentStatusDto, ComponentVersions,
    DaemonConfigSnapshot, RpcErrorData,
};
use fluxdown_ui_components::{
    BusyExt as _, BusyHold, ControlExt as _, FluxIcon, IconControlExt as _, card,
    input_with_action, tabular_numbers,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement,
    SharedString, Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    spinner::Spinner,
    v_flex,
};

use super::Frame;
use crate::{
    ExtensionsView,
    controller::{COMPONENT_KINDS, ComponentSummary, component_slot, component_summary},
    error_text, ui,
};

/// 组件标题文案键（插件依赖提醒也用它显示组件名）。
pub fn title_key(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Ffmpeg => "componentsFfmpegTitle",
        ComponentKind::Ytdlp => "componentsYtdlpTitle",
    }
}

fn desc_key(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Ffmpeg => "componentsFfmpegDesc",
        ComponentKind::Ytdlp => "componentsYtdlpDesc",
    }
}

fn path_hint_key(kind: ComponentKind, platform: &str) -> &'static str {
    match (kind, platform) {
        (ComponentKind::Ffmpeg, "windows") => "componentsManualPathHintFfmpeg",
        (ComponentKind::Ytdlp, "windows") => "componentsManualPathHintYtdlp",
        (ComponentKind::Ffmpeg, "macos") => "componentsManualPathHintFfmpegMacos",
        (ComponentKind::Ytdlp, "macos") => "componentsManualPathHintYtdlpMacos",
        (ComponentKind::Ffmpeg, _) => "componentsManualPathHintFfmpegLinux",
        (ComponentKind::Ytdlp, _) => "componentsManualPathHintYtdlpLinux",
    }
}

fn install_desc_key(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Ffmpeg => "componentsInstallSectionDescFfmpeg",
        ComponentKind::Ytdlp => "componentsInstallSectionDescYtdlp",
    }
}

fn source_key(source: &str) -> &'static str {
    match source {
        "manual" => "componentsSourceManual",
        "managed" => "componentsSourceManaged",
        _ => "componentsSourceSystem",
    }
}

/// 1024 进制的人类可读字节数（`0 B` / `1.5 KB` / `12.3 MB` / `1.0 GB`）。
pub fn format_bytes(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes.max(0), UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 单个组件卡片的本地 UI 状态（快照之外：版本列表、进度、在途操作、路径输入框）。
#[derive(Default)]
pub(crate) struct ComponentUi {
    pub versions: Vec<String>,
    pub latest_stable: String,
    pub versions_loading: bool,
    pub versions_error: Option<String>,
    pub versions_requested: bool,
    pub selected_version: Option<String>,
    /// 展示进度区（本视图发起的安装，或引擎推送的外部安装进度）。
    pub installing: bool,
    /// 本视图发起的安装 RPC 在途。
    pub install_pending: bool,
    pub uninstalling: bool,
    pub saving_path: bool,
    /// 在途的路径写入由「清除」按钮发起（Save 按钮不转圈）。
    pub clearing_path: bool,
    /// 点击「获取版本 / 重试」后的最短转圈保持（渲染时自动拉取不启动）。
    pub versions_hold: BusyHold,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    /// 引擎推送的最近一次安装结果（携带服务端 message，RPC 错误不透传）。
    pub last_result: Option<(bool, String)>,
    pub path_input: Option<Entity<InputState>>,
    /// 最近一次同步进输入框的配置值；配置变更且输入框未聚焦时才覆盖用户输入。
    pub path_synced: String,
}

impl ExtensionsView {
    pub(crate) fn render_components(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        for kind in COMPONENT_KINDS {
            self.prepare_component(kind, window, cx);
        }
        let theme = active_theme(cx);
        let frame = Frame {
            translator: self.translator.read(cx),
            tokens: theme.tokens(),
            extended: theme.extended(),
            components: theme.components(),
            stale: self.controller.is_stale(),
        };
        let cards = COMPONENT_KINDS
            .into_iter()
            .map(|kind| {
                self.render_component_card(kind, frame, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        v_flex()
            .w_full()
            .gap(frame.tokens.spacing.lg)
            .children(cards)
    }

    /// 渲染前的可变准备：创建路径输入框、同步配置值、懒拉版本列表。
    fn prepare_component(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = component_slot(kind);
        let manual_path = self.controller.manual_path(kind).to_owned();
        if self.components[slot].path_input.is_none() {
            let placeholder = self
                .translator
                .read(cx)
                .text(path_hint_key(kind, std::env::consts::OS))
                .to_owned();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(manual_path.clone())
                    .placeholder(placeholder)
            });
            cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.save_manual_path_from_input(kind, window, cx);
                    }
                },
            )
            .detach();
            let ui = &mut self.components[slot];
            ui.path_input = Some(input);
            ui.path_synced = manual_path.clone();
        }
        let ui = &mut self.components[slot];
        if ui.path_synced != manual_path
            && let Some(input) = &ui.path_input
            && !input.read(cx).focus_handle(cx).is_focused(window)
        {
            ui.path_synced = manual_path.clone();
            input.update(cx, |input, cx| input.set_value(manual_path, window, cx));
        }
        let supported = self
            .controller
            .component(kind)
            .is_some_and(|status| component_summary(status).managed_supported);
        if supported && !self.components[slot].versions_requested && !self.controller.is_stale() {
            self.fetch_versions(kind, window, cx);
        }
    }

    fn render_component_card(
        &self,
        kind: ComponentKind,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator, tokens, ..
        } = frame;
        let slot = component_slot(kind);
        let ui = &self.components[slot];
        let status = self.controller.component(kind).map(component_summary);
        let title = translator.text(title_key(kind)).to_owned();
        card(cx)
            .flex()
            .flex_col()
            .w_full()
            .gap(tokens.spacing.md)
            .p(tokens.spacing.lg)
            .child(
                v_flex()
                    .gap(tokens.spacing.xxs)
                    .child(ui::title_text(title.clone(), frame))
                    .child(ui::meta_text(
                        translator.text(desc_key(kind)).to_owned(),
                        frame,
                    )),
            )
            .child(self.render_status_row(&title, status, frame))
            .when_some(status, |this, status| {
                this.child(self.render_system_path_row(status, frame))
            })
            .child(ui::divider(frame))
            .child(self.render_manual_path(kind, ui, &title, frame, cx))
            .child(ui::divider(frame))
            .child(self.render_install_section(kind, ui, status, &title, frame, cx))
    }

    fn render_status_row(
        &self,
        title: &str,
        status: Option<ComponentSummary<'_>>,
        frame: Frame<'_>,
    ) -> impl IntoElement {
        let Frame {
            translator,
            tokens,
            extended,
            stale,
            ..
        } = frame;
        let Some(status) = status else {
            // 快照已到但没有该组件：daemon 未编译组件支持（或当前平台不可用）。
            if !stale {
                return ui::status_line(
                    FluxIcon::CircleAlert,
                    extended.colors.warning,
                    translator.text("settingsUnsupportedOnPlatform").to_owned(),
                    frame,
                );
            }
            return h_flex()
                .gap(tokens.spacing.sm)
                .items_center()
                .child(
                    Spinner::new()
                        .with_size(extended.icon.md)
                        .color(tokens.colors.muted_foreground),
                )
                .child(ui::meta_text(
                    translator.text("componentsStatusLoading").to_owned(),
                    frame,
                ));
        };
        if status.source == "none" {
            let key = if status.managed_supported {
                "componentsStatusNotFound"
            } else {
                "componentsStatusNotFoundUnsupported"
            };
            return ui::status_line(
                FluxIcon::CircleAlert,
                extended.colors.warning,
                translator.text_with(key, &[("name", title)]),
                frame,
            );
        }
        ui::status_line(
            FluxIcon::CircleCheck,
            extended.colors.success,
            v_flex()
                .gap(tokens.spacing.xs)
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .items_center()
                        .flex_wrap()
                        .child(ui::neutral_pill(
                            translator.text(source_key(status.source)).to_owned(),
                            frame,
                        ))
                        .when(!status.version.is_empty(), |this| {
                            this.child(
                                div()
                                    .font_features(tabular_numbers())
                                    .child(format!("v{}", status.version)),
                            )
                        }),
                )
                .child(div().w_full().truncate().child(status.path.to_owned())),
            frame,
        )
    }

    fn render_system_path_row(
        &self,
        status: ComponentSummary<'_>,
        frame: Frame<'_>,
    ) -> impl IntoElement {
        let Frame {
            translator, tokens, ..
        } = frame;
        let found = !status.system_path.is_empty();
        ui::status_line(
            FluxIcon::SquareTerminal,
            tokens.colors.muted_foreground,
            h_flex()
                .gap(tokens.spacing.xs)
                .child(
                    div()
                        .flex_none()
                        .child(translator.text("componentsSystemPathLabel").to_owned()),
                )
                .child(div().flex_1().min_w_0().truncate().child(if found {
                    status.system_path.to_owned()
                } else {
                    translator.text("componentsSystemPathNotFound").to_owned()
                })),
            frame,
        )
    }

    fn render_manual_path(
        &self,
        kind: ComponentKind,
        ui: &ComponentUi,
        title: &str,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;
        let slot = component_slot(kind);
        let busy = stale || ui.saving_path;
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                v_flex()
                    .gap(tokens.spacing.xxs)
                    .child(ui::title_text(
                        translator.text("componentsManualPathLabel").to_owned(),
                        frame,
                    ))
                    .child(ui::meta_text(
                        translator.text_with("componentsManualPathDesc", &[("name", title)]),
                        frame,
                    )),
            )
            .child(input_with_action(
                div().w_full().children(
                    ui.path_input
                        .as_ref()
                        .map(|input| Input::new(input).control(cx).w_full().disabled(busy)),
                ),
                h_flex()
                    .gap(tokens.spacing.sm)
                    .items_center()
                    .child(
                        Button::new(("component-path-save", slot))
                            .outline()
                            .control(cx)
                            .label(translator.text("componentsManualPathSave").to_owned())
                            .busy(ui.saving_path && !ui.clearing_path)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_manual_path_from_input(kind, window, cx);
                            })),
                    )
                    .child(
                        Button::new(("component-path-clear", slot))
                            .ghost()
                            .control_icon(cx)
                            .icon(FluxIcon::X)
                            .tooltip(translator.text("componentsManualPathClear").to_owned())
                            .busy(ui.clearing_path)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.clear_manual_path(kind, window, cx);
                            })),
                    ),
                cx,
            ))
    }

    fn render_install_section(
        &self,
        kind: ComponentKind,
        ui: &ComponentUi,
        status: Option<ComponentSummary<'_>>,
        title: &str,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator,
            tokens,
            extended,
            stale,
            ..
        } = frame;
        let slot = component_slot(kind);
        let supported = status.is_none_or(|status| status.managed_supported);
        if !supported {
            return ui::status_line(
                FluxIcon::Info,
                tokens.colors.muted_foreground,
                translator.text_with("componentsManagedUnsupported", &[("name", title)]),
                frame,
            );
        }
        let has_managed = status.is_some_and(|status| status.has_managed());
        let managed_version = status.map(|status| status.managed_version.to_owned());
        let busy = stale || ui.installing || ui.install_pending || ui.uninstalling;
        let versions_busy = ui.versions_loading || ui.versions_hold.is_held();
        let version_label = ui.selected_version.clone().unwrap_or_else(|| {
            translator
                .text(if ui.versions_loading {
                    "componentsVersionsLoading"
                } else {
                    "componentsVersionSelectPlaceholder"
                })
                .to_owned()
        });
        let versions = ui.versions.clone();
        let selected = ui.selected_version.clone();
        let view = cx.entity().downgrade();
        let menu_max_h = active_theme(cx).density().control * 8.;
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(ui::title_text(
                        translator.text("componentsInstallSectionTitle").to_owned(),
                        frame,
                    ))
                    .child(
                        Button::new(("component-versions-refresh", slot))
                            .ghost()
                            .control(cx)
                            .label(translator.text("componentsFetchVersionsButton").to_owned())
                            .busy(versions_busy)
                            .disabled(versions_busy || stale)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.components[slot].versions_hold.start(cx);
                                this.fetch_versions(kind, window, cx);
                            })),
                    ),
            )
            .child(ui::meta_text(
                translator.text(install_desc_key(kind)).to_owned(),
                frame,
            ))
            .when_some(managed_version.filter(|_| has_managed), |this, version| {
                this.child(ui::meta_text(
                    translator.text_with("componentsManagedVersionLabel", &[("version", &version)]),
                    frame,
                ))
            })
            .when_some(ui.versions_error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .items_center()
                        .child(
                            ui::status_line(
                                FluxIcon::CircleAlert,
                                tokens.colors.destructive,
                                translator.text_with(
                                    "componentsVersionsLoadFailed",
                                    &[("message", &error)],
                                ),
                                frame,
                            )
                            .flex_1()
                            .min_w_0(),
                        )
                        .child(
                            Button::new(("component-versions-retry", slot))
                                .outline()
                                .control(cx)
                                .label(translator.text("componentsRetryVersions").to_owned())
                                .busy(versions_busy)
                                .disabled(versions_busy || stale)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.components[slot].versions_hold.start(cx);
                                    this.fetch_versions(kind, window, cx);
                                })),
                        ),
                )
            })
            // 版本下拉吃满剩余宽度，安装 / 卸载与之同行、同为统一控件档。
            .child(input_with_action(
                Button::new(("component-version-select", slot))
                    .outline()
                    .label(version_label)
                    .dropdown_caret(true)
                    .control(cx)
                    .w_full()
                    .disabled(versions.is_empty() || busy)
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                        let menu = versions.iter().fold(menu, |menu, version| {
                            let checked = selected.as_deref() == Some(version.as_str());
                            let view = view.clone();
                            let version = version.clone();
                            menu.item(
                                PopupMenuItem::new(version.clone())
                                    .checked(checked)
                                    .on_click(move |_, _, cx| {
                                        let Ok(()) = view.update(cx, |this, cx| {
                                            this.components[slot].selected_version =
                                                Some(version.clone());
                                            cx.notify();
                                        }) else {
                                            // 扩展视图已释放，结束回调，不再更新状态。
                                            return;
                                        };
                                    }),
                            )
                        });
                        menu.scrollable(true).max_h(menu_max_h)
                    }),
                h_flex()
                    .gap(tokens.spacing.sm)
                    .items_center()
                    .child(
                        Button::new(("component-install", slot))
                            .primary()
                            .control(cx)
                            .label(
                                translator
                                    .text(if ui.installing {
                                        "componentsInstalling"
                                    } else if has_managed {
                                        "componentsReinstallButton"
                                    } else {
                                        "componentsInstallButton"
                                    })
                                    .to_owned(),
                            )
                            .busy(ui.installing)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.install_component(kind, window, cx);
                            })),
                    )
                    .when(has_managed, |this| {
                        this.child(
                            Button::new(("component-uninstall", slot))
                                .outline()
                                .control(cx)
                                .label(translator.text("componentsUninstallButton").to_owned())
                                .busy(ui.uninstalling)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_uninstall_component(kind, window, cx);
                                })),
                        )
                    }),
                cx,
            ))
            .when(ui.installing, |this| {
                let fraction = (ui.total_bytes > 0)
                    .then(|| (ui.downloaded_bytes as f64 / ui.total_bytes as f64).clamp(0., 1.));
                let text = match fraction {
                    Some(fraction) => format!(
                        "{:.1}%  {} / {}",
                        fraction * 100.,
                        format_bytes(ui.downloaded_bytes),
                        format_bytes(ui.total_bytes)
                    ),
                    None => format!(
                        "{} · {}",
                        format_bytes(ui.downloaded_bytes),
                        translator.text("componentsInstallUnknownSize")
                    ),
                };
                this.child(
                    v_flex()
                        .w_full()
                        .gap(tokens.spacing.xs)
                        .child(
                            Progress::new(("component-progress", slot))
                                .w_full()
                                .color(tokens.colors.primary)
                                .loading(fraction.is_none())
                                .value(fraction.map_or(0., |fraction| (fraction * 100.) as f32)),
                        )
                        .child(
                            ui::meta_text(text, frame)
                                .font_features(tabular_numbers())
                                .text_size(extended.caption.size),
                        ),
                )
            })
    }

    fn fetch_versions(&mut self, kind: ComponentKind, window: &mut Window, cx: &mut Context<Self>) {
        let slot = component_slot(kind);
        let ui = &mut self.components[slot];
        if ui.versions_loading {
            return;
        }
        ui.versions_requested = true;
        ui.versions_loading = true;
        ui.versions_error = None;
        let future = self.controller.component_versions(kind);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<ComponentVersions>(value).map_err(|_| protocol_error())
            });

            let Ok(()) = this.update(cx, |this, cx| {
                let message = result
                    .as_ref()
                    .err()
                    .map(|error| error_text(this.translator.read(cx), error));
                let ui = &mut this.components[slot];
                ui.versions_loading = false;
                match result {
                    Ok(versions) => {
                        ui.versions = versions.versions;
                        ui.latest_stable = versions.latest_stable;
                        let selected_known = ui
                            .selected_version
                            .as_ref()
                            .is_some_and(|selected| ui.versions.contains(selected));
                        if !selected_known {
                            ui.selected_version = if ui.latest_stable.is_empty() {
                                ui.versions.first().cloned()
                            } else {
                                Some(ui.latest_stable.clone())
                            };
                        }
                    }
                    Err(_) => ui.versions_error = message,
                }
                cx.notify();
            }) else {
                // 扩展视图已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn install_component(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = component_slot(kind);
        let ui = &mut self.components[slot];
        if ui.install_pending || ui.uninstalling {
            return;
        }
        ui.install_pending = true;
        ui.installing = true;
        ui.downloaded_bytes = 0;
        ui.total_bytes = 0;
        ui.last_result = None;
        let version = ui
            .selected_version
            .clone()
            .filter(|version| !version.is_empty());
        let future = self.controller.install_component(kind, version);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                let ui = &mut this.components[slot];
                ui.install_pending = false;
                ui.installing = false;
                let last_result = ui.last_result.take();
                this.finish_component_op(kind, false, result, last_result, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn confirm_uninstall_component(
        &self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        let name = translator.text(title_key(kind)).to_owned();
        let title = SharedString::from(
            translator.text_with("componentsUninstallConfirmTitle", &[("name", &name)]),
        );
        let body = SharedString::from(
            translator.text_with("componentsUninstallConfirmMsg", &[("name", &name)]),
        );
        let ok = SharedString::from(translator.text("componentsUninstallButton").to_owned());
        let cancel = SharedString::from(translator.text("cancel").to_owned());
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let view = view.clone();
            alert
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(body.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel.clone()),
                    ok.clone(),
                    fluxdown_ui_components::DialogIntent::Destructive,
                    cx,
                ))
                .on_ok(move |_, window, cx| {
                    // 页面已释放时未提交卸载，不能报告确认成功。
                    view.update(cx, |this, cx| this.uninstall_component(kind, window, cx))
                        .is_ok()
                })
        });
    }

    fn uninstall_component(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = component_slot(kind);
        let ui = &mut self.components[slot];
        if ui.uninstalling || ui.install_pending {
            return;
        }
        ui.uninstalling = true;
        let future = self.controller.uninstall_component(kind);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.components[slot].uninstalling = false;
                this.finish_component_op(kind, true, result, None, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn finish_component_op(
        &mut self,
        kind: ComponentKind,
        uninstall: bool,
        result: Result<serde_json::Value, RpcErrorData>,
        last_result: Option<(bool, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        let name = translator.text(title_key(kind)).to_owned();
        match result {
            Ok(_) => {
                let key = if uninstall {
                    "componentsUninstallSuccess"
                } else {
                    "componentsInstallSuccess"
                };
                let message = translator.text_with(key, &[("name", &name)]);
                self.toast_success(message, window, cx);
            }
            Err(error) => {
                // 引擎推送的结果携带真实错误说明；RPC 错误只有错误码。
                let detail = match last_result {
                    Some((false, message)) if !message.is_empty() => message,
                    _ => error_text(translator, &error),
                };
                let key = if uninstall {
                    "componentsUninstallFailed"
                } else {
                    "componentsInstallFailed"
                };
                let message = translator.text_with(key, &[("message", &detail)]);
                self.toast_error(message, window, cx);
            }
        }
    }

    fn save_manual_path_from_input(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = component_slot(kind);
        let Some(input) = &self.components[slot].path_input else {
            return;
        };
        let path = input.read(cx).value().trim().to_owned();
        self.save_manual_path(kind, path, window, cx);
    }

    fn clear_manual_path(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = component_slot(kind);
        if let Some(input) = &self.components[slot].path_input {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let was_saving = self.components[slot].saving_path;
        self.save_manual_path(kind, String::new(), window, cx);
        let ui = &mut self.components[slot];
        if !was_saving && ui.saving_path {
            ui.clearing_path = true;
        }
    }

    /// 写手动路径（空串 = 清除）；成功后用新配置修订号更新控制器并重新探测组件状态。
    pub(crate) fn save_manual_path(
        &mut self,
        kind: ComponentKind,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slot = component_slot(kind);
        let ui = &mut self.components[slot];
        if ui.saving_path {
            return;
        }
        if path == self.controller.manual_path(kind) {
            return;
        }
        ui.saving_path = true;
        ui.path_synced.clone_from(&path);
        let future = self.controller.set_manual_path(kind, path);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<DaemonConfigSnapshot>(value).map_err(|_| protocol_error())
            });

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.components[slot].saving_path = false;
                this.components[slot].clearing_path = false;
                match result {
                    Ok(config) => {
                        this.controller.apply_config(&config);
                        this.refresh_component_status(kind, window, cx);
                    }
                    Err(error) => {
                        let message = error_text(this.translator.read(cx), &error);
                        this.toast_error(message, window, cx);
                    }
                }
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn refresh_component_status(
        &mut self,
        kind: ComponentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let future = self.controller.component_status(kind);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(value) = future.await else {
                return;
            };
            let Ok(status) = serde_json::from_value::<ComponentStatusDto>(value) else {
                return;
            };

            let Ok(()) = this.update(cx, |this, cx| {
                this.controller.apply_component_status(status);
                cx.notify();
            }) else {
                // 扩展视图已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }
}

fn protocol_error() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::ProtocolIncompatible, false)
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{ComponentKind, PathStyle};
    use fluxdown_ui_i18n::I18nCatalog;

    use super::{format_bytes, path_hint_key};

    #[test]
    fn format_bytes_scales_by_1024() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(-5), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(12 * 1024 * 1024 + 300 * 1024), "12.3 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn manual_path_hints_use_the_target_platform_path_style() {
        let catalog = I18nCatalog::load_embedded().expect("embedded translations");
        for locale in ["en", "zh"] {
            let translator = catalog.translator(locale);
            for (platform, style) in [
                ("windows", PathStyle::Windows),
                ("macos", PathStyle::Posix),
                ("linux", PathStyle::Posix),
            ] {
                for (kind, executable) in [
                    (ComponentKind::Ffmpeg, "ffmpeg"),
                    (ComponentKind::Ytdlp, "yt-dlp"),
                ] {
                    let hint = translator.text(path_hint_key(kind, platform));
                    let path = hint
                        .split_whitespace()
                        .find(|part| style.is_absolute(part))
                        .unwrap_or_else(|| panic!("{locale}/{platform}: {hint}"));
                    let filename = path.rsplit(['/', '\\']).next().expect("executable name");
                    if style == PathStyle::Windows {
                        assert_eq!(filename, format!("{executable}.exe"));
                    } else {
                        assert_eq!(filename, executable);
                        assert!(!path.contains('\\'));
                    }
                }
            }
        }
    }
}
