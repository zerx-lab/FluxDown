//! 助手视图：状态、会话事件与对话动作。绘制在 `render*.rs`，历史会话在 `history.rs`，
//! 提问流程在 `ask.rs`，输入草稿与图片附件在 `draft.rs`。

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fluxdown_protocol::{
    AgentEvent, AgentSnapshot, AssistantIssueDto, AssistantIssueParams, AssistantMessageDto,
    AssistantRateParams, AssistantRunDto, AssistantRunItemDto, AssistantSessionDto,
    AssistantSessionKind, AssistantStateDto, RpcErrorData, ServiceEvent,
};
use fluxdown_ui_i18n::Translator;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, ScrollHandle, SharedString,
    Subscription, Window, px,
};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use jiff::ZonedDateTime;

use crate::{
    AssistantPort, PortFuture,
    ask::{Editing, Outgoing},
    dates::local_now,
    draft::{Attachment, SentImages},
    history::{Load, Rename, SessionList},
    image_viewer::ImagePreview,
    images::ImageCache,
    model::{
        ActionError, Gate, SendFailure, SessionQuota, Turn, agent_now_ms, build_timeline,
        clock_offset_ms, gate, merge_config_response, pending_confirmations, rating_comment,
        session_quota, turn_transcript,
    },
};

/// 距底部多近算「跟随最新内容」。
const FOLLOW_BOTTOM_SLACK: f32 = 40.;
/// 输入框默认高度（行数）。
const INPUT_MIN_ROWS: usize = 3;
/// 输入框最多长到几行。
const INPUT_MAX_ROWS: usize = 8;
/// 「感谢反馈」的停留时长。
const THANKS_FLASH: Duration = Duration::from_secs(3);
/// 倒计时刷新间隔。
const TICK: Duration = Duration::from_secs(1);
/// 面板重新打开时，配置 / 会话列表超过这个时长就后台刷新。
const CONFIG_STALE_AFTER: Duration = Duration::from_secs(60);
const SESSIONS_STALE_AFTER: Duration = Duration::from_secs(30);
/// 「已复制」提示的显示时长。
const COPIED_FLASH: Duration = Duration::from_millis(1500);

/// 主窗口中的独立动作（app 注入；本 crate 不认识其他页面）。
pub type WindowAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// app 注入的宿主动作。
#[derive(Clone)]
pub struct AssistantHost {
    /// 打开账户页（登录、套餐都在那里）。
    pub open_account: WindowAction,
    /// 打开套餐购买对话框（选套餐 → 扫码 → 成功）。
    pub open_plan_purchase: WindowAction,
}

/// 当前会话在云端的历史。
pub(crate) struct Detail {
    pub(crate) load: Load,
    pub(crate) session: Option<AssistantSessionDto>,
    pub(crate) messages: Vec<AssistantMessageDto>,
    pub(crate) issues: Vec<AssistantIssueDto>,
    /// 本会话已计入单会话提问上限的次数（云端口径；执行结束后随详情重读更新）。
    pub(crate) questions_used: u64,
    /// 只增不减：任何会让在途读取作废的变化（换会话、重读、账号变化）都递增。
    epoch: u64,
}

impl Detail {
    fn new(epoch: u64) -> Self {
        Self {
            load: Load::Idle,
            session: None,
            messages: Vec::new(),
            issues: Vec::new(),
            questions_used: 0,
            epoch,
        }
    }
}

/// 正在修改的问题单草稿。
pub(crate) struct IssueEditor {
    pub(crate) issue_id: String,
    pub(crate) title: Entity<InputState>,
    pub(crate) body: Entity<TextareaState>,
    /// 输入变化时重绘（提交按钮的可用性取决于标题 / 正文是否为空）。
    _subscriptions: [Subscription; 2],
}

/// 评价点踩后就地出现的可选说明输入。
pub(crate) struct RateComment {
    pub(crate) message_id: String,
    pub(crate) input: Entity<InputState>,
    /// 说明提交中。
    pub(crate) busy: bool,
    /// 已提交：显示「感谢反馈」。
    pub(crate) thanks: bool,
    /// 输入变化时重绘（字数与提交按钮的可用性随之变化）。
    _subscription: Subscription,
}

/// 刚复制过什么：按钮短暂显示勾。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyKind {
    /// 整轮对话（含过程）。
    Turn,
    /// 只复制回答。
    Answer,
}

pub struct AssistantView {
    pub(crate) translator: Entity<Translator>,
    pub(crate) port: Arc<dyn AssistantPort>,
    pub(crate) host: AssistantHost,
    /// 面板根焦点：点面板空白处获得焦点，面板内快捷键据此生效；收起面板时据此判断是否交还焦点。
    pub(crate) root_focus: FocusHandle,
    /// 面板显示会话列表（历史视图）而不是对话；选中会话 / 新对话后回到对话。
    pub(crate) history_open: bool,
    /// 宿主带进来的提问（[`Self::ask`]）：配置加载完成后自动发出，确认无法提问则留在输入框。
    pending_submit: bool,
    /// 账号变化后要清空输入框（下一次渲染里生效，那里才有 `window`）。
    pub(crate) draft_reset: bool,
    /// 待在下一次渲染里写回输入框的文本：编辑重发被中断（当前会话被删 / 清空）后还原暂存的草稿。
    pub(crate) pending_draft: Option<String>,
    pub(crate) input: Entity<TextareaState>,
    /// 输入框当前 placeholder；语言切换后在 render 中同步（输入框只在构造时取一次）。
    pub(crate) input_placeholder: SharedString,
    /// 历史视图的搜索框。
    pub(crate) search: Entity<InputState>,
    pub(crate) search_placeholder: SharedString,
    /// 搜索框当前文本（本地即时过滤用）。
    pub(crate) search_text: String,
    pub(crate) search_gen: u64,
    pub(crate) scroll: ScrollHandle,
    pub(crate) history_scroll: ScrollHandle,
    /// 历史列表容器的焦点（键盘上下 / 回车）。
    pub(crate) history_focus: FocusHandle,
    /// 键盘光标所在的会话。
    pub(crate) history_cursor: Option<String>,
    /// 历史视图里动作失败的提示（文案键）。
    pub(crate) history_error: Option<&'static str>,
    /// 会话列表日期换算用的本机「现在」，打开历史 / 列表更新时刷新。
    pub(crate) now_local: ZonedDateTime,

    pub(crate) state: AssistantStateDto,
    pub(crate) signed_in: bool,
    /// 当前登录账号；账号变化（登出 / 换号）时作废全部账号相关缓存。
    user_id: Option<String>,
    /// 只增不减：回写前与发起时比较，账号变化后旧账号的在途响应一律丢弃。
    pub(crate) account_epoch: u64,
    /// 与 agent 断开：内容保留只读，动作禁用。
    pub(crate) stale: bool,
    /// 配置拉取失败（本机服务 / 云端不可达）。
    pub(crate) config_failed: bool,
    pub(crate) config_loading: bool,
    config_requested: bool,
    /// 配置在途时又来了一次强制刷新：当前这次返回后再取一次。
    config_refetch: bool,
    config_loaded_at: Option<Instant>,
    /// 助手配置被推送改写的次数：配置请求发出后若期间有推送改写了配置，响应里的配置就比推送旧。
    config_push_gen: u64,
    /// agent 主机时钟比本机快多少毫秒（换算确认截止时刻用）。
    clock_offset: i64,

    pub(crate) sessions: SessionList,
    /// 当前会话；`None` = 新对话。
    pub(crate) current: Option<String>,
    pub(crate) detail: Detail,
    /// 当前会话的时间线缓存：只在状态 / 历史 / 问题单变化时重建，倒计时刷新不重建。
    pub(crate) timeline: Vec<Turn>,
    /// 新对话的类型（已有会话沿用其类型）。
    pub(crate) kind: AssistantSessionKind,

    /// 已发出但还没被实时执行接管显示的提问。
    pub(crate) outgoing: Option<Outgoing>,
    pub(crate) outgoing_gen: u64,
    pub(crate) cancelling: bool,
    pub(crate) send_failure: Option<SendFailure>,
    /// 被限频后本地冷却到此刻：期间不再发出提问，避免反复回车让气泡与提示来回闪。
    pub(crate) send_cooldown_until: Option<std::time::Instant>,
    /// 其他动作失败的提示（确认 / 评分 / 问题单 / 取消 / 重命名）。
    pub(crate) action_error: Option<ActionError>,
    /// 已发出、等待结果的确认请求（避免重复点击）。
    pub(crate) confirming: HashSet<String>,
    /// 用户对「过程」折叠区的显式选择（键 = 轮次键）。
    pub(crate) steps_choice: HashMap<String, bool>,
    /// 已展开参数 / 结果的工具调用（键 = 调用键）。
    pub(crate) args_open: HashSet<String>,
    /// 本地确认 / 丢弃后的问题单最新状态。
    pub(crate) issue_overrides: HashMap<String, AssistantIssueDto>,
    pub(crate) issue_busy: HashSet<String>,
    pub(crate) issue_editor: Option<IssueEditor>,
    /// 等待再次点击确认删除的会话；超时或 Esc 取消。
    pub(crate) delete_pending: Option<String>,
    /// 等待再次点击确认的「清空全部会话」；超时或 Esc 取消。
    pub(crate) clear_pending: bool,
    /// 删除请求进行中的会话。
    pub(crate) deleting: HashSet<String>,
    /// 清空请求进行中。
    pub(crate) clearing: bool,
    /// 二次确认的超时代数：旧定时器不能取消新的确认。
    pub(crate) confirm_gen: u64,
    pub(crate) renaming: Option<Rename>,
    /// 正在导出的会话。
    pub(crate) exporting: Option<String>,
    /// 正在编辑重发的历史提问。
    pub(crate) editing: Option<Editing>,
    /// 每条回答（键 = 消息 ID）的本地评价状态：点击后立即写入（乐观更新），失败回滚；
    /// 优先于云端历史里的值。切换会话时清空（新读到的历史即为准）。
    pub(crate) ratings: HashMap<String, Option<String>>,
    /// 评价请求进行中的消息 ID（避免重复点击）。
    pub(crate) rating_busy: HashSet<String>,
    /// 点踩后的说明输入。
    pub(crate) rate_comment: Option<RateComment>,
    /// 附图大图预览。
    pub(crate) image_preview: Option<ImagePreview>,
    /// 侧栏面板是否展开（面板收起时待确认要在入口角标提醒）。
    pub(crate) panel_open: bool,
    pub(crate) copied: Option<(String, CopyKind)>,
    copied_generation: u32,
    /// 待随下一条提问发送的图片；第 n 张对应输入文本里的 `[Image #n]`。
    pub(crate) attachments: Vec<Attachment>,
    /// 上次处理过的输入框文本，用来识别一次删除是否只删掉了占位符的一部分。
    pub(crate) input_text: SharedString,
    /// 正在后台读取 / 转码的图片数（计入数量上限）。
    pub(crate) attaching: usize,
    /// 刚发出的带图提问。
    pub(crate) sent_images: Option<SentImages>,
    /// 历史提问附图（按附件 ID 缓存解码后的缩略图）。
    pub(crate) images: ImageCache,
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
        let search_placeholder = SharedString::from(
            translator
                .read(cx)
                .text("assistantSearchPlaceholder")
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
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(search_placeholder.clone()));
        cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_search_event(event, window, cx);
            },
        )
        .detach();
        Self {
            translator,
            port,
            host,
            root_focus: cx.focus_handle(),
            history_open: false,
            pending_submit: false,
            draft_reset: false,
            pending_draft: None,
            input,
            input_placeholder,
            search,
            search_placeholder,
            search_text: String::new(),
            search_gen: 0,
            scroll: ScrollHandle::new(),
            history_scroll: ScrollHandle::new(),
            history_focus: cx.focus_handle(),
            history_cursor: None,
            history_error: None,
            now_local: local_now(),
            state: AssistantStateDto::default(),
            signed_in: false,
            user_id: None,
            account_epoch: 0,
            stale: false,
            config_failed: false,
            config_loading: false,
            config_requested: false,
            config_refetch: false,
            config_loaded_at: None,
            config_push_gen: 0,
            clock_offset: 0,
            sessions: SessionList::new(),
            current: None,
            detail: Detail::new(0),
            timeline: Vec::new(),
            kind: AssistantSessionKind::Ask,
            outgoing: None,
            outgoing_gen: 0,
            cancelling: false,
            send_failure: None,
            send_cooldown_until: None,
            action_error: None,
            confirming: HashSet::new(),
            steps_choice: HashMap::new(),
            args_open: HashSet::new(),
            issue_overrides: HashMap::new(),
            issue_busy: HashSet::new(),
            issue_editor: None,
            delete_pending: None,
            clear_pending: false,
            deleting: HashSet::new(),
            clearing: false,
            confirm_gen: 0,
            renaming: None,
            exporting: None,
            editing: None,
            ratings: HashMap::new(),
            rating_busy: HashSet::new(),
            copied: None,
            copied_generation: 0,
            attachments: Vec::new(),
            input_text: SharedString::default(),
            attaching: 0,
            sent_images: None,
            rate_comment: None,
            image_preview: None,
            panel_open: false,
            images: ImageCache::default(),
            ticker_running: false,
        }
    }

    /// 面板顶栏插槽：标题、今日用量、新对话与历史会话切换；随本视图重绘。
    pub fn new_panel_header(&self, cx: &mut Context<Self>) -> Entity<crate::AssistantPanelHeader> {
        let view = cx.entity();
        cx.new(|cx| crate::AssistantPanelHeader::new(view, cx))
    }

    /// 所有会话里等待用户确认的工具调用数（状态栏 / 活动栏按钮的提醒点据此绘制）。
    pub fn pending_count(&self) -> usize {
        pending_confirmations(&self.state, self.agent_now())
    }

    /// 入口角标:只在面板收起或停在历史视图时提醒待确认(对话视图里确认卡片就在眼前)。
    pub fn badge_pending(&self) -> bool {
        (!self.panel_open || self.history_open) && self.pending_count() > 0
    }

    /// 面板打开：焦点交给输入框（历史视图则交给列表），配置与会话列表过期就后台刷新。
    pub fn on_panel_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.panel_open {
            self.panel_open = true;
            cx.notify();
        }
        let open = matches!(self.gate(), Gate::Open { .. });
        if self.history_open && open {
            window.focus(&self.history_focus, cx);
        } else {
            self.focus_input(window, cx);
        }
        if !self.signed_in || self.stale {
            return;
        }
        if self.config_requested
            && self
                .config_loaded_at
                .is_none_or(|at| at.elapsed() > CONFIG_STALE_AFTER)
        {
            self.fetch_config(true, cx);
        }
        if open
            && self.sessions.load != Load::Loading
            && self
                .sessions
                .loaded_at
                .is_none_or(|at| at.elapsed() > SESSIONS_STALE_AFTER)
        {
            self.reload_sessions(cx);
        }
        if open {
            self.reload_detail_for_retry(cx);
        }
    }

    /// 面板收起：焦点还在面板里就清掉，让快捷键回到主区。
    pub fn on_panel_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel_open {
            self.panel_open = false;
            cx.notify();
        }
        // 重命名输入框与大图预览不跨面板收起残留（两个都要取，不能短路）。
        let renaming = self.renaming.take();
        let preview = self.image_preview.take();
        if renaming.is_some() || preview.is_some() {
            cx.notify();
        }
        if self.root_focus.contains_focused(window, cx) {
            window.blur(cx);
        }
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
        // 新问题取代整份草稿：账号变化遗留的「待清空」标记（面板没渲染过就没被消费）与暂存的
        // 草稿都不能再作用到这次写入的问题上。
        self.draft_reset = false;
        self.pending_draft = None;
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
        let title = self
            .session_info(current)
            .map(|session| session.title.trim());
        match title.filter(|title| !title.is_empty()) {
            Some(title) => SharedString::from(title.to_owned()),
            None => self.t("assistantUntitled", cx),
        }
    }

    /// 会话信息：优先当前会话已读到的详情，其次列表。
    pub(crate) fn session_info(&self, id: &str) -> Option<&AssistantSessionDto> {
        self.detail
            .session
            .as_ref()
            .filter(|session| session.id == id)
            .or_else(|| self.sessions.items.iter().find(|session| session.id == id))
    }

    /// 顶栏标题可点击重命名：对话视图里的已有会话。
    pub(crate) fn can_rename_current(&self) -> bool {
        matches!(self.gate(), Gate::Open { .. })
            && !self.history_open
            && !self.stale
            && self.current.is_some()
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

    /// 当前会话的提问上限状态：新对话（尚无会话）与不限（`sessionLimit` 为 0 / 缺失）都是 `Free`。
    pub(crate) fn current_session_quota(&self) -> SessionQuota {
        if self.current.is_none() {
            return SessionQuota::Free;
        }
        let limit = self
            .state
            .config
            .as_ref()
            .map_or(0, |config| config.session_limit);
        session_quota(limit, self.detail.questions_used)
    }

    /// 当前会话已达提问上限、且没有进行中的执行（进行中的提问已先占用名额，那时仍显示输入区）。
    pub(crate) fn session_limit_reached(&self) -> bool {
        self.current_session_quota() == SessionQuota::Reached
            && self.current_run().is_none()
            && self.visible_outgoing().is_none()
    }

    /// 被服务端以会话上限拒绝：把本地状态视为已达上限，输入区随即换成提示卡。
    pub(crate) fn mark_session_limit_reached(&mut self) {
        let limit = self
            .state
            .config
            .as_ref()
            .map_or(0, |config| u64::from(config.session_limit));
        if self.current.is_some() {
            self.detail.questions_used = self.detail.questions_used.max(limit);
        }
    }

    /// 现在的 agent 时钟（与 `confirm_deadline_unix_ms` 同一时钟）。
    pub(crate) fn agent_now(&self) -> i64 {
        agent_now_ms(now_unix_ms(), self.clock_offset)
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
        match self.session_info(id).map(|session| session.kind.as_str()) {
            Some("feedback") => AssistantSessionKind::Feedback,
            _ => AssistantSessionKind::Ask,
        }
    }

    // ───────────────────────── 会话事件 ─────────────────────────

    pub fn replace_snapshot(&mut self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) {
        self.stale = false;
        let user = snapshot
            .session
            .as_ref()
            .map(|session| session.user.id.clone());
        self.set_account(user);
        self.apply_state(snapshot.assistant.clone(), cx);
        cx.notify();
    }

    pub fn apply_event(&mut self, event: &ServiceEvent, cx: &mut Context<Self>) {
        let changed = match event {
            ServiceEvent::Agent(AgentEvent::AssistantChanged(state)) => {
                self.apply_state(state.clone(), cx)
            }
            ServiceEvent::Agent(AgentEvent::SessionChanged(session)) => {
                let user = (**session).as_ref().map(|session| session.user.id.clone());
                self.set_account(user)
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

    /// 账号变化（登出 / 换号）：账号相关的一切（会话、历史、配置、草稿、附件、评分、在途请求）
    /// 一律作废。同一账号重复推送不动。返回是否变化。
    fn set_account(&mut self, user: Option<String>) -> bool {
        if self.user_id == user {
            return false;
        }
        self.signed_in = user.is_some();
        self.user_id = user;
        // 旧账号的在途响应回写前会与它比较，对不上就丢弃。
        self.account_epoch += 1;
        self.sessions.reset();
        self.current = None;
        self.reset_detail(Load::Idle);
        self.timeline.clear();
        self.config_failed = false;
        self.config_requested = false;
        self.config_loading = false;
        self.config_refetch = false;
        self.config_loaded_at = None;
        self.send_failure = None;
        self.send_cooldown_until = None;
        self.action_error = None;
        self.history_error = None;
        self.history_cursor = None;
        self.confirming.clear();
        self.steps_choice.clear();
        self.args_open.clear();
        self.issue_overrides.clear();
        self.issue_busy.clear();
        self.issue_editor = None;
        self.delete_pending = None;
        self.clear_pending = false;
        self.deleting.clear();
        self.clearing = false;
        self.renaming = None;
        self.exporting = None;
        self.editing = None;
        self.history_open = false;
        self.pending_submit = false;
        self.search_text.clear();
        self.ratings.clear();
        self.rating_busy.clear();
        self.copied = None;
        self.outgoing = None;
        self.cancelling = false;
        self.sent_images = None;
        self.images.clear();
        self.rate_comment = None;
        self.image_preview = None;
        self.attachments.clear();
        self.attaching = 0;
        self.draft_reset = true;
        true
    }

    /// 用推送 / 快照整体替换助手状态；返回是否变化。
    fn apply_state(&mut self, state: AssistantStateDto, cx: &mut Context<Self>) -> bool {
        let mut state = state;
        // agent 时钟只用来算偏差；它每次推送都在变，不参与「是否变化」的比较。
        if state.now_unix_ms != 0 {
            self.clock_offset = clock_offset_ms(now_unix_ms(), state.now_unix_ms);
        }
        state.now_unix_ms = self.state.now_unix_ms;
        if self.state == state {
            return false;
        }
        let follow = self.near_bottom();
        let before = active_runs(&self.state);
        let runs_changed = self.state.runs != state.runs;
        if self.state.config != state.config {
            self.config_push_gen = self.config_push_gen.wrapping_add(1);
        }
        self.state = state;
        let after = active_runs(&self.state);
        let pending = pending_request_ids(&self.state);
        self.confirming.retain(|id| pending.contains(id.as_str()));
        if self.state.config.is_some() {
            self.config_failed = false;
        }
        if runs_changed {
            self.settle_outgoing();
            self.rebuild_timeline(cx);
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
        if runs_changed {
            self.reload_detail_for_retry(cx);
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

    /// 最后一轮失败、已有执行 ID 却还没读到它的用户消息（重读历史失败 / 还没落到）：重读一次会话历史，
    /// 读到后重试变成「替换」。只在状态推送与面板打开时触发，历史读取自己的结果不会再触发它，不会循环。
    fn reload_detail_for_retry(&mut self, cx: &mut Context<Self>) {
        if !self.signed_in || self.stale || self.detail.load == Load::Loading {
            return;
        }
        let Some(session_id) = self.current.clone() else {
            return;
        };
        if self.timeline.last().is_some_and(Turn::awaiting_history) {
            self.load_detail(session_id, cx);
        }
    }

    /// 倒计时：有待确认的工具时每秒重绘（只重绘，不重建时间线）；待确认消失（到期）后补刷一次并停止。
    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker_running || self.pending_count() == 0 {
            return;
        }
        self.ticker_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let Ok(more) = this.update(cx, |this, cx| {
                    let more = this.pending_count() > 0;
                    this.ticker_running = more;
                    cx.notify();
                    more
                }) else {
                    // 助手页已释放，结束倒计时刷新。
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

    /// 等一个 agent 请求并回写；账号变化后旧账号的响应直接丢弃。
    pub(crate) fn run<T: 'static>(
        &self,
        future: PortFuture<T>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, RpcErrorData>, &mut Context<Self>) + 'static,
    ) {
        let account = self.account_epoch;
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, move |this, cx| {
                if this.account_epoch == account {
                    done(this, result, cx);
                }
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
    }

    /// 同 [`Self::run`]，回写时带 `window`。
    pub(crate) fn run_in<T: 'static>(
        &self,
        window: &mut Window,
        future: PortFuture<T>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, RpcErrorData>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let account = self.account_epoch;
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                if this.account_epoch == account {
                    done(this, result, window, cx);
                }
            }) else {
                // 助手页已随窗口释放，无需回写。
                return;
            };
        })
        .detach();
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

    /// 取云端配置；`refresh` 强制向云端刷新（额度、开放状态可能已变）。
    pub(crate) fn fetch_config(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if self.config_loading {
            // 在途的这次可能比刚发生的事更早：返回后再强制取一次。
            self.config_refetch |= refresh;
            return;
        }
        self.config_loading = true;
        let requested_push_gen = self.config_push_gen;
        let future = if refresh {
            self.port.refresh()
        } else {
            self.port.get()
        };
        self.run(future, cx, move |this, result, cx| {
            this.config_loading = false;
            match result {
                Ok(state) => {
                    // 执行（runs）只信推送；期间若有推送改写了配置，响应里的配置比推送旧，不应用。
                    let stale = this.config_push_gen != requested_push_gen;
                    let merged = merge_config_response(&this.state, state, stale);
                    this.apply_state(merged, cx);
                    this.config_failed = false;
                    this.config_loaded_at = Some(Instant::now());
                }
                Err(error) => {
                    log::warn!("assistant config fetch failed: {:?}", error.code);
                    this.config_failed = true;
                }
            }
            if std::mem::take(&mut this.config_refetch) {
                this.fetch_config(true, cx);
            }
            cx.notify();
        });
    }

    // ───────────────────────── 当前会话 ─────────────────────────

    /// 用空历史换掉当前详情（`epoch` 递增，在途的读取作废）。
    pub(crate) fn reset_detail(&mut self, load: Load) {
        let epoch = self.detail.epoch + 1;
        self.detail = Detail {
            load,
            ..Detail::new(epoch)
        };
    }

    /// 进入某个会话（`None` = 新对话）：清掉上一个会话的界面状态，时间线按新会话重建。
    fn enter_session(&mut self, session: Option<String>, cx: &mut Context<Self>) {
        self.current = session;
        self.reset_detail(Load::Idle);
        self.send_failure = None;
        self.action_error = None;
        self.issue_editor = None;
        self.issue_overrides.clear();
        self.ratings.clear();
        self.sent_images = None;
        self.rate_comment = None;
        self.image_preview = None;
        self.rebuild_timeline(cx);
    }

    pub(crate) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_edit(window, cx);
        self.cancel_transient();
        self.history_open = false;
        self.enter_session(None, cx);
        self.focus_input(window, cx);
        cx.notify();
    }

    /// 切到某个会话；焦点交给输入框，便于直接追问。
    pub(crate) fn select_session(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_edit(window, cx);
        self.cancel_transient();
        self.history_open = false;
        self.focus_input(window, cx);
        if self.current.as_deref() == Some(session_id.as_str()) {
            cx.notify();
            return;
        }
        self.enter_session(Some(session_id.clone()), cx);
        self.scroll.scroll_to_bottom();
        self.load_detail(session_id, cx);
        cx.notify();
    }

    pub(crate) fn load_detail(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.detail.epoch += 1;
        let epoch = self.detail.epoch;
        self.detail.load = Load::Loading;
        let future = self.port.session(session_id.clone());
        self.run(future, cx, move |this, result, cx| {
            // 期间换了会话 / 又读了一次：新的状态自己负责 `load`，旧响应不能再写。
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
                    this.detail.questions_used = detail.questions_used;
                    // 新读到的历史里问题单状态以云端为准。
                    this.issue_overrides.clear();
                    this.rebuild_timeline(cx);
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

    /// 当前会话已不存在（被删 / 清空）：回到新对话。
    pub(crate) fn leave_current(&mut self, cx: &mut Context<Self>) {
        self.abandon_edit();
        self.enter_session(None, cx);
    }

    // ───────────────────────── 时间线与附图 ─────────────────────────

    /// 重建时间线缓存（并为其中的历史附图发起读取）：状态 / 历史 / 问题单变化时调用。
    pub(crate) fn rebuild_timeline(&mut self, cx: &mut Context<Self>) {
        let session_id = self.current.clone().unwrap_or_default();
        self.timeline = build_timeline(
            &session_id,
            &self.detail.messages,
            &self.detail.issues,
            &self.state.runs,
            &self.issue_overrides,
        );
        let wanted: Vec<String> = self
            .timeline
            .iter()
            .flat_map(|turn| turn.image_ids.iter().cloned())
            .collect();
        for id in wanted {
            // 只取还没有状态的；失败的由用户点占位重试，免得每次推送都重取。
            if self.images.get(&id).is_none() {
                self.fetch_image(id, cx);
            }
        }
    }

    /// 取一张历史附图并在后台解码成缩略图 + 原图；已在加载 / 已完成的不重复取。
    pub(crate) fn fetch_image(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.images.begin(&id) {
            return;
        }
        let future = self.port.attachment(id.clone());
        let account = self.account_epoch;
        cx.spawn(async move |this, cx| {
            let image = match future.await {
                Ok(attachment) => {
                    let decoded = cx
                        .background_executor()
                        .spawn(async move {
                            crate::attachment::image_from_data_url(&attachment.data_url)
                        })
                        .await;
                    match decoded {
                        Ok(image) => Some(image),
                        Err(error) => {
                            log::warn!("assistant attachment decode failed: {error:?}");
                            None
                        }
                    }
                }
                Err(error) => {
                    log::warn!("assistant attachment fetch failed: {:?}", error.code);
                    None
                }
            };
            let Ok(()) = this.update(cx, move |this, cx| {
                if this.account_epoch == account {
                    let keep: HashSet<String> = this
                        .timeline
                        .iter()
                        .flat_map(|turn| turn.image_ids.iter().cloned())
                        .collect();
                    this.images.finish(id, image, &keep);
                    cx.notify();
                }
            }) else {
                // 助手页已释放，无需回写。
                return;
            };
        })
        .detach();
        cx.notify();
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
        self.action_error = None;
        let future = self.port.confirm(request_id.clone(), approved);
        self.run(future, cx, move |this, result, cx| {
            // 成功后状态由推送改写；失败则放开按钮让用户重试（若已过期，推送会让卡片变为过期）。
            if let Err(error) = result {
                this.confirming.remove(&request_id);
                this.report_failure("confirm", &error, cx);
            }
            cx.notify();
        });
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

    // ───────────────────────── 复制 ─────────────────────────

    /// 复制一轮对话（含过程）到剪贴板，按钮短暂显示「已复制」。
    pub(crate) fn copy_turn(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(turn) = self.timeline.iter().find(|turn| turn.key == key) else {
            return;
        };
        let text = {
            let translator = self.translator.read(cx);
            turn_transcript(turn, self.agent_now(), &|label, args| {
                translator.text_with(label, args)
            })
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.flash_copied(key, CopyKind::Turn, cx);
    }

    /// 只复制这一轮的回答（不含工具参数与结果）。
    pub(crate) fn copy_answer(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(answer) = self
            .timeline
            .iter()
            .find(|turn| turn.key == key)
            .and_then(|turn| turn.answer.as_deref())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(answer.to_owned()));
        self.flash_copied(key, CopyKind::Answer, cx);
    }

    fn flash_copied(&mut self, key: &str, kind: CopyKind, cx: &mut Context<Self>) {
        let generation = self.copied_generation.wrapping_add(1);
        self.copied_generation = generation;
        self.copied = Some((key.to_owned(), kind));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FLASH).await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this.copied_generation == generation {
                    this.copied = None;
                    cx.notify();
                }
            }) else {
                // 视图已释放，结束回调而不再更新状态。
                return;
            };
        })
        .detach();
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
        window: &mut Window,
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
        self.action_error = None;
        self.rating_busy.insert(message_id.clone());
        self.ratings.insert(message_id.clone(), next.clone());
        if next.as_deref() == Some("down") {
            self.open_rate_comment(message_id.clone(), window, cx);
        } else if self
            .rate_comment
            .as_ref()
            .is_some_and(|comment| comment.message_id == message_id)
        {
            self.rate_comment = None;
        }
        let params = AssistantRateParams {
            session_id,
            message_id: message_id.clone(),
            rating: next.clone(),
            comment: None,
        };
        let future = self.port.rate(params);
        self.run(future, cx, move |this, result, cx| {
            this.rating_busy.remove(&message_id);
            if let Err(error) = result {
                // 回滚到点击之前的状态；期间已被清空 / 改写的不动。
                if this.ratings.get(&message_id) == Some(&next) {
                    if this
                        .rate_comment
                        .as_ref()
                        .is_some_and(|comment| comment.message_id == message_id)
                    {
                        this.rate_comment = None;
                    }
                    match previous {
                        Some(previous) => {
                            this.ratings.insert(message_id, previous);
                        }
                        None => {
                            this.ratings.remove(&message_id);
                        }
                    }
                }
                this.report_failure("rate", &error, cx);
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

    /// 只在编辑器正是这张问题单的时候关掉：别的问题单的编辑不能被这张的提交 / 丢弃结果带走。
    fn close_issue_editor_for(&mut self, issue_id: &str) {
        if self
            .issue_editor
            .as_ref()
            .is_some_and(|editor| editor.issue_id == issue_id)
        {
            self.issue_editor = None;
        }
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
        self.action_error = None;
        let (title, body) = edited.unzip();
        let params = AssistantIssueParams {
            issue_id: issue_id.clone(),
            title,
            body,
        };
        let future = self.port.confirm_issue(params);
        self.run(future, cx, move |this, result, cx| {
            this.issue_busy.remove(&issue_id);
            match result {
                Ok(issue) => {
                    this.close_issue_editor_for(&issue_id);
                    this.issue_overrides.insert(issue_id, issue);
                    this.rebuild_timeline(cx);
                }
                Err(error) => this.report_failure("confirm issue", &error, cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn discard_issue(&mut self, issue_id: String, cx: &mut Context<Self>) {
        if self.stale || !self.issue_busy.insert(issue_id.clone()) {
            return;
        }
        self.action_error = None;
        let snapshot = self.find_issue(&issue_id);
        let future = self.port.discard_issue(issue_id.clone());
        self.run(future, cx, move |this, result, cx| {
            this.issue_busy.remove(&issue_id);
            match result {
                Ok(()) => {
                    if let Some(mut issue) = snapshot {
                        "discarded".clone_into(&mut issue.status);
                        this.issue_overrides.insert(issue_id.clone(), issue);
                    }
                    this.close_issue_editor_for(&issue_id);
                    this.rebuild_timeline(cx);
                }
                Err(error) => this.report_failure("discard issue", &error, cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    // ───────────────────────── 点踩说明 ─────────────────────────

    /// 点踩后打开说明输入（输入框随即获得焦点）。
    fn open_rate_comment(
        &mut self,
        message_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.submit_rate_comment(cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.rate_comment = Some(RateComment {
            message_id,
            input,
            busy: false,
            thanks: false,
            _subscription: subscription,
        });
    }

    /// 说明输入框里的字符数（去首尾空白后按码点计）。
    pub(crate) fn rate_comment_chars(&self, cx: &App) -> usize {
        self.rate_comment.as_ref().map_or(0, |comment| {
            comment.input.read(cx).value().trim().chars().count()
        })
    }

    /// 提交点踩说明：同一 rating 带 `comment` 再调一次 `rate`。
    pub(crate) fn submit_rate_comment(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.current.clone() else {
            return;
        };
        let busy_ratings = &self.rating_busy;
        let Some(comment) = self
            .rate_comment
            .as_mut()
            .filter(|comment| !comment.busy && !busy_ratings.contains(&comment.message_id))
        else {
            return;
        };
        let Some(text) = rating_comment(&comment.input.read(cx).value()) else {
            return;
        };
        comment.busy = true;
        let message_id = comment.message_id.clone();
        // 说明与此前的评价请求不能并发：否则不带说明的那次后到会把云端的说明清空。
        self.rating_busy.insert(message_id.clone());
        let future = self.port.rate(AssistantRateParams {
            session_id,
            message_id: message_id.clone(),
            rating: Some("down".to_owned()),
            comment: Some(text),
        });
        self.run(future, cx, move |this, result, cx| {
            this.rating_busy.remove(&message_id);
            let current = this
                .rate_comment
                .as_mut()
                .filter(|comment| comment.message_id == message_id);
            match (current, result) {
                (Some(comment), Ok(_)) => {
                    comment.busy = false;
                    comment.thanks = true;
                    this.clear_thanks_later(message_id, cx);
                }
                (Some(comment), Err(error)) => {
                    comment.busy = false;
                    this.report_failure("rate comment", &error, cx);
                }
                (None, Err(error)) => this.report_failure("rate comment", &error, cx),
                (None, Ok(_)) => {}
            }
            cx.notify();
        });
        cx.notify();
    }

    /// 跳过 / 关闭点踩说明。
    pub(crate) fn dismiss_rate_comment(&mut self, cx: &mut Context<Self>) {
        if self.rate_comment.take().is_some() {
            cx.notify();
        }
    }

    /// 「感谢反馈」只停留一会儿。
    fn clear_thanks_later(&mut self, message_id: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(THANKS_FLASH).await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this
                    .rate_comment
                    .as_ref()
                    .is_some_and(|comment| comment.message_id == message_id && comment.thanks)
                {
                    this.rate_comment = None;
                    cx.notify();
                }
            }) else {
                // 视图已释放，结束回调而不再更新状态。
                return;
            };
        })
        .detach();
    }

    // ───────────────────────── 回到底部 ─────────────────────────

    /// 「回到最新」：滚到底并重新跟随。
    pub(crate) fn scroll_to_latest(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    // ───────────────────────── 键盘 ─────────────────────────

    /// Esc：依次取消待确认的删除 / 清空、重命名、编辑重发，其次停止进行中的回答，清掉搜索词，
    /// 最后关闭历史视图。什么都没有可取消时交还给上层（不吞掉 Esc）。
    pub(crate) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image_preview.is_some() {
            self.image_preview = None;
        } else if self.rate_comment.is_some() {
            self.rate_comment = None;
        } else if self.delete_pending.is_some() || self.clear_pending {
            self.delete_pending = None;
            self.clear_pending = false;
            self.confirm_gen = self.confirm_gen.wrapping_add(1);
        } else if self.renaming.is_some() {
            self.renaming = None;
        } else if self.editing.is_some() && !self.history_open {
            self.cancel_edit(window, cx);
        } else if self.can_stop() && !self.history_open {
            self.stop(cx);
        } else if self.history_open && !self.search_text.is_empty() {
            self.clear_search(window, cx);
        } else if self.history_open {
            self.toggle_history(window, cx);
        } else {
            cx.propagate();
            return;
        }
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
