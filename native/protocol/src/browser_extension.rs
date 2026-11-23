//! 浏览器扩展安装：三家商店的扩展 ID / 商店地址、外部扩展更新源与 `agent.platform.*` 线上类型。
//!
//! 设置 → 关于的「安装到浏览器」由 agent 在本机执行（与迅雷 / IDM 同一机制，但只在用户点击时）：
//! Chrome / Edge 在 Windows（HKCU 注册表）与 macOS / Linux Edge（用户级 `External Extensions/<id>.json`）
//! 登记指向官方商店的外部扩展更新源，浏览器随后弹出「已添加新扩展」由用户启用，版本始终取商店最新；
//! 同时在该浏览器打开商店详情页，用户也可直接点「添加」。Linux Google Chrome 只有 root 目录可登记，
//! 只打开商店页；Firefox 不支持外部程序安装，用 Firefox 打开 AMO 最新签名包触发安装确认。
//! 卸载 FluxDown 时由安装器清理登记项（浏览器随之移除扩展）。

use serde::{Deserialize, Serialize};

/// Chrome 网上应用店扩展 ID（与 `fluxDown/wxt.config.ts` 的 `key` 推导值一致，NMH 白名单同源）。
pub const CHROME_EXTENSION_ID: &str = "meleenglfggcmcajknpeeeiobnpfmahc";
/// Edge 加载项扩展 ID。
pub const EDGE_EXTENSION_ID: &str = "nglkkjbogjghekbhhcnccnpfedjbdhhd";
/// Firefox 扩展 ID（`browser_specific_settings.gecko.id`）。
pub const FIREFOX_EXTENSION_ID: &str = "fluxdown@fluxdown.app";

/// Chrome 网上应用店详情页。
pub const CHROME_STORE_URL: &str =
    "https://chromewebstore.google.com/detail/fluxdown/meleenglfggcmcajknpeeeiobnpfmahc";
/// Edge 加载项详情页。
pub const EDGE_STORE_URL: &str =
    "https://microsoftedge.microsoft.com/addons/detail/fluxdown/nglkkjbogjghekbhhcnccnpfedjbdhhd";
/// Firefox 附加组件详情页。
pub const FIREFOX_STORE_URL: &str = "https://addons.mozilla.org/firefox/addon/fluxdown/";
/// AMO 最新已签名版本（302 到当前版本的 `.xpi`）；Firefox 打开即弹安装确认。
pub const FIREFOX_LATEST_XPI_URL: &str =
    "https://addons.mozilla.org/firefox/downloads/latest/fluxdown/latest.xpi";

/// Chrome 网上应用店的外部扩展更新源（Windows / macOS 只接受商店来源）。
pub const CHROME_UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";
/// Edge 加载项的外部扩展更新源。
pub const EDGE_UPDATE_URL: &str = "https://edge.microsoft.com/extensionwebstorebase/v1/crx";

/// 设置页提供一键安装的浏览器。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserKind {
    Chrome,
    Edge,
    Firefox,
}

impl BrowserKind {
    /// 展示顺序。
    pub const ALL: [Self; 3] = [Self::Chrome, Self::Edge, Self::Firefox];

    /// 线上取值（同 serde），也是一次性 agent 安装模式的位置参数。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Firefox => "firefox",
        }
    }

    /// 解析 [`Self::as_str`] 的取值。
    ///
    /// ```
    /// use fluxdown_protocol::browser_extension::BrowserKind;
    /// assert_eq!(BrowserKind::parse("edge"), Some(BrowserKind::Edge));
    /// assert_eq!(BrowserKind::parse("safari"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// 展示名（专有名词，不翻译）。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chrome => "Chrome",
            Self::Edge => "Edge",
            Self::Firefox => "Firefox",
        }
    }

    /// 该浏览器商店里的扩展 ID。
    #[must_use]
    pub const fn extension_id(self) -> &'static str {
        match self {
            Self::Chrome => CHROME_EXTENSION_ID,
            Self::Edge => EDGE_EXTENSION_ID,
            Self::Firefox => FIREFOX_EXTENSION_ID,
        }
    }

    /// 商店详情页。
    #[must_use]
    pub const fn store_url(self) -> &'static str {
        match self {
            Self::Chrome => CHROME_STORE_URL,
            Self::Edge => EDGE_STORE_URL,
            Self::Firefox => FIREFOX_STORE_URL,
        }
    }

    /// 一键安装时在该浏览器打开的地址：Chromium 系为商店详情页，Firefox 为 AMO 最新签名包。
    #[must_use]
    pub const fn install_url(self) -> &'static str {
        match self {
            Self::Chrome => CHROME_STORE_URL,
            Self::Edge => EDGE_STORE_URL,
            Self::Firefox => FIREFOX_LATEST_XPI_URL,
        }
    }

    /// Chromium 外部扩展更新源；Firefox 没有外部扩展登记。
    #[must_use]
    pub const fn external_update_url(self) -> Option<&'static str> {
        match self {
            Self::Chrome => Some(CHROME_UPDATE_URL),
            Self::Edge => Some(EDGE_UPDATE_URL),
            Self::Firefox => None,
        }
    }
}

/// 单个浏览器的扩展安装状态（`agent.platform.browserExtensionStatus` 的条目）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExtensionEntryDto {
    pub browser: BrowserKind,
    /// 本机检测到该浏览器（可被拉起）。
    pub browser_installed: bool,
    /// 任一浏览器配置文件里已安装的 FluxDown 扩展最高版本；未安装为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_version: Option<String>,
    /// 已登记指向商店的外部扩展（浏览器会自动安装 / 更新）。
    #[serde(default)]
    pub external_registered: bool,
    /// 本平台支持外部扩展登记；`false` 时一键安装只打开商店页。
    #[serde(default)]
    pub external_supported: bool,
}

/// `agent.platform.browserExtensionStatus` 结果。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExtensionStatusDto {
    pub browsers: Vec<BrowserExtensionEntryDto>,
}

/// `agent.platform.installBrowserExtension` 参数。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExtensionInstallParams {
    pub browser: BrowserKind,
}

/// `agent.platform.installBrowserExtension` 结果。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExtensionInstallResult {
    /// 本次已登记外部扩展（浏览器会弹出启用提示）；`false` = 只打开了商店 / 安装页。
    pub external_registered: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_kind_round_trips_through_wire_and_arg() {
        for kind in BrowserKind::ALL {
            assert_eq!(BrowserKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).expect("serialize"),
                serde_json::Value::String(kind.as_str().to_owned())
            );
        }
    }

    #[test]
    fn store_urls_embed_the_matching_extension_id() {
        assert!(
            BrowserKind::Chrome
                .store_url()
                .ends_with(CHROME_EXTENSION_ID)
        );
        assert!(BrowserKind::Edge.store_url().ends_with(EDGE_EXTENSION_ID));
        assert!(BrowserKind::Firefox.external_update_url().is_none());
    }
}
