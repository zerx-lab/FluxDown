//! 云设备管理对话框：重命名、删除确认（删本机 = 登出）、详情、管理全部（带搜索）。
//!
//! 全部以 `Entity<AccountHost>` 为入口，可从设置窗口的账户页或任何窗口打开。

use fluxdown_protocol::{CloudDevice, LinkDeviceInfo, LinkDeviceParams, LinkRenameParams};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, DialogIntent, dialog_footer, dialog_scroll_body, dialog_title,
    field_error, field_hint, form, form_field,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, FontWeight, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::device_list::{filtered, format_timestamp, normalize_device_name, platform_label_key};
use crate::errors::{ErrorContext, error_text};
use crate::host::AccountHost;
use crate::pages::devices::device_row;
use crate::{AccountCommand, t, t_with};

fn translated(host: &Entity<AccountHost>, key: &str, cx: &App) -> SharedString {
    t(host.read(cx).translator().read(cx), key)
}

// ───────────────────────── 重命名 ─────────────────────────

/// 重命名的对象：云设备（`agent.device.rename`，1–64 字符必填）或直连设备
/// （`agent.link.rename`，仅本机显示；留空 = 清除备注名、恢复对方自报名）。
enum RenameTarget {
    Cloud { device_id: String },
    Link { fingerprint: String },
}

struct RenameDialog {
    host: Entity<AccountHost>,
    target: RenameTarget,
    input: Entity<InputState>,
    hint: Option<SharedString>,
    busy: bool,
    error: Option<SharedString>,
}

pub(crate) fn open_rename(
    host: &Entity<AccountHost>,
    device: &CloudDevice,
    window: &mut Window,
    cx: &mut App,
) {
    open_rename_dialog(
        host,
        RenameTarget::Cloud {
            device_id: device.id.clone(),
        },
        device.name.clone(),
        None,
        window,
        cx,
    );
}

/// 重命名已配对的直连设备：输入框预填当前生效名，留空恢复对方自报名。
pub(crate) fn open_link_rename(
    host: &Entity<AccountHost>,
    device: &LinkDeviceInfo,
    window: &mut Window,
    cx: &mut App,
) {
    let hint = translated(host, "linkedDeviceRenameHint", cx);
    open_rename_dialog(
        host,
        RenameTarget::Link {
            fingerprint: device.fingerprint.clone(),
        },
        device
            .custom_name
            .clone()
            .unwrap_or_else(|| device.name.clone()),
        Some(hint),
        window,
        cx,
    );
}

fn open_rename_dialog(
    host: &Entity<AccountHost>,
    target: RenameTarget,
    initial: String,
    hint: Option<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = translated(host, "accountDeviceRenameTitle", cx);
    let name = SharedString::from(initial);
    let host = host.clone();
    let view = cx.new(|cx| {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        cx.subscribe_in(
            &input,
            window,
            |this: &mut RenameDialog, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit(window, cx);
                }
            },
        )
        .detach();
        RenameDialog {
            host,
            target,
            input,
            hint,
            busy: false,
            error: None,
        }
    });
    let input = view.read(cx).input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(440.))
            // 回车由输入框 `PressEnter` 提交；默认 Confirm 会抢先关窗。
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl RenameDialog {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let raw = self.input.read(cx).value().to_string();
        let (method, params) = match &self.target {
            RenameTarget::Cloud { device_id } => {
                let Some(name) = normalize_device_name(&raw) else {
                    self.error = Some(translated(&self.host, "accountDeviceRenameInvalid", cx));
                    cx.notify();
                    return;
                };
                (
                    fluxdown_protocol::method::AGENT_DEVICE_RENAME,
                    serde_json::json!({ "id": device_id, "name": name }),
                )
            }
            RenameTarget::Link { fingerprint } => {
                // 留空 = 清除备注名；非空走与云设备相同的 1–64 字符校验。
                let name = if raw.trim().is_empty() {
                    None
                } else {
                    let Some(name) = normalize_device_name(&raw) else {
                        self.error = Some(translated(&self.host, "accountDeviceRenameInvalid", cx));
                        cx.notify();
                        return;
                    };
                    Some(name)
                };
                let params = serde_json::to_value(LinkRenameParams {
                    fingerprint: fingerprint.clone(),
                    name,
                })
                .unwrap_or_default();
                (fluxdown_protocol::method::AGENT_LINK_RENAME, params)
            }
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let command = match &self.target {
            RenameTarget::Cloud { .. } => AccountCommand::Device { method, params },
            RenameTarget::Link { .. } => AccountCommand::Link { method, params },
        };
        let future = self.host.read(cx).port().execute(command);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => {
                        this.error = Some(error_text(
                            this.host.read(cx).translator().read(cx),
                            &error,
                            ErrorContext::General,
                        ));
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
}

impl Render for RenameDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        v_flex()
            .w_full()
            .gap(tokens.spacing.lg)
            .child(form(cx).child(form_field(
                translated(&self.host, "accountFieldDeviceName", cx),
                Input::new(&self.input).control(cx).w_full(),
                self.hint.clone(),
                cx,
            )))
            .when_some(self.error.clone(), |column, error| {
                column.child(field_error(error, cx))
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("account-device-rename-cancel")
                            .outline()
                            .label(translated(&self.host, "cancel", cx))
                            .control(cx)
                            .disabled(self.busy)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("account-device-rename-submit")
                            .primary()
                            .label(translated(&self.host, "confirm", cx))
                            .control(cx)
                            .busy(self.busy)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}

// ───────────────────────── 删除 ─────────────────────────

/// 删除确认；删除本机设备需额外警告（确认后会立即登出）。
pub(crate) fn confirm_delete(
    host: &Entity<AccountHost>,
    device: &CloudDevice,
    window: &mut Window,
    cx: &mut App,
) {
    let title = translated(host, "accountDeviceDeleteConfirmTitle", cx);
    let mut description = translated(host, "accountDeviceDeleteConfirmDesc", cx).to_string();
    if device.is_current {
        description.push_str("\n\n");
        description.push_str(&translated(host, "accountDeviceDeleteCurrentWarning", cx));
    }
    let description = SharedString::from(description);
    let ok = translated(host, "accountDeviceDeleteAction", cx);
    let cancel = translated(host, "cancel", cx);
    let host = host.clone();
    let device_id = device.id.clone();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let host = host.clone();
        let device_id = device_id.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .description(description.clone())
            .footer(dialog_footer(
                Some(cancel.clone()),
                ok.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let future = host.update(cx, |host, _| host.delete_device(&device_id));
                let host = host.clone();
                window
                    .spawn(cx, async move |cx| {
                        let result = future.await;
                        let Ok(()) = cx.update(|window, cx| {
                            if let Err(error) = result {
                                let message = error_text(
                                    host.read(cx).translator().read(cx),
                                    &error,
                                    ErrorContext::General,
                                );
                                window.push_notification(toast::error(message), cx);
                            }
                        }) else {
                            // 账户视图或窗口已释放，结束回调，不再更新状态。
                            return;
                        };
                    })
                    .detach();
                true
            })
    });
}

/// 让另一台在线设备下线（无确认：只是释放在线名额，设备仍受信任，可自行重连）；失败弹出错误通知。
pub(crate) fn disconnect_device(
    host: &Entity<AccountHost>,
    device: &CloudDevice,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(future) = host.update(cx, |host, cx| host.begin_disconnect(&device.id, cx)) else {
        // 该设备的下线请求已在途，拒绝重入。
        return;
    };
    let host = host.clone();
    let device_id = device.id.clone();
    window
        .spawn(cx, async move |cx| {
            let result = future.await;
            // 窗口可能已关闭：先在应用层清除在途标记（host 跨窗口共享），再尝试弹通知。
            host.update(cx, |host, cx| host.finish_disconnect(&device_id, cx));
            let Ok(()) = cx.update(|window, cx| {
                if let Err(error) = result {
                    let message = error_text(
                        host.read(cx).translator().read(cx),
                        &error,
                        ErrorContext::General,
                    );
                    window.push_notification(toast::error(message), cx);
                }
            }) else {
                return;
            };
        })
        .detach();
}

/// 解除局域网配对确认。
pub(crate) fn confirm_unpair(
    host: &Entity<AccountHost>,
    device: &LinkDeviceInfo,
    window: &mut Window,
    cx: &mut App,
) {
    let translator = host.read(cx).translator().read(cx).clone();
    let title = t(&translator, "linkedDeviceRemoveTitle");
    let description = t_with(
        &translator,
        "linkedDeviceRemoveDesc",
        &[("name", &device.name)],
    );
    let ok = t(&translator, "linkedDeviceRemove");
    let cancel = t(&translator, "cancel");
    let host = host.clone();
    let fingerprint = device.fingerprint.clone();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let host = host.clone();
        let fingerprint = fingerprint.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .description(description.clone())
            .footer(dialog_footer(
                Some(cancel.clone()),
                ok.clone(),
                DialogIntent::Destructive,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let params = serde_json::to_value(LinkDeviceParams {
                    fingerprint: fingerprint.clone(),
                })
                .unwrap_or_default();
                let future = host.read(cx).port().execute(AccountCommand::Link {
                    method: fluxdown_protocol::method::AGENT_LINK_REMOVE,
                    params,
                });
                let host = host.clone();
                window
                    .spawn(cx, async move |cx| {
                        if let Err(error) = future.await {
                            let Ok(()) = cx.update(|window, cx| {
                                let message = error_text(
                                    host.read(cx).translator().read(cx),
                                    &error,
                                    ErrorContext::General,
                                );
                                window.push_notification(toast::error(message), cx);
                            }) else {
                                // 账户视图或窗口已释放，结束回调，不再更新状态。
                                return;
                            };
                        }
                    })
                    .detach();
                true
            })
    });
}

/// 按指纹打开直连设备重命名（下载页侧栏入口）；设备已不在名册则不动作。
pub fn open_link_rename_by_fingerprint(
    host: &Entity<AccountHost>,
    fingerprint: &str,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(device) = linked_device(host, fingerprint, cx) {
        open_link_rename(host, &device, window, cx);
    }
}

/// 按指纹打开解除配对确认（下载页侧栏入口）；设备已不在名册则不动作。
pub fn confirm_unpair_by_fingerprint(
    host: &Entity<AccountHost>,
    fingerprint: &str,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(device) = linked_device(host, fingerprint, cx) {
        confirm_unpair(host, &device, window, cx);
    }
}

fn linked_device(
    host: &Entity<AccountHost>,
    fingerprint: &str,
    cx: &App,
) -> Option<LinkDeviceInfo> {
    host.read(cx)
        .controller
        .linked_devices()
        .iter()
        .find(|device| device.fingerprint == fingerprint)
        .cloned()
}

// ───────────────────────── 详情 ─────────────────────────

pub(crate) fn open_detail(
    host: &Entity<AccountHost>,
    device: &CloudDevice,
    window: &mut Window,
    cx: &mut App,
) {
    let translator = host.read(cx).translator().read(cx).clone();
    let title = t(&translator, "accountDeviceDetailTitle");
    let platform = device.platform.clone().unwrap_or_default();
    let platform_text = platform_label_key(&platform)
        .map_or_else(|| platform.clone(), |key| t(&translator, key).to_string());
    let mut rows: Vec<(SharedString, String)> = vec![
        (t(&translator, "accountDeviceFieldPlatform"), platform_text),
        (
            t(&translator, "accountDeviceFieldAppVersion"),
            device.app_version.clone().unwrap_or_default(),
        ),
        (
            t(&translator, "accountDeviceFieldLastIp"),
            device.last_ip.clone().unwrap_or_default(),
        ),
        (
            t(&translator, "accountDeviceFieldCreatedAt"),
            format_timestamp(&device.created_at),
        ),
        (
            t(&translator, "accountDeviceFieldLastSeenAt"),
            format_timestamp(&device.last_seen_at),
        ),
    ];
    if let Some(dir) = device
        .default_save_dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
    {
        rows.push((t(&translator, "accountDeviceFieldSaveDir"), dir.to_owned()));
    }
    rows.push((
        t(&translator, "accountDeviceFieldId"),
        device.device_id.clone(),
    ));
    let name = device.name.clone();
    let close = t(&translator, "close");
    let presence = cx.new(|cx| {
        cx.observe(host, |_, _, cx| cx.notify()).detach();
        DevicePresence {
            host: host.clone(),
            device_id: device.id.clone(),
        }
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let rows = rows.clone();
        let name = name.clone();
        let close = close.clone();
        let presence = presence.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(480.))
            .content(move |content, _, cx| {
                let tokens = active_theme(cx).tokens().clone();
                content.min_h_0().child(dialog_scroll_body(
                    "account-device-detail-body",
                    None,
                    v_flex()
                        .w_full()
                        .gap(tokens.spacing.sm)
                        .child(
                            div()
                                .text_size(tokens.typography.sm.size)
                                .line_height(tokens.typography.sm.line_height)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(tokens.colors.foreground)
                                .child(name.clone()),
                        )
                        .child(presence.clone())
                        .children(rows.iter().filter(|(_, value)| !value.is_empty()).map(
                            |(label, value)| {
                                h_flex()
                                    .w_full()
                                    .items_start()
                                    .justify_between()
                                    .gap(tokens.spacing.lg)
                                    .child(field_hint(label.clone(), cx))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_size(tokens.typography.xs.size)
                                            .line_height(tokens.typography.xs.line_height)
                                            .text_color(tokens.colors.foreground)
                                            .child(value.clone()),
                                    )
                            },
                        )),
                    cx,
                ))
            })
            .footer(dialog_footer(
                None,
                close.clone(),
                DialogIntent::Confirm,
                cx,
            ))
    });
}

// ───────────────────────── 管理全部 ─────────────────────────

struct DevicePresence {
    host: Entity<AccountHost>,
    device_id: String,
}

impl Render for DevicePresence {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self.host.read(cx);
        let translator = host.translator().read(cx);
        let key = host
            .controller
            .devices()
            .iter()
            .find(|device| device.id == self.device_id)
            .map_or("devicePresenceUnknown", |device| {
                host.controller.device_presence_key(device)
            });
        v_flex()
            .child(field_hint(t(translator, "accountDeviceFieldOnline"), cx))
            .child(field_hint(t(translator, key), cx))
            .child(field_hint(t(translator, "cloudConnectionStatusHint"), cx))
    }
}

struct ManageAllDialog {
    host: Entity<AccountHost>,
    search: Entity<InputState>,
}

pub(crate) fn open_manage_all(host: &Entity<AccountHost>, window: &mut Window, cx: &mut App) {
    let title = translated(host, "accountDevicesManageAllTitle", cx);
    let placeholder = translated(host, "accountDevicesSearchHint", cx);
    let host = host.clone();
    let view = cx.new(|cx| {
        cx.observe(&host, |_, _, cx| cx.notify()).detach();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        ManageAllDialog { host, search }
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(600.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
}

impl Render for ManageAllDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let query = self.search.read(cx).value().to_string();
        let controller = &self.host.read(cx).controller;
        let devices = filtered(
            controller.devices(),
            &query,
            !controller.is_stale() && controller.cloud_connected(),
        );
        let translator = self.host.read(cx).translator().read(cx).clone();
        let empty = devices.is_empty();
        let no_results = t(&translator, "accountDevicesSearchNoResults");
        let count = self.host.read(cx).controller.devices().len();
        let hint = t_with(
            &translator,
            "accountDevicesManageAll",
            &[("count", &count.to_string())],
        );
        let rows: Vec<_> = devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                device_row(
                    &self.host,
                    "account-devices-all",
                    device,
                    index > 0,
                    &translator,
                    cx,
                )
            })
            .collect();
        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.md)
            .child(Input::new(&self.search).control(cx).w_full())
            .child(field_hint(hint, cx))
            .child(dialog_scroll_body(
                "account-devices-all-scroll",
                Some(px(380.)),
                if empty {
                    div()
                        .w_full()
                        .py(tokens.spacing.lg)
                        .flex()
                        .justify_center()
                        .child(field_hint(no_results, cx))
                        .into_any_element()
                } else {
                    fluxdown_ui_components::card(cx)
                        .w_full()
                        .flex()
                        .flex_col()
                        .children(rows)
                        .into_any_element()
                },
                cx,
            ))
    }
}
