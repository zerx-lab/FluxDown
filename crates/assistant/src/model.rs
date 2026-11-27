//! 助手页的纯模型：云端历史与本机实时执行的合并、工具确认状态推导、可用性判定与错误分类。
//!
//! 这里不碰 GPUI：视图只读这些函数的结果，时间由调用方传入，保证可单测。

use std::{collections::HashMap, ops::Range};

use fluxdown_protocol::{
    ApplicationErrorCode, AssistantConfigDto, AssistantConfirmationState, AssistantIssueDto,
    AssistantMessageDto, AssistantRunDto, AssistantRunItemDto, AssistantRunStatus,
    AssistantStateDto, AssistantToolCallDto, AssistantToolStatus, ErrorReason, RpcErrorData,
    assistant::assistant_image_placeholder,
};
use serde_json::Value;

/// 会话列表一页的条数。
pub(crate) const SESSION_PAGE_SIZE: u32 = 30;
/// 工具参数摘要的最大字符数。
const ARGUMENT_SUMMARY_CHARS: usize = 120;
/// 历史里客户端工具结果预览的最大字符数。
const OUTPUT_PREVIEW_CHARS: usize = 400;
/// 时间戳比较只取到秒（`YYYY-MM-DDTHH:MM:SS`），避免小数位数不同导致的字典序误判。
const TIMESTAMP_SECOND_CHARS: usize = 19;

// ───────────────────────── 时间线 ─────────────────────────

/// 一轮回答里按到达顺序排列的条目。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Text(String),
    Tool(AssistantToolCallDto),
    Issue(AssistantIssueDto),
}

/// 实时执行的状态（历史轮次没有）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Live {
    pub(crate) status: AssistantRunStatus,
    pub(crate) error: Option<String>,
}

/// 一问一答：用户提问、中间过程、最终回答与问题单草稿。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Turn {
    /// 折叠状态等界面状态的键：执行 ID 优先，其次消息 / 起始时间。
    pub(crate) key: String,
    pub(crate) question: String,
    pub(crate) image_count: usize,
    /// 可评价的回答消息 ID：历史里最后一条有内容的 assistant 消息；实时执行取执行的
    /// `message_id`（只有正常作答 / 固定回复才有）。`None` = 本轮没有可评价的回答。
    pub(crate) message_id: Option<String>,
    /// 云端记录的该回答评价（`up` / `down`）；界面以本地状态优先。
    pub(crate) rating: Option<String>,
    /// 最终回答之前的全部条目：中间文本与工具调用（可折叠的「过程」）。
    pub(crate) steps: Vec<Block>,
    /// 最终回答（Markdown）；尚未产出时为 `None`。
    pub(crate) answer: Option<String>,
    pub(crate) issues: Vec<AssistantIssueDto>,
    pub(crate) live: Option<Live>,
    /// 云端历史轮次的起始时间（RFC 3339），用于把问题单挂到对应轮次；实时轮次为 `None`。
    started_at: Option<String>,
}

impl Turn {
    pub(crate) fn run_active(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.status.is_active())
    }

    /// 本轮是否有等待用户确认的工具调用（未过期）。
    pub(crate) fn has_pending_confirmation(&self, now_ms: i64) -> bool {
        let active = self.run_active();
        self.steps.iter().any(|block| match block {
            Block::Tool(call) => matches!(
                confirm_view(call, active, now_ms),
                Some(ConfirmView::Pending { .. })
            ),
            Block::Text(_) | Block::Issue(_) => false,
        })
    }

    pub(crate) fn tool_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|block| matches!(block, Block::Tool(_)))
            .count()
    }

    /// 可评价的回答消息 ID：回答已定稿（有回答文本、有消息 ID，实时执行还须正常结束）。
    pub(crate) fn rateable_message(&self) -> Option<&str> {
        let id = self.message_id.as_deref()?;
        self.answer.as_ref()?;
        match &self.live {
            Some(live)
                if !matches!(
                    live.status,
                    AssistantRunStatus::Completed | AssistantRunStatus::Replied
                ) =>
            {
                None
            }
            _ => Some(id),
        }
    }
}

/// 「过程」折叠区是否展开：有待确认的工具时强制展开；否则取用户选择，缺省为
/// 「执行中且最终回答尚未产出」时展开，回答出现后收起。
pub(crate) fn steps_open(turn: &Turn, user_choice: Option<bool>, now_ms: i64) -> bool {
    if turn.has_pending_confirmation(now_ms) {
        return true;
    }
    user_choice.unwrap_or_else(|| turn.run_active() && turn.answer.is_none())
}

fn split_blocks(blocks: Vec<Block>) -> (Vec<Block>, Option<String>, Vec<AssistantIssueDto>) {
    let mut issues = Vec::new();
    let mut rest = Vec::new();
    for block in blocks {
        match block {
            Block::Issue(issue) => issues.push(issue),
            Block::Text(text) if text.trim().is_empty() => {}
            other => rest.push(other),
        }
    }
    let answer = match rest.last() {
        Some(Block::Text(_)) => match rest.pop() {
            Some(Block::Text(text)) => Some(text),
            Some(other) => {
                rest.push(other);
                None
            }
            None => None,
        },
        _ => None,
    };
    (rest, answer, issues)
}

fn live_turn(run: &AssistantRunDto, overrides: &HashMap<String, AssistantIssueDto>) -> Turn {
    let blocks = run
        .items
        .iter()
        .map(|item| match item {
            AssistantRunItemDto::Text { text } => Block::Text(text.clone()),
            AssistantRunItemDto::Tool { call } => Block::Tool(call.clone()),
            AssistantRunItemDto::IssueDraft { issue } => {
                Block::Issue(resolve_issue(issue, overrides))
            }
        })
        .collect();
    let (steps, answer, issues) = split_blocks(blocks);
    let key = run.run_id.as_ref().map_or_else(
        || format!("live:{}", run.started_at_unix_ms),
        |id| format!("run:{id}"),
    );
    Turn {
        key,
        question: run.question.clone(),
        image_count: 0,
        message_id: run.message_id.clone().filter(|_| {
            matches!(
                run.status,
                AssistantRunStatus::Completed | AssistantRunStatus::Replied
            )
        }),
        rating: None,
        steps,
        answer,
        issues,
        live: Some(Live {
            status: run.status,
            error: run.error.clone().filter(|error| !error.is_empty()),
        }),
        started_at: None,
    }
}

/// 问题单在本地确认 / 丢弃后的最新状态优先于推送与历史里的旧副本。
fn resolve_issue(
    issue: &AssistantIssueDto,
    overrides: &HashMap<String, AssistantIssueDto>,
) -> AssistantIssueDto {
    overrides
        .get(&issue.id)
        .cloned()
        .unwrap_or_else(|| issue.clone())
}

struct HistoryTurn {
    run_id: Option<String>,
    key: String,
    started_at: String,
    question: String,
    image_count: usize,
    /// 最后一条有内容的 assistant 消息：`(消息 ID, 云端评价)`。
    answer_message: Option<(String, Option<String>)>,
    blocks: Vec<Block>,
}

impl HistoryTurn {
    fn new(message: &AssistantMessageDto) -> Self {
        let key = message
            .run_id
            .as_ref()
            .map_or_else(|| format!("msg:{}", message.id), |id| format!("run:{id}"));
        Self {
            run_id: message.run_id.clone(),
            key,
            started_at: message.created_at.clone(),
            question: String::new(),
            image_count: 0,
            answer_message: None,
            blocks: Vec::new(),
        }
    }

    fn push(&mut self, message: &AssistantMessageDto) {
        match message.role.as_str() {
            "user" => {
                self.question.clone_from(&message.content);
                self.image_count = message.images.len();
            }
            "assistant" => {
                if !message.content.trim().is_empty() {
                    self.blocks.push(Block::Text(message.content.clone()));
                    self.answer_message = Some((message.id.clone(), message.rating.clone()));
                }
            }
            "tool_call" => self.blocks.push(Block::Tool(tool_from_call(message))),
            "tool_result" => self.complete_tool(message),
            _ => {}
        }
    }

    fn complete_tool(&mut self, message: &AssistantMessageDto) {
        let position = self.blocks.iter().rposition(|block| match block {
            Block::Tool(call) => {
                call.status == AssistantToolStatus::Running
                    && match message.tool_call_id.as_deref() {
                        Some(id) => call.call_id == id,
                        None => Some(call.name.as_str()) == message.tool_name.as_deref(),
                    }
            }
            Block::Text(_) | Block::Issue(_) => false,
        });
        let preview = (message.tool_kind.as_deref() == Some("client"))
            .then(|| truncate_chars(message.content.trim(), OUTPUT_PREVIEW_CHARS))
            .filter(|text| !text.is_empty());
        let status = if message.is_error {
            AssistantToolStatus::Failed
        } else {
            AssistantToolStatus::Succeeded
        };
        match position.and_then(|index| self.blocks.get_mut(index)) {
            Some(Block::Tool(call)) => {
                call.status = status;
                call.duration_ms = message.duration_ms;
                call.output_preview = preview;
            }
            _ => {
                let mut call = tool_from_call(message);
                call.status = status;
                call.output_preview = preview;
                self.blocks.push(Block::Tool(call));
            }
        }
    }

    fn into_turn(self) -> Turn {
        let (steps, answer, issues) = split_blocks(self.blocks);
        let (message_id, rating) = self
            .answer_message
            .map_or((None, None), |(id, rating)| (Some(id), rating));
        Turn {
            key: self.key,
            question: self.question,
            image_count: self.image_count,
            message_id,
            rating,
            steps,
            answer,
            issues,
            live: None,
            started_at: Some(self.started_at),
        }
    }
}

fn tool_from_call(message: &AssistantMessageDto) -> AssistantToolCallDto {
    let arguments = (message.role == "tool_call")
        .then(|| serde_json::from_str::<Value>(&message.content).ok())
        .flatten()
        .filter(|value| arguments_summary(Some(value)).is_some());
    AssistantToolCallDto {
        call_id: message.tool_call_id.clone().unwrap_or_default(),
        name: message.tool_name.clone().unwrap_or_default(),
        kind: message
            .tool_kind
            .clone()
            .unwrap_or_else(|| "server".to_owned()),
        status: AssistantToolStatus::Running,
        duration_ms: message.duration_ms,
        arguments,
        ..AssistantToolCallDto::default()
    }
}

/// 合并云端历史与本机实时执行。
///
/// - 历史消息按 `seq` 排序，每条用户消息（或执行 ID 变化）开启新一轮；
/// - 进行中的执行永远用实时覆盖层：同一 `runId` 的历史消息被它取代（原位替换，避免半截历史
///   与流式内容并存）；
/// - 已结束的执行若已落库（历史里出现同一 `runId`）以历史为准，不再重复显示；未落库
///   （如首帧前失败、被取消）的保留覆盖层；
/// - 云端问题单按创建时间挂到最近的历史轮次；实时执行里已带的问题单不重复。
pub(crate) fn build_timeline(
    session_id: &str,
    messages: &[AssistantMessageDto],
    issues: &[AssistantIssueDto],
    runs: &[AssistantRunDto],
    overrides: &HashMap<String, AssistantIssueDto>,
) -> Vec<Turn> {
    let live: Vec<&AssistantRunDto> = runs
        .iter()
        .filter(|run| run.session_id == session_id)
        .collect();
    let mut ordered: Vec<&AssistantMessageDto> = messages.iter().collect();
    ordered.sort_by_key(|message| message.seq);

    let mut groups: Vec<HistoryTurn> = Vec::new();
    for message in ordered {
        let starts_new = match groups.last() {
            None => true,
            Some(current) => message.role == "user" || current.run_id != message.run_id,
        };
        if starts_new {
            groups.push(HistoryTurn::new(message));
        }
        if let Some(current) = groups.last_mut() {
            current.push(message);
        }
    }

    let mut consumed = vec![false; live.len()];
    let mut turns = Vec::with_capacity(groups.len() + live.len());
    for group in groups {
        let matched = group.run_id.as_deref().and_then(|id| {
            live.iter()
                .position(|run| run.run_id.as_deref() == Some(id))
        });
        match matched {
            Some(index) => {
                let already = std::mem::replace(&mut consumed[index], true);
                let run = live[index];
                if !run.status.is_active() {
                    turns.push(group.into_turn());
                } else if !already {
                    turns.push(live_turn(run, overrides));
                }
            }
            None => turns.push(group.into_turn()),
        }
    }
    for (index, run) in live.iter().enumerate() {
        if !consumed[index] {
            turns.push(live_turn(run, overrides));
        }
    }
    place_issues(&mut turns, issues, overrides);
    turns
}

fn second_key(timestamp: &str) -> &str {
    timestamp.get(..TIMESTAMP_SECOND_CHARS).unwrap_or(timestamp)
}

fn place_issues(
    turns: &mut Vec<Turn>,
    issues: &[AssistantIssueDto],
    overrides: &HashMap<String, AssistantIssueDto>,
) {
    for issue in issues {
        let shown = turns
            .iter()
            .any(|turn| turn.issues.iter().any(|known| known.id == issue.id));
        if shown {
            continue;
        }
        let issue = resolve_issue(issue, overrides);
        let created = second_key(&issue.created_at);
        let target = turns
            .iter()
            .rposition(|turn| {
                turn.started_at
                    .as_deref()
                    .is_some_and(|start| second_key(start) <= created)
            })
            .or_else(|| turns.iter().position(|turn| turn.started_at.is_some()))
            .or_else(|| turns.len().checked_sub(1));
        match target.and_then(|index| turns.get_mut(index)) {
            Some(turn) => turn.issues.push(issue),
            None => turns.push(Turn {
                key: format!("issue:{}", issue.id),
                question: String::new(),
                image_count: 0,
                message_id: None,
                rating: None,
                steps: Vec::new(),
                answer: None,
                issues: vec![issue],
                live: None,
                started_at: None,
            }),
        }
    }
}

// ───────────────────────── 图片占位符 ─────────────────────────

/// 一次文本编辑：把 `range`（字节区间）替换为 `text`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edit {
    pub(crate) range: Range<usize>,
    pub(crate) text: String,
}

/// 不超过 `index` 的最大字符边界（编辑器给的偏移本应已在边界上，这里只做防御）。
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 在选区处放入第 `number` 张图的占位符（选区被替换，与粘贴一致）。与相邻文字之间补一个
/// 空格，免得占位符黏在词上；该侧已是空白则不重复补。
pub(crate) fn insert_placeholder(text: &str, selection: Range<usize>, number: usize) -> Edit {
    let start = floor_boundary(text, selection.start.min(selection.end));
    let end = floor_boundary(text, selection.start.max(selection.end)).max(start);
    let mut inserted = String::new();
    if text[..start]
        .chars()
        .next_back()
        .is_some_and(|before| !before.is_whitespace())
    {
        inserted.push(' ');
    }
    inserted.push_str(&assistant_image_placeholder(number));
    if text[end..]
        .chars()
        .next()
        .is_none_or(|after| !after.is_whitespace())
    {
        inserted.push(' ');
    }
    Edit {
        range: start..end,
        text: inserted,
    }
}

/// 文本里全部形如 `[Image #n]` 的占位符（`n` 为不带前导 0 的正整数）：区间与编号，按出现顺序。
fn placeholder_spans(text: &str) -> Vec<(Range<usize>, usize)> {
    const OPEN: &str = "[Image #";
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(OPEN) {
        let start = from + found;
        let digits_start = start + OPEN.len();
        let digits = text[digits_start..]
            .bytes()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let digits_end = digits_start + digits;
        let number = text[digits_start..digits_end]
            .parse::<usize>()
            .ok()
            .filter(|number| *number > 0 && !text[digits_start..].starts_with('0'));
        match number {
            Some(number) if text[digits_end..].starts_with(']') => {
                spans.push((start..digits_end + 1, number));
                from = digits_end + 1;
            }
            _ => from = digits_start,
        }
    }
    spans
}

/// 移除第 `number` 张图（共 `count` 张）后文本要做的编辑：删掉它的占位符，更大的编号
/// （仅限 `count` 以内，其余是用户自己写的文字）依次减 1，保持与 `images` 顺序一致。
/// 用户已手动删掉该占位符时只做重排。返回的编辑按位置升序、互不重叠；应用时从后往前。
pub(crate) fn remove_placeholder(text: &str, number: usize, count: usize) -> Vec<Edit> {
    let mut edits = Vec::new();
    for (range, found) in placeholder_spans(text) {
        if found == number {
            let mut range = range;
            // 占位符前面是行首 / 空白时顺带吞掉后面的一个空格，避免留下双空格或行首空格。
            let at_boundary = text[..range.start]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
            if at_boundary && text[range.end..].starts_with(' ') {
                range.end += 1;
            }
            edits.push(Edit {
                range,
                text: String::new(),
            });
        } else if found > number && found <= count {
            edits.push(Edit {
                range,
                text: assistant_image_placeholder(found - 1),
            });
        }
    }
    edits
}

/// 编号 1..=`count` 里在文本中已找不到占位符的图片编号（升序）：用户删掉了占位符，
/// 对应图片随之移除。
pub(crate) fn orphaned_images(text: &str, count: usize) -> Vec<usize> {
    let mut present = vec![false; count];
    for (_, number) in placeholder_spans(text) {
        if let Some(slot) = present.get_mut(number.wrapping_sub(1)) {
            *slot = true;
        }
    }
    (1..=count).filter(|number| !present[number - 1]).collect()
}

/// 一次纯删除（退格、选区删除等）只删掉了某个占位符的一部分时，删除后文本里该占位符的
/// 残余区间；删掉它让占位符整体消失，与删掉一个字符的手感一致。只认编号在 `count` 以内的
/// 占位符。不是纯删除、或没有碰到占位符的局部时返回 `None`。
pub(crate) fn placeholder_remnant(old: &str, new: &str, count: usize) -> Option<Range<usize>> {
    if count == 0 || new.len() >= old.len() {
        return None;
    }
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let start = floor_boundary(old, prefix);
    let suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(new.len() - start);
    let old_end = old.len() - suffix;
    // 新文本在 `start` 之后若还有新增内容就不是纯删除。
    if new.len() - suffix != start || !old.is_char_boundary(old_end) {
        return None;
    }
    let (mut lo, mut hi) = (start, old_end);
    for (span, number) in placeholder_spans(old) {
        let overlaps = span.start < old_end && start < span.end;
        let covered = start <= span.start && span.end <= old_end;
        if number <= count && overlaps && !covered {
            lo = lo.min(span.start);
            hi = hi.max(span.end);
        }
    }
    if lo == start && hi == old_end {
        return None;
    }
    let mut end = start + (hi - old_end);
    // 与 `remove_placeholder` 一致：占位符前是行首 / 空白时顺带吞掉后面的一个空格。
    let at_boundary = new[..lo]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if at_boundary && new[end..].starts_with(' ') {
        end += 1;
    }
    Some(lo..end)
}

/// 把编辑前的偏移映射到编辑后：落在被替换区间内的偏移取区间起点。
pub(crate) fn map_offset(offset: usize, edits: &[Edit]) -> usize {
    let mut removed = 0;
    let mut added = 0;
    for edit in edits {
        if edit.range.end <= offset {
            removed += edit.range.len();
            added += edit.text.len();
        } else if edit.range.start < offset {
            removed += offset - edit.range.start;
        }
    }
    offset.saturating_sub(removed) + added
}

// ───────────────────────── 工具调用 ─────────────────────────

/// 工具调用在界面上的呈现状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolState {
    Running,
    Succeeded,
    Failed,
    /// 所属执行已结束，工具却没有结果（连接中断等）。
    Interrupted,
}

pub(crate) fn tool_state(call: &AssistantToolCallDto, run_active: bool) -> ToolState {
    match call.status {
        AssistantToolStatus::Succeeded => ToolState::Succeeded,
        AssistantToolStatus::Failed => ToolState::Failed,
        AssistantToolStatus::Running if run_active => ToolState::Running,
        AssistantToolStatus::Running => ToolState::Interrupted,
    }
}

/// 确认卡片的呈现状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmView {
    /// 等待确认；`remaining_ms` 为到期前剩余毫秒（无期限时为 `None`）。
    Pending {
        remaining_ms: Option<i64>,
    },
    Approved,
    Declined,
    Expired,
}

impl ConfirmView {
    pub(crate) const fn is_pending(self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

/// 推导确认卡片状态。`None` = 该工具不需要确认（只读）。
///
/// 云端 / agent 的「过期」推送可能晚于本机时钟：待确认超过截止时间即视为过期；所属执行
/// 已结束时仍「待确认」的调用同样不再可操作。
pub(crate) fn confirm_view(
    call: &AssistantToolCallDto,
    run_active: bool,
    now_ms: i64,
) -> Option<ConfirmView> {
    let state = call.confirmation?;
    Some(match state {
        AssistantConfirmationState::Approved => ConfirmView::Approved,
        AssistantConfirmationState::Declined => ConfirmView::Declined,
        AssistantConfirmationState::Expired => ConfirmView::Expired,
        AssistantConfirmationState::Pending => {
            if !run_active {
                ConfirmView::Expired
            } else {
                match call.confirm_deadline_unix_ms {
                    Some(deadline) if deadline <= now_ms => ConfirmView::Expired,
                    Some(deadline) => ConfirmView::Pending {
                        remaining_ms: Some(deadline - now_ms),
                    },
                    None => ConfirmView::Pending { remaining_ms: None },
                }
            }
        }
    })
}

/// 剩余毫秒 → 向上取整的秒数（倒计时显示）。
pub(crate) const fn remaining_seconds(remaining_ms: i64) -> i64 {
    (remaining_ms + 999) / 1000
}

/// 倒计时文案:一分钟以内 `N s`,否则 `M:SS`。
pub(crate) fn format_countdown(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// 所有进行中执行里等待确认的调用数。
pub(crate) fn pending_confirmations(state: &AssistantStateDto, now_ms: i64) -> usize {
    state
        .runs
        .iter()
        .filter(|run| run.status.is_active())
        .flat_map(|run| run.items.iter())
        .filter(|item| match item {
            AssistantRunItemDto::Tool { call } => matches!(
                confirm_view(call, true, now_ms),
                Some(ConfirmView::Pending { .. })
            ),
            AssistantRunItemDto::Text { .. } | AssistantRunItemDto::IssueDraft { .. } => false,
        })
        .count()
}

/// 当前会话之外、有待确认工具的会话（头部提示据此给出跳转）。
pub(crate) fn pending_elsewhere(
    state: &AssistantStateDto,
    current: Option<&str>,
    now_ms: i64,
) -> Option<String> {
    state
        .runs
        .iter()
        .filter(|run| run.status.is_active() && Some(run.session_id.as_str()) != current)
        .find(|run| {
            run.items.iter().any(|item| match item {
                AssistantRunItemDto::Tool { call } => matches!(
                    confirm_view(call, true, now_ms),
                    Some(ConfirmView::Pending { .. })
                ),
                AssistantRunItemDto::Text { .. } | AssistantRunItemDto::IssueDraft { .. } => false,
            })
        })
        .map(|run| run.session_id.clone())
}

/// 模型给出的参数的一行摘要；没有有意义的参数时为 `None`。
pub(crate) fn arguments_summary(arguments: Option<&Value>) -> Option<String> {
    let value = arguments?;
    let empty = match value {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::String(text) => text.trim().is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    };
    if empty {
        return None;
    }
    Some(truncate_chars(&value.to_string(), ARGUMENT_SUMMARY_CHARS))
}

/// 参数的完整缩进 JSON（展开查看）。
pub(crate) fn arguments_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// 工具结果的展开展示：合法 JSON 缩进美化；被截断成非法 JSON 或本就是纯文本时原样（去首尾空白）。
pub(crate) fn format_tool_output(text: &str) -> String {
    let trimmed = text.trim();
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => arguments_pretty(&value),
        Err(_) => trimmed.to_owned(),
    }
}

pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// 工具耗时：不足 1 秒用毫秒，否则一位小数的秒。
pub(crate) fn format_duration(duration_ms: i64) -> String {
    if duration_ms < 1000 {
        format!("{} ms", duration_ms.max(0))
    } else {
        format!("{}.{} s", duration_ms / 1000, (duration_ms % 1000) / 100)
    }
}

// ───────────────────────── 可用性与错误 ─────────────────────────

/// 页面级门禁：决定显示对话还是说明页。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    SignedOut,
    /// 已登录、配置尚未取得。
    Loading,
    /// 服务端未开放（`off` / `internal_only` / 未知原因）。
    NotOpen,
    /// 当前套餐不含助手。
    PlanDisabled,
    /// 配置取不到且本机服务 / 云端报错。
    ServiceError,
    /// 可对话；额度用尽时仍可浏览历史，但不能提问。
    Open {
        quota_exhausted: bool,
    },
}

pub(crate) const fn quota_exhausted(config: &AssistantConfigDto) -> bool {
    config.daily_limit > 0 && config.used_today >= config.daily_limit as u64
}

pub(crate) fn gate(signed_in: bool, state: &AssistantStateDto, fetch_failed: bool) -> Gate {
    if !signed_in {
        return Gate::SignedOut;
    }
    match &state.config {
        None => {
            let failed = fetch_failed
                || (!state.loading
                    && (state.last_error_reason.is_some() || state.last_error_message.is_some()));
            if failed {
                Gate::ServiceError
            } else {
                Gate::Loading
            }
        }
        Some(config) if !config.available => {
            if config.reason.as_deref() == Some("plan_disabled") {
                Gate::PlanDisabled
            } else {
                Gate::NotOpen
            }
        }
        Some(config) => Gate::Open {
            quota_exhausted: quota_exhausted(config),
        },
    }
}

/// 提问失败的归类（决定提示文案与是否需要重新取配置）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendFailure {
    SignedOut,
    NotOpen,
    QuotaExceeded,
    /// 该会话已有执行在进行。
    Busy,
    Unavailable,
    /// 参数被拒（过长等）。
    Rejected,
    Failed,
}

pub(crate) fn classify_send_error(error: &RpcErrorData) -> SendFailure {
    if error.reason == Some(ErrorReason::RateLimited) {
        return SendFailure::QuotaExceeded;
    }
    if error.reason == Some(ErrorReason::SessionExpired) {
        return SendFailure::SignedOut;
    }
    if error.reason == Some(ErrorReason::CloudUnreachable) {
        return SendFailure::Unavailable;
    }
    match error.code {
        ApplicationErrorCode::Unauthorized => SendFailure::SignedOut,
        ApplicationErrorCode::Unsupported => SendFailure::NotOpen,
        ApplicationErrorCode::Conflict => SendFailure::Busy,
        ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout => {
            SendFailure::Unavailable
        }
        ApplicationErrorCode::InvalidArgument => SendFailure::Rejected,
        _ => SendFailure::Failed,
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::AssistantToolEffect;
    use serde_json::json;

    use super::*;

    fn message(
        id: &str,
        run: Option<&str>,
        seq: i64,
        role: &str,
        content: &str,
    ) -> AssistantMessageDto {
        AssistantMessageDto {
            id: id.to_owned(),
            run_id: run.map(str::to_owned),
            seq,
            role: role.to_owned(),
            content: content.to_owned(),
            created_at: format!("2026-11-27T01:00:{seq:02}Z"),
            ..AssistantMessageDto::default()
        }
    }

    fn tool_message(
        id: &str,
        run: &str,
        seq: i64,
        role: &str,
        call_id: &str,
        name: &str,
        is_error: bool,
    ) -> AssistantMessageDto {
        AssistantMessageDto {
            tool_call_id: Some(call_id.to_owned()),
            tool_name: Some(name.to_owned()),
            tool_kind: Some("client".to_owned()),
            is_error,
            duration_ms: (role == "tool_result").then_some(420),
            ..message(id, Some(run), seq, role, "{\"a\":1}")
        }
    }

    fn run(
        session: &str,
        run_id: Option<&str>,
        status: AssistantRunStatus,
        items: Vec<AssistantRunItemDto>,
    ) -> AssistantRunDto {
        AssistantRunDto {
            session_id: session.to_owned(),
            run_id: run_id.map(str::to_owned),
            status,
            question: "live question".to_owned(),
            started_at_unix_ms: 1_000,
            items,
            message_id: None,
            error: None,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn text(value: &str) -> AssistantRunItemDto {
        AssistantRunItemDto::Text {
            text: value.to_owned(),
        }
    }

    fn pending_call(deadline: Option<i64>) -> AssistantToolCallDto {
        AssistantToolCallDto {
            call_id: "c1".to_owned(),
            name: "settings_patch".to_owned(),
            kind: "client".to_owned(),
            request_id: Some("req-1".to_owned()),
            effect: AssistantToolEffect::Write,
            confirmation: Some(AssistantConfirmationState::Pending),
            confirm_deadline_unix_ms: deadline,
            ..AssistantToolCallDto::default()
        }
    }

    fn none() -> HashMap<String, AssistantIssueDto> {
        HashMap::new()
    }

    #[test]
    fn history_turns_group_by_user_message_and_attach_tool_results() {
        // 故意乱序：时间线按 seq 排序。
        let history = [
            message("m4", Some("r1"), 4, "assistant", "about 80 Mbps"),
            message("m1", Some("r1"), 1, "user", "how fast?"),
            tool_message("m3", "r1", 3, "tool_result", "c1", "net_measure", false),
            tool_message("m2", "r1", 2, "tool_call", "c1", "net_measure", false),
            message("m6", Some("r2"), 6, "assistant", "you are welcome"),
            message("m5", Some("r2"), 5, "user", "thanks"),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].question, "how fast?");
        assert_eq!(turns[0].answer.as_deref(), Some("about 80 Mbps"));
        assert_eq!(turns[0].tool_count(), 1);
        match &turns[0].steps[0] {
            Block::Tool(call) => {
                assert_eq!(call.status, AssistantToolStatus::Succeeded);
                assert_eq!(call.duration_ms, Some(420));
                assert!(call.output_preview.is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(turns[1].answer.as_deref(), Some("you are welcome"));
        assert!(turns.iter().all(|turn| turn.live.is_none()));
    }

    #[test]
    fn tool_without_result_in_history_is_interrupted() {
        let history = [
            message("m1", Some("r1"), 1, "user", "q"),
            tool_message("m2", "r1", 2, "tool_call", "c1", "net_measure", false),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        let Block::Tool(call) = &turns[0].steps[0] else {
            panic!("expected tool step");
        };
        assert_eq!(tool_state(call, false), ToolState::Interrupted);
        assert_eq!(turns[0].answer, None);
    }

    #[test]
    fn persisted_finished_run_is_not_duplicated_by_the_overlay() {
        let history = [
            message("m1", Some("r1"), 1, "user", "q"),
            message("m2", Some("r1"), 2, "assistant", "persisted answer"),
        ];
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Completed,
            vec![text("overlay answer")],
        )];
        let turns = build_timeline("s1", &history, &[], &runs, &none());
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].answer.as_deref(), Some("persisted answer"));
        assert!(turns[0].live.is_none());
    }

    #[test]
    fn active_run_replaces_its_partial_history_in_place() {
        let history = [
            message("m0", Some("r0"), 1, "user", "old"),
            message("m1", Some("r1"), 2, "user", "q"),
            message("m2", Some("r1"), 3, "assistant", "partial"),
        ];
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Running,
            vec![text("streaming more text")],
        )];
        let turns = build_timeline("s1", &history, &[], &runs, &none());
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].question, "old");
        assert_eq!(turns[1].answer.as_deref(), Some("streaming more text"));
        assert!(turns[1].run_active());
    }

    #[test]
    fn unpersisted_and_other_session_runs() {
        let runs = [
            run("s1", None, AssistantRunStatus::Starting, Vec::new()),
            run(
                "s1",
                Some("rx"),
                AssistantRunStatus::Failed,
                vec![text("half")],
            ),
            run("other", Some("ry"), AssistantRunStatus::Running, Vec::new()),
        ];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        assert_eq!(turns.len(), 2);
        assert!(turns[0].run_active());
        assert_eq!(turns[0].question, "live question");
        assert_eq!(
            turns[1].live.as_ref().map(|live| live.status),
            Some(AssistantRunStatus::Failed)
        );
        assert_eq!(turns[1].answer.as_deref(), Some("half"));
    }

    #[test]
    fn live_items_split_steps_answer_and_issues() {
        let issue = AssistantIssueDto {
            id: "i1".to_owned(),
            status: "draft".to_owned(),
            ..AssistantIssueDto::default()
        };
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Running,
            vec![
                text("let me check"),
                AssistantRunItemDto::Tool {
                    call: pending_call(None),
                },
                text("final"),
                AssistantRunItemDto::IssueDraft { issue },
            ],
        )];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        assert_eq!(turns[0].steps.len(), 2);
        assert_eq!(turns[0].answer.as_deref(), Some("final"));
        assert_eq!(turns[0].issues.len(), 1);
    }

    #[test]
    fn history_issues_attach_to_the_latest_turn_started_before_them() {
        let history = [
            message("m1", Some("r1"), 1, "user", "q1"),
            message("m2", Some("r1"), 2, "assistant", "a1"),
            message("m3", Some("r2"), 10, "user", "q2"),
            message("m4", Some("r2"), 11, "assistant", "a2"),
        ];
        let issue = |id: &str, created: &str| AssistantIssueDto {
            id: id.to_owned(),
            created_at: created.to_owned(),
            ..AssistantIssueDto::default()
        };
        let issues = [
            issue("early", "2026-11-27T01:00:05.250Z"),
            issue("late", "2026-11-27T01:00:12Z"),
            issue("before-all", "2026-11-27T00:59:00Z"),
        ];
        let turns = build_timeline("s1", &history, &issues, &[], &none());
        let ids = |turn: &Turn| turn.issues.iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&turns[0]), ["early", "before-all"]);
        assert_eq!(ids(&turns[1]), ["late"]);
    }

    #[test]
    fn issue_placement_by_second_ignores_fraction_digits() {
        let history = [
            message("m1", Some("r1"), 1, "user", "q1"),
            message("m2", Some("r2"), 10, "user", "q2"),
        ];
        let issues = [AssistantIssueDto {
            id: "mid".to_owned(),
            created_at: "2026-11-27T01:00:05.250Z".to_owned(),
            ..AssistantIssueDto::default()
        }];
        let turns = build_timeline("s1", &history, &issues, &[], &none());
        assert_eq!(turns[0].issues.len(), 1);
        assert!(turns[1].issues.is_empty());
    }

    #[test]
    fn issues_already_in_a_live_run_are_not_repeated_and_overrides_win() {
        let draft = AssistantIssueDto {
            id: "i1".to_owned(),
            status: "draft".to_owned(),
            ..AssistantIssueDto::default()
        };
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Completed,
            vec![AssistantRunItemDto::IssueDraft {
                issue: draft.clone(),
            }],
        )];
        let mut overrides = HashMap::new();
        overrides.insert(
            "i1".to_owned(),
            AssistantIssueDto {
                status: "submitted".to_owned(),
                ..draft.clone()
            },
        );
        let turns = build_timeline("s1", &[], &[draft], &runs, &overrides);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].issues.len(), 1);
        assert_eq!(turns[0].issues[0].status, "submitted");
    }

    #[test]
    fn issues_without_any_turn_get_their_own_entry() {
        let issues = [AssistantIssueDto {
            id: "i1".to_owned(),
            ..AssistantIssueDto::default()
        }];
        let turns = build_timeline("s1", &[], &issues, &[], &none());
        assert_eq!(turns.len(), 1);
        assert!(turns[0].question.is_empty());
        assert_eq!(turns[0].issues.len(), 1);
    }

    #[test]
    fn confirmation_states() {
        let now = 10_000;
        let view = |call: &AssistantToolCallDto, active| confirm_view(call, active, now);
        // 无确认字段 = 只读工具。
        assert_eq!(view(&AssistantToolCallDto::default(), true), None);
        // 待确认：剩余时间 = 截止 - 现在。
        assert_eq!(
            view(&pending_call(Some(now + 28_500)), true),
            Some(ConfirmView::Pending {
                remaining_ms: Some(28_500)
            })
        );
        assert_eq!(
            view(&pending_call(None), true),
            Some(ConfirmView::Pending { remaining_ms: None })
        );
        // 截止时间已过（推送晚于本机时钟）→ 过期；恰好到点也算过期。
        assert_eq!(
            view(&pending_call(Some(now)), true),
            Some(ConfirmView::Expired)
        );
        assert_eq!(
            view(&pending_call(Some(now - 1)), true),
            Some(ConfirmView::Expired)
        );
        // 执行已结束仍待确认 → 不可再操作。
        assert_eq!(
            view(&pending_call(Some(now + 5_000)), false),
            Some(ConfirmView::Expired)
        );
        // 已处理的状态原样返回，与截止时间无关。
        for (state, expected) in [
            (AssistantConfirmationState::Approved, ConfirmView::Approved),
            (AssistantConfirmationState::Declined, ConfirmView::Declined),
            (AssistantConfirmationState::Expired, ConfirmView::Expired),
        ] {
            let call = AssistantToolCallDto {
                confirmation: Some(state),
                confirm_deadline_unix_ms: Some(now + 99_000),
                ..pending_call(None)
            };
            assert_eq!(view(&call, false), Some(expected));
        }
    }

    #[test]
    fn countdown_rounds_up() {
        assert_eq!(remaining_seconds(1), 1);
        assert_eq!(remaining_seconds(1_000), 1);
        assert_eq!(remaining_seconds(1_001), 2);
        assert_eq!(remaining_seconds(29_500), 30);
        assert_eq!(format_countdown(0), "0 s");
        assert_eq!(format_countdown(59), "59 s");
        assert_eq!(format_countdown(60), "1:00");
        assert_eq!(format_countdown(125), "2:05");
        assert_eq!(format_countdown(-3), "0 s");
    }

    #[test]
    fn pending_counts_cover_active_runs_only() {
        let now = 1_000;
        let pending = || AssistantRunItemDto::Tool {
            call: pending_call(Some(now + 5_000)),
        };
        let state = AssistantStateDto {
            runs: vec![
                run(
                    "a",
                    Some("r1"),
                    AssistantRunStatus::Running,
                    vec![pending()],
                ),
                run(
                    "b",
                    Some("r2"),
                    AssistantRunStatus::Canceled,
                    vec![pending()],
                ),
                run(
                    "c",
                    Some("r3"),
                    AssistantRunStatus::Running,
                    vec![pending()],
                ),
            ],
            ..AssistantStateDto::default()
        };
        assert_eq!(pending_confirmations(&state, now), 2);
        assert_eq!(
            pending_elsewhere(&state, Some("a"), now).as_deref(),
            Some("c")
        );
        assert_eq!(
            pending_elsewhere(&state, Some("c"), now).as_deref(),
            Some("a")
        );
        assert_eq!(pending_confirmations(&state, now + 6_000), 0);
    }

    #[test]
    fn steps_open_defaults_and_overrides() {
        let now = 0;
        let thinking = build_timeline(
            "s1",
            &[],
            &[],
            &[run(
                "s1",
                Some("r1"),
                AssistantRunStatus::Running,
                vec![
                    text("plan"),
                    AssistantRunItemDto::Tool {
                        call: AssistantToolCallDto::default(),
                    },
                ],
            )],
            &none(),
        );
        assert!(steps_open(&thinking[0], None, now));
        assert!(!steps_open(&thinking[0], Some(false), now));

        let answered = build_timeline(
            "s1",
            &[],
            &[],
            &[run(
                "s1",
                Some("r1"),
                AssistantRunStatus::Running,
                vec![
                    AssistantRunItemDto::Tool {
                        call: AssistantToolCallDto::default(),
                    },
                    text("answer"),
                ],
            )],
            &none(),
        );
        assert!(!steps_open(&answered[0], None, now));
        assert!(steps_open(&answered[0], Some(true), now));

        // 待确认时强制展开，用户收起也无效。
        let confirming = build_timeline(
            "s1",
            &[],
            &[],
            &[run(
                "s1",
                Some("r1"),
                AssistantRunStatus::Running,
                vec![
                    AssistantRunItemDto::Tool {
                        call: pending_call(Some(60_000)),
                    },
                    text("x"),
                ],
            )],
            &none(),
        );
        assert!(steps_open(&confirming[0], Some(false), now));
        // 过期后不再强制。
        assert!(!steps_open(&confirming[0], Some(false), 61_000));
    }

    #[test]
    fn gate_reflects_account_plan_quota_and_errors() {
        let open = AssistantConfigDto {
            available: true,
            daily_limit: 5,
            used_today: 4,
            ..AssistantConfigDto::default()
        };
        let with = |config: Option<AssistantConfigDto>| AssistantStateDto {
            config,
            ..AssistantStateDto::default()
        };
        assert_eq!(
            gate(false, &with(Some(open.clone())), false),
            Gate::SignedOut
        );
        assert_eq!(gate(true, &with(None), false), Gate::Loading);
        assert_eq!(gate(true, &with(None), true), Gate::ServiceError);
        assert_eq!(
            gate(
                true,
                &AssistantStateDto {
                    last_error_reason: Some(ErrorReason::CloudUnreachable),
                    ..AssistantStateDto::default()
                },
                false
            ),
            Gate::ServiceError
        );
        assert_eq!(
            gate(true, &with(Some(open.clone())), false),
            Gate::Open {
                quota_exhausted: false
            }
        );
        let used_up = AssistantConfigDto {
            used_today: 5,
            ..open.clone()
        };
        assert_eq!(
            gate(true, &with(Some(used_up)), false),
            Gate::Open {
                quota_exhausted: true
            }
        );
        // 0 = 不限额。
        let unlimited = AssistantConfigDto {
            daily_limit: 0,
            used_today: 999,
            ..open.clone()
        };
        assert_eq!(
            gate(true, &with(Some(unlimited)), false),
            Gate::Open {
                quota_exhausted: false
            }
        );
        let plan = AssistantConfigDto {
            available: false,
            reason: Some("plan_disabled".to_owned()),
            ..open.clone()
        };
        assert_eq!(gate(true, &with(Some(plan)), false), Gate::PlanDisabled);
        for reason in [Some("off"), Some("internal_only"), None] {
            let closed = AssistantConfigDto {
                available: false,
                reason: reason.map(str::to_owned),
                ..open.clone()
            };
            assert_eq!(gate(true, &with(Some(closed)), false), Gate::NotOpen);
        }
    }

    #[test]
    fn send_errors_are_classified() {
        let error = |code| RpcErrorData::new(code, false);
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Unauthorized)),
            SendFailure::SignedOut
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Unavailable)),
            SendFailure::Unavailable
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Conflict)),
            SendFailure::Busy
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Unsupported)),
            SendFailure::NotOpen
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Internal)),
            SendFailure::Failed
        );
        assert_eq!(
            classify_send_error(
                &error(ApplicationErrorCode::Unavailable).with_reason(ErrorReason::RateLimited)
            ),
            SendFailure::QuotaExceeded
        );
        assert_eq!(
            classify_send_error(
                &error(ApplicationErrorCode::Internal).with_reason(ErrorReason::CloudUnreachable)
            ),
            SendFailure::Unavailable
        );
    }

    #[test]
    fn argument_summaries() {
        assert_eq!(arguments_summary(None), None);
        assert_eq!(arguments_summary(Some(&json!(null))), None);
        assert_eq!(arguments_summary(Some(&json!({}))), None);
        assert_eq!(
            arguments_summary(Some(&json!({"limit": 3}))).as_deref(),
            Some("{\"limit\":3}")
        );
        let long = json!({ "text": "界".repeat(500) });
        let summary = arguments_summary(Some(&long));
        assert!(
            summary.as_ref().is_some_and(
                |s| s.chars().count() == ARGUMENT_SUMMARY_CHARS + 1 && s.ends_with('…')
            )
        );
        assert!(arguments_pretty(&json!({"a": [1]})).contains("\n  \"a\""));
    }

    #[test]
    fn tool_output_is_pretty_only_when_valid_json() {
        assert_eq!(
            format_tool_output(" {\"a\":[1]} "),
            "{\n  \"a\": [\n    1\n  ]\n}"
        );
        let truncated = "{\"activeTasks\":0,\"pending";
        assert_eq!(format_tool_output(truncated), truncated);
        assert_eq!(format_tool_output("  plain text\n"), "plain text");
    }

    #[test]
    fn duration_labels() {
        assert_eq!(format_duration(0), "0 ms");
        assert_eq!(format_duration(850), "850 ms");
        assert_eq!(format_duration(1_000), "1.0 s");
        assert_eq!(format_duration(12_345), "12.3 s");
    }

    #[test]
    fn history_answer_carries_message_id_and_rating() {
        let mut first = message("a1", Some("r1"), 2, "assistant", "first answer");
        first.rating = Some("up".to_owned());
        let mut second = message("a2", Some("r2"), 6, "assistant", "final answer");
        second.rating = Some("down".to_owned());
        let messages = [
            message("u1", Some("r1"), 1, "user", "q1"),
            first,
            message("u2", Some("r2"), 3, "user", "q2"),
            message("a2-draft", Some("r2"), 4, "assistant", "let me check"),
            tool_message("t1", "r2", 5, "tool_call", "c1", "fd_list_tasks", false),
            second,
        ];
        let turns = build_timeline("s1", &messages, &[], &[], &none());
        assert_eq!(turns.len(), 2);
        // 每轮各有各的评价对象：取该轮最后一条有内容的 assistant 消息。
        assert_eq!(turns[0].rateable_message(), Some("a1"));
        assert_eq!(turns[0].rating.as_deref(), Some("up"));
        assert_eq!(turns[1].rateable_message(), Some("a2"));
        assert_eq!(turns[1].rating.as_deref(), Some("down"));
    }

    #[test]
    fn history_turn_without_answer_is_not_rateable() {
        let messages = [
            message("u1", Some("r1"), 1, "user", "q"),
            message("a1", Some("r1"), 2, "assistant", "checking"),
            tool_message("t1", "r1", 3, "tool_call", "c1", "fd_list_tasks", false),
        ];
        let turns = build_timeline("s1", &messages, &[], &[], &none());
        // 最后一项是工具调用：没有最终回答，不能评价。
        assert_eq!(turns[0].rateable_message(), None);
    }

    #[test]
    fn live_run_is_rateable_only_after_it_completed() {
        let mut finished = run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Completed,
            vec![text("answer")],
        );
        finished.message_id = Some("m9".to_owned());
        let turns = build_timeline("s1", &[], &[], &[finished.clone()], &none());
        assert_eq!(turns[0].rateable_message(), Some("m9"));
        assert_eq!(turns[0].rating, None);

        let mut replied = finished.clone();
        replied.status = AssistantRunStatus::Replied;
        let turns = build_timeline("s1", &[], &[], &[replied], &none());
        assert_eq!(turns[0].rateable_message(), Some("m9"));

        for status in [
            AssistantRunStatus::Starting,
            AssistantRunStatus::Running,
            AssistantRunStatus::Failed,
            AssistantRunStatus::Canceled,
        ] {
            let mut other = finished.clone();
            other.status = status;
            let turns = build_timeline("s1", &[], &[], &[other], &none());
            assert_eq!(turns[0].rateable_message(), None, "{status:?}");
        }

        let mut empty = finished;
        empty.items.clear();
        let turns = build_timeline("s1", &[], &[], &[empty], &none());
        assert_eq!(turns[0].rateable_message(), None);
    }

    /// 按视图的做法应用编辑：从后往前，前面的偏移才不会失效。
    fn apply(text: &str, edits: &[Edit]) -> String {
        let mut out = text.to_owned();
        for edit in edits.iter().rev() {
            out.replace_range(edit.range.clone(), &edit.text);
        }
        out
    }

    /// 在 `text` 的 `cursor` 处放入第 `number` 张图，返回新文本与新光标。
    fn insert_at(text: &str, cursor: usize, number: usize) -> (String, usize) {
        let edit = insert_placeholder(text, cursor..cursor, number);
        let caret = edit.range.start + edit.text.len();
        (apply(text, &[edit]), caret)
    }

    #[test]
    fn placeholder_insertion_pads_only_where_needed() {
        assert_eq!(insert_at("", 0, 1), ("[Image #1] ".to_owned(), 11));
        // 紧贴文字：两侧各补一个空格。
        assert_eq!(insert_at("ab", 1, 1).0, "a [Image #1] b");
        // 左侧已是空白、右侧是文末：只在右侧补。
        assert_eq!(insert_at("see ", 4, 2).0, "see [Image #2] ");
        // 两侧都已是空白：不补。
        assert_eq!(insert_at("a  b", 2, 1).0, "a [Image #1] b");
        // 多字节文字：偏移是字节偏移。
        assert_eq!(insert_at("看图", 3, 1).0, "看 [Image #1] 图");
    }

    #[test]
    fn placeholder_insertion_replaces_the_selection_and_clamps() {
        let edit = insert_placeholder("hello world", 6..11, 3);
        assert_eq!(apply("hello world", &[edit]), "hello [Image #3] ");
        // 反向选区与越界偏移不 panic，按文末处理。
        let edit = insert_placeholder("ab", 9..9, 1);
        assert_eq!(edit.range, 2..2);
        let edit = insert_placeholder("ab", Range { start: 2, end: 1 }, 1);
        assert_eq!(edit.range, 1..2);
        // 落在多字节字符中间的偏移退到字符边界。
        let edit = insert_placeholder("看", 1..1, 1);
        assert_eq!(edit.range, 0..0);
    }

    #[test]
    fn sequential_insertions_number_images_in_order() {
        let (text, caret) = insert_at("compare ", 8, 1);
        let (text, caret) = insert_at(&text, caret, 2);
        let (text, _) = insert_at(&format!("{text}with this"), caret, 3);
        assert_eq!(text, "compare [Image #1] [Image #2] [Image #3] with this");
    }

    #[test]
    fn removing_a_middle_image_renumbers_the_rest() {
        let text = "a [Image #1] b [Image #2] c [Image #3] d";
        let edits = remove_placeholder(text, 2, 3);
        assert_eq!(apply(text, &edits), "a [Image #1] b c [Image #2] d");
        let edits = remove_placeholder(text, 1, 3);
        assert_eq!(apply(text, &edits), "a b [Image #1] c [Image #2] d");
        // 最后一张：后面没有要重排的。
        let edits = remove_placeholder(text, 3, 3);
        assert_eq!(apply(text, &edits), "a [Image #1] b [Image #2] c d");
    }

    #[test]
    fn removing_an_image_tidies_adjacent_spaces() {
        let text = "[Image #1] [Image #2] what is this";
        let edits = remove_placeholder(text, 1, 2);
        assert_eq!(apply(text, &edits), "[Image #1] what is this");
        // 黏在文字上的占位符：左侧不是空白时只删占位符本身。
        let text = "see[Image #1] now";
        let edits = remove_placeholder(text, 1, 1);
        assert_eq!(apply(text, &edits), "see now");
        // 占位符在文末。
        let text = "see [Image #1]";
        let edits = remove_placeholder(text, 1, 1);
        assert_eq!(apply(text, &edits), "see ");
    }

    #[test]
    fn removing_an_image_whose_placeholder_was_deleted_by_hand_only_renumbers() {
        // 用户手动删掉了 #2 的占位符，图片 2 仍在缩略图条里。
        let text = "a [Image #1] b c [Image #3] d";
        let edits = remove_placeholder(text, 2, 3);
        assert_eq!(apply(text, &edits), "a [Image #1] b c [Image #2] d");
        // #1 也被手动删光了：移除图片 1 只重排。
        let text = "b [Image #2] c [Image #3]";
        let edits = remove_placeholder(text, 1, 3);
        assert_eq!(apply(text, &edits), "b [Image #1] c [Image #2]");
        // 文本里一个占位符都没有：不产生编辑。
        assert!(remove_placeholder("plain text", 1, 2).is_empty());
    }

    #[test]
    fn removing_an_image_removes_every_copy_of_its_placeholder() {
        let text = "[Image #1] and again [Image #1] then [Image #2]";
        let edits = remove_placeholder(text, 1, 2);
        assert_eq!(apply(text, &edits), "and again then [Image #1]");
    }

    #[test]
    fn placeholder_lookalikes_are_left_alone() {
        // 超出图片数的编号、前导 0、缺括号 / 缺数字的写法都是用户自己的文字。
        let text = "[Image #9] [Image #01] [Image #] [Image #1 [Image #x] [Image #2]";
        let edits = remove_placeholder(text, 1, 2);
        assert_eq!(
            apply(text, &edits),
            "[Image #9] [Image #01] [Image #] [Image #1 [Image #x] [Image #1]"
        );
        // 多字节文字包着的占位符正常识别。
        let text = "图[Image #2]文[Image #1]";
        let edits = remove_placeholder(text, 1, 2);
        assert_eq!(apply(text, &edits), "图[Image #1]文");
    }

    #[test]
    fn offsets_follow_placeholder_edits() {
        let text = "a [Image #1] b [Image #2] c";
        let edits = remove_placeholder(text, 1, 2);
        assert_eq!(apply(text, &edits), "a b [Image #1] c");
        // 区间之前的偏移不动；之后的偏移按净删除量前移；落在被删区间内的取区间起点。
        assert_eq!(map_offset(1, &edits), 1);
        assert_eq!(map_offset(5, &edits), 2);
        assert_eq!(map_offset(text.len(), &edits), apply(text, &edits).len());
        assert_eq!(map_offset(2, &edits), 2);
    }

    #[test]
    fn images_whose_placeholder_is_gone_are_orphaned() {
        assert_eq!(orphaned_images("a [Image #1] b [Image #3]", 3), vec![2]);
        assert_eq!(
            orphaned_images("[Image #2] [Image #1]", 2),
            Vec::<usize>::new()
        );
        // 残缺占位符与超出图片数的编号都不算。
        assert_eq!(orphaned_images("[Image #1 [Image #5]", 2), vec![1, 2]);
        assert!(orphaned_images("", 0).is_empty());
    }

    /// 从 `old` 删掉 `range` 后，补删占位符残余得到的文本。
    fn delete(old: &str, range: Range<usize>, count: usize) -> String {
        let mut new = old.to_owned();
        new.replace_range(range, "");
        if let Some(remnant) = placeholder_remnant(old, &new, count) {
            new.replace_range(remnant, "");
        }
        new
    }

    #[test]
    fn partially_deleted_placeholder_is_removed_whole() {
        // 退格删掉 `]`。
        assert_eq!(delete("a [Image #1] b", 11..12, 1), "a b");
        // 从中间删一个字符。
        assert_eq!(delete("[Image #1] hi", 3..4, 1), "hi");
        // 选区跨过占位符一部分和后面的文字。
        assert_eq!(delete("x [Image #1] yz", 8..14, 1), "x z");
        // 多字节文字紧贴占位符。
        assert_eq!(delete("看[Image #1]图", 12..13, 1), "看图");
    }

    #[test]
    fn deletions_not_cutting_a_placeholder_are_left_alone() {
        let old = "a [Image #1] b";
        // 整个占位符被删：没有残余。
        assert_eq!(placeholder_remnant(old, "a  b", 1), None);
        // 删的是普通文字。
        assert_eq!(placeholder_remnant(old, "a [Image #1] ", 1), None);
        // 编号超出图片数：是用户自己的文字。
        assert_eq!(placeholder_remnant(old, "a [Image #1 b", 0), None);
        assert_eq!(placeholder_remnant("[Image #2]", "[Image #2", 1), None);
        // 不是纯删除（替换）。
        assert_eq!(placeholder_remnant(old, "a [Image #X] b", 1), None);
        assert_eq!(placeholder_remnant(old, "a [Image #1] bc", 1), None);
    }
}
