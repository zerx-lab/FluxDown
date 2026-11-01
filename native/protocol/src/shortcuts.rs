//! 快捷键模型：应用内（GPUI keymap）与全局（agent 系统级热键）共用的触发器语法、动作目录、
//! 偏好键与全局热键注册状态。
//!
//! 偏好存为设备本地键（不在同步目录里，键盘布局与平台修饰键因设备而异）：
//! `{ "<动作 id>": ["<触发器>", ...] }`。缺省的动作用平台默认值，空数组 = 用户解绑；
//! 未知动作 id 与无法解析 / 不适用于该作用域的触发器在解析时丢弃。
//!
//! 触发器文本语法（规范形式，`Display` 输出与 `FromStr` 互逆）：
//! - 组合键：`[ctrl-][alt-][shift-][super-]<key>`，修饰键按此固定顺序；`super` = macOS ⌘ /
//!   Windows Win / Linux Super。`<key>` 取自 [`SHORTCUT_KEYS`]。
//! - 双击修饰键：`double:<ctrl|alt|shift|super>`（单独按下并松开同一修饰键两次，间隔不超过
//!   [`DOUBLE_TAP_INTERVAL_MS`]，期间没有其它按键）。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// 应用内快捷键覆盖（设备本地偏好）。
pub const APP_SHORTCUTS_PREF_KEY: &str = "desktop.shortcuts.app";
/// 全局快捷键覆盖（设备本地偏好；由 agent 读取并注册系统级热键）。
pub const GLOBAL_SHORTCUTS_PREF_KEY: &str = "desktop.shortcuts.global";
/// 双击修饰键的两次松开最大间隔。
pub const DOUBLE_TAP_INTERVAL_MS: u64 = 400;

/// 组合键支持的主键（规范名，与 GPUI keystroke 键名一致）。
pub const SHORTCUT_KEYS: &[&str] = &[
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
    "space",
    "enter",
    "tab",
    "escape",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "-",
    "=",
    "[",
    "]",
    "\\",
    ";",
    "'",
    ",",
    ".",
    "/",
    "`",
];

/// 修饰键（双击触发器与组合键共用的平台无关名）。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModifierKey {
    Ctrl,
    Alt,
    Shift,
    /// macOS ⌘ / Windows Win / Linux Super。
    Super,
}

impl ModifierKey {
    pub const ALL: [ModifierKey; 4] = [Self::Ctrl, Self::Alt, Self::Shift, Self::Super];

    /// 规范文本名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
            Self::Shift => "shift",
            Self::Super => "super",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "ctrl" => Some(Self::Ctrl),
            "alt" => Some(Self::Alt),
            "shift" => Some(Self::Shift),
            "super" => Some(Self::Super),
            _ => None,
        }
    }

    /// 当前平台的展示名（macOS 用符号，其余平台用文字）。
    pub fn label(self) -> &'static str {
        if cfg!(target_os = "macos") {
            match self {
                Self::Ctrl => "⌃",
                Self::Alt => "⌥",
                Self::Shift => "⇧",
                Self::Super => "⌘",
            }
        } else {
            match self {
                Self::Ctrl => "Ctrl",
                Self::Alt => "Alt",
                Self::Shift => "Shift",
                Self::Super if cfg!(windows) => "Win",
                Self::Super => "Super",
            }
        }
    }
}

/// 组合键：零到多个修饰键 + 一个主键。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ShortcutChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
    /// 规范主键名，必为 [`SHORTCUT_KEYS`] 之一。
    key: &'static str,
}

impl ShortcutChord {
    /// 构造组合键；主键不在 [`SHORTCUT_KEYS`] 内时返回 `None`（大写字母自动转小写）。
    pub fn new(ctrl: bool, alt: bool, shift: bool, super_key: bool, key: &str) -> Option<Self> {
        let key = canonical_key(key)?;
        Some(Self {
            ctrl,
            alt,
            shift,
            super_key,
            key,
        })
    }

    pub fn key(&self) -> &'static str {
        self.key
    }

    /// 是否带 Shift 以外的修饰键。
    pub fn has_command_modifier(&self) -> bool {
        self.ctrl || self.alt || self.super_key
    }

    /// 已按下的修饰键（规范顺序）。
    pub fn modifiers(&self) -> impl Iterator<Item = ModifierKey> + '_ {
        [
            (self.ctrl, ModifierKey::Ctrl),
            (self.alt, ModifierKey::Alt),
            (self.shift, ModifierKey::Shift),
            (self.super_key, ModifierKey::Super),
        ]
        .into_iter()
        .filter_map(|(on, modifier)| on.then_some(modifier))
    }

    /// 当前平台的展示文本：macOS `⌃⌥⇧⌘D`（Apple 规范顺序），其余平台 `Ctrl+Alt+Shift+Win+D`。
    pub fn label(&self) -> String {
        let key = key_label(self.key);
        if cfg!(target_os = "macos") {
            let mut out = String::new();
            for (on, modifier) in [
                (self.ctrl, ModifierKey::Ctrl),
                (self.alt, ModifierKey::Alt),
                (self.shift, ModifierKey::Shift),
                (self.super_key, ModifierKey::Super),
            ] {
                if on {
                    out.push_str(modifier.label());
                }
            }
            out.push_str(key);
            out
        } else {
            let mut parts: Vec<&str> = self.modifiers().map(ModifierKey::label).collect();
            parts.push(key);
            parts.join("+")
        }
    }
}

fn canonical_key(key: &str) -> Option<&'static str> {
    let lower = key.to_ascii_lowercase();
    SHORTCUT_KEYS
        .iter()
        .copied()
        .find(|candidate| *candidate == lower)
}

/// 主键的平台展示名。
pub fn key_label(key: &str) -> &str {
    let mac = cfg!(target_os = "macos");
    match key {
        "space" => "Space",
        "enter" if mac => "↩",
        "enter" => "Enter",
        "tab" if mac => "⇥",
        "tab" => "Tab",
        "escape" => "Esc",
        "backspace" if mac => "⌫",
        "backspace" => "Backspace",
        "delete" if mac => "⌦",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PgUp",
        "pagedown" => "PgDn",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "a" => "A",
        "b" => "B",
        "c" => "C",
        "d" => "D",
        "e" => "E",
        "f" => "F",
        "g" => "G",
        "h" => "H",
        "i" => "I",
        "j" => "J",
        "k" => "K",
        "l" => "L",
        "m" => "M",
        "n" => "N",
        "o" => "O",
        "p" => "P",
        "q" => "Q",
        "r" => "R",
        "s" => "S",
        "t" => "T",
        "u" => "U",
        "v" => "V",
        "w" => "W",
        "x" => "X",
        "y" => "Y",
        "z" => "Z",
        "f1" => "F1",
        "f2" => "F2",
        "f3" => "F3",
        "f4" => "F4",
        "f5" => "F5",
        "f6" => "F6",
        "f7" => "F7",
        "f8" => "F8",
        "f9" => "F9",
        "f10" => "F10",
        "f11" => "F11",
        "f12" => "F12",
        "f13" => "F13",
        "f14" => "F14",
        "f15" => "F15",
        "f16" => "F16",
        "f17" => "F17",
        "f18" => "F18",
        "f19" => "F19",
        "f20" => "F20",
        other => other,
    }
}

/// 一个快捷键触发器。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ShortcutTrigger {
    Chord(ShortcutChord),
    /// 双击同一修饰键（左右任一侧）。
    DoubleTap(ModifierKey),
}

/// 触发器文本无法解析。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutParseError(pub String);

impl fmt::Display for ShortcutParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid shortcut trigger: {:?}", self.0)
    }
}

impl std::error::Error for ShortcutParseError {}

/// 触发器不适用于某作用域的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutRejection {
    /// 全局组合键必须带 Ctrl / Alt / Super 之一（F1–F20 除外），否则会吞掉普通输入。
    GlobalNeedsModifier,
}

impl ShortcutTrigger {
    /// 当前平台的展示文本；双击触发器只给出修饰键名，「双击」字样由 UI 本地化。
    pub fn label(&self) -> String {
        match self {
            Self::Chord(chord) => chord.label(),
            Self::DoubleTap(modifier) => modifier.label().to_owned(),
        }
    }

    /// 校验触发器能否用于给定作用域。
    pub fn check_scope(&self, scope: ShortcutScope) -> Result<(), ShortcutRejection> {
        match (scope, self) {
            (ShortcutScope::Global, Self::Chord(chord))
                if !chord.has_command_modifier() && !is_function_key(chord.key) =>
            {
                Err(ShortcutRejection::GlobalNeedsModifier)
            }
            _ => Ok(()),
        }
    }
}

fn is_function_key(key: &str) -> bool {
    key.len() >= 2 && key.starts_with('f') && key[1..].bytes().all(|byte| byte.is_ascii_digit())
}

impl fmt::Display for ShortcutTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DoubleTap(modifier) => write!(f, "double:{}", modifier.as_str()),
            Self::Chord(chord) => {
                for modifier in chord.modifiers() {
                    write!(f, "{}-", modifier.as_str())?;
                }
                f.write_str(chord.key)
            }
        }
    }
}

impl FromStr for ShortcutTrigger {
    type Err = ShortcutParseError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let error = || ShortcutParseError(source.to_owned());
        let text = source.trim();
        if let Some(name) = text.strip_prefix("double:") {
            return ModifierKey::parse(name)
                .map(Self::DoubleTap)
                .ok_or_else(error);
        }
        // 主键本身可能是 `-`：`ctrl--` / `-`。
        let (prefix, key) = match text.strip_suffix("--") {
            Some(prefix) => (prefix, "-"),
            None if text == "-" => ("", "-"),
            None => match text.rsplit_once('-') {
                Some((prefix, key)) => (prefix, key),
                None => ("", text),
            },
        };
        let mut chord = ShortcutChord::new(false, false, false, false, key).ok_or_else(error)?;
        let mut last = None;
        for part in prefix.split('-').filter(|part| !part.is_empty()) {
            let modifier = ModifierKey::parse(part).ok_or_else(error)?;
            // 规范顺序且不重复，保证一个组合只有一种文本形式。
            if last.is_some_and(|previous| previous >= modifier) {
                return Err(error());
            }
            last = Some(modifier);
            match modifier {
                ModifierKey::Ctrl => chord.ctrl = true,
                ModifierKey::Alt => chord.alt = true,
                ModifierKey::Shift => chord.shift = true,
                ModifierKey::Super => chord.super_key = true,
            }
        }
        if prefix.split('-').any(str::is_empty) && !prefix.is_empty() {
            return Err(error());
        }
        Ok(Self::Chord(chord))
    }
}

impl Serialize for ShortcutTrigger {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ShortcutTrigger {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// 快捷键作用域。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ShortcutScope {
    /// 应用内：FluxDown 窗口获焦时生效（GPUI keymap）。
    App,
    /// 全局：系统级热键，由 agent 注册，界面关闭后仍生效。
    Global,
}

/// 应用内快捷键的生效区域（冲突判定用：区域重叠才算冲突）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutDomain {
    /// 任意窗口。
    Everywhere,
    /// 下载列表页（GPUI `DownloadView` 上下文）。
    DownloadList,
}

impl ShortcutDomain {
    fn overlaps(self, other: Self) -> bool {
        self == Self::Everywhere || other == Self::Everywhere || self == other
    }
}

/// 可绑定快捷键的一个动作。
#[derive(Clone, Copy, Debug)]
pub struct ShortcutSpec {
    /// 稳定 id（偏好 JSON 键）。
    pub id: &'static str,
    pub scope: ShortcutScope,
    pub domain: ShortcutDomain,
    /// 动作名的 i18n 键。
    pub label_key: &'static str,
    /// 仅 macOS 存在的动作（隐藏应用等）。
    pub mac_only: bool,
    defaults_mac: &'static [&'static str],
    defaults_other: &'static [&'static str],
}

impl ShortcutSpec {
    /// 当前平台是否提供此动作。
    pub fn available(&self) -> bool {
        !self.mac_only || cfg!(target_os = "macos")
    }

    /// 当前平台的默认触发器。
    pub fn defaults(&self) -> Vec<ShortcutTrigger> {
        let raw = if cfg!(target_os = "macos") {
            self.defaults_mac
        } else {
            self.defaults_other
        };
        raw.iter().filter_map(|text| text.parse().ok()).collect()
    }
}

const fn app(
    id: &'static str,
    domain: ShortcutDomain,
    label_key: &'static str,
    defaults_mac: &'static [&'static str],
    defaults_other: &'static [&'static str],
) -> ShortcutSpec {
    ShortcutSpec {
        id,
        scope: ShortcutScope::App,
        domain,
        label_key,
        mac_only: false,
        defaults_mac,
        defaults_other,
    }
}

const fn mac_app(
    id: &'static str,
    label_key: &'static str,
    default: &'static [&'static str],
) -> ShortcutSpec {
    ShortcutSpec {
        id,
        scope: ShortcutScope::App,
        domain: ShortcutDomain::Everywhere,
        label_key,
        mac_only: true,
        defaults_mac: default,
        defaults_other: &[],
    }
}

const fn global(id: &'static str, label_key: &'static str) -> ShortcutSpec {
    ShortcutSpec {
        id,
        scope: ShortcutScope::Global,
        domain: ShortcutDomain::Everywhere,
        label_key,
        mac_only: false,
        defaults_mac: &[],
        defaults_other: &[],
    }
}

use ShortcutDomain::{DownloadList, Everywhere};

/// 动作目录（设置页展示顺序）。全局动作默认不绑定，避免抢占其它软件的系统级热键。
pub const SHORTCUT_SPECS: &[ShortcutSpec] = &[
    // ── 应用内：任意窗口 ──
    app(
        "new_download",
        Everywhere,
        "shortcutActionNewDownload",
        &["super-n"],
        &["ctrl-n"],
    ),
    app(
        "open_settings",
        Everywhere,
        "shortcutActionOpenSettings",
        &["super-,"],
        &["ctrl-,"],
    ),
    app(
        "command_palette",
        Everywhere,
        "shortcutActionCommandPalette",
        &["super-k"],
        &["ctrl-k"],
    ),
    app(
        "close_window",
        Everywhere,
        "shortcutActionCloseWindow",
        &["super-w"],
        &["ctrl-w"],
    ),
    app(
        "minimize_window",
        Everywhere,
        "shortcutActionMinimizeWindow",
        &["super-m"],
        &[],
    ),
    app(
        "toggle_fullscreen",
        Everywhere,
        "shortcutActionToggleFullScreen",
        &["ctrl-super-f"],
        &[],
    ),
    mac_app("hide", "shortcutActionHide", &["super-h"]),
    mac_app("hide_others", "shortcutActionHideOthers", &["alt-super-h"]),
    app("quit", Everywhere, "shortcutActionQuit", &["super-q"], &[]),
    // ── 应用内：下载列表 ──
    app(
        "focus_search",
        DownloadList,
        "shortcutActionFocusSearch",
        &["super-f"],
        &["ctrl-f"],
    ),
    app(
        "select_all",
        DownloadList,
        "shortcutActionSelectAll",
        &["super-a"],
        &["ctrl-a"],
    ),
    app(
        "clear_selection",
        DownloadList,
        "shortcutActionClearSelection",
        &["escape"],
        &["escape"],
    ),
    app(
        "toggle_pause",
        DownloadList,
        "shortcutActionTogglePause",
        &["space"],
        &["space"],
    ),
    app(
        "pause_all",
        DownloadList,
        "shortcutActionPauseAll",
        &[],
        &[],
    ),
    app(
        "resume_all",
        DownloadList,
        "shortcutActionResumeAll",
        &[],
        &[],
    ),
    app(
        "delete_selected",
        DownloadList,
        "shortcutActionDeleteSelected",
        &["delete", "backspace", "super-backspace"],
        &["delete", "backspace", "ctrl-backspace"],
    ),
    app(
        "delete_with_files",
        DownloadList,
        "shortcutActionDeleteWithFiles",
        &["shift-delete"],
        &["shift-delete"],
    ),
    app(
        "open_torrent_file",
        DownloadList,
        "shortcutActionOpenTorrentFile",
        &[],
        &[],
    ),
    app(
        "toggle_detail",
        DownloadList,
        "shortcutActionToggleDetail",
        &["super-i"],
        &["ctrl-i"],
    ),
    app(
        "cycle_density",
        DownloadList,
        "shortcutActionCycleDensity",
        &["shift-d"],
        &["shift-d"],
    ),
    app(
        "cycle_group",
        DownloadList,
        "shortcutActionCycleGroup",
        &["g"],
        &["g"],
    ),
    app(
        "cycle_sort",
        DownloadList,
        "shortcutActionCycleSort",
        &["s"],
        &["s"],
    ),
    // ── 全局 ──
    global("show_window", "shortcutActionShowWindow"),
    global("new_download", "shortcutActionNewDownload"),
    global("open_settings", "shortcutActionOpenSettings"),
    global("pause_all", "shortcutActionPauseAll"),
    global("resume_all", "shortcutActionResumeAll"),
];

/// 按作用域 + id 查动作。
pub fn shortcut_spec(scope: ShortcutScope, id: &str) -> Option<&'static ShortcutSpec> {
    SHORTCUT_SPECS
        .iter()
        .find(|spec| spec.scope == scope && spec.id == id)
}

/// 作用域对应的偏好键。
pub fn shortcuts_pref_key(scope: ShortcutScope) -> &'static str {
    match scope {
        ShortcutScope::App => APP_SHORTCUTS_PREF_KEY,
        ShortcutScope::Global => GLOBAL_SHORTCUTS_PREF_KEY,
    }
}

/// 一个动作的生效绑定。
#[derive(Clone, Debug)]
pub struct ResolvedShortcut {
    pub spec: &'static ShortcutSpec,
    pub triggers: Vec<ShortcutTrigger>,
    /// 用户是否覆盖过（`false` = 平台默认）。
    pub customized: bool,
}

/// 解析某作用域的生效绑定：当前平台可用的全部动作，按目录顺序。
pub fn resolve_shortcuts(scope: ShortcutScope, overrides: Option<&Value>) -> Vec<ResolvedShortcut> {
    let map = overrides.and_then(Value::as_object);
    SHORTCUT_SPECS
        .iter()
        .filter(|spec| spec.scope == scope && spec.available())
        .map(|spec| match map.and_then(|map| map.get(spec.id)) {
            Some(Value::Array(items)) => {
                let mut triggers: Vec<ShortcutTrigger> = Vec::with_capacity(items.len());
                for trigger in items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|text| text.parse::<ShortcutTrigger>().ok())
                    .filter(|trigger| trigger.check_scope(scope).is_ok())
                {
                    if !triggers.contains(&trigger) {
                        triggers.push(trigger);
                    }
                }
                ResolvedShortcut {
                    spec,
                    triggers,
                    customized: true,
                }
            }
            _ => ResolvedShortcut {
                spec,
                triggers: spec.defaults(),
                customized: false,
            },
        })
        .collect()
}

/// 把一个动作的新绑定写进覆盖表，返回新的偏好值；与平台默认相同则删除该条（保持偏好精简）。
pub fn with_shortcut_override(
    overrides: Option<&Value>,
    spec: &ShortcutSpec,
    triggers: &[ShortcutTrigger],
) -> Value {
    let mut map = overrides
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);
    if triggers == spec.defaults().as_slice() {
        map.remove(spec.id);
    } else {
        map.insert(
            spec.id.to_owned(),
            Value::Array(
                triggers
                    .iter()
                    .map(|trigger| Value::String(trigger.to_string()))
                    .collect(),
            ),
        );
    }
    Value::Object(map)
}

/// 同作用域内与 `trigger` 冲突的其它动作（应用内按生效区域重叠判定）。
pub fn shortcut_conflicts<'a>(
    resolved: &'a [ResolvedShortcut],
    spec: &ShortcutSpec,
    trigger: &ShortcutTrigger,
) -> impl Iterator<Item = &'a ResolvedShortcut> {
    let (scope, domain, id) = (spec.scope, spec.domain, spec.id);
    resolved.iter().filter(move |entry| {
        entry.spec.scope == scope
            && entry.spec.id != id
            && (scope == ShortcutScope::Global || entry.spec.domain.overlaps(domain))
            && entry.triggers.contains(trigger)
    })
}

/// agent 使用的全局热键后端。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum GlobalShortcutBackend {
    /// 不支持全局快捷键：headless 构建、无图形会话或平台初始化失败。
    #[default]
    Unavailable,
    Windows,
    MacOs,
    X11,
    /// Wayland：XDG GlobalShortcuts portal（只支持组合键；最终按键由桌面环境确认）。
    WaylandPortal,
}

/// 单个全局快捷键的注册结果。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum GlobalShortcutState {
    /// 已注册，正在监听。
    Active,
    /// 已被其它程序占用。
    InUse,
    /// 当前平台 / 会话不支持此触发器（如 Wayland 下双击修饰键）。
    Unsupported,
    /// 需要系统授权（macOS「辅助功能」）后才能生效。
    PermissionRequired,
    /// 注册失败（详见 `detail`）。
    Failed,
}

/// 单个全局快捷键的状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GlobalShortcutStatusDto {
    /// 动作 id（[`SHORTCUT_SPECS`] 中 `scope = Global` 的条目）。
    pub action: String,
    /// 规范触发器文本。
    pub trigger: String,
    pub state: GlobalShortcutState,
    /// 诊断文本（仅 `Failed`）。
    #[serde(default)]
    pub detail: Option<String>,
}

/// agent 全局快捷键整体状态（`ShellStatusDto.global_shortcuts`）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct GlobalShortcutsStatusDto {
    pub backend: GlobalShortcutBackend,
    pub entries: Vec<GlobalShortcutStatusDto>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn trigger(text: &str) -> ShortcutTrigger {
        text.parse().unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn trigger_text_round_trips_in_canonical_form() {
        for text in [
            "ctrl-shift-d",
            "ctrl-alt-shift-super-f12",
            "super-,",
            "ctrl--",
            "-",
            "space",
            "double:ctrl",
            "double:super",
        ] {
            assert_eq!(trigger(text).to_string(), text);
        }
    }

    #[test]
    fn non_canonical_or_unknown_triggers_are_rejected() {
        for text in [
            "shift-ctrl-d",
            "ctrl-ctrl-d",
            "cmd-d",
            "ctrl-",
            "ctrl--d",
            "ctrl-unknownkey",
            "double:fn",
            "",
        ] {
            assert!(text.parse::<ShortcutTrigger>().is_err(), "{text}");
        }
    }

    #[test]
    fn global_chords_need_a_command_modifier_except_function_keys() {
        assert!(
            trigger("shift-d")
                .check_scope(ShortcutScope::Global)
                .is_err()
        );
        assert!(trigger("g").check_scope(ShortcutScope::Global).is_err());
        assert!(trigger("f9").check_scope(ShortcutScope::Global).is_ok());
        assert!(trigger("alt-d").check_scope(ShortcutScope::Global).is_ok());
        assert!(
            trigger("double:shift")
                .check_scope(ShortcutScope::Global)
                .is_ok()
        );
        assert!(trigger("g").check_scope(ShortcutScope::App).is_ok());
    }

    #[test]
    fn overrides_replace_defaults_and_drop_invalid_entries() {
        let overrides = json!({
            "focus_search": [],
            "cycle_sort": ["shift-s", "bogus", "shift-s"],
            "unknown_action": ["ctrl-u"],
        });
        let resolved = resolve_shortcuts(ShortcutScope::App, Some(&overrides));
        let find = |id: &str| resolved.iter().find(|entry| entry.spec.id == id);
        assert_eq!(find("focus_search").map(|e| e.triggers.len()), Some(0));
        assert_eq!(
            find("cycle_sort").map(|e| e.triggers.clone()),
            Some(vec![trigger("shift-s")])
        );
        assert_eq!(
            find("cycle_group").map(|e| (e.customized, e.triggers.clone())),
            Some((false, vec![trigger("g")]))
        );

        let global = resolve_shortcuts(
            ShortcutScope::Global,
            Some(&json!({ "show_window": ["d", "double:ctrl"] })),
        );
        assert_eq!(
            global
                .iter()
                .find(|e| e.spec.id == "show_window")
                .map(|e| e.triggers.clone()),
            Some(vec![trigger("double:ctrl")])
        );
    }

    #[test]
    fn override_equal_to_defaults_is_removed() {
        let spec = shortcut_spec(ShortcutScope::App, "cycle_group").unwrap_or_else(|| panic!());
        let custom = with_shortcut_override(None, spec, &[trigger("shift-g")]);
        assert_eq!(custom, json!({ "cycle_group": ["shift-g"] }));
        let reset = with_shortcut_override(Some(&custom), spec, &spec.defaults());
        assert_eq!(reset, json!({}));
        let cleared = with_shortcut_override(None, spec, &[]);
        assert_eq!(cleared, json!({ "cycle_group": [] }));
    }

    #[test]
    fn app_conflicts_respect_domains() {
        let resolved = resolve_shortcuts(ShortcutScope::App, None);
        let palette =
            shortcut_spec(ShortcutScope::App, "command_palette").unwrap_or_else(|| panic!());
        // 全局区域的动作与下载列表里的同键冲突。
        let ids: Vec<&str> = shortcut_conflicts(&resolved, palette, &trigger("g"))
            .map(|entry| entry.spec.id)
            .collect();
        assert_eq!(ids, ["cycle_group"]);
        // 自身不算冲突。
        let group = shortcut_spec(ShortcutScope::App, "cycle_group").unwrap_or_else(|| panic!());
        assert_eq!(
            shortcut_conflicts(&resolved, group, &trigger("g")).count(),
            0
        );
    }

    #[test]
    fn every_default_parses_and_fits_its_scope() {
        for spec in SHORTCUT_SPECS {
            for raw in spec.defaults_mac.iter().chain(spec.defaults_other) {
                let parsed = trigger(raw);
                assert_eq!(parsed.to_string(), *raw, "{}", spec.id);
                assert!(parsed.check_scope(spec.scope).is_ok(), "{}", spec.id);
            }
        }
    }
}
