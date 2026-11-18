//! RSS 工作区：订阅列表、条目流和批量操作。

use crate::{RssController, RssPort, editor};
use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, ApplicationErrorCode, DaemonEvent, RssItemDto, RssSourceDto,
    ServiceEvent, WsServerMsg, method,
};
use fluxdown_ui_components::{
    BusyExt as _, BusyHold, CheckState, ControlExt as _, FluxIcon, SidebarPanel, SidebarState,
    caption_number, check_mark, nav_icon_color, tabular_numbers, toolbar_action_button,
    tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement, Pixels, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
    uniform_list,
};
use gpui_component::{
    Disableable as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// 订阅源侧栏默认宽度。
const SOURCE_COLUMN_WIDTH: f32 = 240.;
/// 订阅源侧栏可拖拽的宽度范围。
const SOURCE_COLUMN_MIN_WIDTH: f32 = 176.;
const SOURCE_COLUMN_MAX_WIDTH: f32 = 400.;
/// 条目行高下限（标题 + 元信息 + 可选过滤原因三行，默认字号下恰好 64）。
const ITEM_ROW_HEIGHT: Pixels = px(64.);
/// 条目行上下内边距：64 = sm 行高 18 + 2 × xs 行高 16 + 2 × xxs 2 + 2 × 5。
const ITEM_ROW_PADDING_Y: Pixels = px(5.);
/// 条目状态列宽（基准 px，按文字缩放）。
const STATUS_COLUMN_WIDTH: f32 = 120.;
/// 条目搜索框宽度（基准 px，按文字缩放）。
const SEARCH_WIDTH: f32 = 200.;
/// 「上次抓取 N 分钟前」的最长复查间隔（没有可显示的抓取时间，或时间点可能被新抓取改写时）。
const FETCH_AGE_RECHECK: Duration = Duration::from_secs(60);

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

/// 抓取时间相对文案：`(文案键, 数量)`。
fn fetch_age_label(age_secs: i64) -> (&'static str, i64) {
    if age_secs < 60 {
        ("rssJustNow", 0)
    } else if age_secs < 3_600 {
        ("rssMinutesAgo", age_secs / 60)
    } else if age_secs < 86_400 {
        ("rssHoursAgo", age_secs / 3_600)
    } else {
        ("rssDaysAgo", age_secs / 86_400)
    }
}

/// [`fetch_age_label`] 距下一次跨档（下一个分钟 / 小时 / 天）的时长。
fn fetch_age_changes_in(age_secs: i64) -> Duration {
    let age = age_secs.max(0);
    let unit: i64 = if age < 3_600 {
        60
    } else if age < 86_400 {
        3_600
    } else {
        86_400
    };
    Duration::from_secs(u64::try_from(unit - age % unit).unwrap_or(60))
}

/// 条目行高：三行文字随字号增长，默认字号下等于 `ITEM_ROW_HEIGHT`。
fn item_row_height(cx: &App) -> Pixels {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let text = tokens.typography.sm.line_height
        + tokens.typography.xs.line_height * 2.
        + tokens.spacing.xxs * 2.
        + ITEM_ROW_PADDING_Y * 2.;
    text.max(ITEM_ROW_HEIGHT)
}

pub struct RssView {
    translator: Entity<Translator>,
    controller: RssController,
    search: Entity<InputState>,
    /// 搜索框当前 placeholder；语言切换后在 render 中同步（InputState 只在构造时取一次）。
    search_placeholder: SharedString,
    oldest_first: bool,
    /// 手动点击的最短「进行中」保持（本机 RPC 一帧内返回时转圈也可见）。
    refresh_hold: BusyHold,
    read_hold: BusyHold,
    retry_hold: BusyHold,
    last_error: Option<String>,
    feedback: Option<String>,
    load_error: bool,
    /// 与 shell 顶栏共享，切换页面后仍保留独立宽度与手动收起状态。
    sidebar: Entity<SidebarState>,
}

impl RssView {
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn RssPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        let search_placeholder =
            SharedString::from(translator.read(cx).text("rssSearchHint").to_owned());
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(search_placeholder.clone()));
        cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let sidebar = cx.new(|cx| {
            SidebarState::new(
                px(SOURCE_COLUMN_WIDTH),
                px(SOURCE_COLUMN_MIN_WIDTH)..px(SOURCE_COLUMN_MAX_WIDTH),
                cx,
            )
        });
        cx.observe(&sidebar, |_, _, cx| cx.notify()).detach();
        Self::spawn_fetch_age_ticker(cx);
        Self {
            translator,
            controller: RssController::new(port),
            search,
            search_placeholder,
            oldest_first: false,
            refresh_hold: BusyHold::default(),
            read_hold: BusyHold::default(),
            retry_hold: BusyHold::default(),
            last_error: None,
            feedback: None,
            load_error: false,
            sidebar,
        }
    }

    /// 「上次抓取 N 分钟前」由时钟推进，不在任何被追踪的状态里：retained 渲染只在依赖变化时
    /// 重建本视图，必须在相对时间跨档时主动 notify。视图释放后循环随 `update` 失败结束。
    fn spawn_fetch_age_ticker(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut delay = FETCH_AGE_RECHECK;
            loop {
                cx.background_executor().timer(delay).await;
                let Ok(next) = this.update(cx, |this, cx| {
                    let now = now_unix_secs();
                    let next = this
                        .controller
                        .sources()
                        .iter()
                        .filter(|source| source.last_success_at > 0)
                        .map(|source| fetch_age_changes_in(now - source.last_success_at))
                        .min();
                    if next.is_some() {
                        cx.notify();
                    }
                    next.map_or(FETCH_AGE_RECHECK, |next| next.min(FETCH_AGE_RECHECK))
                }) else {
                    // RSS 页已释放，结束定时刷新。
                    return;
                };
                delay = next;
            }
        })
        .detach();
    }

    /// 供 shell 顶栏与订阅源导航共享的侧栏状态。
    pub fn sidebar_state(&self) -> Entity<SidebarState> {
        self.sidebar.clone()
    }

    fn t(&self, key: &str, cx: &App) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }
    fn with(&self, key: &str, args: &[(&str, &str)], cx: &App) -> String {
        self.translator.read(cx).text_with(key, args)
    }
    fn fail(&mut self, cx: &mut Context<Self>) {
        self.last_error = Some(self.t("localServiceActionFailed", cx).to_string());
        cx.notify();
    }

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.controller.replace_snapshot(snapshot);
        self.last_error = None;
        self.load_error = false;
        self.fetch_items(cx);
        cx.notify();
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let before = self.controller.selected_source.clone();
        let changed = matches!(
            event,
            ServiceEvent::Agent(
                AgentEvent::DaemonSnapshotReplaced(_)
                    | AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(_))
                    | AgentEvent::DaemonConnectionChanged(true)
                    | AgentEvent::Daemon(DaemonEvent::RssChanged { .. })
            )
        );
        let item_event = match event {
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::Engine(
                WsServerMsg::RssItemsChanged {
                    source_id, items, ..
                },
            ))) if before.as_deref() == Some(source_id) => {
                let before_items: HashSet<_> = self
                    .controller
                    .items
                    .iter()
                    .map(|item| item.guid.as_str())
                    .collect();
                Some(
                    items
                        .iter()
                        .filter(|item| !before_items.contains(item.guid.as_str()))
                        .count(),
                )
            }
            _ => None,
        };
        let state_changed = self.controller.apply_event(event);
        let refetch = before != self.controller.selected_source || changed;
        if refetch {
            self.fetch_items(cx);
        }
        if let Some(count) = item_event.filter(|n| *n > 0) {
            self.feedback = Some(self.with("rssItemsUpdated", &[("n", &count.to_string())], cx));
        }
        // 无关事件（如其它任务的下载进度帧）不重绘，保住 retained 渲染的复用。
        if state_changed || refetch || item_event.is_some() {
            cx.notify();
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.controller.mark_stale();
        self.last_error = Some(self.t("localServiceDisconnected", cx).to_string());
        cx.notify();
    }

    fn fetch_items(&mut self, cx: &mut Context<Self>) {
        let Some(load) = self.controller.load_items() else {
            return;
        };
        self.load_error = false;
        cx.spawn(async move |view, cx| {
            let result = load.future.await;

            let Ok(()) = view.update(cx, |this, cx| {
                let current = this.controller.selected_source.as_deref() == Some(&load.source_id)
                    && this.controller.revision == load.revision;
                if current {
                    this.last_error = None;
                    if !this
                        .controller
                        .finish_load(&load.source_id, load.revision, result)
                    {
                        this.load_error = true;
                        this.fail(cx);
                    }
                    cx.notify();
                }
            }) else {
                // 订阅视图已释放，结束回调，不再提交后续操作。
                return;
            };
        })
        .detach();
    }

    fn open_editor(
        &self,
        source: Option<RssSourceDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.controller.stale {
            editor::open_editor(
                self.translator.clone(),
                self.controller.port.clone(),
                source,
                self.controller.queues.clone(),
                self.controller.plugins.clone(),
                window,
                cx,
            );
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.controller.stale || self.controller.refresh_busy {
            return;
        }
        let Some(id) = self.controller.selected_source.clone() else {
            return;
        };
        self.controller.refresh_busy = true;
        let epoch = self.controller.action_epoch;
        let future = self.controller.refresh(id.clone());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = future.await;

            let Ok(()) = view.update(cx, |this, cx| {
                if this.controller.action_epoch != epoch
                    || this.controller.selected_source.as_deref() != Some(&id)
                {
                    return;
                }
                this.controller.refresh_busy = false;
                match &result {
                    // daemon 对「已在抓取」与「订阅不存在」都回 NotFound：前者不是故障，
                    // 后者会由 RssChanged 事件移除该源，这里都不报连接失败，只刷新条目。
                    Err(error) if error.code == ApplicationErrorCode::NotFound => {
                        this.fetch_items(cx);
                    }
                    Err(_) => this.fail(cx),
                    Ok(_) => this.fetch_items(cx),
                }
                cx.notify();
            }) else {
                // 订阅视图已释放，结束回调，不再提交后续操作。
                return;
            };
        })
        .detach();
    }

    fn read_all(&mut self, cx: &mut Context<Self>) {
        if self.controller.stale || self.controller.read_busy {
            return;
        }
        let Some(id) = self.controller.selected_source.clone() else {
            return;
        };
        self.controller.read_busy = true;
        let epoch = self.controller.action_epoch;
        let future = self.controller.item_action(&id, "", "readAll");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = future.await;

            let Ok(()) = view.update(cx, |this, cx| {
                if this.controller.action_epoch != epoch
                    || this.controller.selected_source.as_deref() != Some(&id)
                {
                    return;
                }
                this.controller.read_busy = false;
                if result.is_err() {
                    this.fail(cx);
                } else {
                    this.fetch_items(cx);
                }
                cx.notify();
            }) else {
                // 订阅视图已释放，结束回调，不再提交后续操作。
                return;
            };
        })
        .detach();
    }

    fn act(&mut self, guids: Vec<String>, action: &'static str, cx: &mut Context<Self>) {
        if self.controller.stale {
            return;
        }
        let Some(source_id) = self.controller.selected_source.clone() else {
            return;
        };
        let guids: Vec<_> = guids
            .into_iter()
            .filter(|guid| self.controller.begin_action(&source_id, guid))
            .collect();
        if guids.is_empty() {
            return;
        }
        let epoch = self.controller.action_epoch;
        let port = self.controller.port.clone();
        self.last_error = None;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let mut done = 0;
            let mut failed = 0;
            for guid in guids {
                let result = port
                    .call(
                        method::DAEMON_RSS_ITEM_ACTION,
                        serde_json::json!({"sourceId": source_id, "guid": guid, "action": action}),
                    )
                    .await;
                if result.is_ok() {
                    done += 1;
                } else {
                    failed += 1;
                }

                let Ok(()) = view.update(cx, |this, cx| {
                    this.controller
                        .finish_action(&source_id, &guid, epoch, result.is_ok());
                    cx.notify();
                }) else {
                    // 订阅视图已释放，结束回调，不再提交后续操作。
                    return;
                };
            }

            let Ok(()) = view.update(cx, |this, cx| {
                if this.controller.action_epoch == epoch
                    && this.controller.selected_source.as_deref() == Some(&source_id)
                {
                    if failed > 0 {
                        this.fail(cx);
                    }
                    this.feedback = Some(if done == 1 && failed == 0 && action == "download" {
                        this.t("rssTaskCreated", cx).to_string()
                    } else {
                        this.with(
                            "rssBatchResult",
                            &[("done", &done.to_string()), ("failed", &failed.to_string())],
                            cx,
                        )
                    });
                    if done > 0 {
                        this.fetch_items(cx);
                    }
                }
                cx.notify();
            }) else {
                // 订阅视图已释放，结束回调，不再提交后续操作。
                return;
            };
        })
        .detach();
    }

    fn confirm_delete(
        &mut self,
        source: RssSourceDto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.controller.stale || self.controller.delete_busy {
            return;
        }
        let title = self.t("rssDeleteSource", cx);
        let name = display_name(&source);
        let description =
            SharedString::from(self.with("rssDeleteConfirmDesc", &[("name", &name)], cx));
        let cancel = self.t("cancel", cx);
        let view = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let view = view.clone();
            let source_id = source.source_id.clone();
            alert
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(description.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel.clone()),
                    title.clone(),
                    fluxdown_ui_components::DialogIntent::Destructive,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    // 已释放或不可提交的订阅页不能让确认框报告成功。
                    view.update(cx, |this, cx| {
                        if this.controller.stale || this.controller.delete_busy {
                            return false;
                        }
                        this.controller.delete_busy = true;
                        let future = this.controller.delete(source_id.clone());
                        cx.spawn(async move |view, cx| {
                            let result = future.await;
                            let Ok(()) = view.update(cx, |this, cx| {
                                this.controller.delete_busy = false;
                                if result.is_err() {
                                    this.fail(cx);
                                }
                                cx.notify();
                            }) else {
                                // 订阅页已释放，停止回写删除结果。
                                return;
                            };
                        })
                        .detach();
                        cx.notify();
                        true
                    })
                    .unwrap_or(false)
                })
        });
    }
}

fn display_name(source: &RssSourceDto) -> String {
    if source.name.trim().is_empty() {
        source.url.clone()
    } else {
        source.name.clone()
    }
}

fn date_text(timestamp: i64) -> String {
    if timestamp <= 0 {
        return String::new();
    }
    // UTC civil date from Unix days, without a dependency solely for list-row formatting.
    let days = timestamp.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let hours = timestamp.rem_euclid(86_400) / 3_600;
    let minutes = timestamp.rem_euclid(3_600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02} UTC")
}

fn bytes_text(bytes: i64) -> String {
    if bytes <= 0 {
        return String::new();
    }
    let mut size = bytes as f64;
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB"] {
        if size < 1024. {
            break;
        }
        size /= 1024.;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {unit}")
    }
}

impl RssView {
    fn status_line(&self, source: &RssSourceDto, cx: &App) -> String {
        let interval = if source.interval_minutes > 0 {
            source.interval_minutes
        } else {
            30
        };
        let mut parts = vec![self.with("rssEveryMinutes", &[("n", &interval.to_string())], cx)];
        if self.controller.refresh_busy
            && self.controller.selected_source.as_deref() == Some(&source.source_id)
        {
            parts.push(self.t("rssRefreshing", cx).to_string());
        } else if !source.last_error.is_empty() {
            parts.push(self.with(
                "rssFailedTimes",
                &[("n", &source.fail_count.to_string())],
                cx,
            ));
            parts.push(source.last_error.clone());
        } else if source.last_success_at > 0 {
            let (key, n) = fetch_age_label(now_unix_secs().saturating_sub(source.last_success_at));
            let when = self.with(key, &[("n", &n.to_string())], cx);
            parts.push(self.with("rssLastFetch", &[("when", &when)], cx));
        } else {
            parts.push(self.t("rssNeverFetched", cx).to_string());
        }
        parts.push(
            self.t(
                if source.auto_download {
                    "rssAutoDownloadOn"
                } else {
                    "rssCollectMode"
                },
                cx,
            )
            .to_string(),
        );
        parts.join(" · ")
    }

    fn render_source(
        &self,
        source: RssSourceDto,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let colors = tokens.colors;
        let selected = self.controller.selected_source.as_deref() == Some(&source.source_id);
        let unhealthy = !source.last_error.is_empty();
        let name = display_name(&source);
        let status = self.status_line(&source, cx);
        let id = source.source_id.clone();
        let count = source.unread_count;
        let unread =
            SharedString::from(self.with("rssUnreadCount", &[("n", &count.to_string())], cx));
        let badge_id = SharedString::from(format!("rss-unread-{id}"));
        let (background, hover) = if selected {
            (extended.colors.nav_selected, extended.colors.nav_selected)
        } else {
            (
                transparent(extended.colors.nav_hover),
                extended.colors.nav_hover,
            )
        };
        div()
            .id(SharedString::from(format!("rss-source-{id}")))
            .w_full()
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs + tokens.spacing.xxs)
            .rounded(tokens.radius.md)
            .cursor_pointer()
            .bg(background)
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if this.controller.select_source(&id) {
                    this.fetch_items(cx);
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .child(Icon::new(FluxIcon::Rss).size(extended.icon.lg).text_color(
                        if unhealthy {
                            colors.destructive
                        } else {
                            nav_icon_color(selected, cx)
                        },
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(tokens.typography.sm.size)
                                    .line_height(tokens.typography.sm.line_height)
                                    .text_color(if selected {
                                        extended.colors.nav_selected_foreground
                                    } else {
                                        colors.muted_foreground
                                    })
                                    .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
                                    .child(name),
                            )
                            .child(
                                meta_text(cx)
                                    .truncate()
                                    .when(unhealthy, |this| this.text_color(colors.destructive))
                                    .child(status),
                            ),
                    )
                    .when(count > 0, |row| {
                        row.child(
                            div()
                                .id(badge_id)
                                .flex_none()
                                .tooltip(move |window, cx| tooltip_view(unread.clone(), window, cx))
                                .child(caption_number(count.to_string(), cx)),
                        )
                    }),
            )
    }

    fn render_item(&self, item: RssItemDto, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let status_width = theme.text_extent(STATUS_COLUMN_WIDTH);
        let row_height = item_row_height(cx);
        let extended = theme.extended().clone();
        let colors = tokens.colors;
        let guid = item.guid.clone();
        let source_id = item.source_id.clone();
        let busy = self
            .controller
            .busy
            .contains(&format!("{source_id}\0{guid}"));
        let selected = self.controller.selected.contains(&guid);
        let can_act = !self.controller.stale && !busy;
        let title = SharedString::from(item.title.clone());
        let status = self.t(self.controller.status_key(&item), cx);
        let reason_key = match item.reason.as_str() {
            "not_included" => Some("rssReasonNotIncluded"),
            "excluded" => Some("rssReasonExcluded"),
            "too_small" => Some("rssReasonTooSmall"),
            "too_large" => Some("rssReasonTooLarge"),
            "dup_episode" => Some("rssReasonDupEpisode"),
            "torrent_fetch_failed" => Some("rssReasonTorrentFetchFailed"),
            _ => None,
        };
        let date = date_text(item.pub_date);
        let size = bytes_text(item.enclosure_length);
        let label = self.t(
            if busy {
                "rssActionPreparing"
            } else if item.status == 1 {
                "rssActionRedownload"
            } else {
                "rssActionDownload"
            },
            cx,
        );
        let can_ignore = item.status == 0;
        let task_missing = item.status == 1
            && !self.controller.tasks_loading
            && self.controller.task_status(&item).is_none();
        let group = SharedString::from(format!("rss-item-{source_id}-{guid}"));
        let status_id = SharedString::from(format!("rss-status-{source_id}-{guid}"));
        let mut meta = Vec::new();
        if !date.is_empty() {
            meta.push(format!("{} · {date}", self.t("rssPublishedAt", cx)));
        }
        if !size.is_empty() {
            meta.push(size);
        }
        h_flex()
            .group(group.clone())
            .w_full()
            .h(row_height)
            .flex_none()
            .items_center()
            .gap(tokens.spacing.md)
            .px(tokens.spacing.lg)
            .map(|row| {
                if selected {
                    row.bg(colors.accent)
                } else {
                    row.hover(move |style| style.bg(extended.colors.row_hover))
                }
            })
            .child(
                div()
                    .id(SharedString::from(format!("rss-select-{source_id}-{guid}")))
                    .flex_none()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if !this.controller.selected.remove(&guid) {
                            this.controller.selected.insert(guid.clone());
                        }
                        cx.notify();
                    }))
                    .child(check_mark(
                        if selected {
                            CheckState::Checked
                        } else {
                            CheckState::Unchecked
                        },
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(tokens.spacing.xxs)
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "rss-title-{}-{}",
                                item.source_id, item.guid
                            )))
                            .w_full()
                            .truncate()
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .text_color(colors.foreground)
                            .tooltip(move |window, cx| tooltip_view(title.clone(), window, cx))
                            .child(item.title.clone()),
                    )
                    .when(!meta.is_empty(), |column| {
                        column.child(
                            meta_text(cx)
                                .truncate()
                                .font_features(tabular_numbers())
                                .child(meta.join(" · ")),
                        )
                    })
                    .when_some(reason_key, |column, key| {
                        column.child(meta_text(cx).truncate().child(self.t(key, cx)))
                    }),
            )
            .child(
                // 状态列固定宽度，徽标靠左贴近条目内容；文字被截断时悬停看全文。
                h_flex()
                    .w(status_width)
                    .flex_none()
                    .min_w_0()
                    .child(status_badge(
                        status_id,
                        status,
                        task_missing.then_some(colors.destructive),
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .flex_none()
                    .justify_end()
                    .gap(tokens.spacing.xxs)
                    // 行操作悬停时出现；选中或处理中时常显，保证状态可见。
                    .when(!selected && !busy, |actions| {
                        actions
                            .opacity(0.)
                            .group_hover(group, |style| style.opacity(1.))
                    })
                    .child(
                        Button::new(SharedString::from(format!(
                            "rss-download-{}-{}",
                            item.source_id, item.guid
                        )))
                        .ghost()
                        .label(label)
                        .control(cx)
                        .disabled(!can_act)
                        .on_click(cx.listener({
                            let guid = item.guid.clone();
                            move |this, _, _, cx| this.act(vec![guid.clone()], "download", cx)
                        })),
                    )
                    .when(can_ignore, |row| {
                        row.child(
                            Button::new(SharedString::from(format!(
                                "rss-ignore-{}-{}",
                                item.source_id, item.guid
                            )))
                            .ghost()
                            .label(self.t("rssActionIgnore", cx))
                            .control(cx)
                            .disabled(!can_act)
                            .on_click(cx.listener({
                                let guid = item.guid.clone();
                                move |this, _, _, cx| this.act(vec![guid.clone()], "ignore", cx)
                            })),
                        )
                    }),
            )
    }

    /// 空状态：32px 图标 + 标题 sm MEDIUM + 说明 xs 二级文字，居中。
    fn render_empty(
        &self,
        icon: FluxIcon,
        icon_color: Option<Hsla>,
        title: SharedString,
        description: Option<SharedString>,
        cx: &App,
    ) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.xl)
            .child(
                Icon::new(icon)
                    .size(px(32.))
                    .text_color(icon_color.unwrap_or(theme.extended().colors.text_tertiary)),
            )
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.colors.foreground)
                    .child(title),
            )
            .when_some(description, |this, description| {
                this.child(
                    div()
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_center()
                        .text_color(tokens.colors.muted_foreground)
                        .child(description),
                )
            })
    }

    fn render_empty_keys(&self, title: &str, description: &str, cx: &App) -> Div {
        self.render_empty(
            FluxIcon::Rss,
            None,
            self.t(title, cx),
            Some(self.t(description, cx)),
            cx,
        )
    }

    /// 工具按钮图标；进行中时换成同尺寸 spinner（toolbar_action_button 本身没有 loading）。
    fn tool_icon(icon: FluxIcon, busy: bool, cx: &App) -> AnyElement {
        let icon_size = active_theme(cx).extended().icon.md;
        if busy {
            Spinner::new().with_size(icon_size).into_any_element()
        } else {
            Icon::new(icon).size(icon_size).into_any_element()
        }
    }

    /// 面板头工具按钮（chrome 风格图标按钮 + 悬浮提示）。
    fn tool_button(
        id: &'static str,
        label: SharedString,
        icon: AnyElement,
        destructive: bool,
        disabled: bool,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tooltip = label.clone();
        div()
            .id(SharedString::from(format!("{id}-tooltip")))
            .flex_none()
            .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
            .child(
                toolbar_action_button(id, label, icon, destructive, disabled, cx)
                    .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx))),
            )
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let caption = theme.extended().caption;
        let chrome = theme.extended().colors.chrome;
        let tertiary = theme.extended().colors.text_tertiary;
        let stale = self.controller.stale;
        v_flex()
            .size_full()
            .min_h_0()
            .bg(chrome)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(spacing.xs)
                    .pl(spacing.lg)
                    .pr(spacing.sm)
                    .pt(spacing.md)
                    .pb(spacing.sm)
                    .child(
                        div()
                            .text_size(caption.size)
                            .line_height(caption.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tertiary)
                            .child(self.t("rssSubscriptions", cx)),
                    )
                    .child(caption_number(
                        self.controller.sources.len().to_string(),
                        cx,
                    ))
                    .child(div().flex_1())
                    .child(Self::tool_button(
                        "rss-add-source",
                        self.t("rssAddSource", cx),
                        Self::tool_icon(FluxIcon::Plus, false, cx),
                        false,
                        stale,
                        |this, window, cx| this.open_editor(None, window, cx),
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px(spacing.sm)
                    .pb(spacing.sm)
                    .gap(spacing.xxs)
                    .when(self.controller.sources.is_empty(), |list| {
                        list.child(
                            meta_text(cx)
                                .px(spacing.sm)
                                .py(spacing.lg)
                                .child(self.t("rssSidebarEmptyHint", cx)),
                        )
                    })
                    .children(
                        self.controller
                            .sources
                            .iter()
                            .cloned()
                            .map(|source| self.render_source(source, cx)),
                    )
                    .overflow_y_scrollbar(),
            )
    }
}

/// 元信息文字：xs + 三级文字色。
fn meta_text(cx: &App) -> Div {
    let theme = active_theme(cx);
    let xs = theme.tokens().typography.xs;
    div()
        .text_size(xs.size)
        .line_height(xs.line_height)
        .text_color(theme.extended().colors.text_tertiary)
}

/// 中性小徽标（条目状态）；`tone` 仅用于需要提示的状态（如任务丢失）。
/// 列宽固定，长文案会被截断，悬停提示展示完整状态文字。
fn status_badge(
    id: SharedString,
    text: SharedString,
    tone: Option<Hsla>,
    cx: &App,
) -> Stateful<Div> {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let caption = theme.extended().caption;
    let full = text.clone();
    div()
        .id(id)
        .min_w_0()
        .max_w_full()
        .truncate()
        .px(tokens.spacing.xs + tokens.spacing.xxs)
        .py(px(1.))
        .rounded(tokens.radius.sm)
        .bg(tokens.colors.muted)
        .text_size(caption.size)
        .line_height(caption.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(tone.unwrap_or(tokens.colors.muted_foreground))
        .tooltip(move |window, cx| tooltip_view(full.clone(), window, cx))
        .child(text)
}

fn transparent(color: Hsla) -> Hsla {
    Hsla { a: 0., ..color }
}

impl Render for RssView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 语言切换后同步搜索框 placeholder。
        let placeholder = self.t("rssSearchHint", cx);
        if placeholder != self.search_placeholder {
            self.search_placeholder = placeholder.clone();
            self.search.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
            });
        }
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let control_height = theme.density().control;
        let search_width = theme.text_extent(SEARCH_WIDTH);
        let colors = tokens.colors;
        let xs = tokens.typography.xs;
        let stale = self.controller.stale;
        let source = self
            .controller
            .sources
            .iter()
            .find(|source| {
                Some(source.source_id.as_str()) == self.controller.selected_source.as_deref()
            })
            .cloned();
        let query = self.search.read(cx).value().trim().to_lowercase();
        let visible = self.controller.visible_indices(&query, self.oldest_first);
        let all_visible = !visible.is_empty()
            && visible.iter().all(|&index| {
                self.controller
                    .selected
                    .contains(&self.controller.items[index].guid)
            });
        let any_visible = visible.iter().any(|&index| {
            self.controller
                .selected
                .contains(&self.controller.items[index].guid)
        });
        let selected_count = self.controller.selected.len();
        let selected_guids: Vec<_> = self.controller.selected.iter().cloned().collect();
        let selected_for_ignore: Vec<_> = self
            .controller
            .items
            .iter()
            .filter(|item| item.status == 0 && self.controller.selected.contains(&item.guid))
            .map(|item| item.guid.clone())
            .collect();
        let mut main = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(colors.surface)
            .when_some(self.last_error.clone(), |page, error| {
                page.child(
                    div()
                        .flex_none()
                        .px(tokens.spacing.lg)
                        .py(tokens.spacing.xs + tokens.spacing.xxs)
                        .bg(colors.destructive.opacity(0.08))
                        .text_size(xs.size)
                        .line_height(xs.line_height)
                        .text_color(colors.destructive)
                        .child(error),
                )
            })
            .when_some(self.feedback.clone(), |page, feedback| {
                page.child(
                    h_flex()
                        .flex_none()
                        .items_center()
                        .px(tokens.spacing.lg)
                        .py(tokens.spacing.xxs)
                        .gap(tokens.spacing.md)
                        .bg(colors.accent)
                        .child(
                            div()
                                .flex_1()
                                .text_size(xs.size)
                                .line_height(xs.line_height)
                                .text_color(colors.foreground)
                                .child(feedback),
                        )
                        .child(
                            Button::new("rss-dismiss-feedback")
                                .ghost()
                                .label(self.t("close", cx))
                                .control(cx)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.feedback = None;
                                    cx.notify();
                                })),
                        ),
                )
            });
        if let Some(source) = source {
            let status = self.status_line(&source, cx);
            let title = SharedString::from(display_name(&source));
            let unhealthy = !source.last_error.is_empty();
            let source_for_manage = source.clone();
            let source_for_delete = source.clone();
            let tooltip_title = title.clone();
            main = main.child(
                v_flex()
                    .flex_none()
                    .px(tokens.spacing.lg)
                    .py(tokens.spacing.md)
                    .gap(tokens.spacing.md)
                    .border_b(extended.stroke.thin)
                    .border_color(extended.colors.hairline)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.sm)
                            .child(Icon::new(FluxIcon::Rss).size(extended.icon.lg).text_color(
                                if unhealthy {
                                    colors.destructive
                                } else {
                                    colors.muted_foreground
                                },
                            ))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .id("rss-selected-source-title")
                                            .truncate()
                                            .text_size(tokens.typography.sm.size)
                                            .line_height(tokens.typography.sm.line_height)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors.foreground)
                                            .tooltip(move |window, cx| {
                                                tooltip_view(tooltip_title.clone(), window, cx)
                                            })
                                            .child(title),
                                    )
                                    .child(
                                        meta_text(cx)
                                            .truncate()
                                            .font_features(tabular_numbers())
                                            .when(unhealthy, |this| {
                                                this.text_color(colors.destructive)
                                            })
                                            .child(status),
                                    ),
                            )
                            .child(Self::tool_button(
                                "rss-manage",
                                self.t("rssManageTitle", cx),
                                Self::tool_icon(FluxIcon::Settings, false, cx),
                                false,
                                stale,
                                move |this, window, cx| {
                                    this.open_editor(Some(source_for_manage.clone()), window, cx)
                                },
                                cx,
                            ))
                            .child(Self::tool_button(
                                "rss-delete",
                                self.t("rssDeleteSource", cx),
                                Self::tool_icon(FluxIcon::Trash2, self.controller.delete_busy, cx),
                                true,
                                stale || self.controller.delete_busy,
                                move |this, window, cx| {
                                    this.confirm_delete(source_for_delete.clone(), window, cx)
                                },
                                cx,
                            )),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .gap(tokens.spacing.xs)
                            .child(
                                Button::new("rss-sort")
                                    .ghost()
                                    .icon(FluxIcon::ArrowUpDown)
                                    .label(self.t(
                                        if self.oldest_first {
                                            "rssSortOldest"
                                        } else {
                                            "rssSortNewest"
                                        },
                                        cx,
                                    ))
                                    .control(cx)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.oldest_first = !this.oldest_first;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("rss-read-all")
                                    .ghost()
                                    .icon(FluxIcon::Check)
                                    .label(self.t("rssMarkAllRead", cx))
                                    .control(cx)
                                    .busy(self.controller.read_busy || self.read_hold.is_held())
                                    .disabled(
                                        stale
                                            || self.controller.read_busy
                                            || self.read_hold.is_held(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.read_hold.start(cx);
                                        this.read_all(cx)
                                    })),
                            )
                            .child(
                                Button::new("rss-refresh")
                                    .ghost()
                                    .icon(FluxIcon::RotateCw)
                                    .label(self.t(
                                        if self.controller.refresh_busy
                                            || self.refresh_hold.is_held()
                                        {
                                            "rssRefreshing"
                                        } else {
                                            "rssRefreshNow"
                                        },
                                        cx,
                                    ))
                                    .control(cx)
                                    .busy(
                                        self.controller.refresh_busy || self.refresh_hold.is_held(),
                                    )
                                    .disabled(
                                        stale
                                            || self.controller.refresh_busy
                                            || self.refresh_hold.is_held(),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_hold.start(cx);
                                        this.refresh(cx)
                                    })),
                            )
                            .child(div().flex_1().min_w(tokens.spacing.sm))
                            .child(
                                div().w(search_width).child(
                                    Input::new(&self.search).control(cx).prefix(
                                        Icon::new(FluxIcon::Search)
                                            .size(extended.icon.md)
                                            .text_color(extended.colors.text_tertiary),
                                    ),
                                ),
                            ),
                    ),
            );
            if !self.controller.items.is_empty() || selected_count > 0 {
                let select_state = if all_visible {
                    CheckState::Checked
                } else if any_visible {
                    CheckState::Indeterminate
                } else {
                    CheckState::Unchecked
                };
                main = main.child(
                    h_flex()
                        .flex_wrap()
                        .flex_none()
                        .items_center()
                        .gap(tokens.spacing.sm)
                        .px(tokens.spacing.lg)
                        .py(tokens.spacing.xxs)
                        .border_b(extended.stroke.thin)
                        .border_color(extended.colors.hairline)
                        .child(
                            h_flex()
                                .id("rss-select-visible")
                                .h(control_height)
                                .items_center()
                                .gap(tokens.spacing.md)
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let query = this.search.read(cx).value().to_string();
                                    this.controller.select_visible(
                                        &query,
                                        this.oldest_first,
                                        !all_visible,
                                    );
                                    cx.notify();
                                }))
                                .child(check_mark(select_state, cx))
                                .child(
                                    div()
                                        .text_size(xs.size)
                                        .line_height(xs.line_height)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(extended.colors.text_tertiary)
                                        .child(self.t("rssSelectVisible", cx)),
                                ),
                        )
                        .child(
                            Button::new("rss-select-all-downloadable")
                                .ghost()
                                .label(self.t("rssSelectAllDownloadable", cx))
                                .control(cx)
                                .disabled(
                                    stale
                                        || self.controller.loading
                                        || self
                                            .controller
                                            .bulk_download_candidates()
                                            .next()
                                            .is_none(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.controller.selected = this
                                        .controller
                                        .bulk_download_candidates()
                                        .map(str::to_owned)
                                        .collect();
                                    cx.notify();
                                })),
                        )
                        .when(selected_count > 0, |bar| {
                            bar.child(
                                div()
                                    .text_size(xs.size)
                                    .line_height(xs.line_height)
                                    .font_features(tabular_numbers())
                                    .text_color(colors.muted_foreground)
                                    .child(self.with(
                                        "rssSelectedCount",
                                        &[("n", &selected_count.to_string())],
                                        cx,
                                    )),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("rss-clear-selection")
                                    .ghost()
                                    .label(self.t("rssClearSelection", cx))
                                    .control(cx)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.controller.selected.clear();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("rss-ignore-selected")
                                    .outline()
                                    .label(self.t("rssIgnoreSelected", cx))
                                    .control(cx)
                                    .disabled(
                                        stale
                                            || selected_for_ignore.iter().all(|guid| {
                                                self.controller.busy.contains(&format!(
                                                    "{}\0{guid}",
                                                    source.source_id
                                                ))
                                            }),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.act(selected_for_ignore.clone(), "ignore", cx)
                                    })),
                            )
                            .child(
                                Button::new("rss-download-selected")
                                    .primary()
                                    .label(self.t("rssDownloadSelected", cx))
                                    .control(cx)
                                    .disabled(
                                        stale
                                            || selected_guids.iter().all(|guid| {
                                                self.controller.busy.contains(&format!(
                                                    "{}\0{guid}",
                                                    source.source_id
                                                ))
                                            }),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.act(selected_guids.clone(), "download", cx)
                                    })),
                            )
                        }),
                );
            }
            if self.controller.loading && visible.is_empty() {
                main = main.child(self.render_empty_keys(
                    "rssEmptyFetching",
                    "rssEmptyFetchingHint",
                    cx,
                ));
            } else if visible.is_empty() {
                if !query.is_empty() && !self.controller.items.is_empty() {
                    main = main.child(self.render_empty(
                        FluxIcon::Search,
                        None,
                        SharedString::from(self.with("rssNoMatch", &[("query", &query)], cx)),
                        None,
                        cx,
                    ));
                } else if unhealthy || self.load_error {
                    let retry_fetch = self.load_error;
                    let source_for_manage = source.clone();
                    main = main.child(
                        self.render_empty(
                            FluxIcon::CircleAlert,
                            Some(colors.destructive),
                            self.t("rssEmptyError", cx),
                            Some(self.t("rssEmptyErrorHint", cx)),
                            cx,
                        )
                        .child(
                            h_flex()
                                .pt(tokens.spacing.xs)
                                .gap(tokens.spacing.sm)
                                .child(
                                    Button::new("rss-empty-retry")
                                        .outline()
                                        .label(self.t("rssEmptyRetry", cx))
                                        .control(cx)
                                        .busy(
                                            self.retry_hold.is_held()
                                                || if retry_fetch {
                                                    self.controller.loading
                                                } else {
                                                    self.controller.refresh_busy
                                                },
                                        )
                                        .disabled(
                                            stale
                                                || self.retry_hold.is_held()
                                                || if retry_fetch {
                                                    self.controller.loading
                                                } else {
                                                    self.controller.refresh_busy
                                                },
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.retry_hold.start(cx);
                                            if retry_fetch {
                                                this.fetch_items(cx);
                                                cx.notify();
                                            } else {
                                                this.refresh(cx);
                                            }
                                        })),
                                )
                                .child(
                                    Button::new("rss-empty-manage")
                                        .outline()
                                        .label(self.t("rssCheckConfig", cx))
                                        .control(cx)
                                        .disabled(stale)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_editor(
                                                Some(source_for_manage.clone()),
                                                window,
                                                cx,
                                            )
                                        })),
                                ),
                        ),
                    );
                } else if !source.seeded && source.enabled {
                    main = main.child(self.render_empty_keys(
                        "rssEmptyFetching",
                        "rssEmptyFetchingHint",
                        cx,
                    ));
                } else {
                    main = main.child(self.render_empty_keys("rssEmptyTitle", "rssEmptyDesc", cx));
                }
            } else {
                let count = visible.len();
                main = main.child(
                    uniform_list(
                        "rss-items",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|index| {
                                    this.render_item(
                                        this.controller.items[visible[index]].clone(),
                                        cx,
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .flex_1()
                    .min_h_0()
                    .w_full(),
                );
            }
        } else {
            main = main.child(
                self.render_empty_keys("rssAddSource", "rssSidebarEmptyHint", cx)
                    .child(
                        Button::new("rss-empty-add-source")
                            .primary()
                            .icon(FluxIcon::Plus)
                            .label(self.t("rssAddSource", cx))
                            .control(cx)
                            .disabled(stale)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_editor(None, window, cx)
                            })),
                    ),
            );
        }
        let mut panel = SidebarPanel::new("rss-content", &self.sidebar, main, window, cx);
        if panel.is_sidebar_visible() {
            panel = panel.sidebar(self.render_sidebar(cx));
        }
        panel
    }
}

#[cfg(test)]
mod tests {
    use super::{fetch_age_changes_in, fetch_age_label};

    #[test]
    fn fetch_age_refresh_fires_exactly_when_the_label_changes() {
        for age in [
            0,
            59,
            60,
            61,
            3_599,
            3_600,
            5 * 3_600 + 7,
            86_399,
            86_400,
            3 * 86_400 + 1,
        ] {
            let delay = i64::try_from(fetch_age_changes_in(age).as_secs()).unwrap_or(i64::MAX);
            assert!(delay > 0, "age {age}");
            assert_eq!(
                fetch_age_label(age + delay - 1),
                fetch_age_label(age),
                "age {age}"
            );
            assert_ne!(
                fetch_age_label(age + delay),
                fetch_age_label(age),
                "age {age}"
            );
        }
    }
}
