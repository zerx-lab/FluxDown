use std::time::Instant;

use fluxdown_protocol::{CustomCategoryDto, QueueDto};
use fluxdown_ui_components::{
    Button, FluxIcon, category_icon, nav_icon_color, sidebar_navigation_button, tabular_numbers,
    tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement, Pixels, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, percentage, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Icon, WindowExt as _,
    animation::ease_in_out_cubic,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::{
    controller::DownloadsCommand,
    model::{
        DownloadFilter, DownloadStatusFilter, SidebarSection, SidebarSelection, StatusFolderMotion,
        devices::{DeviceEntry, devices_section_visible},
    },
    pages::downloads::DownloadView,
};

const SHOW_SIDEBAR_CATEGORY_PREF: &str = "ui.show_sidebar_category";
/// 运行中队列的状态圆点直径。
const RUNNING_DOT_SIZE: Pixels = px(6.);
/// 导航行悬停组：状态项的展开箭头只在所在行悬停（或已展开）时显示。
const NAV_ROW_GROUP: &str = "download-sidebar-nav-row";
/// 分区标题悬停组：折叠箭头与分区操作按钮只在悬停标题时显示。
const SECTION_HEADER_GROUP: &str = "download-sidebar-section-header";

fn folder_id(status: DownloadStatusFilter) -> &'static str {
    match status {
        DownloadStatusFilter::All => "download-folder-all",
        DownloadStatusFilter::Incomplete => "download-folder-incomplete",
        DownloadStatusFilter::Completed => "download-folder-completed",
        DownloadStatusFilter::Seeding => "download-folder-seeding",
        DownloadStatusFilter::Failed => "download-folder-failed",
        DownloadStatusFilter::Paused => "download-folder-paused",
    }
}

/// 状态项图标：每个状态一个语义图标（内部仍沿用「状态文件夹」命名，视觉不再是文件夹）。
fn status_icon(status: DownloadStatusFilter) -> FluxIcon {
    match status {
        DownloadStatusFilter::All => FluxIcon::Layers,
        DownloadStatusFilter::Incomplete => FluxIcon::CircleArrowDown,
        DownloadStatusFilter::Completed => FluxIcon::CircleCheck,
        DownloadStatusFilter::Seeding => FluxIcon::ArrowUp,
        DownloadStatusFilter::Failed => FluxIcon::CircleAlert,
        DownloadStatusFilter::Paused => FluxIcon::CirclePause,
    }
}

impl DownloadView {
    /// 分区容器：扁平分组（无卡片边框），分区之间只靠留白与小标题区分；
    /// 首个分区顶部不再额外留白（侧栏根已有顶部内边距）。
    fn section_container(first: bool, cx: &App) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        v_flex().w_full().when(!first, |this| this.pt(spacing.md))
    }

    /// 分区标题：点击折叠 / 展开，右键「隐藏此区块」写回 `ui.show_sidebar_*` 偏好。
    /// 折叠箭头与 `trailing` 操作按钮只在悬停标题时出现。
    fn section_header(
        &self,
        label: SharedString,
        section: SidebarSection,
        open_amount: f32,
        trailing: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let spacing = tokens.spacing;
        let radius = tokens.radius;
        let hover_color = tokens.colors.muted_foreground;
        let header_color = extended.colors.text_tertiary;
        let caption = extended.caption;
        let chevron_size = extended.icon.sm;
        let header_height = theme.density().section_header;
        let chevron_rotation = percentage(open_amount * 0.25);
        let this = cx.weak_entity();
        let hide_label = self.strings.hide_section.clone();

        h_flex()
            .id(match section {
                SidebarSection::Status => "download-status-toggle",
                SidebarSection::Queues => "download-queue-toggle",
                SidebarSection::Devices => "download-devices-toggle",
            })
            .group(SECTION_HEADER_GROUP)
            .h(header_height)
            .px(spacing.sm)
            .items_center()
            .justify_between()
            .gap(spacing.xs)
            .cursor_pointer()
            .rounded(radius.md)
            .text_size(caption.size)
            .line_height(caption.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(header_color)
            .hover(move |style| style.text_color(hover_color))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_section(section, cx);
                cx.notify();
            }))
            .child(div().min_w_0().truncate().child(label))
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(spacing.xxs)
                    .invisible()
                    .group_hover(SECTION_HEADER_GROUP, |style| style.visible())
                    .children(trailing)
                    .child(
                        Icon::new(FluxIcon::ChevronRight)
                            .size(chevron_size)
                            .rotate(chevron_rotation),
                    ),
            )
            .context_menu(move |menu, _window, _cx| {
                let this = this.clone();
                menu.item(
                    PopupMenuItem::new(hide_label.clone()).on_click(move |_, _window, cx| {
                        let Ok(()) = this.update(cx, |this, cx| {
                            let (key, value) =
                                (section.visibility_pref(), serde_json::Value::Bool(false));
                            // 设备区是否显示只对本机有意义：写设备本地偏好，不同步到其他设备。
                            let command = if section.visibility_syncs() {
                                DownloadsCommand::SetSyncedPreference { key, value }
                            } else {
                                DownloadsCommand::SetLocalPreference { key, value }
                            };
                            this.execute_commands(vec![command], cx);
                        }) else {
                            // 视图已释放，结束这次回调而不再更新状态。
                            return;
                        };
                    }),
                )
            })
    }

    /// 队列分区标题右侧齿轮按钮：打开队列管理窗口（随标题悬停出现）。
    fn queue_manage_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let icon_size = extended.icon.sm;
        let nav_hover = extended.colors.nav_hover;
        let hover_foreground = tokens.colors.foreground;
        let open_queue_manager = self.host.open_queue_manager.clone();
        div()
            .id("download-queue-manage")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(icon_size + tokens.spacing.xs * 2.)
            .rounded(tokens.radius.sm)
            .cursor_pointer()
            .text_color(tokens.colors.muted_foreground)
            .hover(move |style| style.bg(nav_hover).text_color(hover_foreground))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                if let Some(open) = open_queue_manager.clone() {
                    open(window, cx);
                }
            })
            .child(Icon::new(FluxIcon::Settings).size(icon_size))
            .into_any_element()
    }

    /// 导航行尾部：可选状态圆点 + 计数（为 0 时不显示）。
    fn nav_trailing(count: usize, selected: bool, dot: Option<Hsla>, cx: &App) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let caption = extended.caption;
        let count_color = if selected {
            tokens.colors.muted_foreground
        } else {
            extended.colors.text_tertiary
        };
        h_flex()
            .flex_none()
            .items_center()
            .gap(tokens.spacing.xs)
            .when_some(dot, |this, color| {
                this.child(
                    div()
                        .flex_none()
                        .size(RUNNING_DOT_SIZE)
                        .rounded_full()
                        .bg(color),
                )
            })
            .when(count > 0, |this| {
                this.child(
                    div()
                        .text_size(caption.size)
                        .line_height(caption.line_height)
                        .font_features(tabular_numbers())
                        .text_color(count_color)
                        .child(SharedString::from(count.to_string())),
                )
            })
    }

    /// 普通导航行（分类子项 / 队列 / 设备）：点击选中；图标选中为强调色、未选中为二级文字色。
    fn nav_item(
        &self,
        id: impl Into<gpui::ElementId>,
        selection: SidebarSelection,
        label: SharedString,
        icon: Icon,
        trailing: (usize, Option<Hsla>),
        cx: &mut Context<Self>,
    ) -> Button {
        let (count, dot) = trailing;
        let selected = self.selected_item == selection;
        let icon_color = nav_icon_color(selected, cx);

        sidebar_navigation_button(
            id,
            label,
            icon.text_color(icon_color),
            Self::nav_trailing(count, selected, dot, cx),
            selected,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_sidebar_item(selection.clone(), cx);
        }))
    }

    fn filter_count(&self, filter: &DownloadFilter, cx: &Context<Self>) -> usize {
        self.table_state.read(cx).delegate().count_matching(filter)
    }

    fn queue_count(&self, queue_id: &str, cx: &Context<Self>) -> usize {
        self.table_state
            .read(cx)
            .delegate()
            .count_in_queue(queue_id)
    }

    /// 设备计数：与表格筛选同一规则（[`SidebarSelection::device_matches`]）——本机计所有
    /// 本地任务；具体设备按远程任务的目标设备计；「全部设备」= 本地 + 远程。
    fn device_count(&self, device_id: &str, cx: &Context<Self>) -> usize {
        self.table_state.read(cx).delegate().count_device(device_id)
    }

    /// 状态项：图标位在悬停时换成分类展开箭头（点击箭头只切换展开，Notion / Linear
    /// 式做法，不额外占一列缩进）；行其余部分点击切换展开并选中该状态。
    /// 失败项在有失败任务时图标用 destructive、下载中有任务时用强调色，作为唯一的状态提示。
    fn status_item(
        &self,
        status: DownloadStatusFilter,
        open_amount: f32,
        has_children: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        let theme = active_theme(cx);
        let colors = theme.tokens().colors;
        let icon_sizes = theme.extended().icon;
        let slot_size = icon_sizes.lg;
        let filter = DownloadFilter::status(status);
        let count = self.filter_count(&filter, cx);
        let selection = SidebarSelection::Download(filter);
        let selected = self.selected_item == selection;
        let icon_color = match status {
            DownloadStatusFilter::Failed if count > 0 => colors.destructive,
            DownloadStatusFilter::Incomplete if count > 0 => {
                theme.extended().colors.nav_selected_icon
            }
            _ => nav_icon_color(selected, cx),
        };

        let status_glyph = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .when(has_children, |this| {
                this.group_hover(NAV_ROW_GROUP, |style| style.invisible())
            })
            .child(
                Icon::new(status_icon(status))
                    .size(icon_sizes.lg)
                    .text_color(icon_color),
            );
        let chevron = has_children.then(|| {
            div()
                .id(format!("{}-chevron", folder_id(status)))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.tokens().radius.sm)
                .invisible()
                .group_hover(NAV_ROW_GROUP, |style| style.visible())
                .text_color(colors.foreground)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.retarget_status_folder(status, cx);
                    cx.notify();
                }))
                .child(
                    Icon::new(FluxIcon::ChevronRight)
                        .size(icon_sizes.md)
                        .rotate(percentage(open_amount * 0.25)),
                )
        });
        let leading = div()
            .relative()
            .flex_none()
            .size(slot_size)
            .child(status_glyph)
            .children(chevron);

        sidebar_navigation_button(
            folder_id(status),
            self.folder_label(status),
            leading,
            Self::nav_trailing(count, selected, None, cx),
            selected,
            cx,
        )
        .group(NAV_ROW_GROUP)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.retarget_status_folder(status, cx);
            this.select_sidebar_item(selection.clone(), cx);
            cx.notify();
        }))
    }

    /// 状态项下的分类子项（内置 `all` 之外的可见分类）；分类的编辑 / 新建走这里的右键菜单。
    fn category_entries(&self) -> Vec<CustomCategoryDto> {
        self.controller
            .categories()
            .visible()
            .filter(|rule| rule.dto.builtin_type.as_deref() != Some("all"))
            .map(|rule| rule.dto.clone())
            .collect()
    }

    /// 分类子项显隐偏好；关闭后状态项保留，但不渲染分类子项也不显示展开箭头。
    fn categories_visible(&self) -> bool {
        self.controller
            .preference_bool(SHOW_SIDEBAR_CATEGORY_PREF, true)
    }

    /// 分类子项显示时的实际渲染数量；用于高度动画计算，偏好关闭时恒为 0。
    fn visible_category_count(&self) -> f32 {
        if self.categories_visible() {
            self.category_entries().len() as f32
        } else {
            0.
        }
    }

    /// 分类子项：比父项多缩进 `spacing.lg`（一级层次足够辨识，不再对齐父项文字，避免
    /// 缩进过深）。缩进做在按钮内边距上，悬停 / 选中底色仍铺满整行宽度。
    fn render_categories(&self, status: DownloadStatusFilter, cx: &mut Context<Self>) -> Div {
        if !self.categories_visible() {
            return v_flex().w_full();
        }
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let row_padding = spacing.sm + spacing.lg;
        let icon_size = theme.extended().icon.md;
        let entries = self.category_entries();
        let mut items = Vec::with_capacity(entries.len());
        for dto in entries {
            let filter = DownloadFilter::with_category(status, &dto.id);
            let count = self.filter_count(&filter, cx);
            let menu = self.category_item_context_menu(&dto, cx);
            let item = self
                .nav_item(
                    format!("download-nav-{}-{}", folder_id(status), dto.id),
                    SidebarSelection::Download(filter),
                    self.strings.category_label(&dto),
                    Icon::new(category_icon(&dto.icon)).size(icon_size),
                    (count, None),
                    cx,
                )
                .pl(row_padding)
                .context_menu(menu);
            items.push(item);
        }
        v_flex().w_full().children(items)
    }

    fn retarget_status_folder(&mut self, status: DownloadStatusFilter, cx: &App) {
        if status != DownloadStatusFilter::All {
            return;
        }
        let next = DownloadStatusFilter::exclusive_toggle(self.expanded_status, status);
        if next == self.expanded_status {
            return;
        }
        if cx.reduce_motion() {
            self.folder_motion = StatusFolderMotion::settled(next);
            self.folder_motion_started_at = None;
        } else {
            self.folder_motion = self.folder_motion.retarget(
                Self::eased_motion_progress(self.folder_motion_started_at),
                next,
            );
            self.folder_motion_started_at = Some(Instant::now());
        }
        self.expanded_status = next;
    }

    fn sidebar_motion_linear_progress(started_at: Option<Instant>) -> f32 {
        let Some(started_at) = started_at else {
            return 1.;
        };
        (started_at.elapsed().as_secs_f32()
            / crate::pages::downloads::SIDEBAR_MOTION_DURATION.as_secs_f32())
        .clamp(0., 1.)
    }

    /// 折叠动画的缓动进度（未开始 / 已结束为 1）。
    fn eased_motion_progress(started_at: Option<Instant>) -> f32 {
        ease_in_out_cubic(Self::sidebar_motion_linear_progress(started_at))
    }

    /// 渲染时的折叠动画缓动进度：减弱动效时直接到终点；动画未结束时请求下一帧。
    /// 分区与状态项两套折叠共用。
    fn render_motion_progress(started_at: Option<Instant>, window: &mut Window, cx: &App) -> f32 {
        if cx.reduce_motion() {
            return 1.;
        }
        let linear = Self::sidebar_motion_linear_progress(started_at);
        if linear < 1. {
            window.request_animation_frame();
        }
        ease_in_out_cubic(linear)
    }

    pub(crate) fn section_is_expanded(&self, section: SidebarSection) -> bool {
        self.section_expanded.get(&section).copied().unwrap_or(true)
    }

    pub(crate) fn toggle_section(&mut self, section: SidebarSection, cx: &App) {
        let expanded = self.section_is_expanded(section);
        let target = if expanded { 1. } else { 0. };
        let from = self
            .section_motion_from
            .get(&section)
            .copied()
            .unwrap_or(target);
        let progress =
            Self::eased_motion_progress(self.section_motion_started_at.get(&section).copied());
        let current = from + (target - from) * progress;
        let next = !expanded;
        if cx.reduce_motion() {
            self.section_motion_from
                .insert(section, if next { 1. } else { 0. });
            self.section_motion_started_at.remove(&section);
        } else {
            self.section_motion_from.insert(section, current);
            self.section_motion_started_at
                .insert(section, Instant::now());
        }
        self.section_expanded.insert(section, next);
    }

    pub(crate) fn section_open_amount(
        &self,
        section: SidebarSection,
        window: &mut Window,
        cx: &App,
    ) -> f32 {
        let target = if self.section_is_expanded(section) {
            1.
        } else {
            0.
        };
        let progress = Self::render_motion_progress(
            self.section_motion_started_at.get(&section).copied(),
            window,
            cx,
        );
        let from = self
            .section_motion_from
            .get(&section)
            .copied()
            .unwrap_or(target);
        from + (target - from) * progress
    }

    fn folder_open_amount(
        &self,
        status: DownloadStatusFilter,
        window: &mut Window,
        cx: &App,
    ) -> f32 {
        let progress = Self::render_motion_progress(self.folder_motion_started_at, window, cx);
        self.folder_motion.amount(status, progress)
    }

    /// 状态项的显示文案。
    fn folder_label(&self, status: DownloadStatusFilter) -> SharedString {
        match status {
            DownloadStatusFilter::All => self.strings.status_all.clone(),
            DownloadStatusFilter::Incomplete => self.strings.status_incomplete.clone(),
            DownloadStatusFilter::Completed => self.strings.status_completed.clone(),
            DownloadStatusFilter::Seeding => self.strings.status_seeding.clone(),
            DownloadStatusFilter::Failed => self.strings.tab_failed.clone(),
            DownloadStatusFilter::Paused => self.strings.status_paused.clone(),
        }
    }

    fn render_status_branch(
        &self,
        status: DownloadStatusFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if status != DownloadStatusFilter::All {
            return v_flex()
                .w_full()
                .child(self.status_item(status, 0., false, cx));
        }
        let open_amount = self.folder_open_amount(status, window, cx);
        let category_count = self.visible_category_count();
        v_flex()
            .w_full()
            .child(self.status_item(status, open_amount, category_count > 0., cx))
            .child({
                let mut body = div()
                    .w_full()
                    .overflow_hidden()
                    .h(active_theme(cx).density().nav_row * (category_count * open_amount));
                // 折叠时高度为 0，不必为看不见的分类子项逐个扫描任务计数。
                if open_amount > 0. {
                    body = body.child(self.render_categories(status, cx));
                }
                body
            })
    }

    /// 状态区：仅「全部」可展开分类，其余状态仅用于筛选。
    fn render_status_section(
        &self,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open_amount = self.section_open_amount(SidebarSection::Status, window, cx);
        let category_count = self.visible_category_count();
        let folder_open_sum = self.folder_open_amount(DownloadStatusFilter::All, window, cx);
        let row_count = DownloadStatusFilter::ALL.len() as f32 + category_count * folder_open_sum;

        let mut body = v_flex().w_full();
        for status in DownloadStatusFilter::ALL {
            body = body.child(self.render_status_branch(status, window, cx));
        }

        Self::section_container(first, cx)
            .child(self.section_header(
                self.strings.sidebar_status.clone(),
                SidebarSection::Status,
                open_amount,
                None,
                cx,
            ))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .h(active_theme(cx).density().nav_row * (row_count * open_amount))
                    .child(body),
            )
    }

    /// 分类子项右键菜单：编辑（内置项不显示）、新建。
    fn category_item_context_menu(
        &self,
        dto: &CustomCategoryDto,
        _cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let open_editor = self.host.open_category_editor.clone();
        let edit_label = self.strings.edit_category.clone();
        let add_label = self.strings.add_category.clone();
        let is_builtin = dto.is_builtin;
        let category_id = dto.id.clone();

        move |menu, _window, _cx| {
            let menu = menu.when_some(
                open_editor.clone().filter(|_| !is_builtin),
                |menu, open_editor| {
                    let category_id = category_id.clone();
                    menu.item(PopupMenuItem::new(edit_label.clone()).on_click(
                        move |_, window, cx| {
                            open_editor(Some(category_id.clone()), window, cx);
                        },
                    ))
                },
            );
            menu.when_some(open_editor.clone(), |menu, open_editor| {
                menu.item(
                    PopupMenuItem::new(add_label.clone()).on_click(move |_, window, cx| {
                        open_editor(None, window, cx);
                    }),
                )
            })
        }
    }

    /// 队列右键菜单：启动/停止（主队列恒运行，只在异常停止时给「启动」）、管理队列、
    /// 删除（主队列不显示，删除前确认）。
    fn queue_item_context_menu(
        &self,
        queue: &QueueDto,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.weak_entity();
        let start_label = self.strings.start_queue_action.clone();
        let stop_label = self.strings.stop_queue_action.clone();
        let manage_label = self.strings.manage_queue_action.clone();
        let delete_label = self.strings.delete_queue_action.clone();
        let delete_ok_label = self.strings.delete.clone();
        let cancel_label = self.strings.cancel.clone();
        let queue_label = self.strings.queue_name(
            &queue.queue_id,
            if queue.name.is_empty() {
                &queue.queue_id
            } else {
                &queue.name
            },
        );
        let description = SharedString::from(
            self.strings
                .queue_delete_confirm_desc
                .replace("{name}", &queue_label),
        );
        let open_queue_manager = self.host.open_queue_manager.clone();
        let queue_id = queue.queue_id.clone();
        let is_running = queue.is_running;
        let is_main = queue.queue_id == fluxdown_protocol::MAIN_QUEUE_ID;

        move |menu, _window, _cx| {
            // 主队列不可停止：运行中时不给启停项（异常停止时仍可「启动」自救）。
            let menu = menu.when(!(is_main && is_running), |menu| {
                menu.item({
                    let this = this.clone();
                    let queue_id = queue_id.clone();
                    if is_running {
                        PopupMenuItem::new(stop_label.clone()).on_click(move |_, _window, cx| {
                            let queue_id = queue_id.clone();

                            let Ok(()) = this.update(cx, |this, cx| {
                                this.execute_commands(
                                    vec![DownloadsCommand::QueueStop { queue_id }],
                                    cx,
                                );
                            }) else {
                                // 视图已释放，结束这次回调而不再更新状态。
                                return;
                            };
                        })
                    } else {
                        PopupMenuItem::new(start_label.clone()).on_click(move |_, _window, cx| {
                            let queue_id = queue_id.clone();

                            let Ok(()) = this.update(cx, |this, cx| {
                                this.execute_commands(
                                    vec![DownloadsCommand::QueueStart { queue_id }],
                                    cx,
                                );
                            }) else {
                                // 视图已释放，结束这次回调而不再更新状态。
                                return;
                            };
                        })
                    }
                })
            });
            let menu = menu.item({
                let open_queue_manager = open_queue_manager.clone();
                PopupMenuItem::new(manage_label.clone()).on_click(move |_, window, cx| {
                    if let Some(open) = open_queue_manager.clone() {
                        open(window, cx);
                    }
                })
            });
            menu.when(!is_main, |menu| {
                let this = this.clone();
                let queue_id = queue_id.clone();
                let delete_label = delete_label.clone();
                let title = delete_label.clone();
                let description = description.clone();
                let ok_label = delete_ok_label.clone();
                let cancel_label = cancel_label.clone();
                menu.separator()
                    .item(
                        PopupMenuItem::new(delete_label).on_click(move |_, window, cx| {
                            let this = this.clone();
                            let queue_id = queue_id.clone();
                            let title = title.clone();
                            let description = description.clone();
                            let ok_label = ok_label.clone();
                            let cancel_label = cancel_label.clone();
                            window.open_alert_dialog(cx, move |dialog, _, cx| {
                                let this = this.clone();
                                let queue_id = queue_id.clone();
                                dialog
                                    .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                                    .description(description.clone())
                                    .footer(fluxdown_ui_components::dialog_footer(
                                        Some(cancel_label.clone()),
                                        ok_label.clone(),
                                        fluxdown_ui_components::DialogIntent::Destructive,
                                        cx,
                                    ))
                                    .on_ok(move |_, _, cx| {
                                        let Ok(()) = this.update(cx, |this, cx| {
                                            this.execute_commands(
                                                vec![DownloadsCommand::QueueDelete {
                                                    queue_id: queue_id.clone(),
                                                }],
                                                cx,
                                            );
                                        }) else {
                                            // 页面已释放，不能把未提交的删除当成成功。
                                            return false;
                                        };
                                        true
                                    })
                            });
                        }),
                    )
            })
        }
    }

    fn render_queue_section(
        &self,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open_amount = self.section_open_amount(SidebarSection::Queues, window, cx);
        let queues: Vec<QueueDto> = self.controller.queues().to_vec();
        let theme = active_theme(cx);
        let running_color = theme.extended().colors.success;
        let icon_size = theme.extended().icon.lg;
        let count = queues.len() as f32;
        let mut items = Vec::with_capacity(queues.len());
        for queue in queues {
            let id = queue.queue_id.clone();
            let label = match id.as_str() {
                fluxdown_protocol::MAIN_QUEUE_ID => self.strings.main_queue.clone(),
                fluxdown_protocol::LATER_QUEUE_ID => self.strings.later_queue.clone(),
                _ => SharedString::from(queue.name.clone()),
            };
            let dot = queue.is_running.then_some(running_color);
            let task_count = self.queue_count(&id, cx);
            let menu = self.queue_item_context_menu(&queue, cx);
            items.push(
                self.nav_item(
                    format!("download-nav-queue-{id}"),
                    SidebarSelection::Queue(id),
                    label,
                    Icon::new(FluxIcon::Rows3).size(icon_size),
                    (task_count, dot),
                    cx,
                )
                .context_menu(menu),
            );
        }
        Self::section_container(first, cx)
            .child(self.section_header(
                self.strings.sidebar_queues.clone(),
                SidebarSection::Queues,
                open_amount,
                Some(self.queue_manage_button(cx)),
                cx,
            ))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .h(active_theme(cx).density().nav_row * (count * open_amount))
                    .child(v_flex().w_full().opacity(open_amount).children(items)),
            )
    }

    /// 「添加设备」按钮：随设备区标题悬停出现；点击交给宿主（app 打开账号 / 配对对话框）。
    fn add_device_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let open_add_device = self.host.open_add_device.clone()?;
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let icon_size = extended.icon.sm;
        let nav_hover = extended.colors.nav_hover;
        let hover_foreground = tokens.colors.foreground;
        let tooltip = self.strings.add_device.clone();
        Some(
            div()
                .id("download-device-add")
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(icon_size + tokens.spacing.xs * 2.)
                .rounded(tokens.radius.sm)
                .cursor_pointer()
                .text_color(tokens.colors.muted_foreground)
                .hover(move |style| style.bg(nav_hover).text_color(hover_foreground))
                .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    open_add_device(window, cx);
                })
                .child(Icon::new(FluxIcon::Plus).size(icon_size))
                .into_any_element(),
        )
    }

    /// 设备区：（有云端其他设备时）全部设备 + 本机 + 账号其他设备（含挂接直连）+ 独立直连设备。
    /// 云条目可选中作为任务筛选，远程任务按目标设备计数；独立直连设备没有任务视图，点击打开
    /// 「新建下载」并预选该设备，右键可下发 / 重命名 / 解除配对。在线状态用圆点；同名已消歧。
    fn render_devices_section(
        &self,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open_amount = self.section_open_amount(SidebarSection::Devices, window, cx);
        let theme = active_theme(cx);
        let icon_size = theme.extended().icon.lg;
        let online_color = theme.extended().colors.success;
        let offline_color = theme.extended().colors.text_tertiary;
        let (standalone, cloud): (Vec<_>, Vec<_>) = self
            .controller
            .other_devices()
            .into_iter()
            .partition(DeviceEntry::is_standalone_link);
        let presence = |device: &DeviceEntry| {
            let dot = match device.online {
                Some(true) => Some(online_color),
                Some(false) => Some(offline_color),
                None => None,
            };
            let label = if device.online.is_none() {
                SharedString::from(format!(
                    "{} · {}",
                    device.label, self.strings.device_presence_unknown
                ))
            } else {
                SharedString::from(device.label.clone())
            };
            (label, dot)
        };
        let mut rows: Vec<(String, SharedString, FluxIcon, Option<Hsla>)> = Vec::new();
        if !cloud.is_empty() {
            rows.push((
                SidebarSelection::ALL_DEVICES.to_owned(),
                self.strings.all_devices.clone(),
                FluxIcon::Layers,
                None,
            ));
        }
        rows.push((
            SidebarSelection::LOCAL_DEVICE.to_owned(),
            self.strings.this_device.clone(),
            FluxIcon::Cpu,
            None,
        ));
        rows.extend(cloud.iter().map(|device| {
            let (label, dot) = presence(device);
            (device.id.clone(), label, FluxIcon::Globe, dot)
        }));
        let count = (rows.len() + standalone.len()) as f32;
        let mut items: Vec<AnyElement> = Vec::with_capacity(rows.len() + standalone.len());
        for (id, label, icon, dot) in rows {
            let task_count = self.device_count(&id, cx);
            items.push(
                self.nav_item(
                    format!("download-nav-device-{id}"),
                    SidebarSelection::Device(id),
                    label,
                    Icon::new(icon).size(icon_size),
                    (task_count, dot),
                    cx,
                )
                .into_any_element(),
            );
        }
        for device in &standalone {
            let (label, dot) = presence(device);
            items.push(self.linked_device_item(device, label, dot, cx));
        }
        Self::section_container(first, cx)
            .child(self.section_header(
                self.strings.sidebar_devices.clone(),
                SidebarSection::Devices,
                open_amount,
                self.add_device_button(cx),
                cx,
            ))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .h(active_theme(cx).density().nav_row * (count * open_amount))
                    .child(v_flex().w_full().opacity(open_amount).children(items)),
            )
    }

    /// 独立直连设备行：不参与选中；点击打开「新建下载」并预选该设备，右键下发 / 重命名 / 解除配对。
    fn linked_device_item(
        &self,
        device: &DeviceEntry,
        label: SharedString,
        dot: Option<Hsla>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let icon_size = active_theme(cx).extended().icon.lg;
        let icon_color = nav_icon_color(false, cx);
        let target = device.target();
        let click_target = target.clone();
        let this = cx.weak_entity();
        let fingerprint = device.id.clone();
        let download_label = self.strings.device_download_to_this.clone();
        let rename_label = self.strings.linked_device_rename.clone();
        let unpair_label = self.strings.linked_device_unpair.clone();
        let rename = self.host.rename_linked_device.clone();
        let unpair = self.host.unpair_linked_device.clone();
        sidebar_navigation_button(
            format!("download-nav-link-{}", device.id),
            label,
            Icon::new(FluxIcon::Network)
                .size(icon_size)
                .text_color(icon_color),
            Self::nav_trailing(0, false, dot, cx),
            false,
            cx,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_new_download_to(click_target.clone(), window, cx);
        }))
        .context_menu(move |menu, _window, _cx| {
            let menu = menu.item({
                let this = this.clone();
                let target = target.clone();
                PopupMenuItem::new(download_label.clone()).on_click(move |_, window, cx| {
                    let target = target.clone();
                    let Ok(()) = this.update(cx, |this, cx| {
                        this.open_new_download_to(target, window, cx);
                    }) else {
                        // 视图已释放，结束这次回调而不再更新状态。
                        return;
                    };
                })
            });
            let menu = menu.when_some(rename.clone(), |menu, rename| {
                let fingerprint = fingerprint.clone();
                menu.item(PopupMenuItem::new(rename_label.clone()).on_click(
                    move |_, window, cx| {
                        rename(fingerprint.clone(), window, cx);
                    },
                ))
            });
            menu.when_some(unpair.clone(), |menu, unpair| {
                let fingerprint = fingerprint.clone();
                menu.separator()
                    .item(PopupMenuItem::new(unpair_label.clone()).on_click(
                        move |_, window, cx| {
                            unpair(fingerprint.clone(), window, cx);
                        },
                    ))
            })
        })
        .into_any_element()
    }

    /// 分区可见性：设备区三态（偏好显式设置则按值，未设置时按「是否存在其他设备」判定——
    /// 账号里只有本机一台不算）；其余分区二态，默认显示。
    fn section_visible(&self, section: SidebarSection) -> bool {
        match section {
            SidebarSection::Devices => devices_section_visible(
                self.controller
                    .preference(section.visibility_pref())
                    .and_then(serde_json::Value::as_bool),
                self.controller.other_devices().len(),
            ),
            _ => self
                .controller
                .preference_bool(section.visibility_pref(), true),
        }
    }

    pub(crate) fn has_visible_sidebar_section(&self) -> bool {
        SidebarSection::ALL
            .into_iter()
            .any(|section| self.section_visible(section))
    }

    /// 侧栏根：与活动栏同为 `chrome` 底色；与内容区之间的分隔线由页面布局负责。
    pub(crate) fn render_sidebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let chrome = theme.extended().colors.chrome;
        let spacing = theme.tokens().spacing;
        let mut root = v_flex()
            .size_full()
            .min_w_0()
            .px(spacing.sm)
            .pt(spacing.sm)
            .overflow_y_scrollbar()
            .bg(chrome);
        let mut first = true;
        for section in SidebarSection::ALL {
            if !self.section_visible(section) {
                continue;
            }
            let content: Div = match section {
                SidebarSection::Status => self.render_status_section(first, window, cx),
                SidebarSection::Queues => self.render_queue_section(first, window, cx),
                SidebarSection::Devices => self.render_devices_section(first, window, cx),
            };
            first = false;
            root = root.child(content);
        }
        root
    }
}
