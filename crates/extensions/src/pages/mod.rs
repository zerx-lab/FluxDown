//! 扩展分类的两个子页：插件（已安装 / 安装 / 市场）与受管组件（ffmpeg / yt-dlp）。

use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::{ComponentTokens, ExtendedTokens, SemanticThemeTokens};

pub mod managed_components;
pub mod market_favorites;
pub mod plugins;
pub mod private_plugins;
pub mod site_defaults;

/// 一帧内不变的渲染输入：文案、主题 token 与本地服务连接状态。
#[derive(Clone, Copy)]
pub(crate) struct Frame<'a> {
    pub translator: &'a Translator,
    pub tokens: &'a SemanticThemeTokens,
    pub extended: &'a ExtendedTokens,
    pub components: &'a ComponentTokens,
    pub stale: bool,
}
