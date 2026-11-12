//! 设置页的自有布局原语：分类页 → 子 Tab → 分组卡片 → 设置行。
//!
//! 视觉规则与全客户端一致（见 `fluxdown_ui_components::kit`）：
//! - 分组卡片 = surface + hairline 描边 + `radius.lg`，行间 hairline 分隔，不加阴影；
//! - 组标题 caption MEDIUM + 三级文字色；行标题 `typography.sm`，说明 `typography.xs` 二级文字色；
//!   说明只占一行，超出时截断，完整内容收进标题旁信息图标的悬浮提示；
//! - 右侧控件一律统一档 [`fluxdown_ui_theme::CONTROL_HEIGHT`]（28）：按钮 / 输入走 `ControlExt::control`，下拉为
//!   outline + caret，数字输入同高；横排行最小行高 = `density.control` + [`ROW_MIN_PADDING`]；
//!   开关保持 `Switch`。数字带物理量时必须显示单位（[`Control::unit`] 固定单位，或数字 + 单位下拉）。
//! - 字段宽度按 Tab 统一：横排行的数字 / 输入 / 下拉 / [`Control::field`] 各报一个宽度档
//!   （[`NUMBER_WIDTH`] / [`INPUT_WIDTH`] / [`DROPDOWN_MIN_WIDTH`]，均为 100% 基准值，经 `text_extent`
//!   随字号缩放），同一 Tab 内全部字段取其中最宽档，使一屏内输入框与下拉同宽；不同 Tab 各取各的。
//!   带固定单位的数字：单位放进字段框右侧的统一单位列（宽度 = 本 Tab 最长单位文字，不计入字段
//!   宽度）；该 Tab 有单位列时，无单位的数字 / 输入 / 下拉同样让出这一列，使字段框左右边缘对齐。
//!   数字旁的单位下拉固定 [`UNIT_DROPDOWN_WIDTH`]。
//! - 列表行内操作（上移 / 下移 / 测试 / 删除…）同高：[`row_button`] / [`row_icon_button`] 只额外禁止被长文本挤压。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, FluxIcon, card, dialog_scroll_body, dialog_title, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Div, ElementId, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};

/// 行内文本输入框宽度档位。
pub(crate) const INPUT_WIDTH: f32 = 240.;
/// 行内数字输入框宽度档位：扣掉两侧 −/+ 步进按钮后放得下 6 位数字（端口、限速值）。
pub(crate) const NUMBER_WIDTH: f32 = 136.;
/// 行内下拉按钮的最小宽度档位（文案更长时自然撑开）。
pub(crate) const DROPDOWN_MIN_WIDTH: f32 = 160.;
/// 数字输入旁单位下拉（KB/s、MB/s…）的固定宽度。
pub(crate) const UNIT_DROPDOWN_WIDTH: f32 = 88.;
/// 横排设置行最小行高 = 控件高（`density.control`）+ 上下各 10px 的留白。
pub(crate) const ROW_MIN_PADDING: f32 = 20.;

/// 横排字段的 Tab 级排版：统一字段宽度 + 固定单位列宽。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FieldLayout {
    /// 统一字段宽度。
    pub(crate) width: Pixels,
    /// 单位列宽（本 Tab 最长固定单位文字）；`None` = 本 Tab 没有固定单位。
    unit: Option<Pixels>,
}

impl FieldLayout {
    /// 按字段宽度档与单位文字计算；单位按 `meta_text` 的字号实测。
    fn measure<'a>(
        tier: f32,
        units: impl IntoIterator<Item = &'a SharedString>,
        window: &Window,
        cx: &App,
    ) -> Self {
        let theme = active_theme(cx);
        let font_size = theme.tokens().typography.xs.size;
        let style = window.text_style();
        let unit = units
            .into_iter()
            .map(|unit| {
                let run = style.to_run(unit.len());
                window
                    .text_system()
                    .shape_line(unit.clone(), font_size, &[run], None)
                    .width
            })
            .reduce(Pixels::max)
            // 亚像素宽度向上取整，避免同宽文字因舍入被截断。
            .map(|width| px(f32::from(width).ceil()));
        Self {
            width: theme.text_extent(tier),
            unit,
        }
    }
}

/// 正文文字（`typography.sm`，正文色）。
pub(crate) fn body_text(cx: &App) -> Div {
    let tokens = active_theme(cx).tokens();
    div()
        .text_size(tokens.typography.sm.size)
        .line_height(tokens.typography.sm.line_height)
        .text_color(tokens.colors.foreground)
}

/// 辅助说明 / 元信息文字（`typography.xs`，二级文字色）。
pub(crate) fn meta_text(cx: &App) -> Div {
    let tokens = active_theme(cx).tokens();
    div()
        .text_size(tokens.typography.xs.size)
        .line_height(tokens.typography.xs.line_height)
        .text_color(tokens.colors.muted_foreground)
}

/// 分区 / 组标题文字（caption MEDIUM，三级文字色）。
pub(crate) fn caption_heading(cx: &App) -> Div {
    let theme = active_theme(cx);
    let extended = theme.extended();
    div()
        .text_size(extended.caption.size)
        .line_height(extended.caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(extended.colors.text_tertiary)
}

/// 页面内容区左右留白（设置页与 Webhook 独立页共用）。
pub(crate) const CONTENT_PADDING_LEFT: f32 = 28.;
pub(crate) const CONTENT_PADDING_RIGHT: f32 = 24.;

/// 页面标题 + 一行描述（`extended.title` + xs 二级文字）。
pub(crate) fn page_heading(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let title_style = theme.extended().title;
    v_flex()
        .w_full()
        .gap(tokens.spacing.xxs)
        .child(
            div()
                .text_size(title_style.size)
                .line_height(title_style.line_height)
                .font_weight(title_style.weight)
                .text_color(tokens.colors.foreground)
                .child(title.into()),
        )
        .child(meta_text(cx).truncate().child(description.into()))
}

/// 对话框底栏：右对齐，`spacing.sm` 间距；`leading` 放左端（次要破坏性操作）。
pub(crate) fn dialog_footer(
    leading: Option<AnyElement>,
    actions: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> Div {
    let tokens = active_theme(cx).tokens();
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(tokens.spacing.sm)
        .children(leading)
        .child(div().flex_1())
        .children(actions)
}

/// 次要破坏性操作按钮（danger-ghost）：透明底 + 破坏色文字，悬停中性灰。
/// 对话框底栏左端的「删除」用它；列表行内用 [`row_danger_button`]。
pub(crate) fn danger_ghost_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    cx: &App,
) -> fluxdown_ui_components::Button {
    let destructive = active_theme(cx).tokens().colors.destructive;
    fluxdown_ui_components::button(id, label, ButtonVariant::Ghost, cx).text_color(destructive)
}

/// 列表行内的文字按钮：套件按钮且不被长文本挤压。
/// 返回值已设置悬停，调用方不得再 `.hover()`。
pub(crate) fn row_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    cx: &App,
) -> fluxdown_ui_components::Button {
    fluxdown_ui_components::button(id, label, variant, cx).flex_shrink_0()
}

/// 列表行内的「删除」：danger-ghost，不被长文本挤压。
pub(crate) fn row_danger_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    cx: &App,
) -> fluxdown_ui_components::Button {
    danger_ghost_button(id, label, cx).flex_shrink_0()
}

/// [`row_button`] 的 loading 版本（异步动作进行中显示旋转图标且不可点击）。
pub(crate) fn row_loading_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    loading: bool,
    cx: &App,
) -> fluxdown_ui_components::Button {
    fluxdown_ui_components::loading_button(id, label, variant, loading, cx).flex_shrink_0()
}

/// [`row_danger_button`] 的 loading 版本。
pub(crate) fn row_loading_danger_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    loading: bool,
    cx: &App,
) -> fluxdown_ui_components::Button {
    let destructive = active_theme(cx).tokens().colors.destructive;
    fluxdown_ui_components::loading_button(id, label, ButtonVariant::Ghost, loading, cx)
        .text_color(destructive)
        .flex_shrink_0()
}

/// 空状态：FluxIcon 32px 三级文字色 + 标题 sm MEDIUM + 说明 xs，居中。
pub(crate) fn empty_state(
    icon: impl Into<Icon>,
    title: SharedString,
    description: SharedString,
    cx: &App,
) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    v_flex()
        .w_full()
        .items_center()
        .gap(tokens.spacing.xs)
        .py(tokens.spacing.lg)
        .child(
            Icon::new(icon)
                .size(px(32.))
                .text_color(extended.colors.text_tertiary),
        )
        .child(body_text(cx).font_weight(FontWeight::MEDIUM).child(title))
        .child(meta_text(cx).text_center().child(description))
}

/// 触发双列排布的最小内容宽度。
pub(crate) const TWO_COLUMN_MIN_WIDTH: f32 = 920.;

pub(crate) type Getter<T> = Rc<dyn Fn(&App) -> T>;
pub(crate) type Setter<T> = Rc<dyn Fn(T, &mut App)>;
/// 自定义渲染：`(disabled, key, window, cx)`。
type Renderer = Rc<dyn Fn(bool, &SharedString, &mut Window, &mut App) -> AnyElement>;
/// 字段渲染：`(disabled, key, width, window, cx)`；`width` = 所在 Tab 的统一字段宽度，
/// `None` 表示竖排行（字段占满行宽）。
type FieldRenderer =
    Rc<dyn Fn(bool, &SharedString, Option<Pixels>, &mut Window, &mut App) -> AnyElement>;

/// 一行设置的右侧控件。
#[derive(Clone)]
pub(crate) enum Control {
    Switch {
        get: Getter<bool>,
        set: Setter<bool>,
    },
    Number {
        min: f64,
        max: f64,
        step: f64,
        get: Getter<f64>,
        set: Setter<f64>,
        /// 固定单位后缀（如 `MB`）；`None` = 无量纲计数。
        unit: Option<SharedString>,
    },
    Input {
        get: Getter<SharedString>,
        set: Setter<SharedString>,
    },
    Dropdown {
        options: Vec<(SharedString, SharedString)>,
        get: Getter<SharedString>,
        set: Setter<SharedString>,
    },
    Custom(Renderer),
    /// 自绘字段（端口 / 令牌输入、字体选择、数字 + 单位下拉…）：按 `tier` 参与 Tab
    /// 统一宽度，渲染时收到统一后的宽度。
    Field {
        tier: f32,
        render: FieldRenderer,
    },
}

impl Control {
    pub(crate) fn switch<G, S>(get: G, set: S) -> Self
    where
        G: Fn(&App) -> bool + 'static,
        S: Fn(bool, &mut App) + 'static,
    {
        Self::Switch {
            get: Rc::new(get),
            set: Rc::new(set),
        }
    }

    pub(crate) fn number<G, S>(min: f64, max: f64, step: f64, get: G, set: S) -> Self
    where
        G: Fn(&App) -> f64 + 'static,
        S: Fn(f64, &mut App) + 'static,
    {
        Self::Number {
            min,
            max,
            step,
            get: Rc::new(get),
            set: Rc::new(set),
            unit: None,
        }
    }

    /// 给数字控件加固定单位后缀；对非数字控件无效。
    #[must_use]
    pub(crate) fn unit(mut self, label: impl Into<SharedString>) -> Self {
        if let Self::Number { unit, .. } = &mut self {
            *unit = Some(label.into());
        }
        self
    }

    /// 固定单位文字（只有带单位的数字控件有）。
    fn unit_label(&self) -> Option<&SharedString> {
        match self {
            Self::Number { unit, .. } => unit.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn input<G, S>(get: G, set: S) -> Self
    where
        G: Fn(&App) -> SharedString + 'static,
        S: Fn(SharedString, &mut App) + 'static,
    {
        Self::Input {
            get: Rc::new(get),
            set: Rc::new(set),
        }
    }

    pub(crate) fn dropdown<G, S>(options: Vec<(SharedString, SharedString)>, get: G, set: S) -> Self
    where
        G: Fn(&App) -> SharedString + 'static,
        S: Fn(SharedString, &mut App) + 'static,
    {
        Self::Dropdown {
            options,
            get: Rc::new(get),
            set: Rc::new(set),
        }
    }

    /// 自定义控件；闭包收到禁用态与本行的稳定状态键。
    pub(crate) fn custom<F, E>(render: F) -> Self
    where
        E: IntoElement,
        F: Fn(bool, &SharedString, &mut Window, &mut App) -> E + 'static,
    {
        Self::Custom(Rc::new(move |disabled, key, window, cx| {
            render(disabled, key, window, cx).into_any_element()
        }))
    }

    /// 自绘字段：`tier` 为本字段需要的宽度档（如 [`INPUT_WIDTH`]），闭包收到所在 Tab
    /// 统一后的字段宽度（`None` = 竖排占满）。
    pub(crate) fn field<F, E>(tier: f32, render: F) -> Self
    where
        E: IntoElement,
        F: Fn(bool, &SharedString, Option<Pixels>, &mut Window, &mut App) -> E + 'static,
    {
        Self::Field {
            tier,
            render: Rc::new(move |disabled, key, width, window, cx| {
                render(disabled, key, width, window, cx).into_any_element()
            }),
        }
    }

    /// 参与 Tab 统一宽度的宽度档；开关与自定义控件不参与。
    fn field_tier(&self) -> Option<f32> {
        match self {
            Self::Number { .. } => Some(NUMBER_WIDTH),
            Self::Input { .. } => Some(INPUT_WIDTH),
            Self::Dropdown { .. } => Some(DROPDOWN_MIN_WIDTH),
            Self::Field { tier, .. } => Some(*tier),
            Self::Switch { .. } | Self::Custom(_) => None,
        }
    }
}

/// 标题旁「?」说明入口的文案：悬浮提示、对话框标题、正文（空行分段；段首
/// 「引导语：」/「Lead: 」加粗成小标题）与关闭按钮。
#[derive(Clone)]
pub(crate) struct Explain {
    pub hint: SharedString,
    pub title: SharedString,
    pub body: SharedString,
    pub close: SharedString,
}

/// 打开说明对话框（只读，正文可滚动）。
fn open_explain_dialog(explain: Explain, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let explain = explain.clone();
        dialog
            .title(dialog_title(explain.title.clone(), cx))
            .w(active_theme(cx).text_extent(560.))
            .content(move |content, _, cx| {
                let tokens = active_theme(cx).tokens().clone();
                let paragraphs = explain.body.split("\n\n").map(|paragraph| {
                    let (lead, text) = paragraph
                        .split_once('：')
                        .or_else(|| paragraph.split_once(": "))
                        .map_or((None, paragraph), |(lead, text)| (Some(lead), text));
                    v_flex()
                        .gap(tokens.spacing.xxs)
                        .when_some(lead, |this, lead| {
                            this.child(
                                body_text(cx)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(lead.to_owned())),
                            )
                        })
                        .child(
                            body_text(cx)
                                .text_color(tokens.colors.muted_foreground)
                                .child(SharedString::from(text.trim().to_owned())),
                        )
                });
                content
                    .min_h_0()
                    .child(dialog_scroll_body(
                        "settings-explain-body",
                        Some(px(460.)),
                        v_flex()
                            .w_full()
                            .gap(tokens.spacing.md)
                            .children(paragraphs),
                        cx,
                    ))
                    .child(
                        dialog_footer(
                            None,
                            [fluxdown_ui_components::button(
                                "settings-explain-close",
                                explain.close.clone(),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .on_click(|_, window, cx| window.close_dialog(cx))
                            .into_any_element()],
                            cx,
                        )
                        .pt(tokens.spacing.sm),
                    )
            })
    });
}

/// 分组卡片内的一行。
#[derive(Clone)]
pub(crate) struct SettingsRow {
    pub(crate) title: SharedString,
    pub(crate) description: Option<SharedString>,
    /// 补充说明；与超出一行的描述一起收进标题旁信息图标的悬浮提示。
    help: Option<SharedString>,
    /// 标题旁的「?」入口；点击打开完整说明对话框。
    explain: Option<Explain>,
    keywords: Vec<SharedString>,
    disabled: bool,
    vertical: bool,
    /// `None` 表示整行自渲染（[`Self::custom`] / [`Self::custom_field`]）。
    control: Option<Control>,
    full: Option<FieldRenderer>,
    /// 整行自渲染里字段的宽度档（[`Self::custom_field`]），参与 Tab 统一宽度。
    full_tier: Option<f32>,
    /// 行下方的附加区（随值动态出现的提示等）；每帧调用，可返回空元素。
    footer: Option<Renderer>,
    /// 显式高度权重；双列切分用。
    weight: Option<f32>,
}

impl SettingsRow {
    pub(crate) fn new(title: impl Into<SharedString>, control: Control) -> Self {
        Self {
            title: title.into(),
            description: None,
            help: None,
            explain: None,
            keywords: Vec::new(),
            disabled: false,
            vertical: false,
            control: Some(control),
            full: None,
            full_tier: None,
            footer: None,
            weight: None,
        }
    }

    /// 整行自渲染（列表、按钮组等无「标题 + 控件」结构的行）。
    pub(crate) fn custom<F, E>(render: F) -> Self
    where
        E: IntoElement,
        F: Fn(bool, &SharedString, &mut Window, &mut App) -> E + 'static,
    {
        Self::custom_field(None, move |disabled, key, _, window, cx| {
            render(disabled, key, window, cx)
        })
    }

    /// 含一个字段的整行自渲染：`tier` 参与 Tab 统一宽度（`None` 不参与），闭包收到统一后的
    /// 字段宽度。
    pub(crate) fn custom_field<F, E>(tier: Option<f32>, render: F) -> Self
    where
        E: IntoElement,
        F: Fn(bool, &SharedString, Option<Pixels>, &mut Window, &mut App) -> E + 'static,
    {
        Self {
            title: SharedString::default(),
            description: None,
            help: None,
            explain: None,
            keywords: Vec::new(),
            disabled: false,
            vertical: false,
            control: None,
            full: Some(Rc::new(move |disabled, key, width, window, cx| {
                render(disabled, key, width, window, cx).into_any_element()
            })),
            full_tier: tier,
            footer: None,
            weight: None,
        }
    }

    #[must_use]
    pub(crate) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub(crate) fn keywords<I, S>(mut self, keywords: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 控件换行到标题下方（长输入、编辑器、选择器）。
    #[must_use]
    pub(crate) fn vertical(mut self) -> Self {
        self.vertical = true;
        self
    }

    /// 补充说明：标题旁显示信息图标，悬浮提示展示（与超出一行的描述合并）。
    #[must_use]
    pub(crate) fn help(mut self, text: impl Into<SharedString>) -> Self {
        self.help = Some(text.into());
        self
    }

    /// 标题旁附加「?」说明入口：悬浮显示提示，点击打开说明对话框。
    #[must_use]
    pub(crate) fn explain(mut self, explain: Explain) -> Self {
        self.explain = Some(explain);
        self
    }

    /// 行下方附加区：在标题 / 控件之下渲染（与本行同属一格，不额外加分隔线）。
    #[must_use]
    pub(crate) fn footer<F, E>(mut self, render: F) -> Self
    where
        E: IntoElement,
        F: Fn(bool, &SharedString, &mut Window, &mut App) -> E + 'static,
    {
        self.footer = Some(Rc::new(move |disabled, key, window, cx| {
            render(disabled, key, window, cx).into_any_element()
        }));
        self
    }

    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let hit = |text: &SharedString| text.to_lowercase().contains(query);
        hit(&self.title)
            || self.description.as_ref().is_some_and(hit)
            || self.help.as_ref().is_some_and(hit)
            || self.explain.as_ref().is_some_and(|e| hit(&e.body))
            || self.keywords.iter().any(hit)
    }

    /// 除标题外参与搜索的文本：描述、帮助说明、关键词。
    pub(crate) fn search_terms(&self) -> impl Iterator<Item = &SharedString> {
        self.description
            .iter()
            .chain(self.help.iter())
            .chain(self.explain.iter().map(|e| &e.body))
            .chain(self.keywords.iter())
    }

    /// 自适应分组排布的行权重。
    fn layout_weight(&self) -> f32 {
        self.weight.unwrap_or(if self.full.is_some() {
            3.0
        } else if self.vertical {
            2.4
        } else {
            1.0
        })
    }

    /// 参与 Tab 统一宽度的字段宽度档；竖排行的字段占满行宽，不参与。
    fn field_tier(&self) -> Option<f32> {
        if self.vertical {
            return None;
        }
        if self.full.is_some() {
            return self.full_tier;
        }
        self.control.as_ref().and_then(Control::field_tier)
    }

    /// 参与 Tab 单位列的固定单位；竖排行与整行自绘不参与。
    fn unit_label(&self) -> Option<&SharedString> {
        if self.vertical || self.full.is_some() {
            return None;
        }
        self.control.as_ref().and_then(Control::unit_label)
    }

    /// `layout`：所在 Tab 的横排字段排版。
    fn render(
        &self,
        key: SharedString,
        layout: FieldLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let row_min_height = theme.density().control + px(ROW_MIN_PADDING);
        let title_min_width = theme.text_extent(160.);
        let badge_size = theme.text_extent(14.);
        let badge_text_size = theme.text_extent(10.);
        let badge_line_height = theme.text_extent(12.);
        let disabled = self.disabled;
        let mut row = div()
            .id(ElementId::from(key.clone()))
            .w_full()
            .px(tokens.spacing.lg)
            .py(if self.vertical {
                tokens.spacing.md
            } else {
                tokens.spacing.sm
            })
            .when(disabled, |this| this.opacity(0.5));

        let layout = (!self.vertical).then_some(layout);
        let width = layout.map(|layout| layout.width);
        if let Some(full) = self.full.clone() {
            return row.child(full(disabled, &key, width, window, cx));
        }

        let control = self
            .control
            .clone()
            .map(|control| render_control(&control, disabled, &key, layout, window, cx));
        // 描述只占一行：超出时截断，完整内容收进标题旁信息图标的悬浮提示。
        // 是否超出由描述容器 prepaint 时实测（取决于窗口宽度与控件宽度），只在变化时回写。
        let overflow = self.description.as_ref().map(|_| {
            window.use_keyed_state(
                ElementId::from(SharedString::from(format!("{key}-desc-overflow"))),
                cx,
                |_, _| false,
            )
        });
        let overflows = overflow.as_ref().is_some_and(|state| *state.read(cx));
        let details: Vec<SharedString> = self
            .description
            .clone()
            .filter(|_| overflows)
            .into_iter()
            .chain(self.help.clone())
            .collect();
        let label = v_flex()
            .gap(tokens.spacing.xxs)
            .min_w_0()
            .child({
                let mut title_row = div()
                    .flex()
                    .items_center()
                    .gap(tokens.spacing.xs)
                    .child(body_text(cx).child(self.title.clone()));
                if !details.is_empty() {
                    let text = SharedString::from(
                        details
                            .iter()
                            .map(SharedString::as_ref)
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                    );
                    title_row = title_row.child(
                        div()
                            .id(ElementId::from(SharedString::from(format!("{key}-info"))))
                            .flex()
                            .flex_none()
                            .items_center()
                            .tooltip(move |window, cx| tooltip_view(text.clone(), window, cx))
                            .child(
                                Icon::new(FluxIcon::Info)
                                    .size(extended.icon.sm)
                                    .text_color(extended.colors.text_tertiary),
                            ),
                    );
                }
                if let Some(explain) = self.explain.clone() {
                    let hint = explain.hint.clone();
                    title_row = title_row.child(
                        div()
                            .id(ElementId::from(SharedString::from(format!(
                                "{key}-explain"
                            ))))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(badge_size)
                            .rounded_full()
                            .border_1()
                            .border_color(extended.colors.text_tertiary)
                            .text_color(extended.colors.text_tertiary)
                            .text_size(badge_text_size)
                            .line_height(badge_line_height)
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .border_color(tokens.colors.foreground)
                                    .text_color(tokens.colors.foreground)
                            })
                            .tooltip(move |window, cx| tooltip_view(hint.clone(), window, cx))
                            .on_click(move |_, window, cx| {
                                open_explain_dialog(explain.clone(), window, cx);
                            })
                            .child("?"),
                    );
                }
                title_row
            })
            .when_some(
                self.description.clone().zip(overflow),
                |this, (description, state)| {
                    this.child(
                        meta_text(cx)
                            .relative()
                            .truncate()
                            .child(description.clone())
                            .child(
                                canvas(
                                    move |bounds, window, cx| {
                                        let overflow = description_overflows(
                                            &description,
                                            bounds.size.width,
                                            window,
                                        );
                                        if *state.read(cx) != overflow {
                                            state.update(cx, |value, cx| {
                                                *value = overflow;
                                                cx.notify();
                                            });
                                        }
                                    },
                                    |_, (), _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            ),
                    )
                },
            );

        let footer = self
            .footer
            .clone()
            .map(|footer| footer(disabled, &key, window, cx));
        if self.vertical {
            row = row.child(
                v_flex()
                    .w_full()
                    .gap(tokens.spacing.sm)
                    .child(label)
                    .children(control.map(|control| div().w_full().child(control)))
                    .children(footer),
            );
        } else {
            let line = div()
                .w_full()
                .flex()
                .items_center()
                .gap(tokens.spacing.lg)
                // 标题列保底 160px；控件列可收缩，避免宽控件把描述挤成一列。
                .child(div().flex_1().min_w(title_min_width).child(label))
                .children(control.map(|control| div().min_w_0().child(control)));
            row = row
                .flex()
                .items_center()
                .min_h(row_min_height)
                .child(v_flex().w_full().child(line).children(footer));
        }
        row
    }
}

/// 描述按当前文字样式排成单行时是否超出 `available`（含换行符一律视为超出）。
fn description_overflows(
    description: &SharedString,
    available: gpui::Pixels,
    window: &Window,
) -> bool {
    if description.contains('\n') {
        return true;
    }
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let run = style.to_run(description.len());
    let line = window
        .text_system()
        .shape_line(description.clone(), font_size, &[run], None);
    line.width > available
}

/// `layout`：横排行为所在 Tab 的字段排版；`None` = 竖排行，字段占满行宽。
fn render_control(
    control: &Control,
    disabled: bool,
    key: &SharedString,
    layout: Option<FieldLayout>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let width = layout.map(|layout| layout.width);
    let unit_column = layout.and_then(|layout| layout.unit);
    match control {
        Control::Custom(render) => render(disabled, key, window, cx),
        Control::Field { render, .. } => render(disabled, key, width, window, cx),
        Control::Switch { get, set } => {
            let checked = get(cx);
            let set = set.clone();
            Switch::new(ElementId::from(SharedString::from(format!("{key}-switch"))))
                .checked(checked)
                .disabled(disabled)
                .on_click(move |checked: &bool, _, cx| set(*checked, cx))
                .into_any_element()
        }
        Control::Number {
            min,
            max,
            step,
            get,
            set,
            unit,
        } => {
            let number = render_number(
                *min, *max, *step, get, set, disabled, key, width, window, cx,
            );
            with_unit_column(number, unit.clone(), width.is_none(), unit_column, cx)
        }
        Control::Input { get, set } => with_unit_column(
            render_input(get, set, disabled, key, width, window, cx),
            None,
            false,
            unit_column,
            cx,
        ),
        Control::Dropdown { options, get, set } => with_unit_column(
            dropdown_button(
                format!("{key}-dropdown"),
                options,
                get(cx),
                disabled,
                width,
                set.clone(),
                cx,
            ),
            None,
            false,
            unit_column,
            cx,
        ),
    }
}

/// 字段 + 右侧单位列。`column` = Tab 统一单位列宽：有则单位左对齐地放进定宽列，无单位的字段
/// 也留出同宽空列，使同屏字段框左右对齐；`None`（竖排行 / Tab 无单位）时单位按文字自身宽度。
/// `fill`：竖排行字段占满行宽。
fn with_unit_column(
    field: AnyElement,
    unit: Option<SharedString>,
    fill: bool,
    column: Option<Pixels>,
    cx: &App,
) -> AnyElement {
    if unit.is_none() && column.is_none() {
        return field;
    }
    let tokens = active_theme(cx).tokens();
    h_flex()
        .gap(tokens.spacing.sm)
        .items_center()
        .when(fill, |this| this.w_full())
        .child(
            div()
                .when(fill, |this| this.flex_1().min_w_0())
                .child(field),
        )
        .child(
            meta_text(cx)
                .flex_shrink_0()
                .when_some(column, |this, width| this.w(width))
                .children(unit),
        )
        .into_any_element()
}

thread_local! {
    /// 下拉按钮最近一次 prepaint 的宽度（按按钮 id）；只在打开菜单时读取，不参与 render。
    static DROPDOWN_TRIGGER_WIDTHS: RefCell<HashMap<SharedString, Pixels>> =
        RefCell::new(HashMap::new());
}

/// 下拉选择按钮：当前值显示为 label，菜单项带勾选态；供 [`Control::Dropdown`]
/// 与需要自定义值映射的分区（如 User-Agent 预设）共用同一视觉。
/// `min_width`：最小宽度（文案更长时自然撑开）；`None` 占满父容器。
/// 展开的菜单与按钮等宽、右缘对齐（同原生 select）：按钮宽度在 prepaint 时按 `id` 记入
/// [`DROPDOWN_TRIGGER_WIDTHS`]，打开菜单时读取。不能用本次 render 新建的 `Rc`：点击后
/// 菜单在下一帧的 render 阶段构建，那时本帧的 prepaint 还没跑，读到的是空值。
pub(crate) fn dropdown_button(
    id: impl Into<SharedString>,
    options: &[(SharedString, SharedString)],
    current: SharedString,
    disabled: bool,
    min_width: Option<Pixels>,
    set: Setter<SharedString>,
    cx: &App,
) -> AnyElement {
    let label = options
        .iter()
        .find(|(value, _)| *value == current)
        .map_or_else(|| current.clone(), |(_, label)| label.clone());
    let options = options.to_vec();
    let id: SharedString = id.into();
    let button = Button::new(ElementId::from(id.clone()))
        .outline()
        .control(cx)
        .label(label)
        .dropdown_caret(true)
        .disabled(disabled)
        .map(|this| match min_width {
            Some(width) => this.min_w(width),
            None => this.w_full(),
        })
        .dropdown_menu_with_anchor(Anchor::TopRight, {
            let id = id.clone();
            move |menu, _, _| {
                let width = DROPDOWN_TRIGGER_WIDTHS
                    .with_borrow(|widths| widths.get(&id).copied())
                    .unwrap_or(px(0.));
                let menu = if width > px(0.) {
                    menu.min_w(width).max_w(width)
                } else {
                    menu
                };
                let current = current.clone();
                let set = set.clone();
                options.iter().fold(menu, |menu, (value, label)| {
                    let checked = *value == current;
                    let value = value.clone();
                    let set = set.clone();
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(checked)
                            .on_click(move |_, _, cx| set(value.clone(), cx)),
                    )
                })
            }
        });
    // 有最小宽度时 h_flex 不拉伸按钮、保留内容宽度；占满父容器时用块级 div，与原布局一致。
    match min_width {
        Some(_) => h_flex(),
        None => div().w_full(),
    }
    .on_children_prepainted(move |bounds, _, _| {
        if let Some(bounds) = bounds.first() {
            DROPDOWN_TRIGGER_WIDTHS.with_borrow_mut(|widths| {
                widths.insert(id.clone(), bounds.size.width);
            });
        }
    })
    .child(button)
    .into_any_element()
}

struct InputSlot {
    state: gpui::Entity<InputState>,
    /// 订阅回调读取的当前行 setter；每次 render 刷新，避免槽被别的行复用后写错键。
    set: Rc<RefCell<Setter<SharedString>>>,
    _subscriptions: Vec<gpui::Subscription>,
}

fn render_input(
    get: &Getter<SharedString>,
    set: &Setter<SharedString>,
    disabled: bool,
    key: &SharedString,
    width: Option<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let value = get(cx);
    let slot = window.use_keyed_state(
        ElementId::from(SharedString::from(format!("{key}-input"))),
        cx,
        {
            let value = value.clone();
            let set = set.clone();
            move |window, cx| {
                let state = cx.new(|cx| InputState::new(window, cx).default_value(value));
                let shared = Rc::new(RefCell::new(set));
                let subscription = cx.subscribe(&state, {
                    let shared = shared.clone();
                    move |_, state, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let set = shared.borrow().clone();
                            set(state.read(cx).value(), cx);
                        }
                    }
                });
                InputSlot {
                    state,
                    set: shared,
                    _subscriptions: vec![subscription],
                }
            }
        },
    );
    slot.update(cx, |slot, cx| {
        *slot.set.borrow_mut() = set.clone();
        // Text 类键 daemon 会 trim 归一化；按 trim 比较，避免逐字输入的空格被回写抹掉。
        if slot.state.read(cx).value().trim() != value.trim() {
            slot.state.update(cx, |state, cx| {
                state.set_value(value.clone(), window, cx);
            });
        }
    });
    let state = slot.read(cx).state.clone();
    Input::new(&state)
        .control(cx)
        .disabled(disabled)
        .map(|this| match width {
            Some(width) => this.w(width),
            None => this.w_full(),
        })
        .into_any_element()
}

/// 订阅回调读取的当前行参数；每次 render 刷新，避免槽被别的行复用后写错键或用错钳制范围。
#[derive(Clone)]
struct NumberParams {
    min: f64,
    max: f64,
    step: f64,
    set: Setter<f64>,
}

struct NumberSlot {
    state: gpui::Entity<InputState>,
    current: f64,
    params: Rc<RefCell<NumberParams>>,
    _subscriptions: Vec<gpui::Subscription>,
}

/// `width`：数字框宽度；`None` 占满父容器。
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_number(
    min: f64,
    max: f64,
    step: f64,
    get: &Getter<f64>,
    set: &Setter<f64>,
    disabled: bool,
    key: &SharedString,
    width: Option<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let value = get(cx);
    let slot = window.use_keyed_state(
        ElementId::from(SharedString::from(format!("{key}-number"))),
        cx,
        {
            let params = Rc::new(RefCell::new(NumberParams {
                min,
                max,
                step,
                set: set.clone(),
            }));
            move |window, cx| {
                let state =
                    cx.new(|cx| InputState::new(window, cx).default_value(format_number(value)));
                let subscriptions = vec![
                    cx.subscribe_in(&state, window, {
                        let params_step = params.clone();
                        move |slot: &mut NumberSlot, state, event: &NumberInputEvent, window, cx| {
                            let NumberInputEvent::Step(action) = event;
                            // 越界或不可解析的草稿（如删空后）从当前已提交值起步。
                            let current = state
                                .read(cx)
                                .value()
                                .parse::<f64>()
                                .ok()
                                .filter(|v| v.is_finite())
                                .unwrap_or(slot.current);
                            let NumberParams {
                                min,
                                max,
                                step,
                                set,
                            } = params_step.borrow().clone();
                            let next = if *action == StepAction::Increment {
                                current + step
                            } else {
                                current - step
                            }
                            .clamp(min, max);
                            state.update(cx, |state, cx| {
                                state.set_value(
                                    SharedString::from(format_number(next)),
                                    window,
                                    cx,
                                );
                            });
                            slot.current = next;
                            set(next, cx);
                        }
                    }),
                    cx.subscribe_in(&state, window, {
                        let params_change = params.clone();
                        move |slot: &mut NumberSlot, state, event: &InputEvent, window, cx| {
                            let text = state.read(cx).value();
                            let rewrite =
                                |value: f64,
                                 state: &gpui::Entity<InputState>,
                                 window: &mut Window,
                                 cx: &mut gpui::Context<NumberSlot>| {
                                    state.update(cx, |state, cx| {
                                        state.set_value(
                                            SharedString::from(format_number(value)),
                                            window,
                                            cx,
                                        );
                                    });
                                };
                            match event {
                                // 失焦 / 回车才收尾：越界钳到最小 / 最大值并提交，
                                // 不可解析的草稿（空串、`-`）回显当前值。
                                InputEvent::Blur | InputEvent::PressEnter { .. } => {
                                    let Some(parsed) =
                                        text.parse::<f64>().ok().filter(|v| v.is_finite())
                                    else {
                                        rewrite(slot.current, state, window, cx);
                                        return;
                                    };
                                    let NumberParams { min, max, set, .. } =
                                        params_change.borrow().clone();
                                    let clamped = parsed.clamp(min, max);
                                    if (clamped - slot.current).abs() >= f64::EPSILON {
                                        slot.current = clamped;
                                        set(clamped, cx);
                                    }
                                    if text.as_str() != format_number(clamped) {
                                        rewrite(clamped, state, window, cx);
                                    }
                                }
                                InputEvent::Change => {
                                    // 输入中只提交范围内的值；越界与不可解析的中间态（删空、
                                    // 输到一半的 `1`）不改写文本，留待失焦 / 回车。
                                    let Some(parsed) =
                                        text.parse::<f64>().ok().filter(|v| v.is_finite())
                                    else {
                                        return;
                                    };
                                    let NumberParams { min, max, set, .. } =
                                        params_change.borrow().clone();
                                    if (min..=max).contains(&parsed)
                                        && (parsed - slot.current).abs() >= f64::EPSILON
                                    {
                                        slot.current = parsed;
                                        set(parsed, cx);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }),
                ];
                NumberSlot {
                    state,
                    current: value,
                    params,
                    _subscriptions: subscriptions,
                }
            }
        },
    );
    slot.update(cx, |slot, cx| {
        *slot.params.borrow_mut() = NumberParams {
            min,
            max,
            step,
            set: set.clone(),
        };
        if (slot.current - value).abs() >= f64::EPSILON {
            slot.current = value;
            slot.state.update(cx, |state, cx| {
                state.set_value(SharedString::from(format_number(value)), window, cx);
            });
        }
    });
    let state = slot.read(cx).state.clone();
    NumberInput::new(&state)
        .control(cx)
        .disabled(disabled)
        .map(|this| match width {
            Some(width) => this.w(width),
            None => this.w_full(),
        })
        .into_any_element()
}

/// 数字输入的回显文本：整数不显示小数位（`3` 而非 `3.0`）；步进累加的浮点噪声
/// （`1.3 - 1` = `0.30000000000000004`）不外显，最多 9 位小数并去掉尾随 0。
fn format_number(value: f64) -> String {
    let text = format!("{value:.9}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// 一组设置：可选小节标题 + 一张卡片（行间发丝线）。
#[derive(Clone)]
pub(crate) struct SettingsSection {
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    rows: Vec<SettingsRow>,
    /// 收起的行：无搜索词时不渲染，但进搜索索引；有搜索词时与普通行一样参与筛选并显示。
    collapsed: Vec<SettingsRow>,
}

impl SettingsSection {
    pub(crate) fn new() -> Self {
        Self {
            title: None,
            subtitle: None,
            rows: Vec::new(),
            collapsed: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub(crate) fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    #[must_use]
    pub(crate) fn row(mut self, row: SettingsRow) -> Self {
        self.rows.push(row);
        self
    }

    /// 收起的行（见 [`Self::collapsed`] 字段）：分组折叠时用，搜索仍能找到并展示它们。
    #[must_use]
    pub(crate) fn collapsed_row(mut self, row: SettingsRow) -> Self {
        self.collapsed.push(row);
        self
    }

    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.collapsed.is_empty()
    }

    /// 小节标题（搜索索引用作面包屑与补充文本）。
    pub(crate) fn heading(&self) -> Option<&SharedString> {
        self.title.as_ref()
    }

    /// 参与搜索的全部行（含收起的行）。
    pub(crate) fn searchable_rows(&self) -> impl Iterator<Item = &SettingsRow> {
        self.rows.iter().chain(&self.collapsed)
    }

    fn filtered(&self, query: &str) -> Option<Self> {
        if query.is_empty() {
            return Some(self.clone());
        }
        let title_hit = self
            .title
            .as_ref()
            .is_some_and(|title| title.to_lowercase().contains(query))
            || self
                .subtitle
                .as_ref()
                .is_some_and(|text| text.to_lowercase().contains(query));
        let rows: Vec<SettingsRow> = self
            .searchable_rows()
            .filter(|row| title_hit || row.matches(query))
            .cloned()
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(Self {
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            rows,
            collapsed: Vec::new(),
        })
    }

    fn layout_weight(&self) -> f32 {
        let heading = if self.title.is_some() { 0.6 } else { 0. };
        heading
            + self
                .rows
                .iter()
                .map(SettingsRow::layout_weight)
                .sum::<f32>()
    }

    /// 本组横排字段的最宽档；无字段返回 `None`。
    fn field_tier(&self) -> Option<f32> {
        self.rows
            .iter()
            .filter_map(SettingsRow::field_tier)
            .reduce(f32::max)
    }

    /// 本组横排行的固定单位文字。
    fn unit_labels(&self) -> impl Iterator<Item = &SharedString> {
        self.rows.iter().filter_map(SettingsRow::unit_label)
    }

    /// `layout`：所在 Tab 的横排字段排版（见 [`SettingsTab`]）。
    pub(crate) fn render(
        &self,
        key: &str,
        index: usize,
        layout: FieldLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().colors;
        let mut card = card(cx).flex().flex_col().w_full().overflow_hidden();
        // 行 key 必须在行增减/搜索过滤后保持稳定：用小节标题 + 行标题（重名按出现序号区分），
        // 否则输入槽会按位置下标被别的设置项复用。
        let section_title: &str = self.title.as_deref().unwrap_or("");
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (row_index, row) in self.rows.iter().enumerate() {
            if row_index > 0 {
                card = card.child(
                    div()
                        .w_full()
                        .h(px(1.))
                        .pl(tokens.spacing.lg)
                        .child(div().size_full().bg(extended.hairline)),
                );
            }
            let occurrence = seen.entry(&*row.title).or_insert(0);
            let row_key = SharedString::from(format!(
                "{key}-{index}-{section_title}-{}#{occurrence}",
                row.title
            ));
            *occurrence += 1;
            card = card.child(row.render(row_key, layout, window, cx));
        }

        let heading = (self.title.is_some() || self.subtitle.is_some()).then(|| {
            v_flex()
                .px(tokens.spacing.xs)
                .pb(tokens.spacing.sm)
                .gap(tokens.spacing.xxs)
                .children(
                    self.title
                        .clone()
                        .map(|title| caption_heading(cx).child(title)),
                )
                .children(
                    self.subtitle
                        .clone()
                        .map(|subtitle| meta_text(cx).child(subtitle)),
                )
        });

        v_flex().w_full().children(heading).child(card)
    }
}

/// 一组分组共用的字段排版：给不经 [`SettingsTab`] 直接渲染分组的独立页用。
pub(crate) fn field_layout_of(
    sections: &[SettingsSection],
    window: &Window,
    cx: &App,
) -> FieldLayout {
    let tier = sections
        .iter()
        .filter_map(SettingsSection::field_tier)
        .fold(NUMBER_WIDTH, f32::max);
    FieldLayout::measure(
        tier,
        sections.iter().flat_map(SettingsSection::unit_labels),
        window,
        cx,
    )
}

/// 分类页内的一个子 Tab。Tab 内横排字段（数字 / 输入 / 下拉 / 自绘字段）统一取各分组
/// 字段宽度档的最大值，同屏字段同宽；不同 Tab 互不影响。
#[derive(Clone)]
pub(crate) struct SettingsTab {
    pub(crate) id: &'static str,
    pub(crate) label: SharedString,
    sections: Vec<SettingsSection>,
    /// 统一字段宽度档（100% 基准值）；按完整分组计算，搜索过滤后沿用，宽度不随命中行跳变。
    field_tier: f32,
    /// 完整分组里的固定单位文字（去重）；同理搜索过滤后沿用，单位列宽不随命中行跳变。
    units: Vec<SharedString>,
    /// 由 capability 注入的整页视图（账户 / 扩展）。
    view: Option<gpui::AnyView>,
    /// 注入视图占满内容区高度并自管滚动（宿主不再包滚动容器）。
    fill: bool,
}

impl SettingsTab {
    pub(crate) fn new(id: &'static str, label: impl Into<SharedString>) -> Self {
        Self {
            id,
            label: label.into(),
            sections: Vec::new(),
            field_tier: NUMBER_WIDTH,
            units: Vec::new(),
            view: None,
            fill: false,
        }
    }

    #[must_use]
    pub(crate) fn section(mut self, section: SettingsSection) -> Self {
        if !section.is_empty() {
            if let Some(tier) = section.field_tier() {
                self.field_tier = self.field_tier.max(tier);
            }
            for unit in section.unit_labels() {
                if !self.units.contains(unit) {
                    self.units.push(unit.clone());
                }
            }
            self.sections.push(section);
        }
        self
    }

    #[must_use]
    pub(crate) fn sections<I: IntoIterator<Item = SettingsSection>>(mut self, sections: I) -> Self {
        for section in sections {
            self = self.section(section);
        }
        self
    }

    #[must_use]
    pub(crate) fn view(mut self, view: gpui::AnyView) -> Self {
        self.view = Some(view);
        self
    }

    /// 本 Tab 的分组（注入整页视图的 Tab 为空）。
    pub(crate) fn section_list(&self) -> &[SettingsSection] {
        &self.sections
    }

    fn filtered(&self, query: &str) -> Option<Self> {
        if self.view.is_some() {
            return (query.is_empty() || self.label.to_lowercase().contains(query))
                .then(|| self.clone());
        }
        let sections: Vec<SettingsSection> = self
            .sections
            .iter()
            .filter_map(|section| section.filtered(query))
            .collect();
        if sections.is_empty() {
            return None;
        }
        Some(Self {
            id: self.id,
            label: self.label.clone(),
            sections,
            field_tier: self.field_tier,
            units: self.units.clone(),
            view: None,
            fill: false,
        })
    }

    fn render(&self, key: &str, width: f32, window: &mut Window, cx: &mut App) -> AnyElement {
        if let Some(view) = self.view.clone() {
            if self.fill {
                return v_flex()
                    .size_full()
                    .min_h_0()
                    .child(view)
                    .into_any_element();
            }
            return div().w_full().child(view).into_any_element();
        }
        // 单分组 Tab 里分组标题与 Tab 标签同名时省略标题，避免视觉重复。
        let mut sections = self.sections.clone();
        if let [section] = sections.as_mut_slice()
            && section.title.as_ref() == Some(&self.label)
        {
            section.title = None;
        }

        let spacing = active_theme(cx).tokens().spacing;
        let gap = spacing.lg;
        let layout = FieldLayout::measure(self.field_tier, &self.units, window, cx);
        // 列数由宿主测得的内容宽度决定：不能用 container_query（它把高度钉死成
        // 父级高度，内容无法撑开，滚动容器就永远没有可滚区）。
        if sections.len() < 2
            || width < f32::from(active_theme(cx).text_extent(TWO_COLUMN_MIN_WIDTH))
        {
            let mut column = v_flex().w_full().gap(gap);
            for (index, section) in sections.iter().enumerate() {
                column = column.child(section.render(key, index, layout, window, cx));
            }
            return column.into_any_element();
        }

        let weights: Vec<f32> = sections
            .iter()
            .map(SettingsSection::layout_weight)
            .collect();
        let total: f32 = weights.iter().sum();
        let mut accumulated = 0.;
        let mut split = 0usize;
        while split + 1 < sections.len() && accumulated + weights[split] / 2. < total / 2. {
            accumulated += weights[split];
            split += 1;
        }
        let split = split.max(1);
        let mut left = v_flex().flex_1().min_w_0().gap(gap);
        let mut right = v_flex().flex_1().min_w_0().gap(gap);
        for (index, section) in sections.iter().enumerate() {
            let child = section.render(key, index, layout, window, cx);
            if index < split {
                left = left.child(child);
            } else {
                right = right.child(child);
            }
        }
        div()
            .w_full()
            .flex()
            .items_start()
            .gap(spacing.xl)
            .child(left)
            .child(right)
            .into_any_element()
    }
}

/// 一个设置分类页。
pub(crate) struct SettingsPage {
    pub(crate) key: &'static str,
    pub(crate) title: SharedString,
    pub(crate) description: SharedString,
    icon: Icon,
    tabs: Vec<SettingsTab>,
}

impl SettingsPage {
    pub(crate) fn new(
        key: &'static str,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        icon: impl Into<Icon>,
    ) -> Self {
        Self {
            key,
            title: title.into(),
            description: description.into(),
            icon: icon.into(),
            tabs: Vec::new(),
        }
    }

    /// 无子 Tab 的单页分类：全部分组归入隐式默认 Tab。
    #[must_use]
    pub(crate) fn sections<I: IntoIterator<Item = SettingsSection>>(self, sections: I) -> Self {
        self.tab(SettingsTab::new("", SharedString::default()).sections(sections))
    }

    #[must_use]
    pub(crate) fn tab(mut self, tab: SettingsTab) -> Self {
        self.tabs.push(tab);
        self
    }

    /// 注入视图的 Tab 改为占满内容区高度、由视图自管滚动（固定头部 + 局部滚动）。
    #[must_use]
    pub(crate) fn fill_height(mut self) -> Self {
        for tab in &mut self.tabs {
            tab.fill = tab.view.is_some();
        }
        self
    }

    /// 指定子 Tab 是否占满内容区高度（未知 id 按首个 Tab）。
    pub(crate) fn tab_fills_height(&self, tab_id: &str) -> bool {
        self.tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .or_else(|| self.tabs.first())
            .is_some_and(|tab| tab.fill)
    }

    pub(crate) fn nav_icon(&self) -> Icon {
        self.icon.clone()
    }

    pub(crate) fn tabs(&self) -> &[SettingsTab] {
        &self.tabs
    }

    /// 按查询过滤；无命中返回 `None`（分类在搜索结果中隐藏）。
    pub(crate) fn filtered(self, query: &str) -> Option<Self> {
        if query.is_empty() {
            return Some(self);
        }
        let tabs: Vec<SettingsTab> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.filtered(query))
            .collect();
        if tabs.is_empty() {
            return None;
        }
        Some(Self { tabs, ..self })
    }

    /// 可见子 Tab（单页分类返回空切片）。
    pub(crate) fn visible_tabs(&self) -> &[SettingsTab] {
        if self.tabs.len() < 2 { &[] } else { &self.tabs }
    }

    /// 渲染指定子 Tab 的内容；未知 id 回退首个 Tab。
    pub(crate) fn render_tab(
        &self,
        tab_id: &str,
        width: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let tab = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .or_else(|| self.tabs.first());
        match tab {
            Some(tab) => tab.render(&format!("{}-{}", self.key, tab.id), width, window, cx),
            None => div().into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Control, DROPDOWN_MIN_WIDTH, INPUT_WIDTH, NUMBER_WIDTH, SettingsRow, SettingsSection,
        SettingsTab, format_number,
    };
    use gpui::SharedString;

    fn number(title: &str) -> SettingsRow {
        SettingsRow::new(
            title,
            Control::number(0.0, 65535.0, 1.0, |_| 0.0, |_, _| {}),
        )
    }

    fn dropdown(title: &str) -> SettingsRow {
        SettingsRow::new(
            title,
            Control::dropdown(Vec::new(), |_| SharedString::default(), |_, _| {}),
        )
    }

    fn input(title: &str) -> SettingsRow {
        SettingsRow::new(
            title,
            Control::input(|_| SharedString::default(), |_, _| {}),
        )
    }

    #[test]
    fn tab_fields_share_widest_tier_per_tab() {
        let ports_and_select = SettingsTab::new("basic", "Basic")
            .section(SettingsSection::new().row(number("Port")))
            .section(SettingsSection::new().row(dropdown("Mode")));
        assert_eq!(ports_and_select.field_tier, DROPDOWN_MIN_WIDTH);

        let numbers_only = SettingsTab::new("seed", "Seed")
            .section(SettingsSection::new().row(number("A")).row(number("B")));
        assert_eq!(numbers_only.field_tier, NUMBER_WIDTH);

        // 竖排行的字段占满行宽，不把同 Tab 的横排字段撑宽。
        let vertical_input = SettingsTab::new("ui", "UI").section(
            SettingsSection::new()
                .row(dropdown("Mode"))
                .row(input("Command").vertical()),
        );
        assert_eq!(vertical_input.field_tier, DROPDOWN_MIN_WIDTH);
    }

    #[test]
    fn search_filter_keeps_tab_field_width() {
        let tab = SettingsTab::new("proxy", "Proxy").section(
            SettingsSection::new()
                .row(dropdown("Type"))
                .row(input("Host")),
        );
        let filtered = tab.filtered("type").expect("dropdown row should match");
        assert_eq!(filtered.sections.len(), 1);
        assert_eq!(filtered.field_tier, INPUT_WIDTH);
    }

    #[test]
    fn collapsed_rows_only_show_up_in_search() {
        let section = SettingsSection::new()
            .row(dropdown("Server"))
            .collapsed_row(input("Host"))
            .collapsed_row(input("Port"));
        let titles = |section: &SettingsSection| -> Vec<String> {
            section
                .rows
                .iter()
                .map(|row| row.title.to_string())
                .collect()
        };
        let idle = section.filtered("").expect("section stays visible");
        assert_eq!(titles(&idle), ["Server"]);
        let hit = section
            .filtered("host")
            .expect("collapsed row is searchable");
        assert_eq!(titles(&hit), ["Host"]);
        assert_eq!(section.searchable_rows().count(), 3);
    }

    #[test]
    fn number_text_hides_float_step_noise() {
        assert_eq!(format_number(1.3 - 1.0), "0.3");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1.3 + 1.0), "2.3");
        assert_eq!(format_number(1024.0), "1024");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(-0.0000000001), "0");
    }
}
