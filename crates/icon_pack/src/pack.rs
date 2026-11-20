//! 图标包文件（wire）：`fluxdown.icon-pack` v1。
//!
//! 与主题文件同一套容错约定：顶层不是对象 / `format` 不符才整体拒绝；其余单字段非法只丢弃
//! 该字段并记 [`Diagnostic`]，绝不因一个坏图标让整包失效。内置包与用户包同一格式。
//! TS 镜像：`web/src/lib/icon-pack/pack.ts`（共用 `tests/fixtures/cases.json` 用例）。

use std::{
    collections::HashMap,
    fmt,
    hash::{DefaultHasher, Hash as _, Hasher as _},
    sync::Arc,
};

use gpui::SharedString;

use serde_json::{Map, Value};

use crate::{ASSET_PREFIX, FileKind};

/// 图标包文件 `format` 字段取值。
pub const ICON_PACK_FORMAT: &str = "fluxdown.icon-pack";
/// 当前 schema 版本。
pub const ICON_PACK_SCHEMA_VERSION: u64 = 1;
/// 单个 SVG 的字节上限。
pub const MAX_SVG_BYTES: usize = 64 * 1024;
/// 单包图标数上限。
pub const MAX_ICONS: usize = 1024;
/// 图标名 / 包 id 的字节上限。
const MAX_NAME_LEN: usize = 64;

const TOP_LEVEL_KEYS: [&str; 9] = [
    "format",
    "schemaVersion",
    "meta",
    "extends",
    "icons",
    "fileNames",
    "fileExtensions",
    "kinds",
    "default",
];
const ICON_KEYS: [&str; 4] = ["mode", "svg", "light", "dark"];

/// 诊断类别（wire 名与 TS 镜像一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    /// 类型 / 取值非法，或映射指向未定义的图标：该字段被丢弃。
    InvalidValue,
    /// 未知键：忽略。
    UnknownKey,
    /// schema 版本高于本客户端：尽力加载。
    NewerVersion,
    /// SVG 含脚本、外部引用或事件属性，或超过大小上限：该图标（变体）被丢弃。
    UnsafeSvg,
}

impl DiagnosticKind {
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::InvalidValue => "invalidValue",
            Self::UnknownKey => "unknownKey",
            Self::NewerVersion => "newerVersion",
            Self::UnsafeSvg => "unsafeSvg",
        }
    }
}

/// 加载过程中的非致命问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 文件内路径（如 `icons.video.svg`、`fileExtensions.mp4`）。
    pub path: String,
    pub kind: DiagnosticKind,
    pub message: String,
}

impl Diagnostic {
    fn new(path: impl Into<String>, kind: DiagnosticKind, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind,
            message: message.into(),
        }
    }
}

/// 整体拒绝加载的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconPackParseError {
    InvalidJson(String),
    NotAnObject,
    /// `format` 缺失或不是 [`ICON_PACK_FORMAT`]。
    UnsupportedFormat(String),
}

impl fmt::Display for IconPackParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(f, "icon pack is not valid JSON: {error}"),
            Self::NotAnObject => f.write_str("icon pack must be a JSON object"),
            Self::UnsupportedFormat(format) => write!(f, "unsupported icon pack format: {format}"),
        }
    }
}

impl std::error::Error for IconPackParseError {}

/// 图标绘制方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconMode {
    /// 只取 alpha，按界面文字色着色（随主题变色）。
    Mask,
    /// 原样绘制 SVG 自带颜色。
    Color,
}

/// 一段已校验的 SVG。`path` 是按内容哈希命名的资源路径（[`crate::IconPackAssets`] 提供字节），
/// GPUI 的 SVG 栅格缓存按它去重；同一进程内稳定。
#[derive(Debug, Clone)]
pub struct IconSvg {
    pub text: Arc<str>,
    pub path: SharedString,
}

impl IconSvg {
    fn new(text: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        Self {
            text: Arc::from(text),
            path: SharedString::from(format!("{ASSET_PREFIX}{:016x}.svg", hasher.finish())),
        }
    }
}

impl PartialEq for IconSvg {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.text == other.text
    }
}

/// 一个图标：基础 SVG 与可选的亮 / 暗背景变体（只为适配明暗，不读主题色值）。
#[derive(Debug, Clone, PartialEq)]
pub struct PackIcon {
    pub mode: IconMode,
    pub svg: IconSvg,
    pub light: Option<IconSvg>,
    pub dark: Option<IconSvg>,
}

impl PackIcon {
    /// 当前明暗模式下使用的 SVG。
    #[must_use]
    pub fn variant(&self, dark: bool) -> &IconSvg {
        let preferred = if dark { &self.dark } else { &self.light };
        preferred.as_ref().unwrap_or(&self.svg)
    }

    /// 全部 SVG（基础 + 变体）。
    pub fn svgs(&self) -> impl Iterator<Item = &IconSvg> {
        std::iter::once(&self.svg)
            .chain(self.light.iter())
            .chain(self.dark.iter())
    }
}

/// `meta` 段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackMeta {
    pub id: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub version: String,
    pub homepage: String,
}

/// 已解析的图标包。所有映射都只指向 `icons` 中存在的图标。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IconPack {
    pub meta: PackMeta,
    /// 本包查不到时继续查找的包（`builtin:<id>` / `custom:<id>`）。
    pub extends: Option<String>,
    icons: HashMap<String, PackIcon>,
    file_names: HashMap<String, String>,
    file_extensions: HashMap<String, String>,
    kinds: HashMap<FileKind, String>,
    default: Option<String>,
}

impl IconPack {
    #[must_use]
    pub fn icon(&self, name: &str) -> Option<&PackIcon> {
        self.icons.get(name)
    }

    pub fn icons(&self) -> impl Iterator<Item = (&str, &PackIcon)> {
        self.icons.iter().map(|(name, icon)| (name.as_str(), icon))
    }

    /// 本包内的具体匹配：精确文件名 → 扩展名（复合扩展名优先：`tar.gz` 先于 `gz`）→ 文件大类。
    /// `lower_name` 须已转小写（可含目录，只取最后一段）；空串只按大类匹配。`default` 不在此列，
    /// 见 [`IconPack::default_icon`]（整条回退链都没有具体匹配时才用）。
    #[must_use]
    pub fn matched_icon<'a>(
        &'a self,
        lower_name: &str,
        kind: FileKind,
    ) -> Option<(&'a str, &'a PackIcon)> {
        let base = lower_name.rsplit(['/', '\\']).next().unwrap_or(lower_name);
        let mut name = None;
        if !base.is_empty() {
            name = self.file_names.get(base).or_else(|| {
                base.match_indices('.')
                    .find_map(|(index, _)| self.file_extensions.get(&base[index + 1..]))
            });
        }
        self.named(name.or_else(|| self.kinds.get(&kind)))
    }

    /// `default` 图标。
    #[must_use]
    pub fn default_icon(&self) -> Option<(&str, &PackIcon)> {
        self.named(self.default.as_ref())
    }

    fn named<'a>(&'a self, name: Option<&'a String>) -> Option<(&'a str, &'a PackIcon)> {
        let name = name?;
        self.icons.get(name).map(|icon| (name.as_str(), icon))
    }
}

/// 解析结果：图标包 + 诊断。
#[derive(Debug, Clone)]
pub struct ParsedPack {
    pub pack: IconPack,
    pub diagnostics: Vec<Diagnostic>,
}

pub use fluxdown_protocol::is_icon_pack_ref as is_pack_ref;

/// 解析图标包文件。
pub fn parse_icon_pack(text: &str) -> Result<ParsedPack, IconPackParseError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| IconPackParseError::InvalidJson(error.to_string()))?;
    let Value::Object(root) = value else {
        return Err(IconPackParseError::NotAnObject);
    };
    match root.get("format") {
        Some(Value::String(format)) if format == ICON_PACK_FORMAT => {}
        Some(Value::String(format)) => {
            return Err(IconPackParseError::UnsupportedFormat(format.clone()));
        }
        _ => return Err(IconPackParseError::UnsupportedFormat(String::new())),
    }

    let mut diagnostics = Vec::new();
    for key in root.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            diagnostics.push(Diagnostic::new(
                key.as_str(),
                DiagnosticKind::UnknownKey,
                "unknown key",
            ));
        }
    }
    match root.get("schemaVersion") {
        Some(Value::Number(number)) => match number.as_u64() {
            Some(version) if version > ICON_PACK_SCHEMA_VERSION => {
                diagnostics.push(Diagnostic::new(
                    "schemaVersion",
                    DiagnosticKind::NewerVersion,
                    format!("schema version {version} is newer than {ICON_PACK_SCHEMA_VERSION}"),
                ))
            }
            Some(version) if version >= 1 => {}
            _ => diagnostics.push(invalid("schemaVersion", "must be a positive integer")),
        },
        None => {}
        Some(_) => diagnostics.push(invalid("schemaVersion", "must be a positive integer")),
    }

    let mut pack = IconPack {
        meta: parse_meta(root.get("meta"), &mut diagnostics),
        ..IconPack::default()
    };
    match root.get("extends") {
        None => {}
        Some(Value::String(target)) if is_pack_ref(target) => pack.extends = Some(target.clone()),
        Some(_) => diagnostics.push(invalid("extends", "must be builtin:<id> or custom:<id>")),
    }
    pack.icons = parse_icons(root.get("icons"), &mut diagnostics);
    pack.file_names = parse_mapping(
        "fileNames",
        root.get("fileNames"),
        &pack.icons,
        &mut diagnostics,
    );
    pack.file_extensions = parse_mapping(
        "fileExtensions",
        root.get("fileExtensions"),
        &pack.icons,
        &mut diagnostics,
    );
    pack.kinds = parse_kinds(root.get("kinds"), &pack.icons, &mut diagnostics);
    pack.default = match root.get("default") {
        None => None,
        Some(Value::String(name)) if pack.icons.contains_key(name) => Some(name.clone()),
        Some(Value::String(_)) => {
            diagnostics.push(invalid("default", "refers to an undefined icon"));
            None
        }
        Some(_) => {
            diagnostics.push(invalid("default", "must be a string"));
            None
        }
    };
    Ok(ParsedPack { pack, diagnostics })
}

fn invalid(path: impl Into<String>, message: &str) -> Diagnostic {
    Diagnostic::new(path, DiagnosticKind::InvalidValue, message)
}

fn object<'a>(
    path: &str,
    value: Option<&'a Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'a Map<String, Value>> {
    match value {
        None => None,
        Some(Value::Object(map)) => Some(map),
        Some(_) => {
            diagnostics.push(invalid(path, "must be an object"));
            None
        }
    }
}

fn parse_meta(value: Option<&Value>, diagnostics: &mut Vec<Diagnostic>) -> PackMeta {
    let mut meta = PackMeta::default();
    let Some(map) = object("meta", value, diagnostics) else {
        return meta;
    };
    for (key, value) in map {
        let path = format!("meta.{key}");
        let slot = match key.as_str() {
            "id" => &mut meta.id,
            "name" => &mut meta.name,
            "author" => &mut meta.author,
            "license" => &mut meta.license,
            "version" => &mut meta.version,
            "homepage" => &mut meta.homepage,
            _ => {
                diagnostics.push(Diagnostic::new(
                    path,
                    DiagnosticKind::UnknownKey,
                    "unknown key",
                ));
                continue;
            }
        };
        match value {
            Value::String(text) => slot.clone_from(text),
            _ => diagnostics.push(invalid(path, "must be a string")),
        }
    }
    meta
}

fn valid_icon_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_LEN
}

fn parse_icons(
    value: Option<&Value>,
    diagnostics: &mut Vec<Diagnostic>,
) -> HashMap<String, PackIcon> {
    let mut icons = HashMap::new();
    let Some(map) = object("icons", value, diagnostics) else {
        return icons;
    };
    for (name, value) in map {
        let path = format!("icons.{name}");
        if !valid_icon_name(name) {
            diagnostics.push(invalid(path, "icon name must be 1-64 bytes"));
            continue;
        }
        if icons.len() >= MAX_ICONS {
            diagnostics.push(invalid(path, "too many icons"));
            continue;
        }
        let Value::Object(entry) = value else {
            diagnostics.push(invalid(path, "must be an object"));
            continue;
        };
        for key in entry.keys() {
            if !ICON_KEYS.contains(&key.as_str()) {
                diagnostics.push(Diagnostic::new(
                    format!("{path}.{key}"),
                    DiagnosticKind::UnknownKey,
                    "unknown key",
                ));
            }
        }
        let mode = match entry.get("mode") {
            None => IconMode::Color,
            Some(Value::String(mode)) if mode == "color" => IconMode::Color,
            Some(Value::String(mode)) if mode == "mask" => IconMode::Mask,
            Some(_) => {
                diagnostics.push(invalid(
                    format!("{path}.mode"),
                    "must be \"mask\" or \"color\"",
                ));
                IconMode::Color
            }
        };
        let Some(svg) = parse_svg(&format!("{path}.svg"), entry.get("svg"), true, diagnostics)
        else {
            continue;
        };
        let light = parse_svg(
            &format!("{path}.light"),
            entry.get("light"),
            false,
            diagnostics,
        );
        let dark = parse_svg(
            &format!("{path}.dark"),
            entry.get("dark"),
            false,
            diagnostics,
        );
        icons.insert(
            name.clone(),
            PackIcon {
                mode,
                svg,
                light,
                dark,
            },
        );
    }
    icons
}

fn parse_svg(
    path: &str,
    value: Option<&Value>,
    required: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IconSvg> {
    match value {
        None if required => {
            diagnostics.push(invalid(path, "is required"));
            None
        }
        None => None,
        Some(Value::String(svg)) if svg.len() > MAX_SVG_BYTES => {
            diagnostics.push(Diagnostic::new(
                path,
                DiagnosticKind::UnsafeSvg,
                "svg is too large",
            ));
            None
        }
        Some(Value::String(svg)) if !svg_is_safe(svg) => {
            diagnostics.push(Diagnostic::new(
                path,
                DiagnosticKind::UnsafeSvg,
                "svg contains scripts, external references or event handlers",
            ));
            None
        }
        Some(Value::String(svg)) => Some(IconSvg::new(svg)),
        Some(_) => {
            diagnostics.push(invalid(path, "must be a string"));
            None
        }
    }
}

fn parse_mapping(
    section: &str,
    value: Option<&Value>,
    icons: &HashMap<String, PackIcon>,
    diagnostics: &mut Vec<Diagnostic>,
) -> HashMap<String, String> {
    let mut mapping = HashMap::new();
    let Some(map) = object(section, value, diagnostics) else {
        return mapping;
    };
    for (key, value) in map {
        let path = format!("{section}.{key}");
        match value {
            Value::String(_) if key.is_empty() => {
                diagnostics.push(invalid(path, "key must not be empty"));
            }
            Value::String(name) if icons.contains_key(name) => {
                mapping.insert(key.to_lowercase(), name.clone());
            }
            Value::String(_) => diagnostics.push(invalid(path, "refers to an undefined icon")),
            _ => diagnostics.push(invalid(path, "must be a string")),
        }
    }
    mapping
}

fn parse_kinds(
    value: Option<&Value>,
    icons: &HashMap<String, PackIcon>,
    diagnostics: &mut Vec<Diagnostic>,
) -> HashMap<FileKind, String> {
    let mut kinds = HashMap::new();
    let Some(map) = object("kinds", value, diagnostics) else {
        return kinds;
    };
    for (key, value) in map {
        let path = format!("kinds.{key}");
        let Some(kind) = FileKind::from_wire(key) else {
            diagnostics.push(Diagnostic::new(
                path,
                DiagnosticKind::UnknownKey,
                "unknown file kind",
            ));
            continue;
        };
        match value {
            Value::String(name) if icons.contains_key(name) => {
                kinds.insert(kind, name.clone());
            }
            Value::String(_) => diagnostics.push(invalid(path, "refers to an undefined icon")),
            _ => diagnostics.push(invalid(path, "must be a string")),
        }
    }
    kinds
}

/// 拒收会执行脚本、读外部资源或展开实体的 SVG。只做保守的字节扫描（宁可误拒）；TS 镜像
/// `svgIsSafe` 逐条同一规则。位图只允许内嵌 PNG（`data:image/png;base64,`），不读外部资源。
#[must_use]
pub fn svg_is_safe(svg: &str) -> bool {
    const FORBIDDEN: [&str; 9] = [
        "<script",
        "<foreignobject",
        "<iframe",
        "<object",
        "<embed",
        "<!entity",
        "<!doctype",
        "javascript:",
        "@import",
    ];
    const INLINE_PNG: &[u8] = b"data:image/png;base64,";
    let lower = svg.to_ascii_lowercase();
    let start = lower.trim_start_matches(|char: char| char.is_ascii_whitespace());
    if !(start.starts_with("<svg") || start.starts_with("<?xml")) || !lower.contains("<svg") {
        return false;
    }
    if FORBIDDEN.iter().any(|needle| lower.contains(needle)) {
        return false;
    }
    let bytes = lower.as_bytes();
    let skip_space = |mut index: usize| {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        index
    };
    // href / xlink:href 只允许文档内片段引用（`#id`）或内嵌 PNG。
    for (index, _) in lower.match_indices("href") {
        let mut cursor = skip_space(index + 4);
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor = skip_space(cursor + 1);
        if matches!(bytes.get(cursor), Some(b'"' | b'\'')) {
            cursor += 1;
        }
        let inline_png = bytes
            .get(cursor..)
            .is_some_and(|rest| rest.starts_with(INLINE_PNG));
        if bytes.get(cursor) != Some(&b'#') && !inline_png {
            return false;
        }
    }
    // url(...) 同理（渐变 / 裁剪路径引用）。
    for (index, _) in lower.match_indices("url(") {
        let mut cursor = index + 4;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\''))
        {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'#') {
            return false;
        }
    }
    // 事件属性：空白后的 `on<字母>+` 紧跟 `=`。
    for (index, byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_whitespace() || !lower[index + 1..].starts_with("on") {
            continue;
        }
        let mut cursor = index + 3;
        let letters_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_lowercase) {
            cursor += 1;
        }
        if cursor > letters_start && bytes.get(skip_space(cursor)) == Some(&b'=') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::svg_is_safe;

    #[test]
    fn svg_safety_rejects_active_content_and_external_references() {
        assert!(svg_is_safe(
            r##"<svg viewBox="0 0 16 16"><use href="#a"/><path fill="url(#g)"/></svg>"##
        ));
        assert!(svg_is_safe(
            "<?xml version=\"1.0\"?><svg><path d=\"M0 0\"/></svg>"
        ));
        assert!(svg_is_safe(
            "<svg viewBox=\"0 0 8 8\"><image width=\"8\" height=\"8\" href=\"data:image/png;base64,iVBORw0KGgo=\"/></svg>"
        ));
        for unsafe_svg in [
            "<svg><script>alert(1)</script></svg>",
            "<svg><image href=\"file:///etc/passwd\"/></svg>",
            "<svg><image href=\"data:image/svg+xml;base64,PHN2Zz4=\"/></svg>",
            "<svg><image href=\"https://x/a.png\"/></svg>",
            "<svg><use xlink:href=\"other.svg#a\"/></svg>",
            "<svg><use href = 'https://x/a.svg#a'/></svg>",
            "<svg><path fill=\"url(https://x/p)\"/></svg>",
            "<svg onload=\"x()\"></svg>",
            "<svg><a\nonclick =\"x()\"/></svg>",
            "<!DOCTYPE svg [<!ENTITY a \"b\">]><svg/>",
            "<html><svg/></html>",
            "<svg><style>@import 'x.css';</style></svg>",
        ] {
            assert!(!svg_is_safe(unsafe_svg), "{unsafe_svg}");
        }
    }
}
