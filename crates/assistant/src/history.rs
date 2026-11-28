//! 历史会话（会话列表）的状态与动作：加载 / 搜索 / 重命名 / 导出 / 删除 / 清空 / 键盘光标。
//!
//! 并发约定：列表请求带单调递增的 `epoch`，任何会使在途结果失效的动作（删除、清空、搜索词变化、
//! 账号变化）都先 `invalidate()`；失效不会让 `load` 停在 `Loading`（回到 `Idle`，由
//! `ensure_loaded` / 调用方重新拉取）。绘制在 `render_history.rs`。

use std::time::{Duration, Instant};

use fluxdown_protocol::{
    ASSISTANT_MAX_TITLE_CHARS, AssistantExportFormat, AssistantExportResult, AssistantRunItemDto,
    AssistantSessionDto, AssistantSessionsParams, AssistantStateDto, RpcErrorData,
};
use fluxdown_ui_components::toast;
use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::{
    WindowExt as _,
    input::{InputEvent, InputState},
};

use crate::{
    model::{ConfirmView, SESSION_PAGE_SIZE, classify_action_error, confirm_view},
    view::AssistantView,
};

/// 搜索框输入防抖。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// 二次点击确认（删除 / 清空）的有效期：超时自动取消。
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(4);

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
    /// 当前 `items` 对应的服务端搜索词（空 = 不搜索）。
    pub(crate) query: String,
    /// 上次成功加载的时刻（面板重新打开时据此判断是否需要刷新）。
    pub(crate) loaded_at: Option<Instant>,
    /// 只增不减：账号切换 / 登出也不重置，旧请求永远对不上新的 epoch。
    epoch: u64,
}

impl SessionList {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            load: Load::Idle,
            more_loading: false,
            query: String::new(),
            loaded_at: None,
            epoch: 0,
        }
    }

    pub(crate) fn has_more(&self) -> bool {
        (self.items.len() as u64) < self.total
    }

    /// 开始一次整页加载：作废更早的请求，返回本次请求的 epoch。
    pub(crate) fn begin_load(&mut self) -> u64 {
        self.epoch += 1;
        self.more_loading = false;
        self.load = Load::Loading;
        self.epoch
    }

    /// 请求发出时的 epoch（加载更多沿用，期间整页刷新则偏移作废）。
    pub(crate) const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(crate) const fn is_current(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    /// 作废所有在途请求（删除 / 清空成功、搜索词变化）。不会让 `load` 停在 `Loading`。
    pub(crate) fn invalidate(&mut self) {
        self.epoch += 1;
        self.more_loading = false;
        if self.load == Load::Loading {
            self.load = Load::Idle;
        }
    }

    /// 账号变化：丢弃全部内容；epoch 继续递增。
    pub(crate) fn reset(&mut self) {
        self.invalidate();
        self.items.clear();
        self.total = 0;
        self.load = Load::Idle;
        self.query.clear();
        self.loaded_at = None;
    }
}

/// 正在重命名的会话：行内 / 标题处的输入框。
pub(crate) struct Rename {
    pub(crate) session_id: String,
    pub(crate) input: Entity<InputState>,
    /// 请求进行中。
    pub(crate) busy: bool,
    _subscription: Subscription,
}

// ───────────────────────── 纯逻辑 ─────────────────────────

/// 本地标题过滤：大小写不敏感的子串匹配；空白搜索词不过滤。返回命中的下标。
pub(crate) fn filter_sessions(items: &[AssistantSessionDto], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, session)| session.title.to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

/// 列表里显示哪些会话：搜索词已发给服务端并取回结果时按服务端结果全显示（命中可能来自消息内容，
/// 标题里没有）；输入还在防抖中（`typed` 与已应用的 `applied` 不同）则先按标题本地过滤，即时反馈。
pub(crate) fn visible_indices(
    items: &[AssistantSessionDto],
    applied: &str,
    typed: &str,
) -> Vec<usize> {
    if typed.trim() == applied.trim() {
        (0..items.len()).collect()
    } else {
        filter_sessions(items, typed)
    }
}

/// 键盘光标移动一步（不循环）。`current` 为当前下标，`len` 为可见条数。
pub(crate) fn step_cursor(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match current {
        None if forward => 0,
        None => last,
        Some(index) if forward => (index + 1).min(last),
        Some(index) => index.min(last).saturating_sub(1),
    })
}

/// 会话标题的提交值：去首尾空白后 1–[`ASSISTANT_MAX_TITLE_CHARS`] 字符（超出按字符截断，
/// 不会切在多字节字符中间）；空白返回 `None`。
pub(crate) fn normalize_title(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(ASSISTANT_MAX_TITLE_CHARS).collect())
}

/// 导出文件的默认名：云端给的名字为空时按格式兜底。
pub(crate) fn export_file_name(suggested: &str, format: AssistantExportFormat) -> String {
    let suggested = suggested.trim();
    if !suggested.is_empty() {
        return suggested.to_owned();
    }
    match format {
        AssistantExportFormat::Markdown => "fluxdown-assistant.md".to_owned(),
        AssistantExportFormat::Json => "fluxdown-assistant.json".to_owned(),
    }
}

/// 每个有待确认工具的进行中会话及其待确认数。
pub(crate) fn pending_per_session(state: &AssistantStateDto, now_ms: i64) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for run in state.runs.iter().filter(|run| run.status.is_active()) {
        let count = run
            .items
            .iter()
            .filter(|item| match item {
                AssistantRunItemDto::Tool { call } => matches!(
                    confirm_view(call, true, now_ms),
                    Some(ConfirmView::Pending { .. })
                ),
                AssistantRunItemDto::Text { .. } | AssistantRunItemDto::IssueDraft { .. } => false,
            })
            .count();
        if count == 0 {
            continue;
        }
        match out.iter_mut().find(|(id, _)| *id == run.session_id) {
            Some((_, total)) => *total += count,
            None => out.push((run.session_id.clone(), count)),
        }
    }
    out
}

// ───────────────────────── 视图动作 ─────────────────────────

impl AssistantView {
    /// 列表里显示的会话下标（见 [`visible_indices`]）。
    pub(crate) fn visible_sessions(&self) -> Vec<usize> {
        visible_indices(
            &self.sessions.items,
            &self.sessions.query,
            &self.search_text,
        )
    }

    /// 重新读取会话列表第一页（已加载更多页时保持原有条数）。
    pub(crate) fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        let limit = u32::try_from(self.sessions.items.len())
            .unwrap_or(SESSION_PAGE_SIZE)
            .max(SESSION_PAGE_SIZE);
        self.request_sessions(limit, cx);
    }

    fn request_sessions(&mut self, limit: u32, cx: &mut Context<Self>) {
        if !self.signed_in {
            return;
        }
        let epoch = self.sessions.begin_load();
        let query = self.sessions.query.clone();
        let future = self.port.sessions(AssistantSessionsParams {
            limit: Some(limit),
            offset: Some(0),
            query: (!query.is_empty()).then_some(query),
        });
        self.run(future, cx, move |this, result, cx| {
            // 期间被作废（删除 / 清空 / 搜索 / 又一次刷新）：作废时已保证 `load` 不会停在加载中。
            if !this.sessions.is_current(epoch) {
                return;
            }
            match result {
                Ok(page) => {
                    this.sessions.items = page.items;
                    this.sessions.total = page.total;
                    this.sessions.load = Load::Ready;
                    this.sessions.loaded_at = Some(Instant::now());
                    this.sync_history_cursor();
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
        self.history_error = None;
        let epoch = self.sessions.epoch();
        let offset = u32::try_from(self.sessions.items.len()).unwrap_or(u32::MAX);
        let query = self.sessions.query.clone();
        let future = self.port.sessions(AssistantSessionsParams {
            limit: Some(SESSION_PAGE_SIZE),
            offset: Some(offset),
            query: (!query.is_empty()).then_some(query),
        });
        self.run(future, cx, move |this, result, cx| {
            // 期间列表被整体刷新 / 作废：这一页的偏移已失效（作废时 `more_loading` 已复位）。
            if !this.sessions.is_current(epoch) {
                return;
            }
            this.sessions.more_loading = false;
            match result {
                Ok(page) => {
                    for item in page.items {
                        if this.sessions.items.iter().all(|known| known.id != item.id) {
                            this.sessions.items.push(item);
                        }
                    }
                    this.sessions.total = page.total;
                }
                Err(error) => {
                    log::warn!("assistant sessions page failed: {:?}", error.code);
                    this.history_error = Some("assistantSessionsLoadFailed");
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    /// 历史视图里的动作失败：就地显示（历史视图没有输入区）；对话视图走输入区的提示。
    pub(crate) fn report_failure(
        &mut self,
        what: &str,
        error: &RpcErrorData,
        cx: &mut Context<Self>,
    ) {
        log::warn!("assistant {what} failed: {:?}", error.code);
        if self.history_open {
            self.history_error = Some(classify_action_error(error).key());
        } else {
            self.action_error = Some(classify_action_error(error));
        }
        cx.notify();
    }

    // ───────────────────────── 视图切换与键盘 ─────────────────────────

    /// 切换历史视图；打开时后台刷新列表并把焦点交给列表，回到对话时焦点交给输入框。
    pub(crate) fn toggle_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history_open = !self.history_open;
        self.cancel_transient();
        if self.history_open {
            self.action_error = None;
            self.history_error = None;
            if self.sessions.load != Load::Loading {
                self.reload_sessions(cx);
            }
            self.history_cursor = self.current.clone();
            window.focus(&self.history_focus, cx);
        } else {
            self.focus_input(window, cx);
        }
        cx.notify();
    }

    /// 取消所有「点一次待确认」的瞬时状态：删除 / 清空二次确认与重命名。
    pub(crate) fn cancel_transient(&mut self) {
        self.delete_pending = None;
        self.clear_pending = false;
        self.renaming = None;
        self.confirm_gen = self.confirm_gen.wrapping_add(1);
    }

    /// 光标行是否仍在列表里；不在则清掉（列表刷新 / 搜索后）。
    pub(crate) fn sync_history_cursor(&mut self) {
        let Some(id) = self.history_cursor.as_deref() else {
            return;
        };
        let visible = self.visible_sessions();
        let present = visible
            .iter()
            .filter_map(|index| self.sessions.items.get(*index))
            .any(|session| session.id == id);
        if !present {
            self.history_cursor = None;
        }
    }

    fn cursor_position(&self, visible: &[usize]) -> Option<usize> {
        let id = self.history_cursor.as_deref()?;
        visible.iter().position(|index| {
            self.sessions
                .items
                .get(*index)
                .is_some_and(|session| session.id == id)
        })
    }

    /// 键盘上下键：移动光标并滚动到可见。
    pub(crate) fn history_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let visible = self.visible_sessions();
        let next = step_cursor(self.cursor_position(&visible), visible.len(), forward);
        self.history_cursor = next
            .and_then(|position| visible.get(position))
            .and_then(|index| self.sessions.items.get(*index))
            .map(|session| session.id.clone());
        if let Some(position) = next {
            self.history_scroll.scroll_to_item(position);
        }
        cx.notify();
    }

    /// 回车：打开光标所在的会话；没有光标时打开第一条。
    pub(crate) fn history_open_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.is_some() {
            return;
        }
        let visible = self.visible_sessions();
        let target = self
            .cursor_position(&visible)
            .or_else(|| (!visible.is_empty()).then_some(0))
            .and_then(|position| visible.get(position))
            .and_then(|index| self.sessions.items.get(*index))
            .map(|session| session.id.clone());
        if let Some(id) = target {
            self.select_session(id, window, cx);
        }
    }

    // ───────────────────────── 搜索 ─────────────────────────

    /// 搜索框事件：输入变化即时更新本地过滤并防抖请求服务端。
    pub(crate) fn on_search_event(
        &mut self,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.search_text = self.search.read(cx).value().to_string();
                self.search_gen = self.search_gen.wrapping_add(1);
                let generation = self.search_gen;
                self.sync_history_cursor();
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                    let Ok(()) = this.update(cx, move |this, cx| {
                        if this.search_gen == generation {
                            this.apply_search(cx);
                        }
                    }) else {
                        // 助手页已释放，停止防抖回调。
                        return;
                    };
                })
                .detach();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                // 立即应用搜索词，并打开第一条结果。
                self.search_gen = self.search_gen.wrapping_add(1);
                self.apply_search(cx);
                self.history_open_cursor(window, cx);
            }
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    /// 把搜索框里的词发给服务端（词没变则不请求）。
    fn apply_search(&mut self, cx: &mut Context<Self>) {
        let term = self.search_text.trim().to_owned();
        if term == self.sessions.query {
            return;
        }
        self.sessions.invalidate();
        self.sessions.query = term;
        self.request_sessions(SESSION_PAGE_SIZE, cx);
    }

    /// 清空搜索框（Esc / 离开历史）。
    pub(crate) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_text.is_empty() {
            return;
        }
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.search_text.clear();
        self.search_gen = self.search_gen.wrapping_add(1);
        self.apply_search(cx);
        cx.notify();
    }

    // ───────────────────────── 删除 / 清空 ─────────────────────────

    /// 二次确认到期自动取消。
    fn expire_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_gen = self.confirm_gen.wrapping_add(1);
        let generation = self.confirm_gen;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(CONFIRM_TIMEOUT).await;
            let Ok(()) = this.update(cx, move |this, cx| {
                if this.confirm_gen == generation
                    && (this.delete_pending.is_some() || this.clear_pending)
                {
                    this.delete_pending = None;
                    this.clear_pending = false;
                    cx.notify();
                }
            }) else {
                // 助手页已释放，结束超时回调。
                return;
            };
        })
        .detach();
    }

    pub(crate) fn request_delete(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_pending = false;
        self.delete_pending = Some(session_id);
        self.expire_confirmation(window, cx);
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
        if self.stale || !self.deleting.insert(session_id.clone()) {
            return;
        }
        self.history_error = None;
        let future = self.port.delete_session(session_id.clone());
        self.run(future, cx, move |this, result, cx| {
            this.deleting.remove(&session_id);
            match result {
                Ok(()) => {
                    this.sessions
                        .items
                        .retain(|session| session.id != session_id);
                    this.sessions.total = this.sessions.total.saturating_sub(1);
                    // 在途的列表请求可能早于云端删除生效：作废它们，免得被删的会话重新出现；
                    // 作废时若正在整页加载，立刻重新拉取，列表不会停在加载中。
                    let was_loading = this.sessions.load == Load::Loading;
                    this.sessions.invalidate();
                    if was_loading {
                        this.reload_sessions(cx);
                    }
                    if this
                        .renaming
                        .as_ref()
                        .is_some_and(|rename| rename.session_id == session_id)
                    {
                        this.renaming = None;
                    }
                    if this.current.as_deref() == Some(session_id.as_str()) {
                        this.leave_current(cx);
                    }
                    this.sync_history_cursor();
                }
                Err(error) => this.report_failure("delete session", &error, cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn request_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_pending = None;
        self.clear_pending = true;
        self.expire_confirmation(window, cx);
        cx.notify();
    }

    pub(crate) fn cancel_clear(&mut self, cx: &mut Context<Self>) {
        if self.clear_pending {
            self.clear_pending = false;
            cx.notify();
        }
    }

    /// 清空全部会话（云端只在用户侧隐藏）：成功后列表清空、回到新对话；在途的列表请求结果作废，
    /// 列表直接是「已加载、为空」，不会停在加载中。
    pub(crate) fn clear_sessions(&mut self, cx: &mut Context<Self>) {
        self.clear_pending = false;
        self.delete_pending = None;
        if self.stale || self.clearing {
            return;
        }
        self.clearing = true;
        self.history_error = None;
        let future = self.port.clear_sessions();
        self.run(future, cx, move |this, result, cx| {
            this.clearing = false;
            match result {
                Ok(_) => {
                    this.sessions.invalidate();
                    this.sessions.items.clear();
                    this.sessions.total = 0;
                    this.sessions.load = Load::Ready;
                    this.sessions.loaded_at = Some(Instant::now());
                    this.history_cursor = None;
                    this.renaming = None;
                    if this.current.is_some() {
                        this.leave_current(cx);
                    }
                }
                Err(error) => this.report_failure("clear sessions", &error, cx),
            }
            cx.notify();
        });
        cx.notify();
    }

    // ───────────────────────── 重命名 ─────────────────────────

    /// 会话当前显示的标题（原文，可能为空）。
    fn session_title(&self, session_id: &str) -> String {
        self.detail
            .session
            .as_ref()
            .filter(|session| session.id == session_id)
            .or_else(|| {
                self.sessions
                    .items
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .map(|session| session.title.trim().to_owned())
            .unwrap_or_default()
    }

    pub(crate) fn start_rename(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stale || self.renaming.as_ref().is_some_and(|rename| rename.busy) {
            return;
        }
        self.delete_pending = None;
        let title = self.session_title(&session_id);
        let placeholder = self.t("assistantRenamePlaceholder", cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(title)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.clamp_rename_input(window, cx),
                InputEvent::PressEnter { .. } => this.commit_rename(cx),
                // 失焦不提交也不取消：点确认 / 取消按钮会先让输入框失焦。
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.renaming = Some(Rename {
            session_id,
            input,
            busy: false,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// 标题输入框最多 [`ASSISTANT_MAX_TITLE_CHARS`] 个字符：超出部分直接截掉。
    fn clamp_rename_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.renaming.as_ref() else {
            return;
        };
        let value = rename.input.read(cx).value();
        if value.chars().count() > ASSISTANT_MAX_TITLE_CHARS {
            let clamped: String = value.chars().take(ASSISTANT_MAX_TITLE_CHARS).collect();
            rename
                .input
                .update(cx, |input, cx| input.set_value(clamped, window, cx));
        }
        cx.notify();
    }

    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.renaming.as_mut() else {
            return;
        };
        if rename.busy || self.stale {
            return;
        }
        let session_id = rename.session_id.clone();
        let typed = rename.input.read(cx).value();
        let Some(title) = normalize_title(&typed) else {
            self.renaming = None;
            cx.notify();
            return;
        };
        if title == self.session_title(&session_id) {
            self.renaming = None;
            cx.notify();
            return;
        }
        if let Some(rename) = self.renaming.as_mut() {
            rename.busy = true;
        }
        self.history_error = None;
        self.action_error = None;
        let future = self.port.rename_session(session_id.clone(), title);
        self.run(future, cx, move |this, result, cx| {
            match result {
                Ok(renamed) => {
                    if let Some(item) = this
                        .sessions
                        .items
                        .iter_mut()
                        .find(|session| session.id == renamed.id)
                    {
                        item.title.clone_from(&renamed.title);
                    }
                    if let Some(session) = this
                        .detail
                        .session
                        .as_mut()
                        .filter(|session| session.id == renamed.id)
                    {
                        session.title = renamed.title;
                    }
                    if this
                        .renaming
                        .as_ref()
                        .is_some_and(|rename| rename.session_id == session_id)
                    {
                        this.renaming = None;
                    }
                }
                Err(error) => {
                    // 保留输入框，用户可改后重试。
                    if let Some(rename) = this.renaming.as_mut() {
                        rename.busy = false;
                    }
                    this.report_failure("rename session", &error, cx);
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    // ───────────────────────── 导出 ─────────────────────────

    /// 导出会话：云端生成内容，随后让用户选保存位置并写盘（写盘在后台线程）。
    pub(crate) fn export_session(
        &mut self,
        session_id: String,
        format: AssistantExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stale || self.exporting.is_some() {
            return;
        }
        self.exporting = Some(session_id.clone());
        let future = self.port.export_session(session_id, format);
        self.run_in(window, future, cx, move |this, result, window, cx| {
            this.exporting = None;
            match result {
                Ok(file) => this.save_export(file, format, window, cx),
                Err(error) => {
                    log::warn!("assistant export failed: {:?}", error.code);
                    let message = this.t("assistantActionFailed", cx);
                    window.push_notification(toast::error(message), cx);
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    fn save_export(
        &mut self,
        file: AssistantExportResult,
        format: AssistantExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = export_file_name(&file.file_name, format);
        let directory = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
        let receiver = cx.prompt_for_new_path(&directory, Some(&name));
        let content = file.content;
        cx.spawn_in(window, async move |this, cx| {
            let outcome: Result<String, String> = match receiver.await {
                Ok(Ok(Some(path))) => {
                    let saved = path
                        .file_name()
                        .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
                    cx.background_executor()
                        .spawn(async move { std::fs::write(&path, content) })
                        .await
                        .map(|()| saved)
                        .map_err(|error| error.to_string())
                }
                // 用户取消了保存。
                Ok(Ok(None)) => return,
                Ok(Err(error)) => Err(format!("{error:#}")),
                Err(error) => Err(error.to_string()),
            };
            let Ok(()) = this.update_in(cx, move |this, window, cx| {
                let toast = match outcome {
                    Ok(saved) => {
                        toast::success(this.with("assistantExported", &[("name", &saved)], cx))
                    }
                    Err(error) => {
                        log::warn!("assistant export save failed: {error}");
                        toast::error(this.with("assistantExportFailed", &[("error", &error)], cx))
                    }
                };
                window.push_notification(toast, cx);
            }) else {
                // 助手页已随窗口释放，无需提示。
                return;
            };
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        AssistantConfirmationState, AssistantRunDto, AssistantRunStatus, AssistantToolCallDto,
        AssistantToolEffect,
    };

    use super::*;

    fn session(id: &str, title: &str) -> AssistantSessionDto {
        AssistantSessionDto {
            id: id.to_owned(),
            title: title.to_owned(),
            ..AssistantSessionDto::default()
        }
    }

    #[test]
    fn epoch_never_goes_back_across_reset() {
        let mut list = SessionList::new();
        let first = list.begin_load();
        list.reset();
        // 登出再登录后的新请求不会与旧账号的在途请求撞上同一个 epoch。
        let second = list.begin_load();
        assert!(second > first);
        assert!(!list.is_current(first));
        assert!(list.is_current(second));
    }

    #[test]
    fn invalidate_never_leaves_the_list_loading() {
        let mut list = SessionList::new();
        let epoch = list.begin_load();
        list.more_loading = true;
        list.invalidate();
        assert!(!list.is_current(epoch));
        assert_eq!(list.load, Load::Idle);
        assert!(!list.more_loading);
    }

    #[test]
    fn invalidate_keeps_a_finished_load_state() {
        let mut list = SessionList::new();
        list.load = Load::Ready;
        list.invalidate();
        assert_eq!(list.load, Load::Ready);
        list.load = Load::Failed;
        list.invalidate();
        assert_eq!(list.load, Load::Failed);
    }

    #[test]
    fn reset_clears_account_bound_content() {
        let mut list = SessionList::new();
        list.items.push(session("a", "A"));
        list.total = 7;
        list.query = "x".to_owned();
        list.load = Load::Ready;
        list.reset();
        assert!(list.items.is_empty());
        assert_eq!(list.total, 0);
        assert!(list.query.is_empty());
        assert_eq!(list.load, Load::Idle);
    }

    #[test]
    fn title_filter_is_case_insensitive_and_blank_shows_all() {
        let items = [
            session("1", "Resume downloads"),
            session("2", "速度太慢"),
            session("3", "RESUME broken"),
        ];
        assert_eq!(filter_sessions(&items, "resume"), vec![0, 2]);
        assert_eq!(filter_sessions(&items, "  速度 "), vec![1]);
        assert_eq!(filter_sessions(&items, "   "), vec![0, 1, 2]);
        assert!(filter_sessions(&items, "nothing").is_empty());
    }

    #[test]
    fn server_results_are_not_refiltered_by_title() {
        // 服务端按消息内容命中：标题里没有搜索词，也要显示。
        let items = [session("1", "Untitled")];
        assert_eq!(visible_indices(&items, "proxy", "proxy"), vec![0]);
        // 输入还在防抖中：先按标题本地过滤。
        assert!(visible_indices(&items, "", "proxy").is_empty());
        assert_eq!(visible_indices(&items, "", "untitled"), vec![0]);
    }

    #[test]
    fn cursor_clamps_at_both_ends() {
        assert_eq!(step_cursor(None, 0, true), None);
        assert_eq!(step_cursor(None, 3, true), Some(0));
        assert_eq!(step_cursor(None, 3, false), Some(2));
        assert_eq!(step_cursor(Some(0), 3, false), Some(0));
        assert_eq!(step_cursor(Some(2), 3, true), Some(2));
        assert_eq!(step_cursor(Some(1), 3, true), Some(2));
        // 列表缩短后光标越界：先收回末尾再移动。
        assert_eq!(step_cursor(Some(9), 3, false), Some(1));
    }

    #[test]
    fn titles_are_trimmed_and_capped_by_characters() {
        assert_eq!(normalize_title("  hello  ").as_deref(), Some("hello"));
        assert_eq!(normalize_title(" \n\t"), None);
        let long = "字".repeat(ASSISTANT_MAX_TITLE_CHARS + 20);
        let capped = normalize_title(&long).unwrap();
        assert_eq!(capped.chars().count(), ASSISTANT_MAX_TITLE_CHARS);
        let emoji = "😀".repeat(ASSISTANT_MAX_TITLE_CHARS + 1);
        assert_eq!(
            normalize_title(&emoji).unwrap().chars().count(),
            ASSISTANT_MAX_TITLE_CHARS
        );
    }

    #[test]
    fn export_name_falls_back_by_format() {
        assert_eq!(
            export_file_name(" chat.md ", AssistantExportFormat::Markdown),
            "chat.md"
        );
        assert_eq!(
            export_file_name("", AssistantExportFormat::Json),
            "fluxdown-assistant.json"
        );
        assert_eq!(
            export_file_name("  ", AssistantExportFormat::Markdown),
            "fluxdown-assistant.md"
        );
    }

    fn pending_tool(deadline: i64) -> AssistantRunItemDto {
        AssistantRunItemDto::Tool {
            call: AssistantToolCallDto {
                call_id: "c".to_owned(),
                name: "fd_pause".to_owned(),
                kind: "client".to_owned(),
                effect: AssistantToolEffect::Write,
                request_id: Some("r".to_owned()),
                confirmation: Some(AssistantConfirmationState::Pending),
                confirm_deadline_unix_ms: Some(deadline),
                ..AssistantToolCallDto::default()
            },
        }
    }

    fn run(
        session: &str,
        status: AssistantRunStatus,
        items: Vec<AssistantRunItemDto>,
    ) -> AssistantRunDto {
        AssistantRunDto {
            session_id: session.to_owned(),
            status,
            items,
            ..AssistantRunDto::default()
        }
    }

    #[test]
    fn pending_counts_group_by_session_and_skip_expired_or_finished() {
        let state = AssistantStateDto {
            runs: vec![
                run(
                    "a",
                    AssistantRunStatus::Running,
                    vec![pending_tool(2_000), pending_tool(2_000), pending_tool(500)],
                ),
                run("b", AssistantRunStatus::Running, vec![pending_tool(2_000)]),
                // 已结束的执行里「待确认」不可再操作。
                run(
                    "c",
                    AssistantRunStatus::Completed,
                    vec![pending_tool(2_000)],
                ),
            ],
            ..AssistantStateDto::default()
        };
        assert_eq!(
            pending_per_session(&state, 1_000),
            vec![("a".to_owned(), 2), ("b".to_owned(), 1)]
        );
        assert!(pending_per_session(&state, 3_000).is_empty());
    }
}
