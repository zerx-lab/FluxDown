//! 下载页的顶栏内容：页面插槽 [`DownloadTitleBar`]（「下载任务」主按钮，外观可在布局编辑模式下切换），
//! 以及作为 chrome 布局条目摆放的搜索框 [`DownloadSearchBox`] 与「视图」选项菜单
//! [`DownloadViewMenu`]（可在顶栏内移动、可隐藏，见 [`DownloadPageWidget`]）。
//!
//! 三者都是独立实体（shell 在标题栏里渲染它们，不在 [`DownloadView`] 的元素树内），
//! 通过弱引用把操作转发给下载页；视图偏好的改写沿用 `mutate_prefs` 的重算与防抖持久化。

use std::rc::Rc;

use fluxdown_ui_components::{CheckState, ControlExt as _, FluxIcon, check_mark, segmented_tabs};
use fluxdown_ui_i18n::{Translator, keys};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, FocusHandle,
    Focusable as _, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled,
    WeakEntity, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Icon, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    popover::Popover,
    scroll::ScrollableElement as _,
    table::TableState,
    v_flex,
};

use crate::{
    components::task_table::{DownloadTableDelegate, DraggedColumnMenuItem},
    controller::DownloadsCommand,
    model::{
        new_task_button::{NEW_TASK_BUTTON_PREF, NewTaskButtonShape, NewTaskButtonStyle},
        view_prefs::{
            DetailPlacement, HeaderAlign, SortDir, ViewDensity, ViewGroupBy, ViewPrefs, ViewSortKey,
        },
    },
    pages::downloads::DownloadView,
    strings::SEARCH_SHORTCUT_HINT,
};

/// 搜索框宽度（100% 下的 px，经 `text_extent` 随文字放大；窄窗口下可收缩到 [`SEARCH_MIN_WIDTH`]）。
const SEARCH_WIDTH: f32 = 280.;
const SEARCH_MIN_WIDTH: f32 = 160.;
/// 视图菜单宽度（100% 下的 px，容纳四个分段标签，经 `text_extent` 放大）。
const VIEW_MENU_WIDTH: f32 = 320.;
/// 视图菜单滚动区最多容纳的行数（以 `nav_row` 计）：列页 10 列 + 提示 + 分隔 + 重置约 12 行，
/// 留余量后取 15（默认缩放下即 420px）。按行高而非固定 px 封顶，字体 / 界面缩放抬高行高时
/// 上限随之放大，底部「重置为默认列」等行不会被裁到滚动区外。
const VIEW_MENU_MAX_BODY_ROWS: f32 = 15.;
/// 弹层打开在顶栏下方：可见高度需扣除顶栏、标签头与上下留白（标签头与留白约 3 行高）。
const VIEW_MENU_WINDOW_RESERVE_ROWS: f32 = 3.;

/// 视图菜单的分页（分段标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ViewMenuPage {
    #[default]
    Columns,
    Group,
    Sort,
    Display,
}

impl ViewMenuPage {
    const ALL: [Self; 4] = [Self::Columns, Self::Group, Self::Sort, Self::Display];

    fn label_key(self) -> &'static str {
        match self {
            Self::Columns => "viewSectionColumns",
            Self::Group => "viewSectionGroupBy",
            Self::Sort => "viewSectionSort",
            Self::Display => "viewSectionDisplay",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|page| *page == self)
            .unwrap_or_default()
    }
}

const GROUP_OPTIONS: [ViewGroupBy; 7] = [
    ViewGroupBy::None,
    ViewGroupBy::Status,
    ViewGroupBy::Date,
    ViewGroupBy::Type,
    ViewGroupBy::Queue,
    ViewGroupBy::Site,
    ViewGroupBy::Directory,
];

const SORT_OPTIONS: [ViewSortKey; 7] = [
    ViewSortKey::Smart,
    ViewSortKey::Created,
    ViewSortKey::Name,
    ViewSortKey::Size,
    ViewSortKey::Progress,
    ViewSortKey::Speed,
    ViewSortKey::Status,
];

fn group_by_key(group_by: ViewGroupBy) -> &'static str {
    match group_by {
        ViewGroupBy::None => "viewGroupNone",
        ViewGroupBy::Status => "viewGroupStatus",
        ViewGroupBy::Date => "viewGroupDate",
        ViewGroupBy::Type => "viewGroupType",
        ViewGroupBy::Queue => "viewGroupQueue",
        ViewGroupBy::Site => "viewGroupSite",
        ViewGroupBy::Directory => "viewGroupDirectory",
    }
}

fn sort_key_key(sort_key: ViewSortKey) -> &'static str {
    match sort_key {
        ViewSortKey::Smart => "viewSortSmart",
        ViewSortKey::Created => "viewSortCreated",
        ViewSortKey::Name => "viewSortName",
        ViewSortKey::Size => "viewSortSize",
        ViewSortKey::Progress => "viewSortProgress",
        ViewSortKey::Speed => "viewSortSpeed",
        ViewSortKey::Status => "viewSortStatus",
    }
}

/// 顶栏内可交互元素的包裹：拦截左键按下冒泡，避免触发标题栏的窗口拖拽 / 双击缩放。
/// 包裹本身是 flex 容器：调用方放开收缩时，子元素的 `flex_shrink` / `min_w` 才能生效。
fn interactive(child: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_none()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(child)
}

/// 下载页专属的 chrome 布局条目：只在下载页活跃时显示，只能放在顶栏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadPageWidget {
    Search,
    ViewMenu,
}

impl DownloadPageWidget {
    /// 默认顺序（顶栏右侧自左向右）。
    pub const ALL: [Self; 2] = [Self::Search, Self::ViewMenu];

    /// chrome 布局中的稳定 id。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::ViewMenu => "viewMenu",
        }
    }

    /// 编辑布局时的占位 / 拖拽预览文案键。
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Search => "chromeWidgetSearch",
            Self::ViewMenu => "viewMenuLabel",
        }
    }
}

/// 下载页的顶栏插槽：「下载任务」主按钮，外观见 [`NewTaskButtonStyle`]。
/// 布局编辑模式下点击按钮弹出外观菜单，而不是打开新建下载。
pub struct DownloadTitleBar {
    view: WeakEntity<DownloadView>,
    translator: Entity<Translator>,
    style: NewTaskButtonStyle,
    editing: bool,
}

impl DownloadTitleBar {
    pub(crate) fn new(
        view: WeakEntity<DownloadView>,
        translator: Entity<Translator>,
        style: NewTaskButtonStyle,
    ) -> Self {
        Self {
            view,
            translator,
            style,
            editing: false,
        }
    }

    fn set_style(&mut self, style: NewTaskButtonStyle, cx: &mut Context<Self>) {
        if self.style != style {
            self.style = style;
            cx.notify();
        }
    }

    /// 跟随 shell 的布局编辑模式（宿主转发 `ChromeEditingChanged`）。
    pub fn set_editing(&mut self, editing: bool, cx: &mut Context<Self>) {
        if self.editing != editing {
            self.editing = editing;
            cx.notify();
        }
    }

    fn text(&self, key: &str, cx: &App) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }

    /// 编辑模式的外观菜单：形态单选 + 主色填充开关。
    fn style_menu(
        &self,
        cx: &App,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = self.view.clone();
        let style = self.style;
        let title = self.text("newTaskButtonStyle", cx);
        let shapes = [
            (NewTaskButtonShape::Icon, self.text("newTaskButtonIcon", cx)),
            (
                NewTaskButtonShape::Label,
                self.text("newTaskButtonText", cx),
            ),
        ];
        let accent_label = self.text("newTaskButtonAccent", cx);
        move |menu, _, _| {
            let menu = shapes
                .iter()
                .fold(menu.label(title.clone()), |menu, (shape, label)| {
                    let view = view.clone();
                    let next = NewTaskButtonStyle {
                        shape: *shape,
                        ..style
                    };
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(style.shape == *shape)
                            .on_click(move |_, _, cx| apply_new_task_button_style(&view, next, cx)),
                    )
                });
            let view = view.clone();
            let next = NewTaskButtonStyle {
                accent: !style.accent,
                ..style
            };
            menu.separator().item(
                PopupMenuItem::new(accent_label.clone())
                    .checked(style.accent)
                    .on_click(move |_, _, cx| apply_new_task_button_style(&view, next, cx)),
            )
        }
    }
}

fn apply_new_task_button_style(
    view: &WeakEntity<DownloadView>,
    style: NewTaskButtonStyle,
    cx: &mut App,
) {
    let Ok(()) = view.update(cx, |view, cx| view.set_new_task_button_style(style, cx)) else {
        // 下载页已释放，无需保存外观。
        return;
    };
}

impl Render for DownloadTitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        // 顶栏主按钮比统一控件矮一档：主色填充时整块底色可见，与 40px 顶栏留出上下呼吸。
        let height = theme.density().control - tokens.spacing.xs;
        let radius = tokens.radius.md;
        let label = self.text("downloadTask", cx);
        let icon = Icon::new(FluxIcon::Plus).size(theme.extended().icon.md);
        let button = Button::new("download-create")
            .icon(icon)
            .map(|button| {
                if self.style.accent {
                    button.primary()
                } else {
                    button.ghost()
                }
            })
            .rounded(radius);
        let button = match self.style.shape {
            NewTaskButtonShape::Icon => Styled::size(button.with_size(Size::Medium), height),
            NewTaskButtonShape::Label => button.label(label.clone()).control(cx).h(height),
        };
        let tooltip = match (self.editing, self.style.shape) {
            (true, _) => Some(self.text("newTaskButtonStyleHint", cx)),
            (false, NewTaskButtonShape::Icon) => Some(label),
            (false, NewTaskButtonShape::Label) => None,
        };
        let button = match tooltip {
            Some(tooltip) => button.tooltip(tooltip),
            None => button,
        };
        let content = if self.editing {
            div()
                .flex()
                .p(tokens.spacing.xxs)
                .rounded(tokens.radius.sm)
                .border_1()
                .border_dashed()
                .border_color(tokens.colors.primary)
                .child(button.dropdown_menu_with_anchor(Anchor::TopLeft, self.style_menu(cx)))
                .into_any_element()
        } else {
            let view = self.view.clone();
            button
                .on_click(move |_, window, cx| {
                    let Ok(()) = view.update(cx, |view, cx| view.open_new_download(window, cx))
                    else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                })
                .into_any_element()
        };
        h_flex()
            .h_full()
            .flex_none()
            .items_center()
            .child(interactive(content))
    }
}

impl DownloadView {
    /// 偏好 → 顶栏主按钮外观；只在持久化值变化时下发，自己写出的值回流不再覆盖。
    pub(crate) fn sync_new_task_button(&mut self, cx: &mut Context<Self>) {
        let value = self.controller.preference(NEW_TASK_BUTTON_PREF);
        if self.applied_new_task_button.as_ref() == value {
            return;
        }
        self.applied_new_task_button = value.cloned();
        let style = NewTaskButtonStyle::from_value(value);
        self.push_new_task_button_style(style, cx);
    }

    /// 编辑模式里选中新外观：立即生效并写入设备本地偏好；写入失败时回到已保存的外观。
    fn set_new_task_button_style(&mut self, style: NewTaskButtonStyle, cx: &mut Context<Self>) {
        let value = style.to_value();
        self.applied_new_task_button = Some(value.clone());
        self.push_new_task_button_style(style, cx);
        let future = self
            .controller
            .execute(DownloadsCommand::SetLocalPreference {
                key: NEW_TASK_BUTTON_PREF,
                value,
            });
        cx.spawn(async move |this, cx| {
            if future.await.is_ok() {
                return;
            }
            let Ok(()) = this.update(cx, |this, cx| {
                let stored = this.controller.preference(NEW_TASK_BUTTON_PREF);
                this.applied_new_task_button = stored.cloned();
                let stored = NewTaskButtonStyle::from_value(stored);
                this.push_new_task_button_style(stored, cx);
                this.last_error = Some(this.strings.action_failed.clone());
                cx.notify();
            }) else {
                // 下载页已释放，无需回滚外观。
                return;
            };
        })
        .detach();
    }

    fn push_new_task_button_style(&self, style: NewTaskButtonStyle, cx: &mut Context<Self>) {
        let Some(title_bar) = self.title_bar.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        title_bar.update(cx, |title_bar, cx| title_bar.set_style(style, cx));
    }
}

/// 顶栏搜索框（chrome 布局条目 `search`）。
pub struct DownloadSearchBox {
    view: WeakEntity<DownloadView>,
    search_input: Entity<InputState>,
    /// 搜索框外壳的焦点句柄：输入框（后代）聚焦时外壳切换为聚焦外观。
    search_focus: FocusHandle,
}

impl DownloadSearchBox {
    pub(crate) fn new(
        view: WeakEntity<DownloadView>,
        search_input: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            view,
            search_input,
            search_focus: cx.focus_handle(),
        }
    }
}

impl Render for DownloadSearchBox {
    /// chrome 上的浅色填充胶囊，无描边；聚焦时变为内容底色 + 强调色描边。
    /// 未输入时右侧显示快捷键提示徽标。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let colors = tokens.colors;
        let chrome = extended.colors;
        let caption = extended.caption;
        let input_focus = self.search_input.read(cx).focus_handle(cx);
        let empty = self.search_input.read(cx).value().is_empty();
        let search = h_flex()
            .id("download-search-box")
            .track_focus(&self.search_focus)
            .w(theme.text_extent(SEARCH_WIDTH))
            .min_w(theme.text_extent(SEARCH_MIN_WIDTH))
            .flex_shrink(1.)
            .h(theme.density().control)
            .pl(tokens.spacing.sm)
            .pr(tokens.spacing.xs)
            .gap(tokens.spacing.xs)
            .items_center()
            .rounded(tokens.radius.md)
            .border(extended.stroke.thin)
            .map(|this| {
                // 聚焦外观在渲染时判定（焦点变化会刷新窗口）；悬停样式在 gpui 里晚于
                // 焦点样式应用，所以只在未聚焦时挂悬停，避免聚焦态被悬停盖成灰底。
                if self.search_focus.contains_focused(window, cx) {
                    this.bg(colors.surface)
                        .border_color(colors.primary.opacity(0.6))
                } else {
                    this.bg(chrome.nav_hover)
                        .border_color(colors.primary.opacity(0.))
                        .hover(move |style| style.bg(chrome.nav_selected))
                }
            })
            .cursor_text()
            .on_click(move |_, window, cx| window.focus(&input_focus, cx))
            // 原下载页 `escape` 语义：清空查询并把焦点交回下载页。
            .on_action(
                cx.listener(|this, action: &gpui_component::input::Escape, window, cx| {
                    let Ok(()) = this.view.update(cx, |view, cx| {
                        view.on_search_escape(action, window, cx);
                    }) else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                }),
            )
            .child(
                Icon::new(FluxIcon::Search)
                    .size(extended.icon.md)
                    .text_color(colors.muted_foreground),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.search_input)
                        .appearance(false)
                        .with_size(Size::Small)
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .w_full()
                        .cleanable(true),
                ),
            )
            .when(empty, |this| {
                this.child(
                    div()
                        .flex_none()
                        .px(tokens.spacing.xs)
                        .rounded(tokens.radius.sm)
                        .border(extended.stroke.thin)
                        .border_color(chrome.hairline)
                        .bg(colors.surface)
                        .text_size(caption.size)
                        .line_height(caption.line_height)
                        .text_color(chrome.text_tertiary)
                        .child(SEARCH_SHORTCUT_HINT),
                )
            });
        interactive(search)
            .flex_shrink(1.)
            .min_w(theme.text_extent(SEARCH_MIN_WIDTH))
    }
}

/// 「视图」选项菜单（chrome 布局条目 `viewMenu`）。
pub struct DownloadViewMenu {
    view: WeakEntity<DownloadView>,
    translator: Entity<Translator>,
    table_state: Entity<TableState<DownloadTableDelegate>>,
    /// 「视图」弹层当前分页。
    view_menu_page: ViewMenuPage,
    /// 弹层相对按钮的锚点：随所在顶栏区域变化（左侧区域向右展开，右侧区域向左展开）。
    anchor: Anchor,
}

impl DownloadViewMenu {
    pub(crate) fn new(
        view: &Entity<DownloadView>,
        translator: Entity<Translator>,
        table_state: Entity<TableState<DownloadTableDelegate>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 视图偏好变化（列 / 分组 / 排序 / 显示）后刷新已打开的弹层。
        cx.observe(view, |_, _, cx| cx.notify()).detach();
        Self {
            view: view.downgrade(),
            translator,
            table_state,
            view_menu_page: ViewMenuPage::default(),
            anchor: Anchor::TopRight,
        }
    }

    /// 按摆放位置设置弹层锚点；未变化时不写，避免渲染期无条件改写实体。
    pub fn set_anchor(&mut self, anchor: Anchor, cx: &mut Context<Self>) {
        if self.anchor != anchor {
            self.anchor = anchor;
            cx.notify();
        }
    }
}

impl Render for DownloadViewMenu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let translator = self.translator.read(cx);
        let trigger_label = SharedString::from(translator.text("viewMenuLabel").to_owned());
        let tooltip = SharedString::from(translator.text("viewOptionsTitle").to_owned());
        let view = self.view.clone();
        let translator = self.translator.clone();
        let table_state = self.table_state.clone();
        let view_menu = cx.entity().downgrade();

        interactive(
            Popover::new("download-view-popover")
                .anchor(self.anchor)
                .p_0()
                .w(active_theme(cx).text_extent(VIEW_MENU_WIDTH))
                .trigger(
                    Button::new("download-view")
                        .ghost()
                        .control(cx)
                        .icon(FluxIcon::SlidersHorizontal)
                        .label(trigger_label)
                        .tooltip(tooltip),
                )
                .content(move |_, window, cx| {
                    let page = view_menu
                        .upgrade()
                        .map(|view_menu| view_menu.read(cx).view_menu_page)
                        .unwrap_or_default();
                    let density = active_theme(cx).density();
                    let body_max_height = (density.nav_row * VIEW_MENU_MAX_BODY_ROWS)
                        .min(
                            window.viewport_size().height
                                - density.title_bar
                                - density.nav_row * VIEW_MENU_WINDOW_RESERVE_ROWS,
                        )
                        .max(density.nav_row * 3.);
                    view_menu_content(
                        ViewMenuContext {
                            view_menu: &view_menu,
                            view: &view,
                            translator: &translator,
                            table_state: &table_state,
                            page,
                            body_max_height,
                        },
                        cx,
                    )
                }),
        )
    }
}

/// 视图菜单共用的样式快照（颜色与尺寸均取自已缩放的 token）。
#[derive(Clone, Copy)]
struct MenuStyle {
    pad_x: Pixels,
    gap: Pixels,
    section_top: Pixels,
    section_bottom: Pixels,
    radius: Pixels,
    row_height: Pixels,
    stroke: Pixels,
    body_size: Pixels,
    body_line_height: Pixels,
    caption_size: Pixels,
    caption_line_height: Pixels,
    icon_sm: Pixels,
    icon_md: Pixels,
    foreground: Hsla,
    tertiary: Hsla,
    hover: Hsla,
    hairline: Hsla,
    accent: Hsla,
}

impl MenuStyle {
    fn from_app(cx: &App) -> Self {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        Self {
            pad_x: tokens.spacing.md,
            gap: tokens.spacing.sm,
            section_top: tokens.spacing.sm,
            section_bottom: tokens.spacing.xxs,
            radius: tokens.radius.sm,
            row_height: theme.density().nav_row,
            stroke: extended.stroke.thin,
            body_size: tokens.typography.sm.size,
            body_line_height: tokens.typography.sm.line_height,
            caption_size: extended.caption.size,
            caption_line_height: extended.caption.line_height,
            icon_sm: extended.icon.sm,
            icon_md: extended.icon.md,
            foreground: tokens.colors.foreground,
            tertiary: extended.colors.text_tertiary,
            hover: extended.colors.row_hover,
            hairline: extended.colors.hairline,
            accent: tokens.colors.accent,
        }
    }

    fn section_title(self, label: impl Into<SharedString>) -> Div {
        div()
            .px(self.pad_x)
            .pt(self.section_top)
            .pb(self.section_bottom)
            .text_size(self.caption_size)
            .line_height(self.caption_line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(self.tertiary)
            .child(label.into())
    }

    fn separator(self) -> Div {
        div()
            .h(self.stroke)
            .my(self.section_bottom)
            .bg(self.hairline)
    }

    /// 单选 / 开关行：左侧对勾标记当前值。
    fn option_row(
        self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        checked: bool,
        on_click: impl Fn(&mut App) + 'static,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .h(self.row_height)
            .mx(self.section_bottom)
            .px(self.pad_x - self.section_bottom)
            .gap(self.gap)
            .items_center()
            .rounded(self.radius)
            .cursor_pointer()
            .text_size(self.body_size)
            .line_height(self.body_line_height)
            .text_color(self.foreground)
            .hover(move |style| style.bg(self.hover))
            .child(div().flex_none().w(self.icon_md).when(checked, |this| {
                this.child(
                    Icon::new(FluxIcon::Check)
                        .size(self.icon_md)
                        .text_color(self.foreground),
                )
            }))
            .child(div().flex_1().min_w_0().truncate().child(label.into()))
            .on_click(move |_, _, cx| on_click(cx))
    }
}

/// 经下载页改写视图偏好（重算可见行 + 防抖持久化，与快捷键 `on_cycle_*` 同路径）。
fn update_prefs(
    view: &WeakEntity<DownloadView>,
    mutate: impl FnOnce(&mut ViewPrefs) + 'static,
    cx: &mut App,
) {
    let Ok(()) = view.update(cx, |view, cx| view.mutate_prefs(mutate, cx)) else {
        // 视图已释放，结束这次回调而不再更新状态。
        return;
    };
}

/// 视图菜单一次渲染所需的上下文。
struct ViewMenuContext<'a> {
    view_menu: &'a WeakEntity<DownloadViewMenu>,
    view: &'a WeakEntity<DownloadView>,
    translator: &'a Entity<Translator>,
    table_state: &'a Entity<TableState<DownloadTableDelegate>>,
    page: ViewMenuPage,
    /// 滚动区最大高度（已按窗口可见高度收紧）。
    body_max_height: Pixels,
}

/// 视图菜单：顶部分段标签切页（列 / 分组 / 排序 / 显示），下方为可纵向滚动的选项区；
/// 每页内容都不超过弹层最大高度，窗口过矮时滚动而非被裁切。
fn view_menu_content(menu: ViewMenuContext<'_>, cx: &App) -> impl IntoElement + use<> {
    let style = MenuStyle::from_app(cx);
    let spacing = active_theme(cx).tokens().spacing;
    let translator = menu.translator.read(cx);
    let tabs = {
        let view_menu = menu.view_menu.clone();
        segmented_tabs(
            "download-view-menu-tabs",
            ViewMenuPage::ALL
                .map(|page| SharedString::from(translator.text(page.label_key()).to_owned())),
            menu.page.index(),
            move |index, _, cx| {
                let Some(page) = ViewMenuPage::ALL.get(index).copied() else {
                    return;
                };

                let Ok(()) = view_menu.update(cx, |view_menu, cx| {
                    view_menu.view_menu_page = page;
                    cx.notify();
                }) else {
                    // 视图已释放，结束这次回调而不再更新状态。
                    return;
                };
            },
            cx,
        )
    };
    let body = match menu.page {
        ViewMenuPage::Columns => columns_page(&menu, style, cx),
        ViewMenuPage::Group => group_page(&menu, style, cx),
        ViewMenuPage::Sort => sort_page(&menu, style, cx),
        ViewMenuPage::Display => display_page(&menu, style, cx),
    };

    v_flex()
        .w_full()
        .child(
            div()
                .flex()
                .px(style.pad_x)
                .pt(style.pad_x)
                .pb(spacing.sm)
                .child(tabs),
        )
        .child(
            v_flex()
                .w_full()
                .max_h(menu.body_max_height)
                .pb(spacing.xs)
                .children(body)
                .overflow_y_scrollbar()
                .id(SharedString::from(format!(
                    "download-view-menu-{:?}",
                    menu.page
                ))),
        )
}

fn menu_text(menu: &ViewMenuContext<'_>, key: &str, cx: &App) -> SharedString {
    SharedString::from(menu.translator.read(cx).text(key).to_owned())
}

/// 列页：勾选显隐 + 拖动排序（手柄在右）+ 重置。
fn columns_page(menu: &ViewMenuContext<'_>, style: MenuStyle, cx: &App) -> Vec<AnyElement> {
    let table_state = menu.table_state;
    let delegate = table_state.read(cx).delegate();
    let columns = delegate.columns.clone();
    let visible_count = columns.iter().filter(|column| column.visible).count();
    let persist: Rc<dyn Fn(&mut App)> = {
        let view = menu.view.clone();
        Rc::new(move |cx: &mut App| {
            let Ok(()) = view.update(cx, |view, cx| view.schedule_persist_prefs(cx)) else {
                // 视图已释放，结束这次回调而不再更新状态。
                return;
            };
        })
    };
    let mut rows = columns
        .into_iter()
        .map(|column| {
            let kind = column.kind;
            let visible = column.visible;
            let label = kind.label(&delegate.strings);
            let drag = DraggedColumnMenuItem {
                kind,
                label: label.clone(),
            };
            let table_for_checkbox = table_state.clone();
            let table_for_drop = table_state.clone();
            let persist_checkbox = Rc::clone(&persist);
            let persist_drop = Rc::clone(&persist);
            h_flex()
                .id(format!("download-column-menu-item-{}", kind.key()))
                .h(style.row_height)
                .mx(style.section_bottom)
                .px(style.pad_x - style.section_bottom)
                .gap(style.gap)
                .items_center()
                .rounded(style.radius)
                .cursor_pointer()
                .text_size(style.body_size)
                .line_height(style.body_line_height)
                .text_color(style.foreground)
                .hover(move |this| this.bg(style.hover))
                .drag_over::<DraggedColumnMenuItem>(move |this, _, _, _| this.bg(style.accent))
                .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                .on_drop(move |drag: &DraggedColumnMenuItem, _, cx| {
                    table_for_drop.update(cx, |table, cx| {
                        table.delegate_mut().move_column_kind(drag.kind, kind);
                        table.refresh(cx);
                        cx.notify();
                    });
                    persist_drop(cx);
                })
                // 整行可点切换显隐（比小复选框更好点中）；拖动行不会产生 click。
                .on_click(move |_, _, cx| {
                    let changed = table_for_checkbox.update(cx, |table, cx| {
                        let changed = table.delegate_mut().set_column_visible(kind, !visible);
                        if changed {
                            table.refresh(cx);
                            cx.notify();
                        }
                        changed
                    });
                    if changed {
                        persist_checkbox(cx);
                    }
                })
                .child(check_mark(
                    if visible {
                        CheckState::Checked
                    } else {
                        CheckState::Unchecked
                    },
                    cx,
                ))
                .child(div().min_w_0().flex_1().truncate().child(label))
                .child(
                    Icon::new(FluxIcon::GripVertical)
                        .size(style.icon_sm)
                        .text_color(style.tertiary),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    if visible_count == 1 {
        rows.push(
            div()
                .px(style.pad_x)
                .py(style.section_bottom)
                .text_size(style.caption_size)
                .line_height(style.caption_line_height)
                .text_color(style.tertiary)
                .child(menu_text(menu, keys::VIEW_COLUMNS_AT_LEAST_ONE, cx))
                .into_any_element(),
        );
    }
    rows.push(style.separator().into_any_element());
    let table_state = table_state.clone();
    rows.push(
        style
            .option_row(
                "download-columns-reset",
                menu_text(menu, keys::VIEW_COLUMNS_RESET_ACTION, cx),
                false,
                move |cx| {
                    table_state.update(cx, |table, cx| {
                        table.delegate_mut().reset_columns();
                        table.refresh(cx);
                        cx.notify();
                    });
                    persist(cx);
                },
            )
            .into_any_element(),
    );
    rows
}

/// 分组页：单选分组方式。
fn group_page(menu: &ViewMenuContext<'_>, style: MenuStyle, cx: &App) -> Vec<AnyElement> {
    let current = menu.table_state.read(cx).delegate().prefs().group_by;
    GROUP_OPTIONS
        .into_iter()
        .map(|group_by| {
            let view = menu.view.clone();
            style
                .option_row(
                    SharedString::from(format!("download-view-group-{group_by:?}")),
                    menu_text(menu, group_by_key(group_by), cx),
                    current == group_by,
                    move |cx| update_prefs(&view, move |prefs| prefs.group_by = group_by, cx),
                )
                .into_any_element()
        })
        .collect()
}

/// 排序页：排序键 + 方向。
fn sort_page(menu: &ViewMenuContext<'_>, style: MenuStyle, cx: &App) -> Vec<AnyElement> {
    let prefs = menu.table_state.read(cx).delegate().prefs();
    let (current_key, current_dir) = (prefs.sort_key, prefs.sort_dir);
    let mut rows = SORT_OPTIONS
        .into_iter()
        .map(|sort_key| {
            let view = menu.view.clone();
            style
                .option_row(
                    SharedString::from(format!("download-view-sort-{sort_key:?}")),
                    menu_text(menu, sort_key_key(sort_key), cx),
                    current_key == sort_key,
                    move |cx| update_prefs(&view, move |prefs| prefs.select_sort_key(sort_key), cx),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    rows.push(style.separator().into_any_element());
    for (sort_dir, key) in [
        (SortDir::Asc, "viewSortAscending"),
        (SortDir::Desc, "viewSortDescending"),
    ] {
        let view = menu.view.clone();
        rows.push(
            style
                .option_row(
                    SharedString::from(format!("download-view-sort-dir-{sort_dir:?}")),
                    menu_text(menu, key, cx),
                    current_dir == sort_dir,
                    move |cx| update_prefs(&view, move |prefs| prefs.sort_dir = sort_dir, cx),
                )
                .into_any_element(),
        );
    }
    rows
}

/// 显示页：密度 + 详情面板（开关 + 位置）。
fn display_page(menu: &ViewMenuContext<'_>, style: MenuStyle, cx: &App) -> Vec<AnyElement> {
    let prefs = menu.table_state.read(cx).delegate().prefs().clone();
    let mut rows = vec![
        style
            .section_title(menu_text(menu, "viewSectionDensity", cx))
            .into_any_element(),
    ];
    for (density, key) in [
        (ViewDensity::Compact, "viewDensityCompact"),
        (ViewDensity::Comfortable, "viewDensityComfortable"),
        (ViewDensity::Relaxed, "viewDensityRelaxed"),
    ] {
        let view = menu.view.clone();
        rows.push(
            style
                .option_row(
                    SharedString::from(format!("download-view-density-{density:?}")),
                    menu_text(menu, key, cx),
                    prefs.density == density,
                    move |cx| update_prefs(&view, move |prefs| prefs.density = density, cx),
                )
                .into_any_element(),
        );
    }
    rows.push(style.separator().into_any_element());
    rows.push(
        style
            .section_title(menu_text(menu, "viewSectionHeaderAlign", cx))
            .into_any_element(),
    );
    for (align, key) in [
        (HeaderAlign::Left, "viewHeaderAlignLeft"),
        (HeaderAlign::Center, "viewHeaderAlignCenter"),
        (HeaderAlign::Right, "viewHeaderAlignRight"),
    ] {
        let view = menu.view.clone();
        rows.push(
            style
                .option_row(
                    SharedString::from(format!("download-view-header-align-{align:?}")),
                    menu_text(menu, key, cx),
                    prefs.header_align == align,
                    move |cx| update_prefs(&view, move |prefs| prefs.header_align = align, cx),
                )
                .into_any_element(),
        );
    }
    rows.push(style.separator().into_any_element());
    rows.push(
        style
            .section_title(menu_text(menu, "viewSectionDetailPanel", cx))
            .into_any_element(),
    );
    let view = menu.view.clone();
    rows.push(
        style
            .option_row(
                "download-view-detail-open",
                menu_text(menu, "viewDetailShow", cx),
                prefs.detail_open,
                move |cx| update_prefs(&view, |prefs| prefs.detail_open = !prefs.detail_open, cx),
            )
            .into_any_element(),
    );
    for (placement, key) in [
        (DetailPlacement::Bottom, "viewDetailBottom"),
        (DetailPlacement::Right, "viewDetailRight"),
    ] {
        let view = menu.view.clone();
        rows.push(
            style
                .option_row(
                    SharedString::from(format!("download-view-detail-{placement:?}")),
                    menu_text(menu, key, cx),
                    prefs.detail_placement == placement,
                    move |cx| {
                        update_prefs(&view, move |prefs| prefs.detail_placement = placement, cx)
                    },
                )
                .into_any_element(),
        );
    }
    rows
}
