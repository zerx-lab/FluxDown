//! 下载能力提供给 shell 的可摆放控件：全局速度、全部暂停 / 全部开始 / 失败重试、限速、代理、
//! 定时关机 / 睡眠、剩余空间。全局任务操作默认摆在顶栏，其余在状态栏，可经 chrome 布局移动；
//! 控件窗口级显示（不随路由切换消失），由 app 把 [`DownloadWidget`] 登记为 shell 控件。
//!
//! 下载限速走云同步偏好写入；上传限速与代理模式走 `DownloadsCommand::PatchConfig`
//!（`upload_limit_bytes` 为字节/秒，`proxy_mode` 为现有四模式之一）。
//! 定时关机 / 睡眠走宿主注入的 `ShutdownPort`（状态机与真正执行归 agent，本文件只发请求 +
//! 刷新倒计时显示）；选中的操作记在设备本地偏好 `download.power_action`。

use std::{collections::BTreeMap, rc::Rc, time::Duration};

/// 数字输入弹窗的确认回调。
type NumberConfirm = Rc<dyn Fn(i64, &mut App)>;

use fluxdown_protocol::{ApplicationErrorCode, PowerAction};
use fluxdown_ui_components::{
    ControlExt as _, DialogIntent, FluxIcon, dialog_footer, dialog_title, field_hint, form,
    form_field, tabular_numbers, toolbar_action_button, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Context, Div, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled, WeakEntity,
    Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Icon, Sizable as _, Size, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};

use crate::{
    components::task_table::ToolbarCommand,
    controller::{DownloadsCommand, SPEED_LIMIT_RECENT_PREF, UPLOAD_LIMIT_RECENT_PREF},
    model::{
        format_bytes,
        shutdown::{
            POWER_ACTION_PREF, ShutdownRequest, ShutdownStatus, power_action_pref_value,
            read_power_action,
        },
        speed_limit_recent::{push_recent, read_speed_limit_recent},
    },
    pages::downloads::DownloadView,
};

/// 下载 / 上传限速预设（MB/s）。
const SPEED_PRESETS_MB: [i64; 4] = [1, 5, 10, 50];
/// 定时关机 / 睡眠延迟预设（分钟）。
const SHUTDOWN_PRESETS_MIN: [i64; 4] = [1, 5, 10, 30];

/// 下载能力的可摆放控件；[`Self::id`] 是 chrome 布局里的稳定 id（与 Web 同名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadWidget {
    DownloadSpeed,
    UploadSpeed,
    PauseAll,
    ResumeAll,
    RetryFailed,
    DownloadLimit,
    UploadLimit,
    Proxy,
    Shutdown,
    DiskFree,
}

impl DownloadWidget {
    /// 目录顺序；各控件的默认区域由宿主 chrome 目录决定。
    pub const ALL: [Self; 10] = [
        Self::DownloadSpeed,
        Self::UploadSpeed,
        Self::PauseAll,
        Self::ResumeAll,
        Self::RetryFailed,
        Self::DownloadLimit,
        Self::UploadLimit,
        Self::Proxy,
        Self::Shutdown,
        Self::DiskFree,
    ];

    /// chrome 布局中的稳定 id。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::DownloadSpeed => "downloadSpeed",
            Self::UploadSpeed => "uploadSpeed",
            Self::PauseAll => "pauseAll",
            Self::ResumeAll => "resumeAll",
            Self::RetryFailed => "retryFailed",
            Self::DownloadLimit => "downloadLimit",
            Self::UploadLimit => "uploadLimit",
            Self::Proxy => "proxy",
            Self::Shutdown => "shutdown",
            Self::DiskFree => "diskFree",
        }
    }

    /// 编辑布局时的占位 / 拖拽预览文案键。
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::DownloadSpeed => "chromeWidgetDownloadSpeed",
            Self::UploadSpeed => "chromeWidgetUploadSpeed",
            Self::PauseAll => "pauseAll",
            Self::ResumeAll => "resumeAll",
            Self::RetryFailed => "retryFailed",
            Self::DownloadLimit => "speedLimitTitle",
            Self::UploadLimit => "uploadLimit",
            Self::Proxy => "statusBarProxyLabel",
            Self::Shutdown => "shutdownTitle",
            Self::DiskFree => "chromeWidgetDiskFree",
        }
    }

    /// 纯文字读数（速度、剩余空间）：没有图标形态，放不进窄活动栏。
    #[must_use]
    pub const fn is_text_readout(self) -> bool {
        matches!(
            self,
            Self::DownloadSpeed | Self::UploadSpeed | Self::DiskFree
        )
    }
}

/// 控件摆放处的呈现要求：`compact` = 窄活动栏，只留图标、数值并入悬浮提示；
/// `menu_anchor` = 下拉菜单相对触发器的锚点（底部区域向上弹、顶部区域向下弹）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetPlacement {
    pub compact: bool,
    pub menu_anchor: Anchor,
}

/// 限速控件的静态参数。
struct SpeedLimitControl {
    element_id: &'static str,
    icon: FluxIcon,
    title_key: &'static str,
    /// `speed_limit_bytes` 或 `upload_limit_bytes`（字节/秒）。
    config_key: &'static str,
    /// 最近自定义值的本机偏好键（KB/s 整数数组）。
    recent_pref: &'static str,
}

const DOWNLOAD_LIMIT: SpeedLimitControl = SpeedLimitControl {
    element_id: "status-download-limit",
    icon: FluxIcon::ArrowDown,
    title_key: "speedLimitTitle",
    config_key: "speed_limit_bytes",
    recent_pref: SPEED_LIMIT_RECENT_PREF,
};

const UPLOAD_LIMIT: SpeedLimitControl = SpeedLimitControl {
    element_id: "status-upload-limit",
    icon: FluxIcon::ArrowUp,
    title_key: "uploadLimit",
    config_key: "upload_limit_bytes",
    recent_pref: UPLOAD_LIMIT_RECENT_PREF,
};

/// 紧凑摆放的悬浮提示：标题 + 当前值。
pub(super) fn compact_tooltip(title: &str, value: &str) -> SharedString {
    SharedString::from(format!("{title} · {value}"))
}

/// 纯文字读数（图标 + 数值）：自带 caption 字号、等宽数字与二级文字色，放在任何承载条都一致。
fn readout_cell(icon: FluxIcon, text: impl Into<SharedString>, cx: &App) -> AnyElement {
    let theme = active_theme(cx);
    let extended = theme.extended();
    let spacing = theme.tokens().spacing;
    let muted = theme.tokens().colors.muted_foreground;
    h_flex()
        .flex_none()
        .items_center()
        .gap(spacing.xxs)
        .px(spacing.xs)
        .whitespace_nowrap()
        .text_size(extended.caption.size)
        .line_height(extended.caption.line_height)
        .font_features(tabular_numbers())
        .text_color(muted)
        .child(Icon::new(icon).size(extended.icon.sm).text_color(muted))
        .child(text.into())
        .into_any_element()
}

/// 状态栏带文字的小按钮外壳：ghost、`density.statusControl` 高、横向 `spacing.xs`。
///
/// gpui-component 按钮会按 `Size` 在内部 label 上覆盖字号与图标尺寸，所以内容一律经
/// [`status_button_content`] 作为子元素传入，确保 caption 字号 + `icon.sm` 生效。
pub(super) fn status_button(id: &'static str, cx: &App) -> Button {
    let theme = active_theme(cx);
    Button::new(id)
        .ghost()
        .with_size(Size::XSmall)
        .h(theme.density().status_control)
        .px(theme.tokens().spacing.xs)
}

/// 状态栏按钮内容：可选图标 + 可选文字，caption 字号、等宽数字；`color` 为空时继承按钮前景色。
pub(super) fn status_button_content(
    icon: Option<FluxIcon>,
    text: Option<SharedString>,
    color: Option<Hsla>,
    cx: &App,
) -> Div {
    let theme = active_theme(cx);
    let extended = theme.extended();
    let icon_size = extended.icon.sm;
    h_flex()
        .items_center()
        .gap(theme.tokens().spacing.xxs)
        .text_size(extended.caption.size)
        .line_height(extended.caption.line_height)
        .font_features(tabular_numbers())
        .when_some(color, |this, color| this.text_color(color))
        .when_some(icon, |this, icon| {
            this.child(
                Icon::new(icon)
                    .size(icon_size)
                    .when_some(color, |icon, color| icon.text_color(color)),
            )
        })
        .when_some(text, |this, text| this.child(text))
}

fn format_countdown(remaining: Duration) -> String {
    let total = remaining.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// 预设 MB/s → `speed_limit_bytes`/`upload_limit_bytes` 配置值（字节/秒）。
fn mb_to_bytes(mb: i64) -> i64 {
    mb * 1_048_576
}

/// 自定义弹窗 KB/s 输入 → 配置值（字节/秒）；负值视为 0。
fn kb_to_bytes(kb: i64) -> i64 {
    kb.max(0) * 1024
}

/// 分钟延迟 → `ShutdownRequest::Arm` 所需 `Duration`；负值视为 0。
fn minutes_to_duration(minutes: i64) -> Duration {
    Duration::from_secs(60 * minutes.max(0) as u64)
}

/// 已排定操作对应的文案键。
struct PowerActionKeys {
    countdown: &'static str,
    armed_immediate: &'static str,
    armed_after: &'static str,
    cancel: &'static str,
}

impl PowerActionKeys {
    fn of(action: PowerAction) -> Self {
        match action {
            PowerAction::Shutdown => Self {
                countdown: "shutdownCountdown",
                armed_immediate: "shutdownArmedHintImmediate",
                armed_after: "shutdownArmedHint",
                cancel: "shutdownCancelButton",
            },
            PowerAction::Sleep => Self {
                countdown: "sleepCountdown",
                armed_immediate: "sleepArmedHintImmediate",
                armed_after: "sleepArmedHint",
                cancel: "sleepCancelButton",
            },
        }
    }
}

/// 已排定时的状态文字：倒计时中显示剩余时间，等待任务完成时显示排定的延迟。
fn armed_text(
    translator: &fluxdown_ui_i18n::Translator,
    status: &ShutdownStatus,
    keys: &PowerActionKeys,
) -> String {
    match (status.countdown_remaining, status.armed_delay) {
        (Some(remaining), _) => {
            translator.text_with(keys.countdown, &[("time", &format_countdown(remaining))])
        }
        (None, Some(delay)) if !delay.is_zero() => translator.text_with(
            keys.armed_after,
            &[("m", &(delay.as_secs() / 60).to_string())],
        ),
        (None, _) => translator.text(keys.armed_immediate).to_owned(),
    }
}

/// 记住状态栏选中的电源操作（设备本地偏好）。
fn select_power_action(view: WeakEntity<DownloadView>, action: PowerAction, cx: &mut App) {
    let value = power_action_pref_value(action);
    let Ok(()) = view.update(cx, |this, cx| {
        this.execute_status_config_command(
            DownloadsCommand::SetLocalPreference {
                key: POWER_ACTION_PREF,
                value,
            },
            cx,
        );
    }) else {
        // 视图已释放，结束这次回调而不再更新状态。
        return;
    };
}

fn apply_speed_limit(view: WeakEntity<DownloadView>, key: &'static str, value: i64, cx: &mut App) {
    let Ok(()) = view.update(cx, |this, cx| this.execute_config_patch(key, value, cx)) else {
        // 视图已释放，结束这次回调而不再更新状态。
        return;
    };
}

/// 自定义 / 最近值生效：写限速并把该值记入最近列表。
fn apply_custom_speed_limit(
    view: WeakEntity<DownloadView>,
    key: &'static str,
    recent_pref: &'static str,
    kbps: i64,
    cx: &mut App,
) {
    let Ok(()) = view.update(cx, |this, cx| {
        this.execute_config_patch(key, kb_to_bytes(kbps), cx);
        let recent = read_speed_limit_recent(this.controller.preference(recent_pref));
        if let Some(next) = push_recent(&recent, kbps) {
            this.execute_status_config_command(
                DownloadsCommand::SetLocalPreference {
                    key: recent_pref,
                    value: serde_json::Value::from(next),
                },
                cx,
            );
        }
    }) else {
        // 视图已释放，结束这次回调而不再更新状态。
        return;
    };
}

/// 清除最近自定义值（写 null，与保存目录历史清空同口径）。
fn clear_speed_limit_recent(
    view: WeakEntity<DownloadView>,
    recent_pref: &'static str,
    cx: &mut App,
) {
    let Ok(()) = view.update(cx, |this, cx| {
        this.execute_status_config_command(
            DownloadsCommand::SetLocalPreference {
                key: recent_pref,
                value: serde_json::Value::Null,
            },
            cx,
        );
    }) else {
        // 视图已释放，结束这次回调而不再更新状态。
        return;
    };
}

/// 数字输入弹窗文案：自定义限速（KB/s）与自定义关机延迟（分钟）复用。
#[derive(Clone)]
struct NumberPrompt {
    title: SharedString,
    label: SharedString,
    /// 输入框尾部单位（`KB/s` / `分钟`）。
    unit: SharedString,
    hint: Option<SharedString>,
    cancel_label: SharedString,
    confirm_label: SharedString,
}

/// 弹窗输入解析：空白、非数字或小于 `min` 都视为无效，避免误当成 0 触发副作用。
fn parse_number_input(text: &str, min: i64) -> Option<i64> {
    text.trim()
        .parse::<i64>()
        .ok()
        .filter(|value| *value >= min)
}

/// 单个数字输入确认弹窗：标题 → 表单字段（标签 + 带单位输入框 + 可选说明）→ 底栏。
fn open_number_prompt(
    window: &mut Window,
    cx: &mut App,
    prompt: NumberPrompt,
    min: i64,
    initial: Option<i64>,
    on_confirm: NumberConfirm,
) {
    let input = cx.new(|cx| {
        let state = InputState::new(window, cx);
        match initial {
            Some(value) => state.default_value(value.to_string()),
            None => state,
        }
    });
    let dialog_input = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let content_input = dialog_input.clone();
        let ok_input = dialog_input.clone();
        let on_confirm = on_confirm.clone();
        let prompt = prompt.clone();
        dialog
            .title(dialog_title(prompt.title.clone(), cx))
            .w(active_theme(cx).text_extent(520.))
            .content({
                let prompt = prompt.clone();
                move |content, _, cx| {
                    let unit = field_hint(prompt.unit.clone(), cx).flex_none();
                    content.child(form(cx).child(form_field(
                        prompt.label.clone(),
                        Input::new(&content_input).control(cx).suffix(unit).w_full(),
                        prompt.hint.clone(),
                        cx,
                    )))
                }
            })
            .footer(dialog_footer(
                Some(prompt.cancel_label.clone()),
                prompt.confirm_label.clone(),
                DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, _, cx| {
                let text = ok_input.read(cx).value().to_string();
                let Some(value) = parse_number_input(&text, min) else {
                    return false;
                };
                on_confirm(value, cx);
                true
            })
    });
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl DownloadView {
    /// 下载限速仍经云同步偏好写入；其余数值配置使用 daemon 增量写回。
    fn execute_config_patch(&mut self, key: &'static str, value: i64, cx: &mut Context<Self>) {
        // 下载限速属于云同步目录（download.speed_limit_bytes）：必须走偏好写入链路，
        // 直写 daemon 配置不会被标记为本地改动，之后会被云端旧值覆盖。
        let command = if key == "speed_limit_bytes" {
            DownloadsCommand::SetSyncedPreference {
                key: "download.speed_limit_bytes",
                value: serde_json::Value::from(value),
            }
        } else {
            let mut values = BTreeMap::new();
            values.insert(key.to_owned(), value.to_string());
            DownloadsCommand::PatchConfig {
                values,
                expected_revision: self.controller.config_revision(),
            }
        };
        self.execute_status_config_command(command, cx);
    }

    /// 状态栏配置冲突 / 失败复用现有横幅字段，不新增错误文案或宿主回调。
    pub(super) fn execute_status_config_command(
        &mut self,
        command: DownloadsCommand,
        cx: &mut Context<Self>,
    ) {
        let future = self.controller.execute(command);
        let conflict_message = SharedString::from(
            self.translator
                .read(cx)
                .text("localServiceConflict")
                .to_owned(),
        );
        let failed_message = self.strings.action_failed.clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = future.await {
                let message = if error.code == ApplicationErrorCode::Conflict {
                    conflict_message
                } else {
                    failed_message
                };

                let Ok(()) = this.update(cx, |this, cx| {
                    this.last_error = Some(message);
                    cx.notify();
                }) else {
                    // 视图已释放，结束这次回调而不再更新状态。
                    return;
                };
            }
        })
        .detach();
    }

    /// 下载 / 上传限速控件：图标 + 当前值的幽灵按钮，点开自动收起的菜单
    /// （关闭 + 预设 + 最近自定义 + 自定义… + 清除最近）。
    /// 紧凑摆放只留图标，当前值并入悬浮提示。
    fn render_speed_limit_control(
        &self,
        control: &SpeedLimitControl,
        placement: WidgetPlacement,
        view: WeakEntity<Self>,
        cx: &App,
    ) -> AnyElement {
        let SpeedLimitControl {
            element_id,
            icon,
            title_key,
            config_key,
            recent_pref,
        } = *control;
        let translator = self.translator.read(cx);
        let value = self
            .controller
            .config_str(config_key)
            .parse::<i64>()
            .unwrap_or(0);
        let off_label = SharedString::from(translator.text("statusSpeedLimitOff").to_owned());
        let trigger_label = if value <= 0 {
            off_label.clone()
        } else {
            SharedString::from(format!("{}/s", format_bytes(value as u64)))
        };
        let title = SharedString::from(translator.text(title_key).to_owned());
        let recent = read_speed_limit_recent(self.controller.preference(recent_pref));
        let custom_current =
            value > 0 && !SPEED_PRESETS_MB.iter().any(|mb| mb_to_bytes(*mb) == value);
        let current_kbps = custom_current.then(|| (value / 1024).max(1));
        let custom_label = SharedString::from(translator.text("speedLimitCustom").to_owned());
        let custom_item_label = if custom_current {
            SharedString::from(translator.text_with(
                "speedLimitCustomWithValue",
                &[("value", &format!("{}/s", format_bytes(value as u64)))],
            ))
        } else {
            custom_label.clone()
        };
        let clear_label = SharedString::from(translator.text("speedLimitClearRecent").to_owned());
        let desc_key = if config_key == "upload_limit_bytes" {
            "uploadLimitDesc"
        } else {
            "speedLimitDesc"
        };
        let prompt = NumberPrompt {
            title: title.clone(),
            label: custom_label.clone(),
            unit: SharedString::from(translator.text("statusSpeedLimitKbs").to_owned()),
            hint: Some(SharedString::from(translator.text(desc_key).to_owned())),
            cancel_label: SharedString::from(translator.text("cancel").to_owned()),
            confirm_label: SharedString::from(translator.text("confirm").to_owned()),
        };
        let tooltip = if placement.compact {
            compact_tooltip(&title, &trigger_label)
        } else {
            title.clone()
        };

        // 限速生效时触发器用强调色文字，一眼可见当前被限速；未限速保持中性。
        let limited_color = (value > 0).then(|| active_theme(cx).extended().colors.accent_text);

        status_button(element_id, cx)
            .child(status_button_content(
                Some(icon),
                (!placement.compact).then_some(trigger_label),
                limited_color,
                cx,
            ))
            .tooltip(tooltip)
            .dropdown_menu_with_anchor(placement.menu_anchor, move |menu, _, _| {
                let mut menu = menu.item(
                    PopupMenuItem::new(off_label.clone())
                        .checked(value <= 0)
                        .on_click({
                            let view = view.clone();
                            move |_, _, cx| apply_speed_limit(view.clone(), config_key, 0, cx)
                        }),
                );
                for mb in SPEED_PRESETS_MB {
                    let bytes = mb_to_bytes(mb);
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(format!("{mb} MB/s")))
                            .checked(bytes == value)
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    apply_speed_limit(view.clone(), config_key, bytes, cx)
                                }
                            }),
                    );
                }
                let mut recent_hit = false;
                for kbps in recent.iter().copied() {
                    let bytes = kb_to_bytes(kbps);
                    let checked = bytes == value;
                    recent_hit |= checked;
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(format!(
                            "{}/s",
                            format_bytes(bytes as u64)
                        )))
                        .checked(checked)
                        .on_click({
                            let view = view.clone();
                            move |_, _, cx| {
                                apply_custom_speed_limit(
                                    view.clone(),
                                    config_key,
                                    recent_pref,
                                    kbps,
                                    cx,
                                );
                            }
                        }),
                    );
                }
                menu = menu.separator().item(
                    PopupMenuItem::new(custom_item_label.clone())
                        .checked(custom_current && !recent_hit)
                        .on_click({
                            let view = view.clone();
                            let prompt = prompt.clone();
                            move |_, window, cx| {
                                let view = view.clone();
                                open_number_prompt(
                                    window,
                                    cx,
                                    prompt.clone(),
                                    0,
                                    current_kbps,
                                    Rc::new(move |kb, cx| {
                                        apply_custom_speed_limit(
                                            view.clone(),
                                            config_key,
                                            recent_pref,
                                            kb,
                                            cx,
                                        );
                                    }),
                                );
                            }
                        }),
                );
                if !recent.is_empty() {
                    menu = menu.item(PopupMenuItem::new(clear_label.clone()).on_click({
                        let view = view.clone();
                        move |_, _, cx| clear_speed_limit_recent(view.clone(), recent_pref, cx)
                    }));
                }
                menu
            })
            .into_any_element()
    }

    /// 定时关机 / 睡眠控件：未启用时只显示电源图标（悬浮提示 + 菜单：操作单选 + 延迟预设）；
    /// 已启用 / 倒计时中显示 warning 色倒计时文字，点击取消（紧凑摆放只留 warning 色图标）。
    /// 没有任务时选延迟即立即倒计时，「完成后立即」只在有任务时出现。
    /// 宿主未注入关机端口时为 `None`。
    fn render_shutdown_control(
        &self,
        placement: WidgetPlacement,
        view: WeakEntity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        let translator = self.translator.read(cx);
        let shutdown = self.host.shutdown.clone()?;
        let status = self
            .host
            .shutdown_status
            .as_ref()
            .map(|status| status.get())
            .unwrap_or_default();
        let warning = active_theme(cx).extended().colors.warning;

        if status.armed_delay.is_some() {
            let keys = PowerActionKeys::of(status.action);
            let cancel_label = SharedString::from(translator.text(keys.cancel).to_owned());
            let text = SharedString::from(armed_text(translator, &status, &keys));
            let (label, tooltip) = if placement.compact {
                (None, compact_tooltip(&cancel_label, &text))
            } else {
                (Some(text), cancel_label)
            };
            return Some(
                status_button("shutdown-cancel", cx)
                    .tooltip(tooltip)
                    .child(status_button_content(
                        Some(FluxIcon::Power),
                        label,
                        Some(warning),
                        cx,
                    ))
                    .on_click(move |_, _, cx| shutdown(ShutdownRequest::Disarm, cx))
                    .into_any_element(),
            );
        }

        let stats = self.controller.runtime_stats();
        let has_work =
            stats.active_tasks > 0 || stats.pending_tasks > 0 || stats.retry_pending_tasks > 0;
        let action = read_power_action(self.controller.preference(POWER_ACTION_PREF));
        let title = SharedString::from(translator.text("shutdownTitle").to_owned());
        let action_items = [
            (PowerAction::Shutdown, "powerActionShutdown"),
            (PowerAction::Sleep, "powerActionSleep"),
        ]
        .map(|(item, key)| (item, SharedString::from(translator.text(key).to_owned())));
        let immediate_label = SharedString::from(translator.text("shutdownImmediate").to_owned());
        let custom_label = SharedString::from(translator.text("speedLimitCustom").to_owned());
        let prompt = NumberPrompt {
            title: title.clone(),
            label: SharedString::from(translator.text("shutdownDelayLabel").to_owned()),
            unit: SharedString::from(translator.text("shutdownMinutesUnit").to_owned()),
            hint: Some(SharedString::from(
                translator.text("shutdownDelayHint").to_owned(),
            )),
            cancel_label: SharedString::from(translator.text("cancel").to_owned()),
            confirm_label: SharedString::from(translator.text("confirm").to_owned()),
        };
        let minute_presets: Vec<(i64, SharedString)> = SHUTDOWN_PRESETS_MIN
            .iter()
            .map(|&minutes| {
                let label =
                    translator.text_with("shutdownDelayMinutes", &[("m", &minutes.to_string())]);
                (minutes, SharedString::from(label))
            })
            .collect();
        let arm = move |minutes: i64| ShutdownRequest::Arm {
            delay: minutes_to_duration(minutes),
            action,
        };

        let trigger = status_button("shutdown-trigger", cx)
            .min_w(active_theme(cx).density().status_control)
            .child(status_button_content(Some(FluxIcon::Power), None, None, cx))
            .tooltip(title)
            .dropdown_menu_with_anchor(placement.menu_anchor, move |menu, _, _| {
                let mut menu = menu;
                for (item, label) in action_items.clone() {
                    menu = menu.item(PopupMenuItem::new(label).checked(item == action).on_click({
                        let view = view.clone();
                        move |_, _, cx| select_power_action(view.clone(), item, cx)
                    }));
                }
                menu = menu.separator();
                if has_work {
                    menu = menu.item(PopupMenuItem::new(immediate_label.clone()).on_click({
                        let shutdown = shutdown.clone();
                        move |_, _, cx| shutdown(arm(0), cx)
                    }));
                }
                for (minutes, label) in minute_presets.clone() {
                    menu = menu.item(PopupMenuItem::new(label).on_click({
                        let shutdown = shutdown.clone();
                        move |_, _, cx| shutdown(arm(minutes), cx)
                    }));
                }
                menu.separator()
                    .item(PopupMenuItem::new(custom_label.clone()).on_click({
                        let shutdown = shutdown.clone();
                        let prompt = prompt.clone();
                        move |_, window, cx| {
                            let shutdown = shutdown.clone();
                            open_number_prompt(
                                window,
                                cx,
                                prompt.clone(),
                                // 0 分钟等同「完成后立即」（独立菜单项，仅有任务时可用）；自定义必须 ≥ 1。
                                1,
                                None,
                                Rc::new(move |minutes, cx| shutdown(arm(minutes), cx)),
                            );
                        }
                    }))
            });
        Some(trigger.into_any_element())
    }

    /// 全部暂停 / 全部开始 / 失败重试小图标按钮；不可用时置灰禁用，tooltip 改为说明原因
    /// （tooltip 挂在外层，禁用时仍可悬停）。
    fn render_status_action(
        &self,
        command: ToolbarCommand,
        view: WeakEntity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        let (id, label, icon, blocker) = match command {
            ToolbarCommand::PauseAll => (
                "status-pause-all",
                self.strings.pause_all.clone(),
                FluxIcon::Pause,
                self.pause_all_blocker(),
            ),
            ToolbarCommand::ResumeAll => (
                "status-resume-all",
                self.strings.resume_all.clone(),
                FluxIcon::Play,
                self.resume_all_blocker(),
            ),
            ToolbarCommand::RetryFailed => (
                "status-retry-failed",
                SharedString::from(self.translator.read(cx).text("retryFailed").to_owned()),
                FluxIcon::RotateCw,
                self.retry_failed_blocker(cx),
            ),
            _ => return None,
        };
        let theme = active_theme(cx);
        let icon_size = theme.extended().icon.sm;
        let control_size = theme.density().status_control;
        let disabled = blocker.is_some();
        let tooltip_label = blocker.unwrap_or_else(|| label.clone());
        // 禁用的按钮不派发点击（`toolbar_action_button` 经 `Button::disabled`）。
        let button = toolbar_action_button(
            id,
            label,
            Icon::new(icon).size(icon_size),
            false,
            disabled,
            cx,
        )
        .size(control_size)
        .on_click(move |_, _, cx| {
            let Ok(()) = view.update(cx, |this, cx| this.execute_toolbar(command, cx)) else {
                // 视图已释放，结束这次回调而不再更新状态。
                return;
            };
        });
        Some(
            div()
                .id(SharedString::from(format!("{id}-tooltip")))
                .flex_none()
                .tooltip(move |window, cx| tooltip_view(tooltip_label.clone(), window, cx))
                .child(button)
                .into_any_element(),
        )
    }

    /// 渲染一个可摆放控件（由 shell 在布局指定的位置调用）。`None` = 当前没有内容
    /// （尚无剩余空间数据、宿主未注入关机端口），该位置不占空间。
    pub fn render_widget(
        &self,
        widget: DownloadWidget,
        placement: WidgetPlacement,
        view: &WeakEntity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        let stats = self.controller.runtime_stats();
        match widget {
            DownloadWidget::DownloadSpeed => Some(readout_cell(
                FluxIcon::ArrowDown,
                format!("{}/s", format_bytes(stats.total_download_bps.max(0) as u64)),
                cx,
            )),
            DownloadWidget::UploadSpeed => Some(readout_cell(
                FluxIcon::ArrowUp,
                format!("{}/s", format_bytes(stats.total_upload_bps.max(0) as u64)),
                cx,
            )),
            DownloadWidget::PauseAll => {
                self.render_status_action(ToolbarCommand::PauseAll, view.clone(), cx)
            }
            DownloadWidget::ResumeAll => {
                self.render_status_action(ToolbarCommand::ResumeAll, view.clone(), cx)
            }
            DownloadWidget::RetryFailed => {
                self.render_status_action(ToolbarCommand::RetryFailed, view.clone(), cx)
            }
            DownloadWidget::DownloadLimit => {
                Some(self.render_speed_limit_control(&DOWNLOAD_LIMIT, placement, view.clone(), cx))
            }
            DownloadWidget::UploadLimit => {
                Some(self.render_speed_limit_control(&UPLOAD_LIMIT, placement, view.clone(), cx))
            }
            DownloadWidget::Proxy => Some(self.render_proxy_control(placement, view.clone(), cx)),
            DownloadWidget::Shutdown => self.render_shutdown_control(placement, view.clone(), cx),
            DownloadWidget::DiskFree => stats.disk_free_bytes.map(|bytes| {
                let text = self
                    .translator
                    .read(cx)
                    .text_with("diskSpaceFreeLabel", &[("size", &format_bytes(bytes))]);
                readout_cell(FluxIcon::HardDrive, text, cx)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        format_countdown, kb_to_bytes, mb_to_bytes, minutes_to_duration, parse_number_input,
    };

    #[test]
    fn number_input_rejects_blank_garbage_and_below_minimum() {
        assert_eq!(parse_number_input(" 15 ", 1), Some(15));
        assert_eq!(parse_number_input("", 1), None);
        assert_eq!(parse_number_input("abc", 1), None);
        assert_eq!(parse_number_input("-5", 1), None);
        assert_eq!(parse_number_input("0", 1), None);
        assert_eq!(parse_number_input("0", 0), Some(0));
        assert_eq!(parse_number_input("", 0), None);
    }

    #[test]
    fn mb_preset_converts_to_bytes_per_second() {
        assert_eq!(mb_to_bytes(1), 1_048_576);
        assert_eq!(mb_to_bytes(50), 52_428_800);
    }

    #[test]
    fn kb_custom_input_converts_to_bytes_and_clamps_negative() {
        assert_eq!(kb_to_bytes(512), 524_288);
        assert_eq!(kb_to_bytes(0), 0);
        assert_eq!(kb_to_bytes(-10), 0);
    }

    #[test]
    fn minutes_preset_converts_to_duration_and_clamps_negative() {
        assert_eq!(minutes_to_duration(5), Duration::from_secs(300));
        assert_eq!(minutes_to_duration(0), Duration::ZERO);
        assert_eq!(minutes_to_duration(-1), Duration::ZERO);
    }

    #[test]
    fn countdown_switches_from_mm_ss_to_h_mm_ss_at_one_hour() {
        assert_eq!(format_countdown(Duration::from_secs(59)), "0:59");
        assert_eq!(format_countdown(Duration::from_secs(3599)), "59:59");
        assert_eq!(format_countdown(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(format_countdown(Duration::from_secs(3661)), "1:01:01");
    }
}
