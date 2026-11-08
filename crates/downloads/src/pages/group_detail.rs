//! 任务组详情：独立窗口，概览 + 成员列表（复用 [`DownloadTableDelegate`]）。
//!
//! 弹出窗口没有主窗口 controller 可借，自建一份 [`DownloadsController`] 并经
//! [`crate::session::attach`] 订阅会话即可获得完整任务/队列/组数据。

use std::{rc::Rc, sync::Arc};

use fluxdown_protocol::{AgentEvent, AgentSnapshot, DaemonEvent, ServiceEvent, WsServerMsg};
use fluxdown_ui_components::tabular_numbers;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString,
    Styled, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{table::TableState, v_flex};

use crate::{
    components::task_table::{
        DownloadTableDelegate, TableFilter, render_download_table, spawn_midnight_refresh,
    },
    controller::{DownloadsController, DownloadsPort, GroupSummary},
    model::RowId,
    pages::task_detail::detail_row,
    strings::DownloadStrings,
};

pub struct GroupDetailView {
    group_id: String,
    controller: DownloadsController,
    translator: Entity<Translator>,
    strings: DownloadStrings,
    table_state: Entity<TableState<DownloadTableDelegate>>,
    last_error: Option<SharedString>,
}

impl GroupDetailView {
    pub fn new(
        translator: Entity<Translator>,
        group_id: String,
        port: Arc<dyn DownloadsPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strings = DownloadStrings::from_translator(translator.read(cx));
        let controller = DownloadsController::new(port);
        let store = Rc::clone(controller.store());
        let filter_group_id = group_id.clone();
        let table_state = cx.new(|cx| {
            let mut delegate = DownloadTableDelegate::new(strings.clone(), store);
            delegate.set_filter(TableFilter::Group(filter_group_id));
            TableState::new(delegate, window, cx)
                .row_selectable(false)
                .col_selectable(false)
        });
        spawn_midnight_refresh(&table_state, cx);

        cx.observe(&translator, |this, translator, cx| {
            this.strings = DownloadStrings::from_translator(translator.read(cx));
            this.table_state.update(cx, |table, cx| {
                table.delegate_mut().set_strings(this.strings.clone());
                table.delegate_mut().refresh_view();
                table.refresh(cx);
                cx.notify();
            });
            cx.notify();
        })
        .detach();

        Self {
            group_id,
            controller,
            translator,
            strings,
            table_state,
            last_error: None,
        }
    }

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.controller.replace_snapshot(snapshot);
        self.last_error = None;
        self.refresh_table(cx);
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let changed = self.controller.apply_event(event);
        if self.is_other_group_task_event(event) {
            // 控制器已应用；别组任务的进度不影响本组表格与概览，不重算可见行 / 汇总。
            return;
        }
        if changed {
            self.refresh_table(cx);
        } else {
            cx.notify();
        }
    }

    /// 单任务高频事件（任务变更 / 进度 / 运行态 / 元数据）涉及的任务不属于本组。
    fn is_other_group_task_event(&self, event: &ServiceEvent) -> bool {
        let ServiceEvent::Agent(AgentEvent::Daemon(event)) = event else {
            return false;
        };
        let task_id = match event {
            DaemonEvent::TaskChanged(task) => return task.group_id != self.group_id,
            DaemonEvent::TaskRuntimeChanged(runtime) => &runtime.task_id,
            DaemonEvent::Engine(
                WsServerMsg::TaskProgress { task_id, .. }
                | WsServerMsg::TaskMetaProbed { task_id, .. },
            ) => task_id,
            _ => return false,
        };
        let store = self.controller.store();
        store
            .find_local(task_id)
            .and_then(|ix| store.row(RowId::Local(ix)))
            .is_none_or(|row| row.group_id != self.group_id)
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.controller.mark_stale();
        self.last_error = Some(self.strings.disconnected.clone());
        cx.notify();
    }

    fn refresh_table(&mut self, cx: &mut Context<Self>) {
        let hide_source = self.controller.hide_task_source();
        let queues: Vec<(String, String)> = self
            .controller
            .queues()
            .iter()
            .map(|queue| (queue.queue_id.clone(), queue.name.clone()))
            .collect();
        self.table_state.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.set_queue_names(queues);
            delegate.set_hide_source(hide_source);
            delegate.refresh_view();
            // 行变化只需重绘；列配置未变时不重建 `col_groups`，保住用户拖拽的列宽。
            if delegate.take_columns_dirty() {
                table.refresh(cx);
            }
            // 行从共享的 `TaskStore` 读取：表格必须显式 notify，retained 渲染才会重画行。
            cx.notify();
        });
        cx.notify();
    }

    fn summary(&mut self) -> GroupSummary {
        self.controller
            .group_summaries()
            .iter()
            .find(|summary| summary.id == self.group_id)
            .cloned()
            .unwrap_or_default()
    }

    fn t(&self, cx: &App, key: &str) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }

    fn render_overview(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let summary = self.summary();
        let status = if self.controller.tables_loading() {
            // 任务表分页载入期间成员数不完整，不能显示「没有成员」。
            self.strings.tasks_loading_title.clone()
        } else if summary.aggregate.counts.total == 0 {
            self.t(cx, "groupDetailNoMembers")
        } else {
            SharedString::from(format!(
                "{}/{} · {:.0}%",
                summary.aggregate.counts.done,
                summary.aggregate.counts.total,
                summary.aggregate.progress * 100.0
            ))
        };
        v_flex()
            .gap(tokens.spacing.sm)
            .p(tokens.spacing.md)
            .border_b(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(
                div()
                    .text_size(extended.title.size)
                    .line_height(extended.title.line_height)
                    .font_weight(extended.title.weight)
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(if summary.meta.name.is_empty() {
                        self.group_id.clone()
                    } else {
                        summary.meta.name.clone()
                    })),
            )
            .child(
                v_flex()
                    .child(detail_row(
                        self.t(cx, "groupMemberOfLabel"),
                        div().font_features(tabular_numbers()).child(status),
                        cx,
                    ))
                    .child(detail_row(
                        self.t(cx, "groupDetailSource"),
                        div()
                            .truncate()
                            .child(SharedString::from(summary.meta.source_url.clone())),
                        cx,
                    ))
                    .child(detail_row(
                        self.t(cx, "groupDetailSaveDir"),
                        div()
                            .truncate()
                            .child(SharedString::from(summary.meta.save_dir.clone())),
                        cx,
                    )),
            )
    }

    /// 成员表格与主窗口共用 [`render_download_table`]：同一套字号 / 数字 / 状态色规则。
    fn render_members(&self, cx: &mut Context<Self>) -> impl IntoElement {
        render_download_table("group-detail-members-table", &self.table_state, cx)
    }
}

impl gpui::Render for GroupDetailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        v_flex()
            .size_full()
            .min_h_0()
            .bg(tokens.colors.surface)
            .child(self.render_overview(cx))
            .when_some(self.last_error.clone(), |this, error| {
                this.child(
                    div()
                        .px(tokens.spacing.md)
                        .pt(tokens.spacing.sm)
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.destructive)
                        .child(error),
                )
            })
            .child(
                div()
                    .px(tokens.spacing.md)
                    .pt(tokens.spacing.md)
                    .pb(tokens.spacing.xs)
                    .text_size(extended.caption.size)
                    .line_height(extended.caption.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(extended.colors.text_tertiary)
                    .child(self.t(cx, "groupDetailMembersTab")),
            )
            .child(self.render_members(cx))
    }
}
