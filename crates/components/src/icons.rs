//! FluxDown 自带图标集。
//!
//! gpui-component 只内置约百个图标，文件类型 / 任务状态语义不全（视频只能借
//! `Play`、音频文档图片共用 `File`）。这里内嵌 Lucide 1.48.0 的子集（ISC，见
//! `assets/icons/LICENSE`），线宽统一 1.75，经 [`FluxIcon`] 与 gpui-component 的
//! `Icon` / `Button::icon` / `PopupMenuItem::icon` 无缝互用。资源由
//! [`ComponentAssets`] 提供，composition root 负责并入应用 `AssetSource`。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::IconNamed;

macro_rules! flux_icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// FluxDown 自带图标。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum FluxIcon {
            $($variant,)*
        }

        impl FluxIcon {
            /// 全部图标（资源枚举与测试用）。
            pub const ALL: &'static [FluxIcon] = &[$(FluxIcon::$variant,)*];

            /// 嵌入资源路径。
            pub const fn asset_path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("fluxdown/ui-icons/", $file, ".svg"),)*
                }
            }

            const fn bytes(self) -> &'static [u8] {
                match self {
                    $(Self::$variant => include_bytes!(concat!("../assets/icons/", $file, ".svg")),)*
                }
            }
        }
    };
}

flux_icons! {
    AppWindow => "app-window",
    Archive => "archive",
    ArrowDown => "arrow-down",
    ArrowUp => "arrow-up",
    ArrowUpDown => "arrow-up-down",
    Bell => "bell",
    Bookmark => "bookmark",
    Box => "box",
    Check => "check",
    CheckboxCheck => "checkbox-check",
    CheckboxMinus => "checkbox-minus",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    CircleAlert => "circle-alert",
    CircleArrowDown => "circle-arrow-down",
    CircleCheck => "circle-check",
    CirclePause => "circle-pause",
    CircleUser => "circle-user",
    Clock => "clock",
    CloudUpload => "cloud-upload",
    Code => "code",
    Columns3 => "columns-3",
    Crown => "crown",
    Copy => "copy",
    Cpu => "cpu",
    Database => "database",
    Diamond => "diamond",
    Disc => "disc",
    Disc3 => "disc-3",
    Download => "download",
    Ellipsis => "ellipsis",
    ExternalLink => "external-link",
    File => "file",
    FileArchive => "file-archive",
    FileImage => "file-image",
    FileMusic => "file-music",
    FilePlay => "file-play",
    FileText => "file-text",
    Film => "film",
    Flag => "flag",
    Folder => "folder",
    FolderOpen => "folder-open",
    Folders => "folders",
    Gamepad2 => "gamepad-2",
    Gauge => "gauge",
    Gem => "gem",
    Gift => "gift",
    Globe => "globe",
    GripVertical => "grip-vertical",
    Group => "group",
    HardDrive => "hard-drive",
    Image => "image",
    Info => "info",
    Keyboard => "keyboard",
    Layers => "layers",
    Library => "library",
    Link => "link",
    ListFilter => "list-filter",
    QrCode => "qr-code",
    Magnet => "magnet",
    Moon => "moon",
    Music => "music",
    Network => "network",
    Package => "package",
    Palette => "palette",
    PanelBottom => "panel-bottom",
    PanelRight => "panel-right",
    Pause => "pause",
    Pen => "pen",
    Play => "play",
    Plus => "plus",
    Power => "power",
    Printer => "printer",
    RotateCw => "rotate-cw",
    Rows3 => "rows-3",
    Rss => "rss",
    Rocket => "rocket",
    Search => "search",
    Shield => "shield",
    Settings => "settings",
    SlidersHorizontal => "sliders-horizontal",
    Smartphone => "smartphone",
    Subtitles => "subtitles",
    Sparkles => "sparkles",
    Star => "star",
    SquareTerminal => "square-terminal",
    Sun => "sun",
    TriangleAlert => "triangle-alert",
    Trash2 => "trash-2",
    Type => "type",
    User => "user",
    Webhook => "webhook",
    X => "x",
    Zap => "zap",
}

impl IconNamed for FluxIcon {
    fn path(self) -> SharedString {
        SharedString::new_static(self.asset_path())
    }
}

/// 自定义分类的 wire 图标名 → 图标。侧栏分类子项与设置里的分类编辑器共用这一张
/// 表，保证两处显示一致；未知名字回退通用文件图标。
pub fn category_icon(name: &str) -> FluxIcon {
    match name {
        "folders" => FluxIcon::Folders,
        "film" => FluxIcon::Film,
        "music" => FluxIcon::Music,
        "fileText" => FluxIcon::FileText,
        "image" => FluxIcon::Image,
        "archive" => FluxIcon::Archive,
        "code" => FluxIcon::Code,
        "database" => FluxIcon::Database,
        "hardDrive" => FluxIcon::HardDrive,
        "gamepad" => FluxIcon::Gamepad2,
        "globe" => FluxIcon::Globe,
        "bookmark" => FluxIcon::Bookmark,
        "box" => FluxIcon::Box,
        "package2" => FluxIcon::Package,
        "cpu" => FluxIcon::Cpu,
        "disc" => FluxIcon::Disc,
        "smartphone" => FluxIcon::Smartphone,
        "font" | "type" => FluxIcon::Type,
        "library" => FluxIcon::Library,
        "pen" => FluxIcon::Pen,
        "printer" => FluxIcon::Printer,
        "subtitles" => FluxIcon::Subtitles,
        "zap" => FluxIcon::Zap,
        _ => FluxIcon::File,
    }
}

/// 组件 crate 拥有的嵌入资源（[`FluxIcon`] 全集）。
pub struct ComponentAssets;

impl AssetSource for ComponentAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(FluxIcon::ALL
            .iter()
            .find(|icon| icon.asset_path() == path)
            .map(|icon| Cow::Borrowed(icon.bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FluxIcon::ALL
            .iter()
            .map(|icon| icon.asset_path())
            .filter(|asset| asset.starts_with(path))
            .map(SharedString::new_static)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui::AssetSource as _;

    use super::{ComponentAssets, FluxIcon};

    #[test]
    fn every_icon_path_is_unique_and_loads_svg() -> gpui::Result<()> {
        let mut seen = HashSet::new();
        for icon in FluxIcon::ALL {
            assert!(seen.insert(icon.asset_path()), "duplicate {icon:?}");
            let bytes = ComponentAssets.load(icon.asset_path())?;
            assert!(
                bytes.is_some_and(|bytes| bytes.starts_with(b"<svg")),
                "{icon:?} missing or not an svg"
            );
        }
        assert!(ComponentAssets.load("icons/file.svg")?.is_none());
        Ok(())
    }
}
