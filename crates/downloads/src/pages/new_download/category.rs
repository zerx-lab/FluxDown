//! 保存目录的分类下拉：按识别出的文件名预测分类目录并让目录框自动跟随，
//! 同行左侧的下拉可切换「自动 / 不分类 / 指定分类」。纯规则见 [`crate::model::save_dir_category`]。
//!
//! 状态机（默认 `Auto`）：
//! - `Auto`：预测输入（重命名框 / 第一条链接已识别名 / 第一条 URL）变化时，目录框未被手改
//!   （空、等于表单上次写入值，尚未写过时等于初始目录）才改写；用户手动改过的目录（包括改回
//!   初始目录或改成另一个分类目录）既不再覆盖，提交也不路由，以用户选择为准；
//! - `NoCategory`：目录框 = 初始目录，提交不路由；
//! - `Category(id)`：目录框 = 该分类目录，提交不路由（用户显式选择优先）。
//!
//! 表单当前对应的分类（显式所选 / 自动命中）下用户手动选过的目录记入分类目录记忆，
//! 目录下拉优先列出。渲染只读 entity 里的状态（预测结果在输入变化时写入 `category_predicted`）。

use fluxdown_ui_components::{ControlExt as _, FluxIcon, category_icon};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled,
    Window, div, px,
};
use gpui_component::{
    Icon,
    button::Button,
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};

use super::NewDownloadView;
use crate::model::save_dir_category::{
    CategoryMode, auto_follow_target, category_mode_save_dir, category_options,
    category_route_eligible, matched_category, predict_save_dir, restore_category_mode,
    save_dir_untouched,
};

/// 分类下拉触发器定宽（图标 + 名称 + chevron）。
const CATEGORY_PICKER_WIDTH: f32 = 150.;
/// 分类菜单最小宽度（容纳「名称 + 目录」两行）。
const CATEGORY_MENU_MIN_WIDTH: f32 = 260.;
/// 分类菜单最大宽度：超长目录（深层路径、含空格 / 非 ASCII 的 Windows 路径）在中间省略，
/// 保留盘符 / 根与末尾目录名，菜单不随路径无限变宽。
const CATEGORY_MENU_MAX_WIDTH: f32 = 420.;
/// 分类菜单最大高度（分类多时滚动）。
const CATEGORY_MENU_MAX_HEIGHT: f32 = 420.;

/// 菜单里的一项。
struct MenuEntry {
    mode: CategoryMode,
    icon: FluxIcon,
    title: SharedString,
    description: Option<SharedString>,
}

impl NewDownloadView {
    /// 本机始终显示，空分类列表仍可切换「自动 / 不分类」。
    fn category_picker_visible(&self) -> bool {
        !self.is_remote()
    }

    /// 快捷添加的网址规则在本表单内生效：追加进分类列表并重算路由 / 候选，模式回「自动」
    /// 让网址优先预测立即命中。偏好写回由对话框自己发起（最新列表 + 新条目整体写回），
    /// 这里只管本地反馈。
    pub(super) fn apply_url_rule(
        &mut self,
        entry: fluxdown_protocol::CustomCategoryDto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .context
            .custom_categories
            .iter()
            .any(|known| known.id == entry.id)
        {
            self.context.custom_categories.push(entry);
        }
        let categories = self.context.custom_categories.clone();
        self.context.category_route =
            fluxdown_protocol::CategoryRouteDto::from_categories(&categories);
        self.context.category_options = category_options(&categories);
        self.category_mode = CategoryMode::Auto;
        self.category_mode_touched = true;
        self.sync_category(window, cx);
        cx.notify();
    }

    /// 宿主随偏好快照推送的最新 `custom_categories`：整体替换原始列表并重算路由 / 候选，
    /// 让快捷添加网址规则永远基于最新列表写回（设置页 / 其他设备的并发修改不被旧快照覆盖）。
    /// 当前手选的分类若已不在候选里则回到「自动」。
    pub fn set_custom_categories(
        &mut self,
        categories: Vec<fluxdown_protocol::CustomCategoryDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if categories == self.context.custom_categories {
            return;
        }
        self.context.category_route =
            fluxdown_protocol::CategoryRouteDto::from_categories(&categories);
        self.context.category_options = category_options(&categories);
        self.context.custom_categories = categories;
        if !self.category_mode_touched
            && !self.category_dir_manually_edited
            && self.save_dir_untouched(cx)
        {
            let mode = restore_category_mode(
                self.context.initial_category_mode.as_deref(),
                &self.context.category_options,
            );
            self.set_category_mode(mode, window, cx);
            self.category_mode_touched = false;
        } else if let CategoryMode::Category(id) = &self.category_mode
            && !self
                .context
                .category_options
                .iter()
                .any(|option| option.id == *id)
        {
            self.category_mode = CategoryMode::Auto;
        }
        self.sync_category(window, cx);
        cx.notify();
    }

    /// 按当前输入预测分类目录：文件名取重命名框（批量时隐藏，不取），否则第一条链接已识别的
    /// 文件名（`out=` / 捕获名），否则空串（交给 URL 末段回退）；URL 取第一条。
    fn predict_category_dir(&self, cx: &App) -> Option<String> {
        let route = self.context.category_route.as_ref()?;
        let first = self.entries.first();
        let rename = if self.is_batch() {
            String::new()
        } else {
            self.rename.read(cx).value().trim().to_owned()
        };
        let name = if rename.is_empty() {
            first.map_or("", |entry| entry.file_name.as_str())
        } else {
            rename.as_str()
        };
        let url = first.map_or("", |entry| entry.url.as_str());
        predict_save_dir(route, name, url).map(str::to_owned)
    }

    fn set_save_dir_text(&self, dir: &str, window: &mut Window, cx: &mut Context<Self>) {
        if *self.save_dir.read(cx).value() != *dir {
            let value = dir.to_owned();
            self.save_dir
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
    }

    /// 目录框是否仍是表单写入的值（用户没手动改过），见 [`dir_untouched`]。
    pub(super) fn save_dir_untouched(&self, cx: &App) -> bool {
        save_dir_untouched(
            &self.save_dir.read(cx).value(),
            &self.context.save_dir,
            self.category_written_dir.as_deref(),
            self.category_dir_manually_edited,
        )
    }

    /// 提交是否挂分类路由：自动模式且目录框未被手改。
    pub(super) fn routes_by_category(&self, cx: &App) -> bool {
        category_route_eligible(&self.category_mode, self.save_dir_untouched(cx))
    }

    /// 分类目录记忆的键：显式所选分类，或自动模式命中（触发器显示）的分类；不分类 / 无命中 /
    /// 远端目标为 `None`。
    pub(super) fn memory_category(&self) -> Option<&str> {
        if !self.category_picker_visible() {
            return None;
        }
        match &self.category_mode {
            CategoryMode::Auto => matched_category(
                &self.context.category_options,
                self.category_predicted.as_deref(),
            )
            .map(|option| option.id.as_str()),
            CategoryMode::NoCategory => None,
            CategoryMode::Category(id) => Some(id.as_str()),
        }
    }

    /// 预测输入变化后更新预测结果；自动模式仅在目录未被用户手动指定时跟随。
    pub(super) fn sync_category(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let predicted = self.predict_category_dir(cx);
        if self.category_predicted != predicted {
            self.category_predicted.clone_from(&predicted);
            cx.notify();
        }
        if self.category_mode != CategoryMode::Auto || self.category_dir_manually_edited {
            return;
        }
        let current = self.save_dir.read(cx).value().to_string();
        let Some(target) = auto_follow_target(
            &current,
            &self.context.save_dir,
            self.category_written_dir.as_deref(),
            predicted.as_deref(),
            self.category_dir_manually_edited,
        ) else {
            return;
        };
        self.set_save_dir_text(&target, window, cx);
        self.category_written_dir = Some(target);
        cx.notify();
    }

    /// 下拉选择：`Auto` 立即按当前输入重算并写入；其余模式直接写目录框。写入值都记为
    /// 表单写入值并清除手动标记（任一模式都是显式选择），之后用户再改即视为手改。
    pub(super) fn set_category_mode(
        &mut self,
        mode: CategoryMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offered = self.context.save_dir.trim().to_owned();
        let predicted = (mode == CategoryMode::Auto)
            .then(|| self.predict_category_dir(cx))
            .flatten();
        let Some(target) = category_mode_save_dir(
            &mode,
            &self.context.category_options,
            &offered,
            predicted.as_deref(),
        ) else {
            return;
        };
        if mode == CategoryMode::Auto {
            self.category_predicted = predicted;
        }
        self.category_dir_manually_edited = false;
        self.set_save_dir_text(&target, window, cx);
        self.category_written_dir = Some(target);
        self.category_mode_touched = true;
        self.category_mode = mode;
        cx.notify();
    }

    /// 触发器的图标与文案：自动命中 → `自动 · 分类名`；自动未命中 → `自动分类`；其余显示所选项。
    fn category_trigger(&self) -> (FluxIcon, SharedString) {
        let options = &self.context.category_options;
        match &self.category_mode {
            CategoryMode::Auto => {
                match matched_category(options, self.category_predicted.as_deref()) {
                    Some(option) => (
                        category_icon(&option.icon),
                        self.strings
                            .format_category_auto_matched(&self.strings.category_name(option)),
                    ),
                    None => (FluxIcon::Sparkles, self.strings.category_auto.clone()),
                }
            }
            CategoryMode::NoCategory => (FluxIcon::Folder, self.strings.category_none.clone()),
            CategoryMode::Category(id) => {
                options.iter().find(|option| option.id == *id).map_or_else(
                    || (FluxIcon::Sparkles, self.strings.category_auto.clone()),
                    |option| {
                        (
                            category_icon(&option.icon),
                            self.strings.category_name(option),
                        )
                    },
                )
            }
        }
    }

    /// 固定顺序：自动、不分类，分隔后是各分类（名称 + 目录）。
    fn category_menu_entries(&self) -> (Vec<MenuEntry>, Vec<MenuEntry>) {
        let fixed = vec![
            MenuEntry {
                mode: CategoryMode::Auto,
                icon: FluxIcon::Sparkles,
                title: self.strings.category_auto.clone(),
                description: Some(self.strings.category_auto_desc.clone()),
            },
            MenuEntry {
                mode: CategoryMode::NoCategory,
                icon: FluxIcon::Folder,
                title: self.strings.category_none.clone(),
                description: Some(self.strings.category_none_desc.clone()),
            },
        ];
        let categories = self
            .context
            .category_options
            .iter()
            .map(|option| MenuEntry {
                mode: CategoryMode::Category(option.id.clone()),
                icon: category_icon(&option.icon),
                title: self.strings.category_name(option),
                description: Some(SharedString::from(option.save_dir.clone())),
            })
            .collect();
        (fixed, categories)
    }

    fn render_category_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.category_picker_visible() {
            return None;
        }
        let tokens = active_theme(cx).tokens().clone();
        let (icon, label) = self.category_trigger();
        let (fixed, categories) = self.category_menu_entries();
        let current = self.category_mode.clone();
        let this = cx.weak_entity();
        let add_this = cx.weak_entity();
        let translator = self.translator.read(cx).clone();
        let add_title = SharedString::from(translator.text("addUrlRule").to_owned());
        let add_desc = SharedString::from(translator.text("addUrlRuleDesc").to_owned());
        let (sm, xs) = (tokens.typography.sm, tokens.typography.xs);
        let (foreground, muted) = (tokens.colors.foreground, tokens.colors.muted_foreground);
        let gap = tokens.spacing.xxs;
        let item = move |entry: &MenuEntry| {
            let MenuEntry {
                mode,
                icon,
                title,
                description,
            } = entry;
            let (title, description) = (title.clone(), description.clone());
            let target = mode.clone();
            let this = this.clone();
            PopupMenuItem::element(move |_, _| {
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(gap)
                    .py(gap)
                    .child(
                        div()
                            .text_size(sm.size)
                            .line_height(sm.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(foreground)
                            .child(title.clone()),
                    )
                    .children(description.clone().map(|description| {
                        div()
                            .text_size(xs.size)
                            .line_height(xs.line_height)
                            .text_color(muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_middle()
                            .child(description)
                    }))
            })
            .icon(Icon::new(*icon))
            .checked(*mode == current)
            .on_click(move |_, window, cx| {
                let mode = target.clone();
                let Ok(()) = this.update(cx, |this, cx| {
                    this.set_category_mode(mode, window, cx);
                }) else {
                    // 视图已释放，结束这次回调而不再更新状态。
                    return;
                };
            })
        };
        Some(
            Button::new("new-download-category")
                .outline()
                .icon(icon)
                .label(label)
                .control(cx)
                .dropdown_caret(true)
                .flex_none()
                .w(px(CATEGORY_PICKER_WIDTH))
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    let menu = menu
                        .min_w(px(CATEGORY_MENU_MIN_WIDTH))
                        .max_w(px(CATEGORY_MENU_MAX_WIDTH))
                        .max_h(px(CATEGORY_MENU_MAX_HEIGHT))
                        .scrollable(true);
                    let menu = fixed
                        .iter()
                        .fold(menu, |menu, entry| menu.item(item(entry)));
                    let menu = categories
                        .iter()
                        .fold(menu.separator(), |menu, entry| menu.item(item(entry)));
                    // 尾部固定动作项：按当前链接快捷添加网址保存规则。Fn 闭包可重入，
                    // 捕获值先克隆成本地再移入元素闭包。
                    let (add_title, add_desc, add_this) =
                        (add_title.clone(), add_desc.clone(), add_this.clone());
                    let add_entry = PopupMenuItem::element(move |_, _| {
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .gap(gap)
                            .py(gap)
                            .child(
                                div()
                                    .text_size(sm.size)
                                    .line_height(sm.line_height)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(foreground)
                                    .child(add_title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(xs.size)
                                    .line_height(xs.line_height)
                                    .text_color(muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .child(add_desc.clone()),
                            )
                    })
                    .icon(Icon::new(FluxIcon::Globe))
                    .on_click(move |_, window, cx| {
                        let Some(view) = add_this.upgrade() else {
                            return;
                        };
                        super::url_rule::open(view, window, cx);
                    });
                    menu.separator().item(add_entry)
                })
                .into_any_element(),
        )
    }

    /// 保存目录输入行：左侧分类下拉（可见时），其余吃满的路径输入 / 历史下拉。
    pub(super) fn render_save_dir_input(&self, cx: &mut Context<Self>) -> AnyElement {
        let combo = self.render_save_dir_combo(cx);
        let Some(picker) = self.render_category_picker(cx) else {
            return combo;
        };
        h_flex()
            .w_full()
            .items_center()
            .gap(active_theme(cx).tokens().spacing.sm)
            .child(picker)
            .child(div().flex_1().min_w_0().child(combo))
            .into_any_element()
    }
}
