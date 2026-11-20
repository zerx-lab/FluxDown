//! 内置图标包、偏好投影与回退链。
//!
//! 选择 = 偏好 [`FILE_ICON_PACK_KEY`]（`builtin:<id>` / `custom:<id>`）。回退链从所选包沿
//! `extends` 走到底，末尾总接 [`FALLBACK_PACK`]（Lucide，九个大类全覆盖），因此任何文件都有图标。
//! `builtin:system` 是特殊环节：系统文件管理器图标（只有桌面能取），取不到时用它之后的链。

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

use fluxdown_protocol::FILE_ICON_PACK_KEY;
use gpui::{App, Global, SharedString};
use serde_json::Value;

use crate::{FileKind, IconPack, PackIcon, is_pack_ref, parse_icon_pack};

/// 系统图标的包 id（没有包文件，见模块文档）。
pub const SYSTEM_PACK_ID: &str = "system";
/// 桌面未设置偏好时的选择：保持系统图标。
pub const DEFAULT_DESKTOP_PACK: &str = "builtin:system";
/// 回退链终点。
pub const FALLBACK_PACK: &str = "builtin:lucide";
/// `extends` 最多跟随的层数（防环之外再限深度）。
const MAX_CHAIN: usize = 8;

/// 内置包（设置页按此顺序展示；`system` 只在桌面可用）。
pub const BUILTIN_PACK_IDS: [&str; 5] = [
    SYSTEM_PACK_ID,
    "lucide",
    "material",
    "catppuccin",
    "thunder",
];

const BUILTIN_SOURCES: [(&str, &str); 4] = [
    (
        "lucide",
        include_str!("../../../assets/icon-packs/lucide.json"),
    ),
    (
        "material",
        include_str!("../../../assets/icon-packs/material.json"),
    ),
    (
        "catppuccin",
        include_str!("../../../assets/icon-packs/catppuccin.json"),
    ),
    (
        "thunder",
        include_str!("../../../assets/icon-packs/thunder.json"),
    ),
];

/// 解析失败的内置包不会出现（`builtin_packs_parse_without_diagnostics` 守护）；万一损坏只是缺席。
static BUILTINS: LazyLock<HashMap<&'static str, Arc<IconPack>>> = LazyLock::new(|| {
    BUILTIN_SOURCES
        .iter()
        .filter_map(|(id, text)| {
            parse_icon_pack(text)
                .ok()
                .map(|parsed| (*id, Arc::new(parsed.pack)))
        })
        .collect()
});

/// 内置包（`system` 没有包文件，返回 `None`）。
#[must_use]
pub fn builtin_pack(id: &str) -> Option<Arc<IconPack>> {
    BUILTINS.get(id).cloned()
}

pub(crate) fn builtin_packs() -> impl Iterator<Item = &'static Arc<IconPack>> {
    BUILTINS.values()
}

/// 解析后的一个文件图标。
#[derive(Debug, Clone, PartialEq)]
pub enum FileIconChoice {
    /// 系统文件管理器图标；取不到（Web / 远程 / 失败）时用 `fallback`。
    System {
        fallback: PackIcon,
    },
    Pack(PackIcon),
}

#[derive(Debug, Clone)]
enum ChainLink {
    System,
    Pack(Arc<IconPack>),
}

/// 当前生效的图标包（GPUI 全局）。只由 [`apply_icon_pack_preference`] 修改。
#[derive(Debug, Clone)]
pub struct IconPackState {
    selection: SharedString,
    chain: Arc<[ChainLink]>,
}

impl Global for IconPackState {}

impl Default for IconPackState {
    fn default() -> Self {
        Self::new(DEFAULT_DESKTOP_PACK)
    }
}

impl IconPackState {
    /// 按选择建链；首个引用不可用（未知内置 / 未安装的自定义包）时按桌面默认选择。
    #[must_use]
    pub fn new(selection: &str) -> Self {
        let chain = build_chain(selection)
            .or_else(|| build_chain(DEFAULT_DESKTOP_PACK))
            .unwrap_or_default();
        Self {
            selection: SharedString::from(selection.to_owned()),
            chain: chain.into(),
        }
    }

    /// 偏好里的选择（原值，可能指向不可用的包）。
    #[must_use]
    pub fn selection(&self) -> &str {
        &self.selection
    }

    /// 文件名（已小写）+ 大类 → 图标：沿链取第一个具体匹配；链上都没有具体匹配时取第一个
    /// `default`。系统环节之后的匹配作为系统图标的回退。
    #[must_use]
    pub fn resolve(&self, lower_name: &str, kind: FileKind) -> Option<FileIconChoice> {
        let mut system = false;
        let mut matched = None;
        for link in self.chain.iter() {
            match link {
                ChainLink::System => system = true,
                ChainLink::Pack(pack) => {
                    if let Some((_, icon)) = pack.matched_icon(lower_name, kind) {
                        matched = Some(icon);
                        break;
                    }
                }
            }
        }
        let icon = matched
            .or_else(|| {
                self.chain.iter().find_map(|link| match link {
                    ChainLink::Pack(pack) => pack.default_icon().map(|(_, icon)| icon),
                    ChainLink::System => None,
                })
            })?
            .clone();
        Some(if system {
            FileIconChoice::System { fallback: icon }
        } else {
            FileIconChoice::Pack(icon)
        })
    }
}

/// 引用 → 链；首个引用不可用时 `None`。
fn build_chain(selection: &str) -> Option<Vec<ChainLink>> {
    let mut chain = Vec::new();
    let mut visited: Vec<String> = Vec::new();
    let mut next = Some(selection.to_owned());
    while let Some(reference) = next.take() {
        if visited.len() >= MAX_CHAIN || visited.contains(&reference) || !is_pack_ref(&reference) {
            break;
        }
        visited.push(reference.clone());
        let link = match reference.split_once(':') {
            Some(("builtin", SYSTEM_PACK_ID)) => Some(ChainLink::System),
            Some(("builtin", id)) => builtin_pack(id).map(ChainLink::Pack),
            // 用户图标包库尚未落地：自定义引用一律不可用。
            _ => None,
        };
        let Some(link) = link else {
            break;
        };
        if let ChainLink::Pack(pack) = &link {
            next.clone_from(&pack.extends);
        }
        chain.push(link);
    }
    if chain.is_empty() {
        return None;
    }
    if !visited.iter().any(|reference| reference == FALLBACK_PACK)
        && let Some(pack) = builtin_pack(FALLBACK_PACK.trim_start_matches("builtin:"))
    {
        chain.push(ChainLink::Pack(pack));
    }
    Some(chain)
}

/// 偏好快照 → 当前图标包；与当前选择一致时不做任何事（可在每次快照 / 偏好事件上幂等调用）。
/// 变化时整窗刷新：retained 渲染下行视图不会因为全局变化自动重绘。
pub fn apply_icon_pack_preference(values: &BTreeMap<String, Value>, cx: &mut App) {
    let selection = values
        .get(FILE_ICON_PACK_KEY)
        .and_then(Value::as_str)
        .filter(|value| is_pack_ref(value))
        .unwrap_or(DEFAULT_DESKTOP_PACK);
    if cx
        .try_global::<IconPackState>()
        .is_some_and(|state| state.selection() == selection)
    {
        return;
    }
    cx.set_global(IconPackState::new(selection));
    cx.refresh_windows();
}

/// 当前图标包（未安装全局时为桌面默认）。
#[must_use]
pub fn active_icon_packs(cx: &App) -> IconPackState {
    cx.try_global::<IconPackState>()
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{BUILTIN_SOURCES, FileIconChoice, IconPackState};
    use crate::{FileKind, parse_icon_pack};

    #[test]
    fn builtin_packs_parse_without_diagnostics() {
        for (id, text) in BUILTIN_SOURCES {
            let parsed = parse_icon_pack(text).expect(id);
            assert!(
                parsed.diagnostics.is_empty(),
                "{id}: {:?}",
                parsed.diagnostics
            );
            assert_eq!(parsed.pack.meta.id, id);
            for kind in FileKind::ALL {
                assert!(
                    parsed.pack.matched_icon("", kind).is_some(),
                    "{id} lacks {kind:?}"
                );
            }
        }
    }

    fn pack_svg(state: &IconPackState, name: &str) -> String {
        match state.resolve(name, FileKind::of_name(name)) {
            Some(FileIconChoice::Pack(icon)) => icon.svg.text.to_string(),
            other => panic!("{name}: {other:?}"),
        }
    }

    #[test]
    fn material_distinguishes_specific_extensions_within_one_kind() {
        let state = IconPackState::new("builtin:material");
        assert_ne!(
            pack_svg(&state, "report.pdf"),
            pack_svg(&state, "notes.docx")
        );
        assert_eq!(pack_svg(&state, "a.MP4"), pack_svg(&state, "b.mkv"));
    }

    #[test]
    fn thunder_embeds_png_and_maps_download_types() {
        let state = IconPackState::new("builtin:thunder");
        let torrent = pack_svg(&state, "ubuntu.torrent");
        assert!(torrent.contains("data:image/png;base64,"));
        assert_ne!(torrent, pack_svg(&state, "a.mp4"));
        assert_eq!(pack_svg(&state, "a.MKV"), pack_svg(&state, "b.mp4"));
        assert_eq!(pack_svg(&state, "README"), pack_svg(&state, "x.unknownext"));
    }

    #[test]
    fn system_selection_carries_pack_fallback() {
        let state = IconPackState::new("builtin:system");
        assert!(matches!(
            state.resolve("a.mp4", FileKind::Video),
            Some(FileIconChoice::System { .. })
        ));
    }

    #[test]
    fn unavailable_selection_falls_back_to_desktop_default() {
        let state = IconPackState::new("custom:missing");
        assert_eq!(state.selection(), "custom:missing");
        assert!(matches!(
            state.resolve("a.mp4", FileKind::Video),
            Some(FileIconChoice::System { .. })
        ));
    }
}
