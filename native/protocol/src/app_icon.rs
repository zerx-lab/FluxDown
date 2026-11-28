//! 桌面应用图标（窗口 / 任务栏 / Dock / 托盘 / 快捷方式）：默认、内置备选图标或用户导入的自定义图片。
//!
//! 选择存为设备本地偏好 [`APP_ICON_PREF_KEY`]（不在同步目录里：自定义图片只存在本机）。偏好只由
//! agent 在 `agent.platform.setAppIcon` 成功后写入，保证值引用的图片文件已经落盘；GPUI 与 agent
//! 都只按 [`AppIconChoice::from_preference`] 解析，缺省 / 非法值一律回落默认图标。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 应用图标选择（设备本地偏好），值见 [`AppIconChoice::to_preference`]。
pub const APP_ICON_PREF_KEY: &str = "desktop.app_icon";
/// 自定义图标偏好值前缀：`custom:<revision>`。
const CUSTOM_PREFIX: &str = "custom:";
/// 自定义图标修订号的最大长度（agent 写入 12 位十六进制内容哈希）。
const MAX_REVISION_LEN: usize = 64;
/// `agent.platform.appIconImage` 的边长范围（物理像素）。
pub const APP_ICON_IMAGE_MIN_SIZE: u32 = 16;
/// `agent.platform.appIconImage` 的边长上限，也是自定义图标母版的边长。
pub const APP_ICON_IMAGE_MAX_SIZE: u32 = 1024;

/// 应用图标种类（`agent.platform.setAppIcon` 参数）。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub enum AppIconKind {
    /// 安装包自带图标。
    #[default]
    Default,
    /// 内置备选图标「闪电」。
    Bolt,
    /// 内置备选图标「经典」。
    Classic,
    /// 内置备选图标「霜蓝」。
    Frost,
    /// 内置备选图标「日落」。
    Sunset,
    /// 用户导入的图片。
    Custom,
}

/// 随应用分发的备选图标；偏好值 = [`BuiltinAppIcon::key`]。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BuiltinAppIcon {
    /// 「闪电」。
    Bolt,
    /// 「经典」：品牌蓝渐变。
    Classic,
    /// 「霜蓝」：钴蓝底 + 磨砂箭头。
    Frost,
    /// 「日落」：橙粉紫渐变。
    Sunset,
}

impl BuiltinAppIcon {
    /// 全部内置图标，按设置页展示顺序。
    pub const ALL: [Self; 4] = [Self::Bolt, Self::Classic, Self::Frost, Self::Sunset];

    /// 偏好值与资源文件名使用的稳定键。
    ///
    /// ```
    /// use fluxdown_protocol::app_icon::BuiltinAppIcon;
    ///
    /// assert_eq!(BuiltinAppIcon::Frost.key(), "frost");
    /// ```
    pub const fn key(self) -> &'static str {
        match self {
            Self::Bolt => "bolt",
            Self::Classic => "classic",
            Self::Frost => "frost",
            Self::Sunset => "sunset",
        }
    }

    /// 对应的 `setAppIcon` 种类。
    pub const fn kind(self) -> AppIconKind {
        match self {
            Self::Bolt => AppIconKind::Bolt,
            Self::Classic => AppIconKind::Classic,
            Self::Frost => AppIconKind::Frost,
            Self::Sunset => AppIconKind::Sunset,
        }
    }
}

/// 解析后的应用图标选择。
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum AppIconChoice {
    /// 安装包自带图标。
    #[default]
    Default,
    /// 内置备选图标。
    Builtin(BuiltinAppIcon),
    /// 用户导入的图片；`revision` 是图片内容的哈希，导入新图片即变化（也作缓存键）。
    Custom {
        /// 图片内容修订号（ASCII 字母数字）。
        revision: String,
    },
}

impl AppIconChoice {
    /// 从偏好值解析；缺省、非字符串或无法识别的值回落 [`AppIconChoice::Default`]。
    ///
    /// ```
    /// use fluxdown_protocol::app_icon::{AppIconChoice, BuiltinAppIcon};
    /// use serde_json::json;
    ///
    /// assert_eq!(
    ///     AppIconChoice::from_preference(Some(&json!("bolt"))),
    ///     AppIconChoice::Builtin(BuiltinAppIcon::Bolt)
    /// );
    /// assert_eq!(
    ///     AppIconChoice::from_preference(Some(&json!("custom:0a1b2c"))),
    ///     AppIconChoice::Custom { revision: "0a1b2c".into() }
    /// );
    /// assert_eq!(AppIconChoice::from_preference(Some(&json!("custom:"))), AppIconChoice::Default);
    /// assert_eq!(AppIconChoice::from_preference(None), AppIconChoice::Default);
    /// ```
    pub fn from_preference(value: Option<&Value>) -> Self {
        let Some(text) = value.and_then(Value::as_str) else {
            return Self::Default;
        };
        if let Some(icon) = BuiltinAppIcon::ALL
            .into_iter()
            .find(|icon| icon.key() == text)
        {
            return Self::Builtin(icon);
        }
        match text.strip_prefix(CUSTOM_PREFIX) {
            Some(revision) if is_valid_revision(revision) => Self::Custom {
                revision: revision.to_owned(),
            },
            _ => Self::Default,
        }
    }

    /// 写回偏好的规范值：`default` / 内置键（如 `bolt`）/ `custom:<revision>`。
    ///
    /// ```
    /// use fluxdown_protocol::app_icon::{AppIconChoice, BuiltinAppIcon};
    ///
    /// let custom = AppIconChoice::Custom { revision: "ff00".into() };
    /// assert_eq!(custom.to_preference(), "custom:ff00");
    /// assert_eq!(AppIconChoice::Builtin(BuiltinAppIcon::Sunset).to_preference(), "sunset");
    /// ```
    pub fn to_preference(&self) -> String {
        match self {
            Self::Default => "default".to_owned(),
            Self::Builtin(icon) => icon.key().to_owned(),
            Self::Custom { revision } => format!("{CUSTOM_PREFIX}{revision}"),
        }
    }

    /// 选择的种类。
    pub fn kind(&self) -> AppIconKind {
        match self {
            Self::Default => AppIconKind::Default,
            Self::Builtin(icon) => icon.kind(),
            Self::Custom { .. } => AppIconKind::Custom,
        }
    }
}

fn is_valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= MAX_REVISION_LEN
        && revision.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// `agent.platform.setAppIcon` 参数。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformSetAppIconParams {
    /// 目标种类。
    pub choice: AppIconKind,
    /// 仅 `custom`：要导入的本机图片绝对路径（png / jpg / webp / bmp / gif / ico）；缺省 = 切回已导入的图片。
    #[serde(default)]
    pub source_path: Option<String>,
}

/// `agent.platform.setAppIcon` 结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AppIconStateDto {
    /// 已写入 [`APP_ICON_PREF_KEY`] 的偏好值。
    pub value: String,
}

/// `agent.platform.appIconImage` 参数：当前自定义图标按 `size` 缩放后的 PNG。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformAppIconImageParams {
    /// 目标边长（物理像素），agent 限制在
    /// [`APP_ICON_IMAGE_MIN_SIZE`]..=[`APP_ICON_IMAGE_MAX_SIZE`]。
    pub size: u32,
}

/// `agent.platform.appIconImage` 结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlatformAppIconImageDto {
    /// 当前自定义图标修订号（与偏好值里的一致）。
    pub revision: String,
    /// `size`×`size` 的 PNG，base64 编码。
    pub png: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preference_round_trip_and_rejects_unsafe_revisions() {
        let builtins = BuiltinAppIcon::ALL.map(AppIconChoice::Builtin);
        for choice in [
            AppIconChoice::Default,
            AppIconChoice::Custom {
                revision: "abc123".into(),
            },
        ]
        .into_iter()
        .chain(builtins)
        {
            let value = json!(choice.to_preference());
            assert_eq!(AppIconChoice::from_preference(Some(&value)), choice);
        }
        for invalid in [
            json!("custom:../x"),
            json!("custom:a b"),
            json!(format!("custom:{}", "a".repeat(65))),
            json!("BOLT"),
            json!(true),
        ] {
            assert_eq!(
                AppIconChoice::from_preference(Some(&invalid)),
                AppIconChoice::Default,
                "{invalid}"
            );
        }
    }
}
