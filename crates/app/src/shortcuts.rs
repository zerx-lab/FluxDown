//! 应用内快捷键：目录 id → GPUI 动作 + 键位上下文的注册表，按用户偏好（`desktop.shortcuts.app`）
//! 重建 keymap，并提供「双击修饰键」监听。
//!
//! 目录（`fluxdown_protocol::shortcuts::SHORTCUT_SPECS`）里的每个应用内 id 必须在 [`ACTIONS`]
//! 有映射，否则该设置项不会生效（测试守住）。用户可编辑的绑定只经本模块写入 keymap；不可编辑
//! 的固定绑定（对话框确认键、命令面板内部键）在 [`install`] 里单独注册。

use std::time::{Duration, Instant};

use fluxdown_protocol::shortcuts::{
    APP_SHORTCUTS_PREF_KEY, DOUBLE_TAP_INTERVAL_MS, ModifierKey, ResolvedShortcut, ShortcutScope,
    ShortcutTrigger, resolve_shortcuts,
};
use fluxdown_ui_downloads::actions as dl;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_settings::{SettingsStore, shortcut_label, shortcut_recording};
use gpui::{Action, App, Entity, Global, KeyBinding, KeyBindingContextPredicate, Keystroke};
use std::collections::HashMap;

use crate::actions::{
    CloseWindow, Hide, HideOthers, MinimizeWindow, OpenSettings, Quit, ToggleAssistant,
    ToggleCommandPalette, ToggleFullScreen,
};

/// 一个应用内快捷键 id 对应的 GPUI 动作与键位上下文。
struct ActionBinding {
    id: &'static str,
    /// `None` = 任意窗口；`Some` = 仅该键位上下文内（下载页）。
    context: Option<&'static str>,
    make: fn() -> Box<dyn Action>,
}

const fn everywhere(id: &'static str, make: fn() -> Box<dyn Action>) -> ActionBinding {
    ActionBinding {
        id,
        context: None,
        make,
    }
}

const fn download_list(id: &'static str, make: fn() -> Box<dyn Action>) -> ActionBinding {
    ActionBinding {
        id,
        context: Some(dl::KEY_CONTEXT),
        make,
    }
}

/// 全部应用内快捷键动作（与目录一一对应）。
const ACTIONS: &[ActionBinding] = &[
    everywhere("new_download", || Box::new(dl::NewDownload)),
    everywhere("open_settings", || Box::new(OpenSettings)),
    everywhere("command_palette", || Box::new(ToggleCommandPalette)),
    everywhere("toggle_assistant", || Box::new(ToggleAssistant)),
    everywhere("close_window", || Box::new(CloseWindow)),
    everywhere("minimize_window", || Box::new(MinimizeWindow)),
    everywhere("toggle_fullscreen", || Box::new(ToggleFullScreen)),
    everywhere("hide", || Box::new(Hide)),
    everywhere("hide_others", || Box::new(HideOthers)),
    everywhere("quit", || Box::new(Quit)),
    download_list("focus_search", || Box::new(dl::FocusSearch)),
    download_list("select_all", || Box::new(dl::SelectAllTasks)),
    download_list("clear_selection", || Box::new(dl::ClearSelection)),
    download_list("toggle_pause", || Box::new(dl::TogglePauseSelected)),
    download_list("pause_all", || Box::new(dl::PauseAll)),
    download_list("resume_all", || Box::new(dl::ResumeAll)),
    download_list("delete_selected", || Box::new(dl::DeleteSelected)),
    download_list("delete_with_files", || {
        Box::new(dl::DeleteSelectedWithFiles)
    }),
    download_list("open_torrent_file", || Box::new(dl::OpenTorrentFile)),
    download_list("toggle_detail", || Box::new(dl::ToggleDetailPanel)),
    download_list("cycle_density", || Box::new(dl::CycleDensity)),
    download_list("cycle_group", || Box::new(dl::CycleGroupBy)),
    download_list("cycle_sort", || Box::new(dl::CycleSort)),
];

/// 跟随某个目录快捷键键位、只在更窄键位上下文生效的附属动作：不进目录、不单独可编辑。
///
/// 无上下文的绑定按「最深层」参与优先级比较，单写 `AssistantView` 会输给全局的新建下载
/// （焦点在输入框时 `Input` 更深）。`AssistantView > !AssistantView` 让附属绑定在面板内任意
/// 深度都按最深层匹配，同深度时后装入者胜出（附属绑定紧跟原绑定装入）。
const FOLLOWERS: &[(&str, ActionBinding)] = &[(
    "new_download",
    ActionBinding {
        id: "assistant_new_chat",
        context: Some(ASSISTANT_PANEL_PREDICATE),
        make: || Box::new(fluxdown_ui_assistant::actions::NewChat),
    },
)];

/// 助手面板内的固定按键（不进目录、不可编辑，也没有可跟随的目录键位）：Esc 取消 / 停止 / 退出
/// 历史视图，历史列表里上下键移动光标、回车打开。键位上下文限定在助手面板内，不抢对话框与
/// 其他输入框的按键；装入时机晚于 gpui-component 的初始化，同深度的 `Input` 绑定输给它们。
const ASSISTANT_FIXED: &[(&str, ActionBinding)] = &[
    (
        "escape",
        ActionBinding {
            id: "assistant_dismiss",
            context: Some(ASSISTANT_PANEL_PREDICATE),
            make: || Box::new(fluxdown_ui_assistant::actions::Dismiss),
        },
    ),
    (
        "up",
        ActionBinding {
            id: "assistant_history_prev",
            context: Some(fluxdown_ui_assistant::actions::HISTORY_KEY_CONTEXT),
            make: || Box::new(fluxdown_ui_assistant::actions::HistoryPrev),
        },
    ),
    (
        "down",
        ActionBinding {
            id: "assistant_history_next",
            context: Some(fluxdown_ui_assistant::actions::HISTORY_KEY_CONTEXT),
            make: || Box::new(fluxdown_ui_assistant::actions::HistoryNext),
        },
    ),
    (
        "enter",
        ActionBinding {
            id: "assistant_history_open",
            context: Some(fluxdown_ui_assistant::actions::HISTORY_KEY_CONTEXT),
            make: || Box::new(fluxdown_ui_assistant::actions::HistoryOpen),
        },
    ),
];

/// 焦点在助手面板（`fluxdown_ui_assistant::actions::KEY_CONTEXT`）内任意位置。
const ASSISTANT_PANEL_PREDICATE: &str = "AssistantView || (AssistantView > !AssistantView)";

fn action_binding(id: &str) -> Option<&'static ActionBinding> {
    ACTIONS.iter().find(|binding| binding.id == id)
}

/// 当前已装入 keymap 的应用内绑定表（按目录顺序的触发器列表），用于判断是否需要重建。
#[derive(Default)]
struct InstalledShortcuts {
    table: Vec<Vec<ShortcutTrigger>>,
    /// 双击修饰键 → 动作 id。
    double_taps: Vec<(ModifierKey, &'static str)>,
}

impl Global for InstalledShortcuts {}

fn resolved(store: &Entity<SettingsStore>, cx: &App) -> Vec<ResolvedShortcut> {
    resolve_shortcuts(
        ShortcutScope::App,
        store.read(cx).pref(APP_SHORTCUTS_PREF_KEY),
    )
}

/// 组合键 → GPUI 键位串；`super` 在 GPUI 里写作 `cmd`（各平台均映射到平台键）。
fn chord_keystroke(chord: &fluxdown_protocol::shortcuts::ShortcutChord) -> String {
    let mut text = String::new();
    for (on, name) in [
        (chord.ctrl, "ctrl"),
        (chord.alt, "alt"),
        (chord.shift, "shift"),
        (chord.super_key, "cmd"),
    ] {
        if on {
            text.push_str(name);
            text.push('-');
        }
    }
    text.push_str(chord.key());
    text
}

fn build_binding(binding: &ActionBinding, keystroke: &str, cx: &App) -> Result<KeyBinding, String> {
    let predicate = binding
        .context
        .map(|context| {
            KeyBindingContextPredicate::parse(context)
                .map(std::rc::Rc::new)
                .map_err(|error| format!("{error:#}"))
        })
        .transpose()?;
    KeyBinding::load(keystroke, (binding.make)(), predicate, false, None, cx)
        .map_err(|error| format!("{error:#}"))
}

/// 装配应用内快捷键：固定绑定 + 用户可编辑绑定 + 双击修饰键监听，并随偏好变化重建。
pub fn install(cx: &mut App, store: &Entity<SettingsStore>, translator: &Entity<Translator>) {
    cx.bind_keys([KeyBinding::new(
        "space",
        gpui_component::dialog::Confirm { secondary: false },
        Some(fluxdown_ui_components::DIALOG_PRIMARY_KEY_CONTEXT),
    )]);
    fluxdown_ui_command_palette::bind_keys(cx);
    let assistant_keys: Vec<KeyBinding> = ASSISTANT_FIXED
        .iter()
        .filter_map(
            |(keystroke, binding)| match build_binding(binding, keystroke, cx) {
                Ok(key_binding) => Some(key_binding),
                Err(error) => {
                    log::warn!(
                        "skipping fixed shortcut `{}` = `{keystroke}`: {error}",
                        binding.id
                    );
                    None
                }
            },
        )
        .collect();
    cx.bind_keys(assistant_keys);

    sync(store, translator, cx);
    let (observed_store, observed_translator) = (store.clone(), translator.clone());
    cx.observe(store, move |_, cx| {
        sync(&observed_store, &observed_translator, cx);
    })
    .detach();
    install_double_tap(cx);
}

/// 偏好解析结果与已装入的绑定表不同时，重建受管动作的键位。
fn sync(store: &Entity<SettingsStore>, translator: &Entity<Translator>, cx: &mut App) {
    let resolved = resolved(store, cx);
    let table: Vec<Vec<ShortcutTrigger>> = resolved
        .iter()
        .map(|entry| entry.triggers.clone())
        .collect();
    if cx
        .try_global::<InstalledShortcuts>()
        .is_some_and(|installed| installed.table == table)
    {
        return;
    }

    let mut bindings = Vec::new();
    let mut double_taps = Vec::new();
    for entry in &resolved {
        let Some(binding) = action_binding(entry.spec.id) else {
            log::error!("shortcut `{}` has no GPUI action mapping", entry.spec.id);
            continue;
        };
        for trigger in &entry.triggers {
            match trigger {
                ShortcutTrigger::Chord(chord) => {
                    let keystroke = chord_keystroke(chord);
                    let followers = FOLLOWERS
                        .iter()
                        .filter(|(id, _)| *id == entry.spec.id)
                        .map(|(_, follower)| follower);
                    for target in std::iter::once(binding).chain(followers) {
                        match build_binding(target, &keystroke, cx) {
                            Ok(key_binding) => bindings.push(key_binding),
                            Err(error) => log::warn!(
                                "skipping shortcut `{}` = `{keystroke}`: {error}",
                                target.id
                            ),
                        }
                    }
                }
                ShortcutTrigger::DoubleTap(modifier) => {
                    double_taps.push((*modifier, binding.id));
                }
            }
        }
    }

    // 只替换受管动作的绑定；其他 crate（gpui-component、命令面板、对话框）的绑定原样保留。
    let managed: Vec<&'static str> = ACTIONS
        .iter()
        .chain(FOLLOWERS.iter().map(|(_, follower)| follower))
        .map(|binding| (binding.make)().name())
        .collect();
    let keymap = cx.key_bindings();
    {
        let mut keymap = keymap.borrow_mut();
        let retained: Vec<KeyBinding> = keymap
            .bindings()
            .filter(|binding| !managed.contains(&binding.action().name()))
            .cloned()
            .collect();
        keymap.clear();
        keymap.add_bindings(retained);
        keymap.add_bindings(bindings);
    }
    cx.set_global(InstalledShortcuts { table, double_taps });
    cx.refresh_windows();
    // macOS 原生菜单的 key equivalent 取自 keymap，需重新装配。
    crate::menus::apply_menus(translator, cx);
}

/// 命令面板等处的快捷键提示：每个已绑定动作的第一个触发器的展示文本；未绑定的动作不在表内。
pub fn hints(
    store: &Entity<SettingsStore>,
    translator: &Translator,
    cx: &App,
) -> HashMap<&'static str, String> {
    resolved(store, cx)
        .iter()
        .filter_map(|entry| {
            let trigger = entry.triggers.first()?;
            Some((entry.spec.id, shortcut_label(trigger, translator)))
        })
        .collect()
}

/// 一次独立修饰键敲击或其它按键。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TapInput {
    Tap(ModifierKey),
    Other,
}

/// 双击配对：同一修饰键在间隔内连续敲击两次即触发；任何其它按键或另一个修饰键的敲击打断。
struct TapPairing {
    last: Option<(ModifierKey, Instant)>,
    interval: Duration,
}

impl TapPairing {
    fn new(interval: Duration) -> Self {
        Self {
            last: None,
            interval,
        }
    }

    fn reset(&mut self) {
        self.last = None;
    }

    fn feed(&mut self, input: TapInput, now: Instant) -> Option<ModifierKey> {
        match input {
            TapInput::Other => {
                self.last = None;
                None
            }
            TapInput::Tap(modifier) => match self.last.take() {
                Some((previous, at))
                    if previous == modifier
                        && now.saturating_duration_since(at) <= self.interval =>
                {
                    Some(modifier)
                }
                _ => {
                    self.last = Some((modifier, now));
                    None
                }
            },
        }
    }
}

/// GPUI 在「只按了一个修饰键并松开」时合成无修饰符、键名为修饰键名的按键。
fn tap_input(keystroke: &Keystroke) -> TapInput {
    if keystroke.modifiers.modified() {
        return TapInput::Other;
    }
    match keystroke.key.as_str() {
        "control" => TapInput::Tap(ModifierKey::Ctrl),
        "alt" => TapInput::Tap(ModifierKey::Alt),
        "shift" => TapInput::Tap(ModifierKey::Shift),
        "platform" => TapInput::Tap(ModifierKey::Super),
        _ => TapInput::Other,
    }
}

fn install_double_tap(cx: &mut App) {
    let mut pairing = TapPairing::new(Duration::from_millis(DOUBLE_TAP_INTERVAL_MS));
    cx.observe_keystrokes(move |event, window, cx| {
        if shortcut_recording(cx) {
            pairing.reset();
            return;
        }
        let Some(modifier) = pairing.feed(tap_input(&event.keystroke), Instant::now()) else {
            return;
        };
        let Some(installed) = cx.try_global::<InstalledShortcuts>() else {
            return;
        };
        let actions: Vec<Box<dyn Action>> = installed
            .double_taps
            .iter()
            .filter(|(bound, _)| *bound == modifier)
            .filter_map(|(_, id)| action_binding(id))
            .map(|binding| (binding.make)())
            .collect();
        for action in actions {
            window.dispatch_action(action, cx);
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::shortcuts::{
        SHORTCUT_KEYS, SHORTCUT_SPECS, ShortcutChord, ShortcutDomain,
    };

    use super::*;

    #[test]
    fn every_app_spec_has_an_action_mapping() {
        for spec in SHORTCUT_SPECS
            .iter()
            .filter(|spec| spec.scope == ShortcutScope::App)
        {
            let binding = action_binding(spec.id)
                .unwrap_or_else(|| panic!("app shortcut `{}` has no GPUI action", spec.id));
            let expected = match spec.domain {
                ShortcutDomain::Everywhere => None,
                ShortcutDomain::DownloadList => Some(dl::KEY_CONTEXT),
            };
            assert_eq!(binding.context, expected, "{}", spec.id);
        }
    }

    #[test]
    fn assistant_panel_takes_new_download_chord() {
        let new_download = action_binding("new_download").unwrap();
        let (_, follower) = FOLLOWERS
            .iter()
            .find(|(id, _)| *id == "new_download")
            .unwrap();
        let keymap = gpui::Keymap::new(vec![
            KeyBinding::new("cmd-n", dl::NewDownload, new_download.context),
            KeyBinding::new(
                "cmd-n",
                fluxdown_ui_assistant::actions::NewChat,
                follower.context,
            ),
        ]);
        let keystroke = [Keystroke::parse("cmd-n").unwrap()];
        let first_action = |stack: &[&str]| {
            let contexts: Vec<gpui::KeyContext> = stack
                .iter()
                .map(|context| gpui::KeyContext::parse(context).unwrap())
                .collect();
            let (bindings, _) = keymap.bindings_for_input(&keystroke, &contexts);
            bindings.first().map(|binding| binding.action().name())
        };
        let new_chat = Some(fluxdown_ui_assistant::actions::NewChat.name());
        let assistant = fluxdown_ui_assistant::actions::KEY_CONTEXT;
        assert_eq!(first_action(&["Root", assistant, "Input"]), new_chat);
        assert_eq!(first_action(&["Root", assistant]), new_chat);
        assert_eq!(
            first_action(&["Root", "DownloadView", "Input"]),
            Some(dl::NewDownload.name())
        );
        assert_eq!(first_action(&[]), Some(dl::NewDownload.name()));
    }

    #[test]
    fn assistant_fixed_keys_stay_inside_the_panel() {
        use fluxdown_ui_assistant::actions as ai;
        // 与 `ASSISTANT_FIXED` 同序同上下文；这里只验证上下文谓词的作用域。
        let context_of = |keystroke: &str| {
            ASSISTANT_FIXED
                .iter()
                .find(|(key, _)| *key == keystroke)
                .and_then(|(_, binding)| binding.context)
        };
        let keymap = gpui::Keymap::new(vec![
            KeyBinding::new("escape", ai::Dismiss, context_of("escape")),
            KeyBinding::new("up", ai::HistoryPrev, context_of("up")),
            KeyBinding::new("down", ai::HistoryNext, context_of("down")),
            KeyBinding::new("enter", ai::HistoryOpen, context_of("enter")),
        ]);
        let first_action = |key: &'static str, stack: &[&str]| {
            let keystroke = [Keystroke::parse(key).unwrap()];
            let contexts: Vec<gpui::KeyContext> = stack
                .iter()
                .map(|context| gpui::KeyContext::parse(context).unwrap())
                .collect();
            let (bindings, _) = keymap.bindings_for_input(&keystroke, &contexts);
            bindings.first().map(|binding| binding.action().name())
        };
        let panel = ai::KEY_CONTEXT;
        let history = ai::HISTORY_KEY_CONTEXT;
        assert_eq!(
            first_action("escape", &["Root", panel, "Input"]),
            Some(ai::Dismiss.name())
        );
        assert_eq!(
            first_action("escape", &["Root", panel]),
            Some(ai::Dismiss.name())
        );
        assert_eq!(first_action("escape", &["Root", "Dialog", "Input"]), None);
        assert_eq!(first_action("escape", &["Root"]), None);
        assert_eq!(
            first_action("up", &["Root", panel, history]),
            Some(ai::HistoryPrev.name())
        );
        assert_eq!(
            first_action("down", &["Root", panel, history]),
            Some(ai::HistoryNext.name())
        );
        assert_eq!(
            first_action("enter", &["Root", panel, history]),
            Some(ai::HistoryOpen.name())
        );
        assert_eq!(first_action("enter", &["Root", panel, "Input"]), None);
        assert_eq!(first_action("up", &["Root", "DownloadView"]), None);
    }

    #[test]
    fn every_action_mapping_has_an_app_spec() {
        for binding in ACTIONS {
            assert!(
                SHORTCUT_SPECS
                    .iter()
                    .any(|spec| spec.scope == ShortcutScope::App && spec.id == binding.id),
                "mapping `{}` has no app spec",
                binding.id
            );
        }
        let mut ids: Vec<_> = ACTIONS.iter().map(|binding| binding.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len(), "duplicate mapping ids");
    }

    #[test]
    fn chord_keystrokes_parse_for_every_key() {
        for key in SHORTCUT_KEYS {
            let chord = ShortcutChord::new(true, true, true, true, key).expect("catalog key");
            let text = chord_keystroke(&chord);
            let parsed = Keystroke::parse_with_os(&text, gpui::OperatingSystem::Mac)
                .unwrap_or_else(|error| panic!("`{text}` must parse: {error}"));
            assert!(
                parsed.modifiers.control
                    && parsed.modifiers.alt
                    && parsed.modifiers.shift
                    && parsed.modifiers.platform,
                "{text}"
            );
            assert_eq!(parsed.key, *key, "{text}");
        }
    }

    #[test]
    fn modifier_only_keystrokes_become_taps() {
        let tap = |key: &str| Keystroke {
            modifiers: gpui::Modifiers::default(),
            key: key.to_owned(),
            key_char: None,
        };
        assert_eq!(tap_input(&tap("control")), TapInput::Tap(ModifierKey::Ctrl));
        assert_eq!(tap_input(&tap("alt")), TapInput::Tap(ModifierKey::Alt));
        assert_eq!(tap_input(&tap("shift")), TapInput::Tap(ModifierKey::Shift));
        assert_eq!(
            tap_input(&tap("platform")),
            TapInput::Tap(ModifierKey::Super)
        );
        assert_eq!(tap_input(&tap("function")), TapInput::Other);
        assert_eq!(tap_input(&tap("a")), TapInput::Other);
        let chord = Keystroke::parse("ctrl-a").expect("parse");
        assert_eq!(tap_input(&chord), TapInput::Other);
    }

    #[test]
    fn double_tap_pairs_within_interval_only() {
        let interval = Duration::from_millis(DOUBLE_TAP_INTERVAL_MS);
        let start = Instant::now();
        let ctrl = TapInput::Tap(ModifierKey::Ctrl);
        let mut pairing = TapPairing::new(interval);

        assert_eq!(pairing.feed(ctrl, start), None);
        assert_eq!(
            pairing.feed(ctrl, start + interval / 2),
            Some(ModifierKey::Ctrl)
        );
        // 触发后重新计数：第三次敲击不会立刻再触发。
        assert_eq!(
            pairing.feed(ctrl, start + interval / 2 + Duration::from_millis(10)),
            None
        );

        // 超时：第二次视作新一轮的第一次。
        let mut pairing = TapPairing::new(interval);
        assert_eq!(pairing.feed(ctrl, start), None);
        assert_eq!(pairing.feed(ctrl, start + interval * 2), None);
        assert_eq!(
            pairing.feed(ctrl, start + interval * 2 + Duration::from_millis(50)),
            Some(ModifierKey::Ctrl)
        );
    }

    #[test]
    fn other_input_or_other_modifier_breaks_pairing() {
        let interval = Duration::from_millis(DOUBLE_TAP_INTERVAL_MS);
        let start = Instant::now();
        let ctrl = TapInput::Tap(ModifierKey::Ctrl);
        let shift = TapInput::Tap(ModifierKey::Shift);
        let step = Duration::from_millis(50);

        let mut pairing = TapPairing::new(interval);
        assert_eq!(pairing.feed(ctrl, start), None);
        assert_eq!(pairing.feed(TapInput::Other, start + step), None);
        assert_eq!(pairing.feed(ctrl, start + step * 2), None);

        let mut pairing = TapPairing::new(interval);
        assert_eq!(pairing.feed(ctrl, start), None);
        assert_eq!(pairing.feed(shift, start + step), None);
        assert_eq!(pairing.feed(ctrl, start + step * 2), None);
        assert_eq!(
            pairing.feed(ctrl, start + step * 3),
            Some(ModifierKey::Ctrl)
        );
    }
}
