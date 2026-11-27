//! 助手视图：状态、用户动作与会话事件处理。绘制在 `render.rs`。

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, AssistantAskParams, AssistantChannel, AssistantIssueDto,
    AssistantIssueParams, AssistantMessageDto, AssistantRateParams, AssistantRunDto,
    AssistantRunItemDto, AssistantSessionDto, AssistantSessionKind, AssistantSessionsParams,
    AssistantStateDto, RpcErrorData, ServiceEvent,
};
use fluxdown_ui_components::{size_units::format_bytes, toast};
use fluxdown_ui_i18n::Translator;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, Image, PathPromptOptions, ScrollHandle,
    SharedString, Subscription, Task, Window, px,
};
use gpui_component::{
    WindowExt as _,
    input::{InputEvent, InputState, TextareaState},
};

use crate::{
    AssistantPort, PortFuture,
    attachment::{ImageError, ImageSource, Prepared, clipboard_sources, prepare},
    model::{
        Gate, SESSION_PAGE_SIZE, SendFailure, Turn, classify_send_error, gate, insert_placeholder,
        map_offset, orphaned_images, pending_confirmations, placeholder_remnant,
        remove_placeholder,
    },
};

/// 距底部多近算「跟随最新内容」。
const FOLLOW_BOTTOM_SLACK: f32 = 40.;
/// 输入框默认高度（行数）。
const INPUT_MIN_ROWS: usize = 3;
/// 输入框最多长到几行。
const INPUT_MAX_ROWS: usize = 8;
/// 倒计时刷新间隔。
const TICK: Duration = Duration::from_secs(1);

/// 主窗口中的独立动作（app 注入；本 crate 不认识其他页面）。
pub type WindowAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// app 注入的宿主动作。
#[derive(Clone)]
pub struct AssistantHost {
    /// 打开账户页（登录、套餐都在那里）。
    pub open_account: WindowAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    Idle,
    Loading,
    Ready,
    Failed,
}

/// 云端会话列表。
pub(crate) struct SessionList {
    pub(crate) items: Vec<AssistantSessionDto>,
    pub(crate) total: u64,
    pub(crate) load: Load,
    pub(crate) more_loading: bool,
    epoch: u64,
}

impl SessionList {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            load: Load::Idle,
            more_loading: false,
            epoch: 0,
        }
    }

    pub(crate) fn has_more(&self) -> bool {
        (self.items.len() as u64) < self.total
    }
}

/// 当前会话在云端的历史。
pub(crate) struct Detail {
    pub(crate) load: Load,
    pub(crate) session: Option<AssistantSessionDto>,
    pub(crate) messages: Vec<AssistantMessageDto>,
    pub(crate) issues: Vec<AssistantIssueDto>,
    epoch: u64,
}

impl Detail {
    fn new() -> Self {
        Self {
            load: Load::Idle,
            session: None,
            messages: Vec::new(),
            issues: Vec::new(),
            epoch: 0,
        }
    }
}

/// 正在修改的问题单草稿。
pub(crate) struct IssueEditor {
    pub(crate) issue_id: String,
    pub(crate) title: Entity<InputState>,
    pub(crate) body: Entity<TextareaState>,
    /// 输入变化时重绘(提交按钮的可用性取决于标题 / 正文是否为空)。
    _subscriptions: [Subscription; 2],
}

/// 一张已处理好、待随下一条提问发送的图片。
pub(crate) struct Attachment {
    /// `data:image/<type>;base64,...`。
    pub(crate) data_url: String,
    /// 输入区的缩略图。
    pub(crate) thumbnail: Arc<Image>,
}

/// 刚发出的带图提问：气泡据此显示本地缩略图（云端历史里只有图片 ID）。
pub(crate) struct SentImages {
    pub(crate) session_id: String,
    /// 提问文本（含占位符），用来对上对应的那一轮。
    pub(crate) text: String,
    pub(crate) thumbnails: Vec<Arc<Image>>,
}

pub struct AssistantView {
    pub(crate) translator: Entity<Translator>,
    pub(crate) port: Arc<dyn AssistantPort>,
    pub(crate) host: AssistantHost,
    /// 面板显示会话列表（历史视图）而不是对话；选中会话 / 新对话后回到对话。
    pub(crate) history_open: bool,
    /// 宿主带进来的提问（[`Self::ask`]）：配置加载完成后自动发出，确认无法提问则留在输入框。
    pending_submit: bool,
    pub(crate) input: Entity<TextareaState>,
    /// 输入框当前 placeholder；语言切换后在 render 中同步（输入框只在构造时取一次）。
    pub(crate) input_placeholder: SharedString,
    pub(crate) scroll: ScrollHandle,

    pub(crate) state: AssistantStateDto,
    pub(crate) signed_in: bool,
    /// 与 agent 断开：内容保留只读，动作禁用。
    pub(crate) stale: bool,
    /// 配置拉取失败（本机服务 / 云端不可达）。
    pub(crate) config_failed: bool,
    pub(crate) config_loading: bool,
    config_requested: bool,

    pub(crate) sessions: SessionList,
    /// 当前会话；`None` = 新对话。
    pub(crate) current: Option<String>,
    pub(crate) detail: Detail,
    /// 新对话的类型（已有会话沿用其类型）。
    pub(crate) kind: AssistantSessionKind,

    pub(crate) sending: bool,
    pub(crate) cancelling: bool,
    pub(crate) send_failure: Option<SendFailure>,
    /// 其他动作失败的提示（确认 / 评分 / 问题单 / 删除）。
    pub(crate) action_failed: bool,
    /// 已发出、等待结果的确认请求（避免重复点击）。
    pub(crate) confirming: HashSet<String>,
    /// 用户对「过程」折叠区的显式选择（键 = 轮次键）。
    pub(crate) steps_choice: HashMap<String, bool>,
    /// 已展开参数 JSON 的工具调用（键 = 调用 ID / 请求 ID）。
    pub(crate) args_open: HashSet<String>,
    /// 本地确认 / 丢弃后的问题单最新状态。
    pub(crate) issue_overrides: HashMap<String, AssistantIssueDto>,
    pub(crate) issue_busy: HashSet<String>,
    pub(crate) issue_editor: Option<IssueEditor>,
    /// 等待再次点击确认删除的会话：删除按钮原位变为危险态，鼠标移出该行即取消。
    pub(crate) delete_pending: Option<String>,
    /// 等待再次点击确认的「清空全部会话」；鼠标移出按钮即取消。
    pub(crate) clear_pending: bool,
    /// 清空请求进行中。
    pub(crate) clearing: bool,
    /// 每条回答（键 = 消息 ID）的本地评价状态：点击后立即写入（乐观更新），失败回滚；
    /// 优先于云端历史里的值。切换会话时清空（新读到的历史即为准）。
    pub(crate) ratings: HashMap<String, Option<String>>,
    /// 评价请求进行中的消息 ID（避免重复点击）。
    pub(crate) rating_busy: HashSet<String>,
    /// 待随下一条提问发送的图片；第 n 张对应输入文本里的 `[Image #n]`。
    pub(crate) attachments: Vec<Attachment>,
    /// 上次处理过的输入框文本，用来识别一次删除是否只删掉了占位符的一部分。
    input_text: SharedString,
    /// 正在后台读取 / 转码的图片数（计入数量上限）。
    pub(crate) attaching: usize,
    /// 刚发出的带图提问。
    pub(crate) sent_images: Option<SentImages>,
    /// 倒计时刷新任务是否在运行（只在有待确认的工具时存在）。
    ticker_running: bool,
}

impl AssistantView {
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn AssistantPort>,
        host: AssistantHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        let input_placeholder = SharedString::from(
            translator
                .read(cx)
                .text("assistantInputPlaceholder")
                .to_owned(),
        );
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(INPUT_MIN_ROWS, INPUT_MAX_ROWS)
                .submit_on_enter(true)
                .placeholder(input_placeholder.clone())
        });
        cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.input_changed(window, cx),
                // Enter 发送，Shift+Enter 换行（输入框自己处理换行）。
                InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                _ => {}
            },
        )
        .detach();
        Self {
            translator,
            port,
            host,
            history_open: false,
            pending_submit: false,
            input,
            input_placeholder,
            scroll: ScrollHandle::new(),
            state: AssistantStateDto::default(),
            signed_in: false,
            stale: false,
            config_failed: false,
            config_loading: false,
            config_requested: false,
            sessions: SessionList::new(),
            current: None,
            detail: Detail::new(),
            kind: AssistantSessionKind::Ask,
            sending: false,
            cancelling: false,
            send_failure: None,
            action_failed: false,
            confirming: HashSet::new(),
            steps_choice: HashMap::new(),
            args_open: HashSet::new(),
            issue_overrides: HashMap::new(),
            issue_busy: HashSet::new(),
            issue_editor: None,
            delete_pending: None,
            clear_pending: false,
            clearing: false,
            ratings: HashMap::new(),
            rating_busy: HashSet::new(),
            attachments: Vec::new(),
            input_text: SharedString::default(),
            attaching: 0,
            sent_images: None,
            ticker_running: false,
        }
    }

    /// 面板顶栏插槽：标题、今日用量、新对话与历史会话切换；随本视图重绘。
    pub fn new_panel_header(&self, cx: &mut Context<Self>) -> Entity<crate::AssistantPanelHeader> {
        let view = cx.entity();
        cx.new(|cx| crate::AssistantPanelHeader::new(view, cx))
    }

    /// 面板打开时把焦点交给输入框。
    pub fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 宿主入口（命令面板「问 AI 助手」）：开新对话并发出这段问题。配置还在加载时等加载完再发；
    /// 无法提问（未登录 / 未开放 / 额度用尽 / 正在发送）时问题留在输入框里由用户处理。
    pub fn ask(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.new_chat(window, cx);
        self.kind = AssistantSessionKind::Ask;
        // 新问题取代整份草稿：草稿里图片占位符对应的附件一并作废。
        self.attachments.clear();
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.sync_input_text(cx);
        self.pending_submit = true;
        cx.notify();
    }

    /// 渲染期推进 [`Self::ask`] 带进来的提问：门禁确定后只处理一次。
    pub(crate) fn flush_pending_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_submit {
            return;
        }
        match self.gate() {
            Gate::Loading => {}
            Gate::Open { .. } => {
                self.pending_submit = false;
                cx.defer_in(window, |this, window, cx| this.send(window, cx));
            }
            Gate::SignedOut | Gate::NotOpen | Gate::PlanDisabled | Gate::ServiceError => {
                self.pending_submit = false;
            }
        }
    }

    /// 切换历史视图（会话列表）。
    pub(crate) fn toggle_history(&mut self, cx: &mut Context<Self>) {
        self.history_open = !self.history_open;
        if self.history_open && self.sessions.load == Load::Failed {
            self.reload_sessions(cx);
        }
        cx.notify();
    }

    /// 面板顶栏标题：历史视图为「历史会话」，对话视图为当前会话标题（新对话 / 无标题有各自文案）。
    pub(crate) fn heading(&self, cx: &App) -> SharedString {
        if !matches!(self.gate(), Gate::Open { .. }) {
            return self.t("assistantNavTitle", cx);
        }
        if self.history_open {
            return self.t("assistantHistory", cx);
        }
        let Some(current) = self.current.as_deref() else {
            return self.t("assistantNewChat", cx);
        };
        self.detail
            .session
            .as_ref()
            .filter(|session| session.id == current)
            .or_else(|| {
                self.sessions
                    .items
                    .iter()
                    .find(|session| session.id == current)
            })
            .map(|session| session.title.trim())
            .filter(|title| !title.is_empty())
            .map_or_else(
                || self.t("assistantUntitled", cx),
                |title| SharedString::from(title.to_owned()),
            )
    }

    /// 今日用量（有每日上限时）。
    pub(crate) fn usage(&self, cx: &App) -> Option<String> {
        self.state
            .config
            .as_ref()
            .filter(|config| config.daily_limit > 0)
            .map(|config| {
                self.with(
                    "assistantUsage",
                    &[
                        ("used", &config.used_today.to_string()),
                        ("limit", &config.daily_limit.to_string()),
                    ],
                    cx,
                )
            })
    }

    pub(crate) fn t(&self, key: &str, cx: &App) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
    }

    pub(crate) fn with(&self, key: &str, args: &[(&str, &str)], cx: &App) -> String {
        self.translator.read(cx).text_with(key, args)
    }

    pub(crate) fn gate(&self) -> Gate {
        gate(self.signed_in, &self.state, self.config_failed)
    }

    /// 当前会话里进行中的执行。
    pub(crate) fn current_run(&self) -> Option<&AssistantRunDto> {
        let id = self.current.as_deref()?;
        self.state
            .runs
            .iter()
            .rev()
            .find(|run| run.session_id == id && run.status.is_active())
    }

    /// 当前会话的类型（新对话取待发送的类型）。
    pub(crate) fn current_kind(&self) -> AssistantSessionKind {
        let Some(id) = self.current.as_deref() else {
            return self.kind;
        };
        let session = self
            .detail
            .session
            .as_ref()
            .filter(|session| session.id == id)
            .or_else(|| self.sessions.items.iter().find(|session| session.id == id));
        match session.map(|session| session.kind.as_str()) {
            Some("feedback") => AssistantSessionKind::Feedback,
            _ => AssistantSessionKind::Ask,
        }
    }

    // ───────────────────────── 会话事件 ─────────────────────────

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.stale = false;
        self.set_signed_in(snapshot.session.is_some());
        self.apply_state(snapshot.assistant.clone(), cx);
        cx.notify();
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let changed = match event {
            ServiceEvent::Agent(AgentEvent::AssistantChanged(state)) => {
                self.apply_state(state.clone(), cx)
            }
            ServiceEvent::Agent(AgentEvent::SessionChanged(session)) => {
                self.set_signed_in(session.is_some())
            }
            _ => false,
        };
        if changed {
            cx.notify();
        }
    }

    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        cx.notify();
    }

    /// 登录态变化：账号相关的缓存（会话、历史、配置）一律作废。返回是否变化。
    fn set_signed_in(&mut self, signed_in: bool) -> bool {
        if self.signed_in == signed_in {
            return false;
        }
        self.signed_in = signed_in;
        self.sessions = SessionList::new();
        self.current = None;
        self.detail = Detail::new();
        self.config_failed = false;
        self.config_requested = false;
        self.send_failure = None;
        self.action_failed = false;
        self.confirming.clear();
        self.steps_choice.clear();
        self.args_open.clear();
        self.issue_overrides.clear();
        self.issue_busy.clear();
        self.issue_editor = None;
        self.delete_pending = None;
        self.history_open = false;
        self.pending_submit = false;
        self.clear_pending = false;
        self.ratings.clear();
        self.sent_images = None;
        true
    }

    /// 用推送 / 快照整体替换助手状态；返回是否变化。
    fn apply_state(&mut self, state: AssistantStateDto, cx: &mut Context<Self>) -> bool {
        if self.state == state {
            return false;
        }
        let follow = self.near_bottom();
        let before = active_runs(&self.state);
        self.state = state;
        let after = active_runs(&self.state);
        let pending = pending_request_ids(&self.state);
        self.confirming.retain(|id| pending.contains(id.as_str()));
        if self.state.config.is_some() {
            self.config_failed = false;
        }

        // 有执行结束：刷新云端历史与会话列表（已落库的历史取代实时覆盖层，列表排序 / 标题更新）。
        let finished: Vec<String> = before
            .difference(&after)
            .map(|(session, _)| session.clone())
            .collect();
        if !finished.is_empty() && self.signed_in {
            self.reload_sessions(cx);
            if let Some(current) = self.current.clone()
                && finished.contains(&current)
            {
                self.load_detail(current, cx);
            }
        }
        self.ensure_ticker(cx);
        if follow {
            self.scroll.scroll_to_bottom();
        }
        true
    }

    fn near_bottom(&self) -> bool {
        let max = self.scroll.max_offset().y;
        let offset = self.scroll.offset().y;
        max <= px(0.) || offset + max < px(FOLLOW_BOTTOM_SLACK)
    }

    /// 倒计时:有待确认的工具时每秒重绘;待确认消失(到期)后补刷一次并停止。
    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker_running || pending_confirmations(&self.state, now_unix_ms()) == 0 {
            return;
        }
        self.ticker_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let Ok(more) = this.update(cx, |this, cx| {
                    let more = pending_confirmations(&this.state, now_unix_ms()) > 0;
                    this.ticker_running = more;
                    cx.notify();
                    more
                }) else {
                    // 助手页已释放,结束倒计时刷新。
                    return;
                };
                if !more {
                    return;
                }
            }
        })
        .detach();
    }

    // ───────────────────────── 异步辅助 ─────────────────────────

    fn run<T: 'static>(
        future: PortFuture<T>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, RpcErrorData>, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, move |this, cx| done(this, result, cx)) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
    }

    fn fail_action(&mut self, what: &str, error: &RpcErrorData, cx: &mut Context<Self>) {
        log::warn!("assistant {what} failed: {:?}", error.code);
        self.action_failed = true;
        cx.notify();
    }

    // ───────────────────────── 配置 ─────────────────────────

    /// 页面首次可见时拉取配置与会话列表（渲染期调用，只在状态为空闲时触发一次）。
    pub(crate) fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if !self.signed_in || self.stale {
            return;
        }
        if !self.config_requested {
            self.config_requested = true;
            self.fetch_config(false, cx);
        }
        if matches!(self.gate(), Gate::Open { .. }) && self.sessions.load == Load::Idle {
            self.reload_sessions(cx);
        }
    }

    pub(crate) fn fetch_config(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if self.config_loading {
            return;
        }
        self.config_loading = true;
        let future = if refresh {
            self.port.refresh()
        } else {
            self.port.get()
        };
        Self::run(future, cx, move |this, result, cx| {
            this.config_loading = false;
            match result {
                Ok(state) => {
                    // 执行（runs）只信推送：这次响应可能比推送更旧，整体替换会让进行中的执行倒退。
                    let runs = this.state.runs.clone();
                    this.apply_state(AssistantStateDto { runs, ..state }, cx);
                    this.config_failed = false;
                }
                Err(error) => {
                    log::warn!("assistant config fetch failed: {:?}", error.code);
                    this.config_failed = true;
                }
            }
            cx.notify();
        });
    }

    // ───────────────────────── 会话列表 ─────────────────────────

    /// 重新读取会话列表第一页（已加载更多页时保持原有条数）。
    pub(crate) fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        if !self.signed_in {
            return;
        }
        self.sessions.epoch += 1;
        let epoch = self.sessions.epoch;
        self.sessions.load = Load::Loading;
        let limit = u32::try_from(self.sessions.items.len())
            .unwrap_or(SESSION_PAGE_SIZE)
            .max(SESSION_PAGE_SIZE);
        let future = self.port.sessions(AssistantSessionsParams {
            limit: Some(limit),
            offset: Some(0),
        });
        Self::run(future, cx, move |this, result, cx| {
            if this.sessions.epoch != epoch {
                return;
            }
            match result {
                Ok(page) => {
                    this.sessions.items = page.items;
                    this.sessions.total = page.total;
                    this.sessions.load = Load::Ready;
                }
                Err(error) => {
                    log::warn!("assistant sessions load failed: {:?}", error.code);
                    this.sessions.load = Load::Failed;
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn load_more_sessions(&mut self, cx: &mut Context<Self>) {
        if self.sessions.more_loading || !self.sessions.has_more() {
            return;
        }
        self.sessions.more_loading = true;
        let epoch = self.sessions.epoch;
        let offset = u32::try_from(self.sessions.items.len()).unwrap_or(u32::MAX);
        let future = self.port.sessions(AssistantSessionsParams {
            limit: Some(SESSION_PAGE_SIZE),
            offset: Some(offset),
        });
        Self::run(future, cx, move |this, result, cx| {
            this.sessions.more_loading = false;
            // 期间列表被整体刷新：这一页的偏移已失效。
            if this.sessions.epoch == epoch {
                match result {
                    Ok(page) => {
                        for item in page.items {
                            if this.sessions.items.iter().all(|known| known.id != item.id) {
                                this.sessions.items.push(item);
                            }
                        }
                        this.sessions.total = page.total;
                    }
                    Err(error) => this.fail_action("sessions page", &error, cx),
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    // ───────────────────────── 当前会话 ─────────────────────────

    pub(crate) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.current = None;
        self.history_open = false;
        self.detail = Detail {
            epoch: self.detail.epoch + 1,
            ..Detail::new()
        };
        self.send_failure = None;
        self.action_failed = false;
        self.delete_pending = None;
        self.issue_editor = None;
        self.ratings.clear();
        self.sent_images = None;
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn select_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.history_open = false;
        if self.current.as_deref() == Some(session_id.as_str()) {
            cx.notify();
            return;
        }
        self.current = Some(session_id.clone());
        self.detail = Detail {
            epoch: self.detail.epoch + 1,
            ..Detail::new()
        };
        self.send_failure = None;
        self.action_failed = false;
        self.delete_pending = None;
        self.issue_editor = None;
        self.issue_overrides.clear();
        self.ratings.clear();
        self.sent_images = None;
        self.scroll.scroll_to_bottom();
        self.load_detail(session_id, cx);
        cx.notify();
    }

    pub(crate) fn load_detail(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.detail.epoch += 1;
        let epoch = self.detail.epoch;
        self.detail.load = Load::Loading;
        let future = self.port.session(session_id.clone());
        Self::run(future, cx, move |this, result, cx| {
            if this.detail.epoch != epoch || this.current.as_deref() != Some(session_id.as_str()) {
                return;
            }
            match result {
                Ok(detail) => {
                    let follow = this.near_bottom() || this.detail.messages.is_empty();
                    this.detail.load = Load::Ready;
                    this.detail.session = Some(detail.session);
                    this.detail.messages = detail.messages;
                    this.detail.issues = detail.issues;
                    // 新读到的历史里问题单状态以云端为准。
                    this.issue_overrides.clear();
                    if follow {
                        this.scroll.scroll_to_bottom();
                    }
                }
                Err(error) => {
                    log::warn!("assistant session load failed: {:?}", error.code);
                    this.detail.load = Load::Failed;
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn request_delete(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.delete_pending = Some(session_id);
        cx.notify();
    }

    pub(crate) fn cancel_delete(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.delete_pending.as_deref() == Some(session_id) {
            self.delete_pending = None;
            cx.notify();
        }
    }

    pub(crate) fn delete_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.delete_pending = None;
        let future = self.port.delete_session(session_id.clone());
        Self::run(future, cx, move |this, result, cx| {
            match result {
                Ok(()) => {
                    this.sessions
                        .items
                        .retain(|session| session.id != session_id);
                    this.sessions.total = this.sessions.total.saturating_sub(1);
                    if this.current.as_deref() == Some(session_id.as_str()) {
                        this.leave_current();
                    }
                }
                Err(error) => this.fail_action("delete session", &error, cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn request_clear(&mut self, cx: &mut Context<Self>) {
        self.clear_pending = true;
        cx.notify();
    }

    pub(crate) fn cancel_clear(&mut self, cx: &mut Context<Self>) {
        if self.clear_pending {
            self.clear_pending = false;
            cx.notify();
        }
    }

    /// 清空全部会话（云端只在用户侧隐藏）：成功后列表清空、回到新对话；在途的列表请求结果作废。
    pub(crate) fn clear_sessions(&mut self, cx: &mut Context<Self>) {
        self.clear_pending = false;
        self.delete_pending = None;
        self.clearing = true;
        let future = self.port.clear_sessions();
        Self::run(future, cx, move |this, result, cx| {
            this.clearing = false;
            match result {
                Ok(_) => {
                    this.sessions.epoch += 1;
                    this.sessions.items.clear();
                    this.sessions.total = 0;
                    this.sessions.more_loading = false;
                    if this.current.is_some() {
                        this.leave_current();
                    }
                }
                Err(error) => this.fail_action("clear sessions", &error, cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    /// 当前会话已不存在（被删 / 清空）：回到新对话。
    fn leave_current(&mut self) {
        self.current = None;
        self.detail = Detail {
            epoch: self.detail.epoch + 1,
            ..Detail::new()
        };
        self.issue_editor = None;
        self.ratings.clear();
        self.sent_images = None;
    }

    // ───────────────────────── 提问 / 停止 ─────────────────────────

    /// 输入框文本（已去首尾空白）。
    pub(crate) fn draft(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    pub(crate) fn can_send(&self, cx: &App) -> bool {
        let Gate::Open {
            quota_exhausted: false,
        } = self.gate()
        else {
            return false;
        };
        if self.stale || self.sending || self.attaching > 0 || self.current_run().is_some() {
            return false;
        }
        let draft = self.draft(cx);
        if draft.is_empty() {
            return false;
        }
        let limit = self
            .state
            .config
            .as_ref()
            .map_or(0, |config| config.max_message_chars as usize);
        limit == 0 || draft.chars().count() <= limit
    }

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let text = self.draft(cx);
        let images: Vec<String> = self
            .attachments
            .iter()
            .map(|attachment| attachment.data_url.clone())
            .collect();
        let thumbnails: Vec<Arc<Image>> = self
            .attachments
            .iter()
            .map(|attachment| Arc::clone(&attachment.thumbnail))
            .collect();
        let params = AssistantAskParams {
            session_id: self.current.clone(),
            kind: self.current_kind(),
            channel: AssistantChannel::Desktop,
            text: text.clone(),
            images,
            locale: Some(self.translator.read(cx).locale().to_owned()),
        };
        self.sending = true;
        self.send_failure = None;
        self.action_failed = false;
        let future = self.port.ask(params);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(answer) => {
                        let created = this.current.as_deref() != Some(answer.session_id.as_str());
                        // 本地缩略图记在这次提问所属的会话下，气泡据此显示。
                        this.sent_images = (!thumbnails.is_empty()).then(|| SentImages {
                            session_id: answer.session_id.clone(),
                            text,
                            thumbnails,
                        });
                        if created {
                            this.current = Some(answer.session_id);
                            // 新会话在云端还没有任何已落库的历史：直接以空历史起步，
                            // 提问内容由推送的实时执行显示。
                            this.detail = Detail {
                                load: Load::Ready,
                                epoch: this.detail.epoch + 1,
                                ..Detail::new()
                            };
                        }
                        this.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.sync_input_text(cx);
                        this.attachments.clear();
                        this.scroll.scroll_to_bottom();
                        this.reload_sessions(cx);
                    }
                    Err(error) => {
                        let failure = classify_send_error(&error);
                        log::warn!("assistant ask failed: {:?} ({failure:?})", error.code);
                        this.send_failure = Some(failure);
                        // 额度 / 开放状态可能已变：重新取配置让页面门禁跟上。
                        if matches!(failure, SendFailure::QuotaExceeded | SendFailure::NotOpen) {
                            this.fetch_config(true, cx);
                        }
                    }
                }
                cx.notify();
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.current_run().map(|run| run.session_id.clone()) else {
            return;
        };
        if self.cancelling {
            return;
        }
        self.cancelling = true;
        Self::run(self.port.cancel(session_id), cx, |this, result, cx| {
            this.cancelling = false;
            if let Err(error) = result {
                this.fail_action("cancel", &error, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn set_kind(&mut self, kind: AssistantSessionKind, cx: &mut Context<Self>) {
        if self.current.is_none() && self.kind != kind {
            self.kind = kind;
            cx.notify();
        }
    }

    // ───────────────────────── 工具确认 ─────────────────────────

    pub(crate) fn confirm_tool(
        &mut self,
        request_id: String,
        approved: bool,
        cx: &mut Context<Self>,
    ) {
        if self.stale || !self.confirming.insert(request_id.clone()) {
            return;
        }
        self.action_failed = false;
        Self::run(
            self.port.confirm(request_id.clone(), approved),
            cx,
            move |this, result, cx| {
                // 成功后状态由推送改写；失败则放开按钮让用户重试（若已过期，推送会让卡片变为过期）。
                if let Err(error) = result {
                    this.confirming.remove(&request_id);
                    this.fail_action("confirm", &error, cx);
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn toggle_steps(&mut self, key: &str, currently_open: bool, cx: &mut Context<Self>) {
        self.steps_choice.insert(key.to_owned(), !currently_open);
        cx.notify();
    }

    pub(crate) fn toggle_arguments(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.args_open.remove(key) {
            self.args_open.insert(key.to_owned());
        }
        cx.notify();
    }

    // ───────────────────────── 评分 ─────────────────────────

    /// 一条回答当前显示的评价：本地状态优先，其次云端历史里的值。
    pub(crate) fn shown_rating<'a>(
        &'a self,
        message_id: &str,
        server: Option<&'a str>,
    ) -> Option<&'a str> {
        self.ratings
            .get(message_id)
            .map_or(server, |rating| rating.as_deref())
    }

    /// 评价一条回答：点未选中的 = 选中（赞 / 踩之间直接切换），点已选中的 = 取消。
    /// 先本地更新，失败回滚并提示。`server` 为云端历史里该回答的评价。
    pub(crate) fn rate(
        &mut self,
        message_id: String,
        rating: &'static str,
        server: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(session_id) = self.current.clone() else {
            return;
        };
        if self.stale || self.rating_busy.contains(&message_id) {
            return;
        }
        let previous = self.ratings.get(&message_id).cloned();
        let shown = self.shown_rating(&message_id, server.as_deref());
        let next = (shown != Some(rating)).then(|| rating.to_owned());
        self.action_failed = false;
        self.rating_busy.insert(message_id.clone());
        self.ratings.insert(message_id.clone(), next.clone());
        let params = AssistantRateParams {
            session_id,
            message_id: message_id.clone(),
            rating: next.clone(),
            comment: None,
        };
        Self::run(self.port.rate(params), cx, move |this, result, cx| {
            this.rating_busy.remove(&message_id);
            if let Err(error) = result {
                // 回滚到点击之前的状态；期间已被清空 / 改写的不动。
                if this.ratings.get(&message_id) == Some(&next) {
                    match previous {
                        Some(previous) => {
                            this.ratings.insert(message_id, previous);
                        }
                        None => {
                            this.ratings.remove(&message_id);
                        }
                    }
                }
                this.fail_action("rate", &error, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    // ───────────────────────── 图片附件 ─────────────────────────

    /// 云端接收图片时的 `(单条消息最多张数, 单张最大字节数)`；不接收（或张数为 0）为 `None`。
    pub(crate) fn image_limits(&self) -> Option<(usize, u64)> {
        let config = self.state.config.as_ref()?;
        (config.images_supported && config.max_images > 0)
            .then_some((config.max_images as usize, config.max_image_bytes))
    }

    /// 现在能否添加图片（入口可用、粘贴 / 拖入被接管的前提）。
    pub(crate) fn can_attach(&self) -> bool {
        matches!(
            self.gate(),
            Gate::Open {
                quota_exhausted: false
            }
        ) && !self.stale
            && !self.sending
            && self.image_limits().is_some()
    }

    /// 还能再加几张（已有 + 处理中都占名额）。
    fn free_image_slots(&self) -> usize {
        let max = self.image_limits().map_or(0, |(max, _)| max);
        max.saturating_sub(self.attachments.len() + self.attaching)
    }

    /// 图片附件相关的警示 toast。
    fn warn(&self, key: &str, args: &[(&str, &str)], window: &mut Window, cx: &mut Context<Self>) {
        let message = self.with(key, args, cx);
        window.push_notification(toast::warning(message), cx);
    }

    fn warn_limit(&self, window: &mut Window, cx: &mut Context<Self>) {
        let max = self.image_limits().map_or(0, |(max, _)| max);
        self.warn(
            "assistantAttachErrTooMany",
            &[("max", &max.to_string())],
            window,
            cx,
        );
    }

    /// 输入框收到粘贴：剪贴板里是图片（或全是图片文件）就接管，返回 `true`；否则交还输入框按文字粘贴。
    pub(crate) fn paste_images(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_attach() {
            return false;
        }
        let sources = clipboard_sources(item);
        if sources.is_empty() {
            return false;
        }
        self.add_images(sources, window, cx);
        true
    }

    /// 把文件拖进输入卡片。
    pub(crate) fn drop_files(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_attach() {
            return;
        }
        let sources = paths.iter().cloned().map(ImageSource::File).collect();
        self.add_images(sources, window, cx);
    }

    /// 点「+」：系统文件选择器（可多选）。
    pub(crate) fn pick_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_attach() {
            return;
        }
        if self.free_image_slots() == 0 {
            self.warn_limit(window, cx);
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths,
                // 用户取消了选择。
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    log::warn!("assistant image picker failed: {error:#}");
                    return;
                }
                Err(error) => {
                    log::warn!("assistant image picker closed: {error}");
                    return;
                }
            };
            let sources = paths.into_iter().map(ImageSource::File).collect();
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                this.add_images(sources, window, cx);
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
    }

    /// 把一批图片交给后台读取 / 转码；完成后按顺序加入附件并在光标处插入占位符。
    fn add_images(
        &mut self,
        mut sources: Vec<ImageSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, max_bytes)) = self.image_limits() else {
            return;
        };
        let room = self.free_image_slots();
        if sources.len() > room {
            self.warn_limit(window, cx);
            sources.truncate(room);
        }
        if sources.is_empty() {
            return;
        }
        self.attaching += sources.len();
        let executor = cx.background_executor().clone();
        let tasks: Vec<Task<Result<Prepared, ImageError>>> = sources
            .into_iter()
            .map(|source| executor.spawn(async move { prepare(source, max_bytes) }))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut results = Vec::with_capacity(tasks.len());
            for task in tasks {
                results.push(task.await);
            }
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                this.finish_adding(results, max_bytes, window, cx);
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
        cx.notify();
    }

    fn finish_adding(
        &mut self,
        results: Vec<Result<Prepared, ImageError>>,
        max_bytes: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attaching = self.attaching.saturating_sub(results.len());
        let max_images = self.image_limits().map_or(0, |(max, _)| max);
        let mut added = false;
        let mut over_limit = false;
        let mut unsupported = false;
        let mut unreadable = false;
        let mut too_large = false;
        for result in results {
            match result {
                // 占位符编号取「加入后的张数」，与 `attachments` 的顺序一致。
                Ok(prepared) if self.attachments.len() < max_images => {
                    self.attachments.push(Attachment {
                        data_url: prepared.data_url,
                        thumbnail: prepared.thumbnail,
                    });
                    let number = self.attachments.len();
                    self.insert_image_placeholder(number, window, cx);
                    added = true;
                }
                Ok(_) => over_limit = true,
                Err(ImageError::Unsupported) => unsupported = true,
                Err(ImageError::Unreadable) => unreadable = true,
                Err(ImageError::TooLarge) => too_large = true,
            }
        }
        if added {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        if over_limit {
            self.warn_limit(window, cx);
        }
        if too_large {
            let size = format_bytes(max_bytes, 2);
            self.warn("assistantAttachErrTooLarge", &[("size", &size)], window, cx);
        }
        if unsupported {
            self.warn("assistantAttachErrType", &[], window, cx);
        }
        if unreadable {
            self.warn("assistantAttachErrRead", &[], window, cx);
        }
        cx.notify();
    }

    /// 在输入框光标处（有选区则替换选区）放入第 `number` 张图的占位符。
    fn insert_image_placeholder(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            let text = input.value();
            let edit = insert_placeholder(&text, input.selected_range(), number);
            input.set_selected_range(edit.range, cx);
            input.replace(edit.text, window, cx);
        });
        self.sync_input_text(cx);
    }

    /// 记下输入框当前文本：程序改动（`set_value` 不发 `Change`、占位符增删）后调用，
    /// 免得下一次用户编辑被拿来和过期文本比较。
    fn sync_input_text(&mut self, cx: &App) {
        self.input_text = self.input.read(cx).value();
    }

    /// 输入框文本变了：删到一半的占位符整体删掉；占位符已不在文本里的图片随之移除
    /// （手动删除、剪切、撤销插入等）。
    fn input_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        let previous = std::mem::replace(&mut self.input_text, text.clone());
        if let Some(range) = placeholder_remnant(&previous, &text, self.attachments.len()) {
            self.input.update(cx, |input, cx| {
                input.set_selected_range(range, cx);
                input.replace("", window, cx);
            });
            self.sync_input_text(cx);
        }
        // 从大到小移除，前面的编号才不受重排影响。
        let orphaned = orphaned_images(&self.input_text, self.attachments.len());
        for number in orphaned.into_iter().rev() {
            self.remove_image(number, window, cx);
        }
        cx.notify();
    }

    /// 移除第 `number` 张图：删掉文本里它的占位符，后面的占位符与缩略图编号依次减 1。
    pub(crate) fn remove_image(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.attachments.len();
        if self.sending || number == 0 || number > count {
            return;
        }
        self.attachments.remove(number - 1);
        let text = self.input.read(cx).value();
        let edits = remove_placeholder(&text, number, count);
        if !edits.is_empty() {
            self.input.update(cx, |input, cx| {
                let selection = input.selected_range();
                // 从后往前改，前面的偏移才不会失效；逐处替换保留撤销历史与滚动位置。
                for edit in edits.iter().rev() {
                    input.set_selected_range(edit.range.clone(), cx);
                    input.replace(edit.text.clone(), window, cx);
                }
                let start = map_offset(selection.start, &edits);
                let end = map_offset(selection.end, &edits);
                input.set_selected_range(start..end, cx);
            });
            self.sync_input_text(cx);
        }
        cx.notify();
    }

    /// 这一轮提问的本地缩略图：只认刚发出的那条（最后一轮、文本一致、张数对得上）。
    pub(crate) fn local_thumbnails(&self, turn: &Turn, last: bool) -> Option<&[Arc<Image>]> {
        let sent = self.sent_images.as_ref()?;
        let session = self.current.as_deref()?;
        let same = last
            && sent.session_id == session
            && sent.text == turn.question
            && (turn.live.is_some() || turn.image_count == sent.thumbnails.len());
        same.then_some(sent.thumbnails.as_slice())
    }

    // ───────────────────────── 问题单 ─────────────────────────

    /// 在实时执行与云端历史里找问题单（含本地覆盖）。
    pub(crate) fn find_issue(&self, issue_id: &str) -> Option<AssistantIssueDto> {
        if let Some(issue) = self.issue_overrides.get(issue_id) {
            return Some(issue.clone());
        }
        self.state
            .runs
            .iter()
            .flat_map(|run| run.items.iter())
            .find_map(|item| match item {
                AssistantRunItemDto::IssueDraft { issue } if issue.id == issue_id => {
                    Some(issue.clone())
                }
                _ => None,
            })
            .or_else(|| {
                self.detail
                    .issues
                    .iter()
                    .find(|issue| issue.id == issue_id)
                    .cloned()
            })
    }

    pub(crate) fn edit_issue(
        &mut self,
        issue_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(issue) = self.find_issue(issue_id) else {
            return;
        };
        let title = SharedString::from(issue.title);
        let body = SharedString::from(issue.body);
        let title_input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let body_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 14)
                .default_value(body)
        });
        let subscriptions = [
            cx.subscribe(&title_input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&body_input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        self.issue_editor = Some(IssueEditor {
            issue_id: issue.id,
            title: title_input,
            body: body_input,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    pub(crate) fn cancel_issue_edit(&mut self, cx: &mut Context<Self>) {
        self.issue_editor = None;
        cx.notify();
    }

    /// 确认提交问题单；`edited` 为修改后的标题 / 正文。
    pub(crate) fn confirm_issue(
        &mut self,
        issue_id: String,
        edited: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if self.stale || !self.issue_busy.insert(issue_id.clone()) {
            return;
        }
        self.action_failed = false;
        let (title, body) = edited.unzip();
        let params = AssistantIssueParams {
            issue_id: issue_id.clone(),
            title,
            body,
        };
        Self::run(
            self.port.confirm_issue(params),
            cx,
            move |this, result, cx| {
                this.issue_busy.remove(&issue_id);
                match result {
                    Ok(issue) => {
                        this.issue_overrides.insert(issue_id, issue);
                        this.issue_editor = None;
                    }
                    Err(error) => this.fail_action("confirm issue", &error, cx),
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn discard_issue(&mut self, issue_id: String, cx: &mut Context<Self>) {
        if self.stale || !self.issue_busy.insert(issue_id.clone()) {
            return;
        }
        self.action_failed = false;
        let snapshot = self.find_issue(&issue_id);
        Self::run(
            self.port.discard_issue(issue_id.clone()),
            cx,
            move |this, result, cx| {
                this.issue_busy.remove(&issue_id);
                match result {
                    Ok(()) => {
                        if let Some(mut issue) = snapshot {
                            "discarded".clone_into(&mut issue.status);
                            this.issue_overrides.insert(issue_id, issue);
                        }
                        this.issue_editor = None;
                    }
                    Err(error) => this.fail_action("discard issue", &error, cx),
                }
                cx.notify();
            },
        );
        cx.notify();
    }
}

/// 进行中的执行：`(会话 ID, 起始时间)`。
fn active_runs(state: &AssistantStateDto) -> HashSet<(String, i64)> {
    state
        .runs
        .iter()
        .filter(|run| run.status.is_active())
        .map(|run| (run.session_id.clone(), run.started_at_unix_ms))
        .collect()
}

/// 仍待确认的请求 ID。
fn pending_request_ids(state: &AssistantStateDto) -> HashSet<&str> {
    use fluxdown_protocol::AssistantConfirmationState::Pending;
    state
        .runs
        .iter()
        .flat_map(|run| run.items.iter())
        .filter_map(|item| match item {
            AssistantRunItemDto::Tool { call } if call.confirmation == Some(Pending) => {
                call.request_id.as_deref()
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
