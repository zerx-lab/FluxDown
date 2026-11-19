//! 汉字字体：按界面语言（简 / 繁）选主字体与缺字回退链。
//!
//! Windows 的 `.SystemUIFont` 取系统图标标题字体（台湾版通常是微軟正黑體 UI），DirectWrite
//! 的系统缺字回退又按**系统区域**而不是应用语言挑字体：正黑体只覆盖繁体（代码页 950），
//! 简体专用字会落到細明體等衬线明体上，同一行字体割裂、细笔画在灰度抗锯齿下断裂（#1003）。
//! 因此在 Windows 上：
//! - 界面语言是中文且主题 / 用户未指定字体时，主字体换成该语言对应的 UI 黑体（与 Windows
//!   在该语言下自身的界面字体一致）；
//! - 所有窗口根节点挂一条汉字回退链（当前字形偏好在前），文件名等用户数据里另一种汉字
//!   也落在黑体上，而不是系统回退里的明体。
//!
//! macOS（苹方）与 Linux（fontconfig / Noto CJK）的系统回退本身就是全覆盖黑体，不作改动。

use gpui::{App, Context, FontFallbacks, IntoElement, Render, SharedString, Window};
use gpui_base::RootPlugin;

/// 汉字字形偏好。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HanScript {
    /// 简体（微软雅黑 UI 覆盖 GB18030，含繁体字，作为兜底最安全）。
    #[default]
    Simplified,
    Traditional,
}

impl HanScript {
    /// FluxDown 已解析的界面 locale 代码（`zh` / `zh-tw`，见 `fluxdown_i18n`）→ 字形偏好；
    /// 非中文 locale 返回 `None`。
    #[must_use]
    pub fn from_locale(code: &str) -> Option<Self> {
        match code {
            "zh" => Some(Self::Simplified),
            "zh-tw" => Some(Self::Traditional),
            _ => None,
        }
    }
}

/// 汉字字体偏好：由界面语言（非中文界面时取系统语言）推导，经 [`set_han_font_preference`]
/// 设置；不是用户偏好，不进入主题文档或同步目录。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HanFontPreference {
    /// 汉字回退链里优先的字形。
    pub script: HanScript,
    /// 界面文案本身是中文：此时主字体也换成对应的汉字黑体。
    pub chinese_ui: bool,
}

#[derive(Default)]
struct HanFontGlobal(HanFontPreference);

impl gpui::Global for HanFontGlobal {}

pub(crate) fn current_preference(cx: &App) -> HanFontPreference {
    cx.try_global::<HanFontGlobal>()
        .map(|global| global.0)
        .unwrap_or_default()
}

/// 更新汉字字体偏好；与当前值不同且主题已安装时重新安装主题（字体与回退链随之更新）。
pub fn set_han_font_preference(preference: HanFontPreference, cx: &mut App) {
    if current_preference(cx) == preference {
        return;
    }
    cx.set_global(HanFontGlobal(preference));
    if cx.has_global::<crate::FluxThemeState>() {
        crate::reinstall_current(cx);
    }
}

/// 按字形偏好排好的汉字 UI 黑体（偏好字形在前）。
fn han_families(script: HanScript) -> [&'static str; 2] {
    const SIMPLIFIED: &str = "Microsoft YaHei UI";
    const TRADITIONAL: &str = "Microsoft JhengHei UI";
    match script {
        HanScript::Simplified => [SIMPLIFIED, TRADITIONAL],
        HanScript::Traditional => [TRADITIONAL, SIMPLIFIED],
    }
}

/// 汉字字体方案：可选的主字体替换 + 根节点回退链。
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct HanFontPlan {
    pub primary: Option<SharedString>,
    pub fallbacks: Option<FontFallbacks>,
}

/// 纯规则：`sans` 仍是平台默认界面字体（未被主题 / 用户指定）且界面是中文时，
/// 主字体换成偏好字形的第一个已安装黑体；回退链总是带上全部汉字黑体。
fn plan_for(
    preference: HanFontPreference,
    sans: &str,
    platform_default_sans: &str,
    installed: impl Fn(&str) -> bool,
) -> HanFontPlan {
    let families = han_families(preference.script);
    let primary = (preference.chinese_ui && sans == platform_default_sans)
        .then(|| families.into_iter().find(|name| installed(name)))
        .flatten()
        .map(SharedString::new_static);
    HanFontPlan {
        primary,
        fallbacks: Some(FontFallbacks::from_fonts(
            families.iter().map(|name| (*name).to_owned()).collect(),
        )),
    }
}

/// 当前平台的汉字字体方案；只在 Windows 生效，其他平台为空方案。
pub(crate) fn plan(preference: HanFontPreference, sans: &str, cx: &App) -> HanFontPlan {
    if !cfg!(target_os = "windows") {
        return HanFontPlan::default();
    }
    let platform_default = gpui_base::TypographyTokens::default().sans;
    plan_for(preference, sans, platform_default.as_ref(), |name| {
        is_installed(name, cx)
    })
}

/// 字体族是否真实存在。Windows 的 `all_font_names` 按系统区域给本地化名（如「微軟正黑體 UI」），
/// 不能拿英文名比对；改为解析一次：缺失时 GPUI 回退到兜底字体栈，解析结果的族名对不上。
fn is_installed(name: &str, cx: &App) -> bool {
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&gpui::font(SharedString::from(name.to_owned())));
    text_system
        .get_font_for_id(font_id)
        .is_some_and(|font| font.family.as_ref() == name)
}

/// 在窗口根节点设置正文默认字重与汉字回退链，内容和 Root 浮层一同继承。
struct WindowTextLayer;

impl Render for WindowTextLayer {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

impl RootPlugin for WindowTextLayer {
    fn style(&self, surface: &mut gpui::Stateful<gpui::Div>, _window: &mut Window, cx: &mut App) {
        use gpui::Styled as _;
        let Some(state) = cx.try_global::<crate::FluxThemeState>() else {
            return;
        };
        if let Some(weight) = state.appearance().font_weight {
            surface.text_style().font_weight = Some(gpui::FontWeight(f32::from(weight)));
        }
        if let Some(fallbacks) = state.font_fallbacks() {
            surface.text_style().font_fallbacks = Some(fallbacks.clone());
        }
    }
}

pub(crate) fn register_root_plugin(cx: &mut App) {
    gpui_base::Root::register_plugin::<WindowTextLayer>(cx, |_, _| WindowTextLayer);
}

#[cfg(test)]
mod tests {
    use super::{HanFontPreference, HanScript, plan_for};

    const DEFAULT: &str = ".SystemUIFont";
    const YAHEI: &str = "Microsoft YaHei UI";
    const JHENGHEI: &str = "Microsoft JhengHei UI";

    fn pref(script: HanScript, chinese_ui: bool) -> HanFontPreference {
        HanFontPreference { script, chinese_ui }
    }

    fn fallback_names(plan: &super::HanFontPlan) -> Vec<String> {
        plan.fallbacks
            .as_ref()
            .map(|fallbacks| fallbacks.fallback_list().to_vec())
            .unwrap_or_default()
    }

    #[test]
    fn locale_codes_map_to_han_script() {
        assert_eq!(HanScript::from_locale("zh"), Some(HanScript::Simplified));
        assert_eq!(
            HanScript::from_locale("zh-tw"),
            Some(HanScript::Traditional)
        );
        assert_eq!(HanScript::from_locale("en"), None);
        assert_eq!(HanScript::from_locale("tr"), None);
    }

    #[test]
    fn chinese_ui_replaces_platform_default_with_matching_script() {
        let all = |_: &str| true;
        let simplified = plan_for(pref(HanScript::Simplified, true), DEFAULT, DEFAULT, all);
        assert_eq!(simplified.primary.as_deref(), Some(YAHEI));
        assert_eq!(fallback_names(&simplified), [YAHEI, JHENGHEI]);

        let traditional = plan_for(pref(HanScript::Traditional, true), DEFAULT, DEFAULT, all);
        assert_eq!(traditional.primary.as_deref(), Some(JHENGHEI));
        assert_eq!(fallback_names(&traditional), [JHENGHEI, YAHEI]);
    }

    #[test]
    fn explicit_font_and_non_chinese_ui_keep_primary() {
        let all = |_: &str| true;
        let chosen = plan_for(pref(HanScript::Simplified, true), "Inter", DEFAULT, all);
        assert_eq!(chosen.primary, None);
        assert_eq!(fallback_names(&chosen), [YAHEI, JHENGHEI]);

        let english = plan_for(pref(HanScript::Traditional, false), DEFAULT, DEFAULT, all);
        assert_eq!(english.primary, None);
        assert_eq!(fallback_names(&english), [JHENGHEI, YAHEI]);
    }

    #[test]
    fn missing_preferred_font_uses_next_installed_or_keeps_default() {
        let only_jhenghei = |name: &str| name == JHENGHEI;
        let plan = plan_for(
            pref(HanScript::Simplified, true),
            DEFAULT,
            DEFAULT,
            only_jhenghei,
        );
        assert_eq!(plan.primary.as_deref(), Some(JHENGHEI));

        let none = plan_for(pref(HanScript::Simplified, true), DEFAULT, DEFAULT, |_| {
            false
        });
        assert_eq!(none.primary, None);
    }
}
