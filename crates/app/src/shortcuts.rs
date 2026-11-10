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
    CloseWindow, Hide, HideOthers, MinimizeWindow, OpenSettings, Quit, ToggleCommandPalette,
    ToggleFullScreen,
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
                    match build_binding(binding, &keystroke, cx) {
                        Ok(key_binding) => bindings.push(key_binding),
                        Err(error) => log::warn!(
                            "skipping shortcut `{}` = `{keystroke}`: {error}",
                            entry.spec.id
                        ),
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
