//! 队列管理窗口：左列队列列表，右侧表单编辑（新建 / 更新 / 定时 / 启停 / 删除）。

use std::sync::Arc;

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, DaemonEvent, LATER_QUEUE_ID, MAIN_QUEUE_ID, MAX_TASK_SEGMENTS,
    QueueDto, ServiceEvent,
};
use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, check_row, field_error, field_hint, form as form_layout, form_field,
    form_row, input_with_action, option_group, option_row, sidebar_navigation_button,
    toolbar_action_button, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, App, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};

use crate::components::threads_risk::{parse_segments_input, threads_risk_callout};
use crate::controller::{DownloadsCommand, DownloadsPort, QueueFields};

/// 左列队列列表宽度。
const LIST_WIDTH: f32 = 184.;
/// 队列运行状态圆点直径。
const STATUS_DOT_SIZE: gpui::Pixels = px(6.);
/// 时 / 分下拉菜单最大高度（24 小时项需滚动）。
const TIME_MENU_MAX_HEIGHT: gpui::Pixels = px(280.);
/// 分钟下拉的步长。
const MINUTE_STEP: u16 = 5;

/// 队列默认线程数上限（与 `download.default_segments` 的范围一致）。
const MAX_SEGMENTS: i32 = MAX_TASK_SEGMENTS;

/// 空串 → 0；否则须为 `0..=max` 内的整数，非法（非数字 / 负数 / 溢出 / 超限）→ `None`。
fn parse_non_negative(input: &str, max: i32) -> Option<i32> {
    let input = input.trim();
    if input.is_empty() {
        return Some(0);
    }
    let value = i32::try_from(input.parse::<i64>().ok()?).ok()?;
    (0..=max).contains(&value).then_some(value)
}

/// 星期位掩码单日切换：`bit_index` 0 = 周一 … 6 = 周日。
fn toggle_day_bit(days: i32, bit: i32, checked: bool) -> i32 {
    if checked { days | bit } else { days & !bit }
}

/// 解析 daemon 的 `HH:MM` 为当日分钟数；空串或非法值视为不定时。
fn parse_time(text: &str) -> Option<u16> {
    let (hours, minutes) = text.trim().split_once(':')?;
    let hours: u16 = hours.parse().ok()?;
    let minutes: u16 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// 当日分钟数 → daemon wire `HH:MM`；`None` → 空串（不定时）。
fn format_time(time: Option<u16>) -> String {
    time.map(|minutes| format!("{:02}:{:02}", minutes / 60, minutes % 60))
        .unwrap_or_default()
}

/// 分钟候选：按步长取整点，外加当前值（旧数据可能不是步长整数倍），升序去重。
fn minute_choices(current: u16) -> Vec<u16> {
    let mut choices: Vec<u16> = (0..60).step_by(usize::from(MINUTE_STEP)).collect();
    if let Err(index) = choices.binary_search(&current) {
        choices.insert(index, current);
    }
    choices
}

/// 定时字段：启动 / 停止。
#[derive(Clone, Copy)]
enum ScheduleSlot {
    Start,
    Stop,
}

/// 一份正在编辑的队列表单；`queue_id = None` 表示「新建」。
struct QueueForm {
    queue_id: Option<String>,
    is_running: bool,
    name: Entity<InputState>,
    max_concurrent: Entity<InputState>,
    speed_limit: Entity<InputState>,
    upload_limit: Entity<InputState>,
    save_dir: Entity<InputState>,
    segments: Entity<InputState>,
    /// 线程数输入变化即重绘本视图（风险提示随输入联动）。
    _segments_watch: Subscription,
    user_agent: Entity<InputState>,
    schedule_enabled: bool,
    /// 当日分钟数；`None` = 不定时。
    schedule_start: Option<u16>,
    schedule_stop: Option<u16>,
    /// bit0 = 周一 … bit6 = 周日。
    schedule_days: i32,
    picking_dir: bool,
}

impl QueueForm {
    fn blank(window: &mut Window, cx: &mut Context<QueueManagerView>) -> Self {
        let (segments, segments_watch) = watched_input("0".to_owned(), window, cx);
        Self {
            queue_id: None,
            is_running: true,
            name: cx.new(|cx| InputState::new(window, cx)),
            max_concurrent: cx.new(|cx| InputState::new(window, cx).default_value("0")),
            speed_limit: cx.new(|cx| InputState::new(window, cx).default_value("0")),
            upload_limit: cx.new(|cx| InputState::new(window, cx).default_value("0")),
            save_dir: cx.new(|cx| InputState::new(window, cx)),
            segments,
            _segments_watch: segments_watch,
            user_agent: cx.new(|cx| InputState::new(window, cx)),
            schedule_enabled: false,
            schedule_start: None,
            schedule_stop: None,
            schedule_days: 127,
            picking_dir: false,
        }
    }

    fn for_queue(
        queue: &QueueDto,
        window: &mut Window,
        cx: &mut Context<QueueManagerView>,
    ) -> Self {
        let (segments, segments_watch) =
            watched_input(queue.default_segments.to_string(), window, cx);
        Self {
            queue_id: Some(queue.queue_id.clone()),
            is_running: queue.is_running,
            name: cx.new(|cx| InputState::new(window, cx).default_value(queue.name.clone())),
            max_concurrent: cx.new(|cx| {
                InputState::new(window, cx).default_value(queue.max_concurrent.to_string())
            }),
            speed_limit: cx.new(|cx| {
                InputState::new(window, cx).default_value(queue.speed_limit_kbps.to_string())
            }),
            upload_limit: cx.new(|cx| {
                InputState::new(window, cx).default_value(queue.upload_limit_kbps.to_string())
            }),
            save_dir: cx.new(|cx| {
                InputState::new(window, cx).default_value(queue.default_save_dir.clone())
            }),
            segments,
            _segments_watch: segments_watch,
            user_agent: cx.new(|cx| {
                InputState::new(window, cx).default_value(queue.default_user_agent.clone())
            }),
            schedule_enabled: queue.schedule_enabled,
            schedule_start: parse_time(&queue.schedule_start),
            schedule_stop: parse_time(&queue.schedule_stop),
            schedule_days: queue.schedule_days,
            picking_dir: false,
        }
    }

    /// 内置队列（主队列 / 稍后下载）：引擎侧拒绝改名与删除。
    fn is_builtin(&self) -> bool {
        matches!(
            self.queue_id.as_deref(),
            Some(MAIN_QUEUE_ID) | Some(LATER_QUEUE_ID)
        )
    }

    /// 主队列恒运行：不可停止、不可定时（daemon 拒绝）。
    fn is_main(&self) -> bool {
        self.queue_id.as_deref() == Some(MAIN_QUEUE_ID)
    }

    fn slot_mut(&mut self, slot: ScheduleSlot) -> &mut Option<u16> {
        match slot {
            ScheduleSlot::Start => &mut self.schedule_start,
            ScheduleSlot::Stop => &mut self.schedule_stop,
        }
    }
}

/// 创建输入框并订阅其变更：每次输入都通知本视图重绘。
fn watched_input(
    value: String,
    window: &mut Window,
    cx: &mut Context<QueueManagerView>,
) -> (Entity<InputState>, Subscription) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
    let watch = cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    });
    (input, watch)
}

/// 新建队列后待补发的定时设置。
struct PendingSchedule {
    known_ids: Vec<String>,
    start_time: String,
    stop_time: String,
    days: i32,
}

/// 队列管理页面；实现 [`crate::session`]（app 侧）期望的三方法供 `attach` 驱动。
pub struct QueueManagerView {
    translator: Entity<Translator>,
    port: Arc<dyn DownloadsPort>,
    queues: Vec<QueueDto>,
    form: Option<QueueForm>,
    pending_schedule: Option<PendingSchedule>,
    error: Option<SharedString>,
    stale: bool,
}

impl QueueManagerView {
    #[must_use]
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn DownloadsPort>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        Self {
            translator,
            port,
            queues: Vec::new(),
            form: None,
            pending_schedule: None,
            error: None,
            stale: true,
        }
    }

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.absorb_queues(snapshot.daemon.queues.clone());
        self.stale = false;
        cx.notify();
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        match event {
            ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::QueuesChanged(queues))) => {
                self.absorb_queues(queues.clone());
                self.flush_pending_schedule(cx);
                cx.notify();
            }
            ServiceEvent::Agent(AgentEvent::DaemonSnapshotReplaced(snapshot))
            | ServiceEvent::Agent(AgentEvent::Daemon(DaemonEvent::SnapshotReplaced(snapshot))) => {
                self.absorb_queues(snapshot.queues.clone());
                self.stale = false;
                cx.notify();
            }
            ServiceEvent::Agent(AgentEvent::DaemonConnectionChanged(connected)) => {
                self.stale = !connected;
                cx.notify();
            }
            _ => {}
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        self.pending_schedule = None;
        cx.notify();
    }

    fn flush_pending_schedule(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending_schedule else {
            return;
        };
        let Some(queue_id) = self
            .queues
            .iter()
            .find(|queue| !pending.known_ids.contains(&queue.queue_id))
            .map(|queue| queue.queue_id.clone())
        else {
            return;
        };
        let Some(pending) = self.pending_schedule.take() else {
            return;
        };
        self.run_command(
            DownloadsCommand::QueueSchedule {
                queue_id,
                enabled: true,
                start_time: pending.start_time,
                stop_time: pending.stop_time,
                days: pending.days,
            },
            cx,
        );
    }

    fn absorb_queues(&mut self, mut queues: Vec<QueueDto>) {
        queues.sort_by_key(|queue| queue.position);
        // 运行态来自 daemon，不是本地草稿：切换后以事件为准，确保按钮可以反向操作。
        // 已选队列被远端删除则清除表单，避免向失效 id 保存。
        if let Some(form) = &mut self.form
            && let Some(id) = &form.queue_id
        {
            if let Some(queue) = queues.iter().find(|queue| &queue.queue_id == id) {
                form.is_running = queue.is_running;
            } else {
                self.form = None;
            }
        }
        self.queues = queues;
    }

    fn ensure_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() || self.queues.is_empty() {
            return;
        }
        let queue = self
            .queues
            .iter()
            .find(|queue| queue.queue_id == MAIN_QUEUE_ID)
            .or_else(|| self.queues.first())
            .cloned();
        if let Some(queue) = queue {
            self.form = Some(QueueForm::for_queue(&queue, window, cx));
        }
    }

    fn t(&self, key: &str, cx: &App) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }

    fn queue_label(&self, queue: &QueueDto, cx: &App) -> SharedString {
        match queue.queue_id.as_str() {
            MAIN_QUEUE_ID => self.t("mainQueue", cx),
            LATER_QUEUE_ID => self.t("laterQueue", cx),
            _ => SharedString::from(queue.name.clone()),
        }
    }

    fn fail(&mut self, key: &str, cx: &mut Context<Self>) {
        self.error = Some(self.t(key, cx));
        cx.notify();
    }

    fn run_command(&mut self, command: DownloadsCommand, cx: &mut Context<Self>) {
        let future = self.port.execute(command);
        cx.spawn(async move |this, cx| {
            if future.await.is_err() {
                let Ok(()) = this.update(cx, |this, cx| this.fail("localServiceActionFailed", cx))
                else {
                    // 视图已释放，结束回调而不再更新状态。
                    return;
                };
            }
        })
        .detach();
    }

    fn select_queue(&mut self, queue_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(queue) = self
            .queues
            .iter()
            .find(|queue| queue.queue_id == queue_id)
            .cloned()
        {
            self.form = Some(QueueForm::for_queue(&queue, window, cx));
            self.error = None;
            cx.notify();
        }
    }

    fn new_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form = Some(QueueForm::blank(window, cx));
        self.error = None;
        cx.notify();
    }

    /// 解析队列表单数字：空 → 0；否则必须是不超过 `max` 的非负整数。
    fn parse_int(input: &Entity<InputState>, max: i32, cx: &App) -> Option<i32> {
        parse_non_negative(input.read(cx).value().trim(), max)
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let name = form.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.fail("queueNameRequired", cx);
            return;
        }
        let start = format_time(form.schedule_start);
        let stop = format_time(form.schedule_stop);
        let (Some(speed_limit), Some(upload_limit), Some(max_concurrent), Some(segments)) = (
            Self::parse_int(&form.speed_limit, i32::MAX, cx),
            Self::parse_int(&form.upload_limit, i32::MAX, cx),
            Self::parse_int(&form.max_concurrent, i32::MAX, cx),
            Self::parse_int(&form.segments, MAX_SEGMENTS, cx),
        ) else {
            self.fail("queueInvalidNumber", cx);
            return;
        };
        let fields = QueueFields {
            name,
            speed_limit_kbps: i64::from(speed_limit),
            upload_limit_kbps: i64::from(upload_limit),
            max_concurrent,
            default_save_dir: form.save_dir.read(cx).value().trim().to_owned(),
            default_segments: segments,
            default_user_agent: form.user_agent.read(cx).value().trim().to_owned(),
        };
        let schedule_enabled = form.schedule_enabled;
        let schedule_days = form.schedule_days;
        let is_main = form.is_main();
        match form.queue_id.clone() {
            Some(queue_id) => {
                self.run_command(
                    DownloadsCommand::QueueUpdate {
                        queue_id: queue_id.clone(),
                        fields,
                    },
                    cx,
                );
                // 主队列不可定时：不提交定时，避免被 daemon 拒绝。
                if !is_main {
                    self.run_command(
                        DownloadsCommand::QueueSchedule {
                            queue_id,
                            enabled: schedule_enabled,
                            start_time: start,
                            stop_time: stop,
                            days: schedule_days,
                        },
                        cx,
                    );
                }
            }
            None => {
                if schedule_enabled {
                    // QueueCreate 不带定时字段也不返回 id：记下创建前的队列集合，
                    // 等队列列表回流后取新出现的那个再补发定时（与 web 一致）。
                    self.pending_schedule = Some(PendingSchedule {
                        known_ids: self.queues.iter().map(|q| q.queue_id.clone()).collect(),
                        start_time: start,
                        stop_time: stop,
                        days: schedule_days,
                    });
                }
                self.run_command(DownloadsCommand::QueueCreate(fields), cx);
                self.form = None;
            }
        }
        let _ = window;
        cx.notify();
    }

    fn toggle_running(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let Some(queue_id) = form.queue_id.clone() else {
            return;
        };
        let command = if form.is_running {
            DownloadsCommand::QueueStop { queue_id }
        } else {
            DownloadsCommand::QueueStart { queue_id }
        };
        self.run_command(command, cx);
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let Some(queue_id) = form.queue_id.clone() else {
            return;
        };
        let name = form.name.read(cx).value().to_string();
        let view = cx.weak_entity();
        let title = self.t("deleteQueueAction", cx);
        let description = SharedString::from(
            self.translator
                .read(cx)
                .text_with("queueDeleteConfirmDesc", &[("name", &name)]),
        );
        let cancel = self.t("cancel", cx);
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let view = view.clone();
            let queue_id = queue_id.clone();
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
                    // 队列页释放后没有提交删除，确认框不能报告成功。
                    view.update(cx, |this, cx| {
                        this.run_command(
                            DownloadsCommand::QueueDelete {
                                queue_id: queue_id.clone(),
                            },
                            cx,
                        );
                        this.form = None;
                        cx.notify();
                    })
                    .is_ok()
                })
        });
    }

    fn pick_save_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.as_ref().is_some_and(|form| form.picking_dir) {
            return;
        }
        if let Some(form) = &mut self.form {
            form.picking_dir = true;
        }
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.first().map(|path| path.display().to_string()),
                _ => None,
            };

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if let Some(form) = &mut this.form {
                    form.picking_dir = false;
                    if let Some(path) = picked {
                        form.save_dir
                            .update(cx, |input, cx| input.set_value(path, window, cx));
                    }
                }
                cx.notify();
            }) else {
                // 视图或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 运行状态圆点：运行中为成功色小圆点（小面积允许），停止为三级文字色。
    fn status_dot(running: bool, cx: &App) -> gpui::Div {
        let extended = active_theme(cx).extended().colors;
        div()
            .flex_none()
            .size(STATUS_DOT_SIZE)
            .rounded_full()
            .bg(if running {
                extended.success
            } else {
                extended.text_tertiary
            })
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let creating = self
            .form
            .as_ref()
            .is_some_and(|form| form.queue_id.is_none());
        let selected_id = self.form.as_ref().and_then(|form| form.queue_id.clone());

        let mut rows = v_flex().flex_1().min_h_0().w_full().gap(tokens.spacing.xxs);
        for queue in &self.queues {
            let id = queue.queue_id.clone();
            let selected = selected_id.as_deref() == Some(id.as_str());
            rows = rows.child(
                sidebar_navigation_button(
                    SharedString::from(format!("queue-manager-row-{id}")),
                    self.queue_label(queue, cx),
                    Self::status_dot(queue.is_running, cx),
                    div(),
                    selected,
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_queue(id.clone(), window, cx);
                })),
            );
        }
        if creating {
            rows = rows.child(sidebar_navigation_button(
                "queue-manager-row-draft",
                self.t("createQueueAction", cx),
                Icon::new(FluxIcon::Plus)
                    .size(extended.icon.sm)
                    .text_color(tokens.colors.foreground),
                div(),
                true,
                cx,
            ));
        }

        let new_label = self.t("createQueueAction", cx);
        let tooltip_label = new_label.clone();
        v_flex()
            .flex_none()
            .w(theme.text_extent(LIST_WIDTH))
            .h_full()
            .min_h_0()
            .px(tokens.spacing.sm)
            .py(tokens.spacing.md)
            .bg(extended.colors.chrome)
            .border_r(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(
                h_flex()
                    .w_full()
                    .pl(tokens.spacing.sm)
                    .pb(tokens.spacing.xs)
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(extended.caption.size)
                            .line_height(extended.caption.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(extended.colors.text_tertiary)
                            .child(self.t("sidebarQueues", cx)),
                    )
                    .child(
                        div()
                            .id("queue-manager-new-tooltip")
                            .flex_none()
                            .tooltip(move |window, cx| {
                                tooltip_view(tooltip_label.clone(), window, cx)
                            })
                            .child(
                                toolbar_action_button(
                                    "queue-manager-new",
                                    new_label,
                                    Icon::new(FluxIcon::Plus).size(extended.icon.md),
                                    false,
                                    false,
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.new_queue(window, cx);
                                    },
                                )),
                            ),
                    ),
            )
            .child(rows.overflow_y_scrollbar())
    }

    /// 标题行：队列显示名 + 运行状态徽标（新建时只有标题）。
    fn render_header(&self, form: &QueueForm, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let colors = tokens.colors;
        let title = match &form.queue_id {
            None => self.t("createQueueAction", cx),
            Some(id) => self
                .queues
                .iter()
                .find(|queue| &queue.queue_id == id)
                .map(|queue| self.queue_label(queue, cx))
                .unwrap_or_default(),
        };
        // 状态徽标：中性文字 + 小圆点，不做彩色胶囊。
        let badge = form.queue_id.as_ref().map(|_| {
            let text = if form.is_running {
                self.t("queueRunningBadge", cx)
            } else {
                self.t("queueStoppedBadge", cx)
            };
            h_flex()
                .flex_none()
                .items_center()
                .gap(tokens.spacing.xs)
                .text_size(tokens.typography.xs.size)
                .line_height(tokens.typography.xs.line_height)
                .text_color(colors.muted_foreground)
                .child(Self::status_dot(form.is_running, cx))
                .child(text)
        });
        h_flex()
            .w_full()
            .flex_none()
            .px(tokens.spacing.lg)
            .pt(tokens.spacing.lg)
            .pb(tokens.spacing.md)
            .gap(tokens.spacing.sm)
            .items_center()
            .border_b(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(extended.title.size)
                    .line_height(extended.title.line_height)
                    .font_weight(extended.title.weight)
                    .text_color(colors.foreground)
                    .child(title),
            )
            .children(badge)
    }

    /// 标准字段：标签 → 输入框 → 可选说明。
    fn render_field(
        &self,
        label: SharedString,
        input: &Entity<InputState>,
        hint_key: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        form_field(
            label,
            Input::new(input).control(cx).w_full(),
            hint_key.map(|key| self.t(key, cx)),
            cx,
        )
    }

    fn render_name_field(&self, form: &QueueForm, cx: &mut Context<Self>) -> Div {
        if form.is_builtin() {
            // 内置队列名称固定：标题已显示本地化名，这里只留说明。
            return field_hint(self.t("builtinQueueRenameHint", cx), cx);
        }
        self.render_field(self.t("queueNameLabel", cx), &form.name, None, cx)
    }

    fn render_save_dir_field(&self, form: &QueueForm, cx: &mut Context<Self>) -> Div {
        form_field(
            self.t("queueSaveDir", cx),
            input_with_action(
                Input::new(&form.save_dir).control(cx).w_full(),
                Button::new("queue-manager-browse-dir")
                    .outline()
                    .icon(FluxIcon::FolderOpen)
                    .label(self.t("browse", cx))
                    .control(cx)
                    .disabled(form.picking_dir)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.pick_save_dir(window, cx);
                    })),
                cx,
            ),
            Some(self.t("queueDirInheritHint", cx)),
            cx,
        )
    }

    /// 改写某个定时时刻并重绘。
    fn set_schedule_time(&mut self, slot: ScheduleSlot, time: Option<u16>, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.form {
            *form.slot_mut(slot) = time;
        }
        cx.notify();
    }

    /// 时间下拉：「时」含「不定时」项，「分」按步长；未定时时「分」禁用。
    fn render_time_field(
        &self,
        slot: ScheduleSlot,
        time: Option<u16>,
        cx: &mut Context<Self>,
    ) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        let (label_key, hour_id, minute_id) = match slot {
            ScheduleSlot::Start => (
                "queueScheduleStartLabel",
                "queue-schedule-start-hour",
                "queue-schedule-start-minute",
            ),
            ScheduleSlot::Stop => (
                "queueScheduleStopLabel",
                "queue-schedule-stop-hour",
                "queue-schedule-stop-minute",
            ),
        };
        let unset = self.t("queueScheduleTimeUnset", cx);
        let hour = time.map(|minutes| minutes / 60);
        let minute = time.map_or(0, |minutes| minutes % 60);

        let this = cx.weak_entity();
        let unset_item = unset.clone();
        let hour_button = Button::new(hour_id)
            .outline()
            .label(hour.map_or_else(|| unset.clone(), |h| SharedString::from(format!("{h:02}"))))
            .dropdown_caret(true)
            .control(cx)
            .flex_1()
            .min_w_0()
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                let clear = this.clone();
                let menu = menu.scrollable(true).max_h(TIME_MENU_MAX_HEIGHT).item(
                    PopupMenuItem::new(unset_item.clone())
                        .checked(hour.is_none())
                        .on_click(move |_, _, cx| {
                            let Ok(()) = clear.update(cx, |this, cx| {
                                this.set_schedule_time(slot, None, cx);
                            }) else {
                                // 视图已释放，结束回调而不再更新状态。
                                return;
                            };
                        }),
                );
                (0..24u16).fold(menu, |menu, h| {
                    let this = this.clone();
                    menu.item(
                        PopupMenuItem::new(SharedString::from(format!("{h:02}")))
                            .checked(hour == Some(h))
                            .on_click(move |_, _, cx| {
                                let Ok(()) = this.update(cx, |this, cx| {
                                    this.set_schedule_time(slot, Some(h * 60 + minute), cx);
                                }) else {
                                    // 视图已释放，结束回调而不再更新状态。
                                    return;
                                };
                            }),
                    )
                })
            });

        let this = cx.weak_entity();
        let minute_button = Button::new(minute_id)
            .outline()
            .label(match hour {
                Some(_) => SharedString::from(format!("{minute:02}")),
                None => SharedString::from("--"),
            })
            .dropdown_caret(true)
            .control(cx)
            .flex_1()
            .min_w_0()
            .disabled(hour.is_none())
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                let Some(hour) = hour else { return menu };
                let menu = menu.scrollable(true).max_h(TIME_MENU_MAX_HEIGHT);
                minute_choices(minute).into_iter().fold(menu, |menu, m| {
                    let this = this.clone();
                    menu.item(
                        PopupMenuItem::new(SharedString::from(format!("{m:02}")))
                            .checked(m == minute)
                            .on_click(move |_, _, cx| {
                                let Ok(()) = this.update(cx, |this, cx| {
                                    this.set_schedule_time(slot, Some(hour * 60 + m), cx);
                                }) else {
                                    // 视图已释放，结束回调而不再更新状态。
                                    return;
                                };
                            }),
                    )
                })
            });

        form_field(
            self.t(label_key, cx),
            h_flex()
                .w_full()
                .items_center()
                .gap(spacing.xs)
                .child(hour_button)
                .child(div().flex_none().child(":"))
                .child(minute_button),
            None,
            cx,
        )
    }

    /// 定时：分组卡片首行为开关；开启后同卡片内追加「开始 | 停止」时间与星期复选行。
    /// 主队列恒运行、不可定时，只显示说明。
    fn render_schedule(&self, form: &QueueForm, cx: &mut Context<Self>) -> Div {
        if form.is_main() {
            return field_hint(self.t("mainQueueAlwaysRunningHint", cx), cx);
        }
        let spacing = active_theme(cx).tokens().spacing;
        let enabled = form.schedule_enabled;
        let days = form.schedule_days;
        let switch_row = option_row(
            self.t("queueScheduleEnable", cx),
            Some(self.t("queueScheduleDesc", cx)),
            Switch::new("queue-schedule-enabled")
                .checked(enabled)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    if let Some(form) = &mut this.form {
                        form.schedule_enabled = *checked;
                    }
                    cx.notify();
                })),
            cx,
        )
        .into_any_element();
        if !enabled {
            return option_group([switch_row], cx);
        }

        let mut days_row = h_flex().w_full().flex_wrap().gap(spacing.md);
        for (index, label) in self.t("weekdaysShort", cx).split(',').enumerate() {
            let bit = 1 << index;
            let this = cx.weak_entity();
            days_row = days_row.child(check_row(
                ("queue-schedule-day", index),
                days & bit != 0,
                SharedString::from(label.to_owned()),
                move |checked, _, cx| {
                    let Ok(()) = this.update(cx, |this, cx| {
                        if let Some(form) = &mut this.form {
                            form.schedule_days = toggle_day_bit(form.schedule_days, bit, checked);
                        }
                        cx.notify();
                    }) else {
                        // 视图已释放，结束回调而不再更新状态。
                        return;
                    };
                },
                cx,
            ));
        }
        let details = form_layout(cx)
            .p(spacing.md)
            .child(
                v_flex()
                    .w_full()
                    .gap(spacing.xs + spacing.xxs)
                    .child(form_row(
                        [
                            self.render_time_field(ScheduleSlot::Start, form.schedule_start, cx)
                                .into_any_element(),
                            self.render_time_field(ScheduleSlot::Stop, form.schedule_stop, cx)
                                .into_any_element(),
                        ],
                        cx,
                    ))
                    .child(field_hint(self.t("queueScheduleTimePickHint", cx), cx)),
            )
            .child(form_field(
                self.t("queueScheduleDays", cx),
                days_row,
                None,
                cx,
            ))
            .into_any_element();
        option_group([switch_row, details], cx)
    }

    fn render_body(&self, form: &QueueForm, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = active_theme(cx).tokens().clone();
        let rate_label = |key| {
            SharedString::from(self.translator.read(cx).text_with(
                key,
                &[("unit", fluxdown_ui_components::size_units::rate_labels()[1])],
            ))
        };
        let download_limit = rate_label("queueDownloadLimitWithUnit");
        let upload_limit = rate_label("queueUploadLimitWithUnit");
        let body = form_layout(cx)
            .px(tokens.spacing.lg)
            .pt(tokens.spacing.lg)
            .pb(tokens.spacing.xl)
            .child(self.render_name_field(form, cx))
            .child(form_row(
                [
                    self.render_field(
                        download_limit,
                        &form.speed_limit,
                        Some("queueSpeedLimitHint"),
                        cx,
                    )
                    .into_any_element(),
                    self.render_field(
                        upload_limit,
                        &form.upload_limit,
                        Some("queueUploadLimitDesc"),
                        cx,
                    )
                    .into_any_element(),
                ],
                cx,
            ))
            .child(
                v_flex()
                    .w_full()
                    .child(form_row(
                        [
                            self.render_field(
                                self.t("queueMaxConcurrent", cx),
                                &form.max_concurrent,
                                Some("queueMaxConcurrentHint"),
                                cx,
                            )
                            .into_any_element(),
                            self.render_field(
                                self.t("queueDefaultSegments", cx),
                                &form.segments,
                                Some("queueDefaultSegmentsHint"),
                                cx,
                            )
                            .into_any_element(),
                        ],
                        cx,
                    ))
                    .child(threads_risk_callout(
                        "queue-manager-threads-risk",
                        parse_segments_input(&form.segments.read(cx).value()),
                        self.translator.read(cx),
                    )),
            )
            .child(self.render_save_dir_field(form, cx))
            .child(self.render_field(
                self.t("queueDefaultUserAgent", cx),
                &form.user_agent,
                Some("queueUaHint"),
                cx,
            ))
            .child(self.render_schedule(form, cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(field_error(error, cx))
            });
        div()
            .id("queue-manager-body")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scrollbar()
            .child(body)
    }

    fn render_footer(&self, form: &QueueForm, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let is_creating = form.queue_id.is_none();
        // 底栏：左端次要破坏性操作（danger-ghost），右端「启停」(outline) 在左、「保存」(primary) 在右。
        let mut footer = h_flex()
            .w_full()
            .flex_none()
            .px(tokens.spacing.lg)
            .py(tokens.spacing.md)
            .items_center()
            .gap(tokens.spacing.sm)
            .bg(extended.colors.chrome)
            .border_t(extended.stroke.thin)
            .border_color(extended.colors.hairline);
        if !is_creating && !form.is_builtin() {
            footer = footer.child(
                Button::new("queue-manager-delete")
                    .ghost()
                    .control(cx)
                    .text_color(tokens.colors.destructive)
                    .label(self.t("deleteQueueAction", cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.confirm_delete(window, cx);
                    })),
            );
        }
        footer = footer.child(div().flex_1());
        // 主队列不可停止：运行中不给启停按钮（异常停止时仍可「启动」）。
        if !is_creating && !(form.is_main() && form.is_running) {
            footer = footer.child(
                Button::new("queue-manager-toggle-run")
                    .outline()
                    .control(cx)
                    .label(if form.is_running {
                        self.t("stopQueueAction", cx)
                    } else {
                        self.t("startQueueAction", cx)
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_running(cx);
                    })),
            );
        }
        footer.child(
            Button::new("queue-manager-save")
                .primary()
                .control(cx)
                .label(self.t("queueSaveAction", cx))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.save(window, cx);
                })),
        )
    }

    fn render_content(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tokens = active_theme(cx).tokens().clone();
        let column = v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .bg(tokens.colors.surface);
        let Some(form) = &self.form else {
            // 只有断连且快照尚未到达时才会没有表单：给出只读提示而不是空白面板。
            return column
                .items_center()
                .justify_center()
                .p(tokens.spacing.lg)
                .when(self.stale, |this| {
                    this.child(field_hint(self.t("localServiceDisconnected", cx), cx))
                })
                .into_any_element();
        };
        column
            .when(self.stale, |this| {
                this.child(
                    div()
                        .w_full()
                        .px(tokens.spacing.lg)
                        .py(tokens.spacing.sm)
                        .text_size(tokens.typography.xs.size)
                        .line_height(tokens.typography.xs.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(self.t("localServiceDisconnected", cx)),
                )
            })
            .child(self.render_header(form, cx))
            .child(self.render_body(form, cx))
            .child(self.render_footer(form, cx))
            .into_any_element()
    }
}

impl Render for QueueManagerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_form(window, cx);
        let tokens = active_theme(cx).tokens().clone();
        h_flex()
            .size_full()
            .min_h_0()
            // `h_flex` 默认交叉轴居中；两列都要撑满高度。
            .items_stretch()
            .bg(tokens.colors.surface)
            .child(self.render_list(cx))
            .child(self.render_content(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{format_time, minute_choices, parse_non_negative, parse_time, toggle_day_bit};

    #[test]
    fn parse_non_negative_accepts_empty_and_in_range_values() {
        assert_eq!(parse_non_negative("", 64), Some(0));
        assert_eq!(parse_non_negative("0", 64), Some(0));
        assert_eq!(parse_non_negative("64", 64), Some(64));
        assert_eq!(parse_non_negative("2147483647", i32::MAX), Some(i32::MAX));
    }

    #[test]
    fn parse_non_negative_rejects_invalid_input() {
        assert_eq!(parse_non_negative("-1", 64), None);
        assert_eq!(parse_non_negative("abc", 64), None);
        assert_eq!(parse_non_negative("1.5", 64), None);
        assert_eq!(parse_non_negative("65", 64), None);
        assert_eq!(parse_non_negative("2147483648", i32::MAX), None);
        assert_eq!(parse_non_negative("99999999999999999999", i32::MAX), None);
    }

    #[test]
    fn parse_time_round_trips_wire_format() {
        assert_eq!(parse_time("08:30"), Some(510));
        assert_eq!(parse_time("9:05"), Some(545));
        assert_eq!(format_time(parse_time("9:05")), "09:05");
        assert_eq!(format_time(Some(23 * 60 + 59)), "23:59");
        assert_eq!(format_time(None), "");
    }

    #[test]
    fn parse_time_treats_empty_or_invalid_as_unset() {
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("12:60"), None);
        assert_eq!(parse_time("12"), None);
        assert_eq!(parse_time("ab:cd"), None);
    }

    #[test]
    fn minute_choices_keep_off_step_current_value_in_order() {
        assert_eq!(minute_choices(0).len(), 12);
        let choices = minute_choices(7);
        assert_eq!(choices.len(), 13);
        assert_eq!(&choices[..3], &[0, 5, 7]);
    }

    #[test]
    fn toggle_day_bit_sets_and_clears_only_targeted_bit() {
        // bit0 = 周一；从全选（127 = 0b1111111）取消周一，只清掉 bit0。
        assert_eq!(toggle_day_bit(0b111_1111, 0b1, false), 0b111_1110);
        // 从空掩码勾选周日（bit6），只置位 bit6，不影响其余位。
        assert_eq!(toggle_day_bit(0, 0b100_0000, true), 0b100_0000);
        // 对已置位的位再次勾选（checked=true）是幂等的。
        assert_eq!(toggle_day_bit(0b1, 0b1, true), 0b1);
    }
}
