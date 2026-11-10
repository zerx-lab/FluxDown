//! 提示音：五个下载事件各自的提示音（关闭 / 系统提示音 / 自定义文件）与试听。
//!
//! 偏好是设备本地键（不进云同步目录），由 agent 在本机播放；试听走 `agent.sound.preview`。
//! 值语法与键名见 `fluxdown_protocol::sound`。

use std::path::Path;
use std::rc::Rc;

use fluxdown_protocol::sound::{
    SOUND_FILE_EXTENSIONS, SOUND_PREF_QUEUE_FINISHED, SOUND_PREF_QUEUE_STARTED,
    SOUND_PREF_TASK_COMPLETED, SOUND_PREF_TASK_FAILED, SOUND_PREF_TASK_STARTED,
    SOUND_VALUE_FILE_PREFIX, SOUND_VALUE_OFF, SOUND_VALUE_SYSTEM,
};
use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, RpcErrorData, method};
use fluxdown_ui_components::{ButtonVariant, FluxIcon, icon_button, tooltip_view};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyWindowHandle, App, AsyncApp, Entity, InteractiveElement as _, ParentElement as _,
    PathPromptOptions, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_component::{Icon, WindowExt as _, h_flex, notification::Notification};
use serde_json::json;

use super::SectionContext;
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::{
    Control, DROPDOWN_MIN_WIDTH, Setter, SettingsSection, dropdown_button, meta_text, row_button,
};

/// 自定义文件名文字的最大宽度档位（超出省略，完整路径在悬浮提示里）。
const FILE_NAME_MAX_WIDTH: f32 = 150.;

/// 下拉里的三种取值（与偏好值的映射见 [`Mode::of`]）。
const MODE_OFF: &str = "off";
const MODE_SYSTEM: &str = "system";
const MODE_CUSTOM: &str = "custom";

/// 一行：偏好键 + 标题 / 描述文案键（顺序即展示顺序）。
struct EventRow {
    pref: &'static str,
    title: &'static str,
    description: &'static str,
}

const EVENT_ROWS: [EventRow; 5] = [
    EventRow {
        pref: SOUND_PREF_TASK_STARTED,
        title: "soundTaskStarted",
        description: "soundTaskStartedDesc",
    },
    EventRow {
        pref: SOUND_PREF_TASK_COMPLETED,
        title: "soundTaskCompleted",
        description: "soundTaskCompletedDesc",
    },
    EventRow {
        pref: SOUND_PREF_TASK_FAILED,
        title: "soundTaskFailed",
        description: "soundTaskFailedDesc",
    },
    EventRow {
        pref: SOUND_PREF_QUEUE_STARTED,
        title: "soundQueueStarted",
        description: "soundQueueStartedDesc",
    },
    EventRow {
        pref: SOUND_PREF_QUEUE_FINISHED,
        title: "soundQueueFinished",
        description: "soundQueueFinishedDesc",
    },
];

/// 偏好值对应的选择状态。
#[derive(Debug, PartialEq, Eq)]
enum Mode<'a> {
    Off,
    System,
    /// `file:` 之后的路径。
    Custom(&'a str),
}

impl<'a> Mode<'a> {
    /// 缺失 / 无法识别 = 关闭（与 agent 一致）。
    fn of(value: &'a str) -> Self {
        match value {
            SOUND_VALUE_SYSTEM => Self::System,
            _ => value
                .strip_prefix(SOUND_VALUE_FILE_PREFIX)
                .map_or(Self::Off, Self::Custom),
        }
    }

    fn dropdown_value(&self) -> &'static str {
        match self {
            Self::Off => MODE_OFF,
            Self::System => MODE_SYSTEM,
            Self::Custom(_) => MODE_CUSTOM,
        }
    }
}

/// 文件的扩展名是否在允许列表内（大小写不敏感）。
fn is_supported_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SOUND_FILE_EXTENSIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(extension))
        })
}

pub(super) fn section(ctx: &SectionContext) -> SettingsSection {
    EVENT_ROWS.iter().fold(
        SettingsSection::new()
            .title(ctx.t("soundGroup"))
            .subtitle(ctx.t("soundGroupDesc")),
        |section, row| {
            section.row(ctx.item(row.title, Some(row.description), control(ctx, row.pref)))
        },
    )
}

fn control(ctx: &SectionContext, pref: &'static str) -> Control {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let options = vec![
        (SharedString::from(MODE_OFF), ctx.t("soundOptionOff")),
        (SharedString::from(MODE_SYSTEM), ctx.t("soundOptionSystem")),
        (SharedString::from(MODE_CUSTOM), ctx.t("soundOptionCustom")),
    ];
    let choose_label = ctx.t("soundChoose");
    let preview_label = ctx.t("soundPreview");
    Control::field(
        DROPDOWN_MIN_WIDTH,
        move |disabled, key, width, window, cx| {
            let value = store.read(cx).pref_str(pref, SOUND_VALUE_OFF);
            let mode = Mode::of(&value);
            let off = mode == Mode::Off;
            let window_handle = window.window_handle();
            let theme = active_theme(cx);
            let spacing = theme.tokens().spacing.sm;
            let icon_size = theme.extended().icon.md;

            let mut row = h_flex().items_center().gap(spacing);
            if let Mode::Custom(path) = &mode {
                let name = Path::new(path).file_name().map_or_else(
                    || (*path).to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let full_path = SharedString::from((*path).to_owned());
                row = row
                    .child(
                        div()
                            .id(SharedString::from(format!("{key}-file")))
                            .min_w_0()
                            .max_w(theme.text_extent(FILE_NAME_MAX_WIDTH))
                            .tooltip(move |window, cx| tooltip_view(full_path.clone(), window, cx))
                            .child(meta_text(cx).truncate().child(SharedString::from(name))),
                    )
                    .child(
                        row_button(
                            SharedString::from(format!("{key}-choose")),
                            choose_label.clone(),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .disabled(disabled)
                        .on_click({
                            let store = store.clone();
                            let translator = translator.clone();
                            move |_, _, cx| {
                                choose_file(
                                    store.clone(),
                                    pref,
                                    translator.clone(),
                                    window_handle,
                                    cx,
                                );
                            }
                        }),
                    );
            }
            row = row.child(
                icon_button(
                    SharedString::from(format!("{key}-preview")),
                    preview_label.clone(),
                    Icon::new(FluxIcon::Play).size(icon_size),
                    ButtonVariant::Secondary,
                    cx,
                )
                .disabled(disabled || off)
                .tooltip({
                    let label = preview_label.clone();
                    move |window, cx| tooltip_view(label.clone(), window, cx)
                })
                .on_click({
                    let store = store.clone();
                    let translator = translator.clone();
                    move |_, _, cx| {
                        preview(store.clone(), pref, translator.clone(), window_handle, cx)
                    }
                }),
            );
            let set: Setter<SharedString> = {
                let store = store.clone();
                let translator = translator.clone();
                Rc::new(
                    move |selected: SharedString, cx: &mut App| match selected.as_ref() {
                        MODE_OFF => write(&store, pref, SOUND_VALUE_OFF.to_owned(), cx),
                        MODE_SYSTEM => write(&store, pref, SOUND_VALUE_SYSTEM.to_owned(), cx),
                        _ => {
                            // 已是自定义文件时重复选择不再弹出文件框；否则必须先选定文件才进入该状态。
                            let current = store.read(cx).pref_str(pref, SOUND_VALUE_OFF);
                            if !matches!(Mode::of(&current), Mode::Custom(_)) {
                                choose_file(
                                    store.clone(),
                                    pref,
                                    translator.clone(),
                                    window_handle,
                                    cx,
                                );
                            }
                        }
                    },
                )
            };
            row.child(dropdown_button(
                SharedString::from(format!("{key}-mode")),
                &options,
                SharedString::from(mode.dropdown_value()),
                disabled,
                width,
                set,
                cx,
            ))
        },
    )
}

fn write(store: &Entity<SettingsStore>, pref: &'static str, value: String, cx: &mut App) {
    store.update(cx, |store, cx| store.set_pref_str(pref, value, cx));
}

/// 弹出文件选择框；选中且扩展名受支持才写入 `file:<绝对路径>`，否则弹错误通知、偏好不变。
fn choose_file(
    store: Entity<SettingsStore>,
    pref: &'static str,
    translator: Translator,
    window_handle: AnyWindowHandle,
    cx: &mut App,
) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(SharedString::from(
            translator.text("soundChooseTitle").to_owned(),
        )),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = receiver.await else {
            return;
        };
        let Some(path) = paths.into_iter().next() else {
            return;
        };
        if !is_supported_file(&path) {
            let formats = SOUND_FILE_EXTENSIONS.join(" / ");
            let text = translator.text_with("soundErrorInvalidFile", &[("formats", &formats)]);
            notify_error(window_handle, cx, text);
            return;
        }
        let Some(path) = path.to_str() else {
            let text = translator.text("soundErrorInvalidPath").to_owned();
            notify_error(window_handle, cx, text);
            return;
        };
        let value = format!("{SOUND_VALUE_FILE_PREFIX}{path}");
        cx.update(|cx| write(&store, pref, value, cx));
    })
    .detach();
}

/// 请 agent 在本机试听当前选择；失败（文件缺失 / 无法播放 / 宿主不支持）弹错误通知。
fn preview(
    store: Entity<SettingsStore>,
    pref: &'static str,
    translator: Translator,
    window_handle: AnyWindowHandle,
    cx: &mut App,
) {
    let value = store.read(cx).pref_str(pref, SOUND_VALUE_OFF);
    if Mode::of(&value) == Mode::Off {
        return;
    }
    let future = store
        .read(cx)
        .raw_call(method::AGENT_SOUND_PREVIEW, json!({ "value": value }));
    cx.spawn(async move |cx| {
        let Err(error) = future.await else {
            return;
        };
        notify_error(window_handle, cx, preview_error_text(&translator, &error));
    })
    .detach();
}

/// 优先按原因，再按错误码归类；其余退回通用文案。
fn preview_error_text(translator: &Translator, error: &RpcErrorData) -> String {
    match (error.reason, error.code) {
        (Some(ErrorReason::SoundUnplayable), _) => {
            translator.text("soundErrorUnplayable").to_owned()
        }
        (_, ApplicationErrorCode::Unsupported) => {
            translator.text("soundErrorUnavailable").to_owned()
        }
        _ => rpc_error_text(translator, error),
    }
}

fn notify_error(window_handle: AnyWindowHandle, cx: &mut AsyncApp, text: String) {
    let Ok(()) = window_handle.update(cx, move |_, window: &mut Window, cx| {
        window.push_notification(Notification::error(text), cx);
    }) else {
        // 设置窗口已关闭，不再有可展示的位置。
        return;
    };
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{EVENT_ROWS, Mode, is_supported_file};
    use fluxdown_protocol::sound::SOUND_PREF_KEYS;

    #[test]
    fn rows_cover_every_sound_pref_in_order() {
        let keys: Vec<_> = EVENT_ROWS.iter().map(|row| row.pref).collect();
        assert_eq!(keys, SOUND_PREF_KEYS);
    }

    #[test]
    fn preference_values_map_to_modes() {
        assert_eq!(Mode::of("off"), Mode::Off);
        assert_eq!(Mode::of(""), Mode::Off);
        assert_eq!(Mode::of("loud"), Mode::Off);
        assert_eq!(Mode::of("system"), Mode::System);
        assert_eq!(Mode::of("file:/tmp/a.wav"), Mode::Custom("/tmp/a.wav"));
        assert_eq!(Mode::of("file:").dropdown_value(), "custom");
    }

    #[test]
    fn only_audio_extensions_are_accepted_case_insensitively() {
        for name in [
            "a.wav", "a.MP3", "a.m4a", "a.aiff", "a.ogg", "a.oga", "a.FLAC",
        ] {
            assert!(is_supported_file(Path::new(name)), "{name}");
        }
        for name in ["a.txt", "a", "wav", "a.wav.exe"] {
            assert!(!is_supported_file(Path::new(name)), "{name}");
        }
    }
}
