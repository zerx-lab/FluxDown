//! 助手页的纯模型：云端历史与本机实时执行的合并、工具确认状态推导、可用性判定与错误分类。
//!
//! 这里不碰 GPUI：视图只读这些函数的结果，时间由调用方传入，保证可单测。

use std::{collections::HashMap, ops::Range, sync::Arc};

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
/// 服务端「开通会员」工具名：执行 / 历史里出现它就在该轮回答下显示开通会员卡片，条目本身不进
/// 「过程」。与 FluxCloud `ai::catalog::OFFER_MEMBERSHIP` 保持一致。
pub(crate) const MEMBERSHIP_OFFER_TOOL: &str = "offer_membership";

// ───────────────────────── 时间线 ─────────────────────────

/// 一轮回答里按到达顺序排列的条目。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Text(String),
    /// 工具调用之前的中间说明文字（历史里的 `assistant_step`）：永远属于「过程」，不会被当作最终回答。
    Step(String),
    Tool(AssistantToolCallDto),
    Issue(AssistantIssueDto),
}

/// 实时执行的状态（历史轮次没有）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Live {
    pub(crate) status: AssistantRunStatus,
    pub(crate) error: Option<String>,
    /// 失败原因标识（`timeout` / `maxTurns` / `streamLost` 等）。
    pub(crate) reason: Option<String>,
}

/// 一问一答：用户提问、中间过程、最终回答与问题单草稿。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Turn {
    /// 折叠状态等界面状态的键：执行 ID 优先，其次消息 / 起始时间。
    pub(crate) key: String,
    /// 提问所在的云端用户消息 ID：重新生成 / 编辑重发据此替换；只有已落库的历史轮次有。
    pub(crate) user_message_id: Option<String>,
    pub(crate) question: String,
    pub(crate) image_count: usize,
    /// 历史提问附图的附件 ID（经 `agent.assistant.attachment` 取图）。
    pub(crate) image_ids: Vec<String>,
    /// 可评价的回答消息 ID：历史里最后一条有内容的 assistant 消息；实时执行取执行的
    /// `message_id`（只有正常作答 / 固定回复才有）。`None` = 本轮没有可评价的回答。
    pub(crate) message_id: Option<String>,
    /// 云端记录的该回答评价（`up` / `down`）；界面以本地状态优先。
    pub(crate) rating: Option<String>,
    /// 最终回答之前的全部条目：中间文本与工具调用（可折叠的「过程」）。
    pub(crate) steps: Vec<Block>,
    /// 最终回答（Markdown）；尚未产出时为 `None`。`Arc` 让每次重绘不再复制整段文本。
    pub(crate) answer: Option<Arc<str>>,
    /// 回答没写完就中断（取消 / 超时 / 失败）：内容是中断前已输出的部分。
    pub(crate) answer_interrupted: bool,
    /// 本轮是否显示开通会员卡片：有成功的 `offer_membership` 工具调用（同轮多次也只一张）。
    pub(crate) membership_offer: bool,
    pub(crate) issues: Vec<AssistantIssueDto>,
    pub(crate) live: Option<Live>,
    /// 云端历史轮次的起始时间（RFC 3339），用于把问题单挂到对应轮次；实时轮次为 `None`。
    started_at: Option<String>,
}

impl Turn {
    pub(crate) fn new(key: String) -> Self {
        Self {
            key,
            user_message_id: None,
            question: String::new(),
            image_count: 0,
            image_ids: Vec::new(),
            message_id: None,
            rating: None,
            steps: Vec::new(),
            answer: None,
            answer_interrupted: false,
            membership_offer: false,
            issues: Vec::new(),
            live: None,
            started_at: None,
        }
    }

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
            Block::Text(_) | Block::Step(_) | Block::Issue(_) => false,
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

/// 「过程」折叠区是否展开：有待确认的工具时强制展开；否则取用户选择，缺省为执行进行中展开、
/// 结束后收起（不随「回答是否已出现」开合，免得中间文字后接工具调用时反复抖动）。
pub(crate) fn steps_open(turn: &Turn, user_choice: Option<bool>, now_ms: i64) -> bool {
    if turn.has_pending_confirmation(now_ms) {
        return true;
    }
    user_choice.unwrap_or_else(|| turn.run_active())
}

/// 一轮条目拆分后的结果。
struct SplitBlocks {
    steps: Vec<Block>,
    answer: Option<String>,
    issues: Vec<AssistantIssueDto>,
    membership_offer: bool,
}

fn split_blocks(blocks: Vec<Block>) -> SplitBlocks {
    let mut issues = Vec::new();
    let mut rest = Vec::new();
    let mut membership_offer = false;
    for block in blocks {
        match block {
            Block::Issue(issue) => issues.push(issue),
            Block::Text(text) if text.trim().is_empty() => {}
            // 开通会员工具只以卡片呈现：不论状态都不进过程，成功才出卡片。
            Block::Tool(call) if call.name == MEMBERSHIP_OFFER_TOOL => {
                membership_offer |= call.status == AssistantToolStatus::Succeeded;
            }
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
    SplitBlocks {
        steps: rest,
        answer,
        issues,
        membership_offer,
    }
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
    let SplitBlocks {
        steps,
        answer,
        issues,
        membership_offer,
    } = split_blocks(blocks);
    let key = run.run_id.as_ref().map_or_else(
        || format!("live:{}", run.started_at_unix_ms),
        |id| format!("run:{id}"),
    );
    let mut turn = Turn::new(key);
    turn.question.clone_from(&run.question);
    turn.image_count = run.image_count as usize;
    turn.message_id = run.message_id.clone().filter(|_| {
        matches!(
            run.status,
            AssistantRunStatus::Completed | AssistantRunStatus::Replied
        )
    });
    turn.answer_interrupted = answer.is_some()
        && matches!(
            run.status,
            AssistantRunStatus::Failed | AssistantRunStatus::Canceled
        );
    turn.steps = steps;
    turn.answer = answer.map(Arc::from);
    turn.issues = issues;
    turn.membership_offer = membership_offer;
    turn.live = Some(Live {
        status: run.status,
        error: run.error.clone().filter(|error| !error.is_empty()),
        reason: run.error_reason.clone().filter(|reason| !reason.is_empty()),
    });
    turn
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
    user_message_id: Option<String>,
    question: String,
    image_count: usize,
    image_ids: Vec<String>,
    /// 最后一条有内容的 assistant 消息：`(消息 ID, 云端评价)`。
    answer_message: Option<(String, Option<String>)>,
    answer_interrupted: bool,
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
            user_message_id: None,
            question: String::new(),
            image_count: 0,
            image_ids: Vec::new(),
            answer_message: None,
            answer_interrupted: false,
            blocks: Vec::new(),
        }
    }

    fn push(&mut self, message: &AssistantMessageDto) {
        match message.role.as_str() {
            "user" => {
                self.question.clone_from(&message.content);
                self.image_count = message.images.len();
                self.image_ids.clone_from(&message.images);
                self.user_message_id = Some(message.id.clone());
            }
            "assistant" => {
                if !message.content.trim().is_empty() {
                    self.blocks.push(Block::Text(message.content.clone()));
                    self.answer_message = Some((message.id.clone(), message.rating.clone()));
                    self.answer_interrupted = message.interrupted;
                }
            }
            "assistant_step" => {
                if !message.content.trim().is_empty() {
                    self.blocks.push(Block::Step(message.content.clone()));
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
            Block::Text(_) | Block::Step(_) | Block::Issue(_) => false,
        });
        // 历史里保留完整结果（服务端工具也带上），界面展示时再截断；复制对话时用完整内容。
        let preview = Some(message.content.trim().to_owned()).filter(|text| !text.is_empty());
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
        let SplitBlocks {
            steps,
            answer,
            issues,
            membership_offer,
        } = split_blocks(self.blocks);
        let (message_id, rating) = self
            .answer_message
            .map_or((None, None), |(id, rating)| (Some(id), rating));
        let mut turn = Turn::new(self.key);
        turn.user_message_id = self.user_message_id;
        turn.question = self.question;
        turn.image_count = self.image_count;
        turn.image_ids = self.image_ids;
        turn.message_id = message_id;
        turn.rating = rating;
        turn.steps = steps;
        turn.answer_interrupted = self.answer_interrupted && answer.is_some();
        turn.answer = answer.map(Arc::from);
        turn.issues = issues;
        turn.membership_offer = membership_offer;
        turn.started_at = Some(self.started_at);
        turn
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
            None => {
                let mut turn = Turn::new(format!("issue:{}", issue.id));
                turn.issues.push(issue);
                turns.push(turn);
            }
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
    /// 已批准的写操作在执行中被停止：本机请求可能已生效，结果未知。
    ResultUnknown,
}

pub(crate) fn tool_state(call: &AssistantToolCallDto, run_active: bool) -> ToolState {
    if call.interrupted {
        return ToolState::ResultUnknown;
    }
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

/// 取界面文案：`(键, 占位符参数)`。
pub(crate) type Translate<'a> = &'a dyn Fn(&str, &[(&str, &str)]) -> String;

/// 倒计时文案:一分钟以内 `N s`(单位走 i18n),否则 `M:SS`。
pub(crate) fn format_countdown(seconds: i64, t: Translate) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        t("assistantUnitSeconds", &[("n", &seconds.to_string())])
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

/// 工具耗时：不足 1 秒用毫秒，否则一位小数的秒（单位走 i18n）。
pub(crate) fn format_duration(duration_ms: i64, t: Translate) -> String {
    if duration_ms < 1000 {
        t(
            "assistantUnitMilliseconds",
            &[("n", &duration_ms.max(0).to_string())],
        )
    } else {
        let seconds = format!("{}.{}", duration_ms / 1000, (duration_ms % 1000) / 100);
        t("assistantUnitSeconds", &[("n", &seconds)])
    }
}

/// 工具结果在界面上的展开展示：先截断到预览长度，再按 [`format_tool_output`] 美化。
pub(crate) fn tool_output_display(text: &str) -> String {
    format_tool_output(&truncate_chars(text.trim(), OUTPUT_PREVIEW_CHARS))
}

/// 工具调用状态的文案键：待确认 / 未执行优先于调用状态本身。
pub(crate) fn tool_status_key(confirmation: Option<ConfirmView>, state: ToolState) -> &'static str {
    match (confirmation, state) {
        (Some(ConfirmView::Pending { .. }), _) => "assistantToolWaiting",
        (Some(ConfirmView::Declined | ConfirmView::Expired), _) => "assistantToolSkipped",
        (_, ToolState::ResultUnknown) => "assistantToolResultUnknown",
        (_, ToolState::Running) => "assistantToolRunning",
        (_, ToolState::Succeeded) => "assistantToolDone",
        (_, ToolState::Failed) => "assistantToolFailed",
        (_, ToolState::Interrupted) => "assistantToolInterrupted",
    }
}

// ───────────────────────── 复制对话 ─────────────────────────

/// 一轮对话的 Markdown 副本：提问、全部过程（中间文本、每次工具调用的状态 / 耗时 / 完整参数与
/// 结果）、最终回答、失败 / 停止说明与问题单草稿。`t` 按文案键取界面语言的标签。
pub(crate) fn turn_transcript(turn: &Turn, now_ms: i64, t: Translate) -> String {
    let tr = |key: &str| t(key, &[]);
    let mut out = format!(
        "## {}\n\n{}\n\n## {}\n",
        tr("assistantTranscriptUser"),
        turn.question.trim(),
        tr("assistantTranscriptAssistant"),
    );
    let active = turn.run_active();
    for block in &turn.steps {
        match block {
            Block::Text(text) | Block::Step(text) => push_paragraph(&mut out, text.trim()),
            Block::Tool(call) => push_tool(&mut out, call, active, now_ms, t),
            Block::Issue(_) => {}
        }
    }
    if let Some(answer) = &turn.answer {
        push_paragraph(&mut out, answer.trim());
        if turn.answer_interrupted {
            push_paragraph(&mut out, &tr("assistantAnswerInterrupted"));
        }
    }
    if turn.membership_offer {
        push_paragraph(&mut out, &format!("### {}", tr("assistantMembershipTitle")));
        push_paragraph(&mut out, &tr("assistantMembershipBody"));
    }
    if let Some(live) = &turn.live {
        match live.status {
            AssistantRunStatus::Failed => {
                push_paragraph(&mut out, &tr("assistantRunFailed"));
                push_paragraph(&mut out, &tr(run_error_key(live.reason.as_deref())));
                if let Some(error) = &live.error {
                    push_paragraph(&mut out, error.trim());
                }
            }
            AssistantRunStatus::Canceled => push_paragraph(&mut out, &tr("assistantStopped")),
            AssistantRunStatus::Starting
            | AssistantRunStatus::Running
            | AssistantRunStatus::Completed
            | AssistantRunStatus::Replied => {}
        }
    }
    for issue in &turn.issues {
        push_paragraph(&mut out, &format!("### {}", issue.title.trim()));
        push_paragraph(&mut out, issue.body.trim());
    }
    out
}

fn push_paragraph(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(text);
    out.push('\n');
}

fn push_tool(
    out: &mut String,
    call: &AssistantToolCallDto,
    run_active: bool,
    now_ms: i64,
    t: Translate,
) {
    let tr = |key: &str| t(key, &[]);
    let status = tool_status_key(
        confirm_view(call, run_active, now_ms),
        tool_state(call, run_active),
    );
    let mut head = format!(
        "### {} `{}` · {}",
        tr("assistantTranscriptTool"),
        call.name,
        tr(status)
    );
    if call.kind == "client" {
        head.push_str(" · ");
        head.push_str(&tr("assistantToolLocal"));
    }
    if let Some(duration) = call.duration_ms {
        head.push_str(" · ");
        head.push_str(&format_duration(duration, t));
    }
    push_paragraph(out, &head);
    if let Some(arguments) = call
        .arguments
        .as_ref()
        .filter(|arguments| arguments_summary(Some(arguments)).is_some())
    {
        push_paragraph(out, &format!("{}:", tr("assistantToolArguments")));
        push_paragraph(out, &fenced(&arguments_pretty(arguments), "json"));
    }
    if let Some(output) = call
        .output_preview
        .as_deref()
        .filter(|output| !output.trim().is_empty())
    {
        push_paragraph(out, &format!("{}:", tr("assistantToolOutput")));
        push_paragraph(out, &fenced(&format_tool_output(output), ""));
    }
}

/// Markdown 代码块：围栏比内容里最长的连续反引号多一个（至少三个），内容里的 ``` 不会提前闭合。
fn fenced(body: &str, lang: &str) -> String {
    let longest = body
        .split(|c: char| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{lang}\n{body}\n{fence}")
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
    /// 当日次数用完（套餐每日上限）：重新取配置让额度卡片出现。
    QuotaExceeded,
    /// 每分钟限频：稍后再试，额度并未用完。
    RateLimited,
    /// 该会话已有执行在进行。
    Busy,
    Unavailable,
    /// 参数被拒（过长等）。
    Rejected,
    Failed,
    /// 本会话提问数已达套餐上限：开始新对话才能继续。
    SessionLimit,
}

/// 按失败原因（`reason`）归类，其次按错误码：额度用尽与每分钟限频是两回事，不能混用文案。
pub(crate) fn classify_send_error(error: &RpcErrorData) -> SendFailure {
    match error.reason {
        Some(ErrorReason::AssistantQuotaExceeded) => return SendFailure::QuotaExceeded,
        Some(ErrorReason::RateLimited) => return SendFailure::RateLimited,
        Some(ErrorReason::AssistantSessionLimit) => return SendFailure::SessionLimit,
        Some(ErrorReason::SessionExpired) => return SendFailure::SignedOut,
        Some(ErrorReason::CloudUnreachable) => return SendFailure::Unavailable,
        _ => {}
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

/// 剩余提问次数不超过这个数时，输入区提示还能问几次。
pub(crate) const SESSION_REMAINING_HINT_MAX: u64 = 3;

/// 当前会话的提问上限状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionQuota {
    /// 不限，或剩余次数还多：界面不变。
    Free,
    /// 还剩这么多次（1..=[`SESSION_REMAINING_HINT_MAX`]）：输入区附近提示。
    Low(u64),
    /// 已达上限：不能再提问（是否还有进行中的执行由调用方判断）。
    Reached,
}

/// `limit` = 配置的 `sessionLimit`（0 = 不限），`used` = 会话详情的 `questionsUsed`。
pub(crate) const fn session_quota(limit: u32, used: u64) -> SessionQuota {
    if limit == 0 {
        return SessionQuota::Free;
    }
    let limit = limit as u64;
    if used >= limit {
        return SessionQuota::Reached;
    }
    let remaining = limit - used;
    if remaining <= SESSION_REMAINING_HINT_MAX {
        SessionQuota::Low(remaining)
    } else {
        SessionQuota::Free
    }
}

/// 提问以外的动作（确认 / 评分 / 问题单 / 会话管理）失败的归类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionError {
    /// 确认窗口已过期：该调用没有执行。
    ConfirmExpired,
    Failed,
}

pub(crate) fn classify_action_error(error: &RpcErrorData) -> ActionError {
    if error.reason == Some(ErrorReason::AssistantConfirmExpired) {
        ActionError::ConfirmExpired
    } else {
        ActionError::Failed
    }
}

impl ActionError {
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::ConfirmExpired => "assistantErrConfirmExpired",
            Self::Failed => "assistantActionFailed",
        }
    }
}

impl SendFailure {
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::SignedOut => "assistantErrSignedOut",
            Self::NotOpen => "assistantErrNotOpen",
            Self::QuotaExceeded => "assistantErrQuota",
            Self::RateLimited => "assistantErrRateLimited",
            Self::Busy => "assistantErrBusy",
            Self::Unavailable => "assistantErrUnavailable",
            Self::Rejected => "assistantErrRejected",
            Self::Failed => "assistantErrFailed",
            Self::SessionLimit => "assistantErrSessionLimit",
        }
    }
}

/// 执行失败原因（`AssistantRunDto::error_reason`）对应的文案键；未知原因按 `error` 处理。
pub(crate) fn run_error_key(reason: Option<&str>) -> &'static str {
    match reason {
        Some("timeout") => "assistantRunErrTimeout",
        Some("maxTurns") => "assistantRunErrMaxTurns",
        Some("noAnswer") => "assistantRunErrNoAnswer",
        Some("contextOverflow") => "assistantRunErrContextOverflow",
        Some("imageUnsupported") => "assistantRunErrImageUnsupported",
        Some("unavailable") => "assistantRunErrUnavailable",
        Some("streamLost") => "assistantRunErrStreamLost",
        Some("network") => "assistantRunErrNetwork",
        _ => "assistantRunErrError",
    }
}

/// 是否是已知的失败原因：未知 / 缺省时界面附带显示失败原文。
pub(crate) fn run_error_known(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some(
            "timeout"
                | "maxTurns"
                | "noAnswer"
                | "contextOverflow"
                | "imageUnsupported"
                | "unavailable"
                | "streamLost"
                | "network"
        )
    )
}

/// agent 主机时钟比本机快多少毫秒（本机收到时刻 − agent 生成该状态的时刻）；旧 agent 不带时钟
/// （`now_unix_ms == 0`）视为无偏差。
pub(crate) const fn clock_offset_ms(local_now_ms: i64, agent_now_ms: i64) -> i64 {
    if agent_now_ms == 0 {
        0
    } else {
        local_now_ms - agent_now_ms
    }
}

/// 把本机时刻换算成 agent 时钟下的时刻（与 `confirm_deadline_unix_ms` 同一时钟）。
pub(crate) const fn agent_now_ms(local_now_ms: i64, offset_ms: i64) -> i64 {
    local_now_ms - offset_ms
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::AssistantToolEffect;
    use serde_json::json;

    use super::*;

    /// 测试用翻译：`<键>` 加 `{参数=值}` 后缀，断言不依赖具体语言。
    fn fake_t(key: &str, args: &[(&str, &str)]) -> String {
        let mut out = format!("<{key}>");
        for (name, value) in args {
            out.push_str(&format!("{{{name}={value}}}"));
        }
        out
    }

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
            ..AssistantRunDto::default()
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
    fn transcript_keeps_full_tool_calls_and_results() {
        let long_result = format!("{}```tail", "x".repeat(OUTPUT_PREVIEW_CHARS + 50));
        let history = [
            message("m1", Some("r1"), 1, "user", "how fast?"),
            AssistantMessageDto {
                content: "{\"url\":\"https://a.example\"}".to_owned(),
                ..tool_message("m2", "r1", 2, "tool_call", "c1", "net_measure", false)
            },
            AssistantMessageDto {
                content: long_result.clone(),
                tool_kind: Some("server".to_owned()),
                ..tool_message("m3", "r1", 3, "tool_result", "c1", "net_measure", false)
            },
            message("m4", Some("r1"), 4, "assistant", "about 80 Mbps"),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        let text = turn_transcript(&turns[0], 0, &fake_t);
        assert!(text.starts_with("## <assistantTranscriptUser>\n\nhow fast?\n"));
        assert!(text.contains("`net_measure` · <assistantToolDone>"));
        assert!(text.contains("```json\n{\n  \"url\": \"https://a.example\"\n}\n```"));
        // 完整结果不截断；内容含 ``` 时围栏加长，不会提前闭合。
        assert!(text.contains(&format!("````\n{long_result}\n````")));
        assert!(text.trim_end().ends_with("about 80 Mbps"));
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
        assert!(matches!(tool_state(call, false), ToolState::Interrupted));
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

    fn offer_call(call_id: &str, status: AssistantToolStatus) -> AssistantRunItemDto {
        AssistantRunItemDto::Tool {
            call: AssistantToolCallDto {
                call_id: call_id.to_owned(),
                name: MEMBERSHIP_OFFER_TOOL.to_owned(),
                kind: "server".to_owned(),
                status,
                ..AssistantToolCallDto::default()
            },
        }
    }

    fn offer_message(id: &str, seq: i64, role: &str, is_error: bool) -> AssistantMessageDto {
        AssistantMessageDto {
            tool_kind: Some("server".to_owned()),
            ..tool_message(
                id,
                "r1",
                seq,
                role,
                "offer-r1",
                MEMBERSHIP_OFFER_TOOL,
                is_error,
            )
        }
    }

    #[test]
    fn live_membership_offer_needs_a_succeeded_call_and_never_enters_steps() {
        let turn_of = |status: AssistantToolStatus| {
            let runs = [run(
                "s1",
                Some("r1"),
                AssistantRunStatus::Running,
                vec![text("intro"), offer_call("offer-r1", status)],
            )];
            build_timeline("s1", &[], &[], &runs, &none()).remove(0)
        };
        let succeeded = turn_of(AssistantToolStatus::Succeeded);
        assert!(succeeded.membership_offer);
        assert_eq!(succeeded.tool_count(), 0);
        // 移除工具条目后，最后一个 Text 仍是回答。
        assert_eq!(succeeded.answer.as_deref(), Some("intro"));
        assert!(succeeded.steps.is_empty());
        for status in [AssistantToolStatus::Running, AssistantToolStatus::Failed] {
            let turn = turn_of(status);
            assert!(!turn.membership_offer);
            assert_eq!(turn.tool_count(), 0);
            assert!(turn.steps.is_empty());
            assert_eq!(turn.answer.as_deref(), Some("intro"));
        }
    }

    #[test]
    fn live_membership_offer_keeps_other_steps_and_answer_after_it() {
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Completed,
            vec![
                text("let me look"),
                AssistantRunItemDto::Tool {
                    call: pending_call(None),
                },
                offer_call("offer-r1", AssistantToolStatus::Succeeded),
                text("final"),
            ],
        )];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        assert!(turns[0].membership_offer);
        // 过程只剩前面的文本与另一个工具；回答是最后的文本。
        assert_eq!(turns[0].steps.len(), 2);
        assert_eq!(turns[0].tool_count(), 1);
        assert_eq!(turns[0].answer.as_deref(), Some("final"));
    }

    #[test]
    fn membership_offer_is_one_card_per_turn_and_any_success_counts() {
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Completed,
            vec![
                offer_call("c1", AssistantToolStatus::Failed),
                offer_call("c2", AssistantToolStatus::Succeeded),
                offer_call("c3", AssistantToolStatus::Succeeded),
                text("final"),
            ],
        )];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        assert_eq!(turns.len(), 1);
        assert!(turns[0].membership_offer);
        assert_eq!(turns[0].tool_count(), 0);
        assert!(turns[0].steps.is_empty());
        assert_eq!(turns[0].answer.as_deref(), Some("final"));
    }

    #[test]
    fn history_membership_offer_follows_the_tool_result() {
        let history = |is_error: bool| {
            [
                message("m1", Some("r1"), 1, "user", "how to upgrade?"),
                offer_message("m2", 2, "tool_call", false),
                offer_message("m3", 3, "tool_result", is_error),
                message("m4", Some("r1"), 4, "assistant", "here you go"),
            ]
        };
        let ok = build_timeline("s1", &history(false), &[], &[], &none());
        assert!(ok[0].membership_offer);
        assert_eq!(ok[0].tool_count(), 0);
        assert!(ok[0].steps.is_empty());
        assert_eq!(ok[0].answer.as_deref(), Some("here you go"));

        let failed = build_timeline("s1", &history(true), &[], &[], &none());
        assert!(!failed[0].membership_offer);
        assert_eq!(failed[0].tool_count(), 0);
        assert!(failed[0].steps.is_empty());
        assert_eq!(failed[0].answer.as_deref(), Some("here you go"));
    }

    #[test]
    fn history_membership_offer_without_result_shows_no_card() {
        let history = [
            message("m1", Some("r1"), 1, "user", "q"),
            offer_message("m2", 2, "tool_call", false),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert!(!turns[0].membership_offer);
        assert!(turns[0].steps.is_empty());
    }

    #[test]
    fn transcript_mentions_the_membership_card_after_the_answer() {
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Completed,
            vec![
                offer_call("offer-r1", AssistantToolStatus::Succeeded),
                text("final"),
            ],
        )];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        let out = turn_transcript(&turns[0], 0, &fake_t);
        assert!(!out.contains(MEMBERSHIP_OFFER_TOOL));
        let answer_at = out.find("final");
        let card_at = out.find("### <assistantMembershipTitle>");
        assert!(answer_at < card_at && card_at.is_some());
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
        assert_eq!(remaining_seconds(0), 0);
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

        // 回答文本出现不会把进行中的「过程」收起（否则中间文字后接工具调用时反复抖动）。
        let answering = build_timeline(
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
        assert!(steps_open(&answering[0], None, now));
        assert!(!steps_open(&answering[0], Some(false), now));

        // 执行结束后默认收起，用户展开的选择保留。
        let finished = build_timeline(
            "s1",
            &[],
            &[],
            &[run(
                "s1",
                Some("r1"),
                AssistantRunStatus::Completed,
                vec![
                    AssistantRunItemDto::Tool {
                        call: AssistantToolCallDto::default(),
                    },
                    text("answer"),
                ],
            )],
            &none(),
        );
        assert!(!steps_open(&finished[0], None, now));
        assert!(steps_open(&finished[0], Some(true), now));

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
        // 每分钟限频不是额度用尽：各走各的文案。
        assert_eq!(
            classify_send_error(
                &error(ApplicationErrorCode::Unavailable).with_reason(ErrorReason::RateLimited)
            ),
            SendFailure::RateLimited
        );
        assert_eq!(
            classify_send_error(
                &error(ApplicationErrorCode::Unavailable)
                    .with_reason(ErrorReason::AssistantQuotaExceeded)
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
        let ms = |n: &str| format!("<assistantUnitMilliseconds>{{n={n}}}");
        let s = |n: &str| format!("<assistantUnitSeconds>{{n={n}}}");
        assert_eq!(format_duration(0, &fake_t), ms("0"));
        assert_eq!(format_duration(850, &fake_t), ms("850"));
        assert_eq!(format_duration(1_000, &fake_t), s("1.0"));
        assert_eq!(format_duration(12_345, &fake_t), s("12.3"));
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

    #[test]
    fn assistant_steps_always_land_in_the_process() {
        let mut history = vec![
            message("m1", Some("r1"), 1, "user", "q"),
            message("m2", Some("r1"), 2, "assistant_step", "let me check"),
            tool_message("m3", "r1", 3, "tool_call", "c1", "net_measure", false),
            tool_message("m4", "r1", 4, "tool_result", "c1", "net_measure", false),
            message("m5", Some("r1"), 5, "assistant_step", "   "),
            message("m6", Some("r1"), 6, "assistant", "done"),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert_eq!(turns[0].answer.as_deref(), Some("done"));
        assert_eq!(turns[0].message_id.as_deref(), Some("m6"));
        // 空白的中间说明被丢弃；剩 Step + Tool，且顺序保持。
        assert_eq!(turns[0].steps.len(), 2);
        assert_eq!(turns[0].steps[0], Block::Step("let me check".to_owned()));
        assert!(matches!(turns[0].steps[1], Block::Tool(_)));

        // 最后一条是 assistant_step（没有最终回答）：它仍是过程，不会被当成回答，也不可评价。
        history.truncate(2);
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert_eq!(turns[0].answer, None);
        assert_eq!(turns[0].message_id, None);
        assert_eq!(turns[0].rateable_message(), None);
        assert_eq!(turns[0].steps, vec![Block::Step("let me check".to_owned())]);
    }

    #[test]
    fn transcript_includes_step_text_in_order() {
        let history = [
            message("m1", Some("r1"), 1, "user", "q"),
            message("m2", Some("r1"), 2, "assistant_step", "step text"),
            message("m3", Some("r1"), 3, "assistant", "final text"),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        let out = turn_transcript(&turns[0], 0, &fake_t);
        let step_at = out.find("step text");
        let answer_at = out.find("final text");
        assert!(step_at.is_some() && step_at < answer_at);
    }

    #[test]
    fn history_turn_carries_user_message_id_and_images() {
        let mut user = message("u1", Some("r1"), 1, "user", "look");
        user.images = vec!["att-1".to_owned(), "att-2".to_owned()];
        let history = [user, message("a1", Some("r1"), 2, "assistant", "ok")];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert_eq!(turns[0].user_message_id.as_deref(), Some("u1"));
        assert_eq!(turns[0].image_count, 2);
        assert_eq!(turns[0].image_ids, ["att-1", "att-2"]);

        // 实时轮次没有云端用户消息 ID，附图张数来自执行。
        let mut live = run("s1", None, AssistantRunStatus::Starting, Vec::new());
        live.image_count = 3;
        let turns = build_timeline("s1", &[], &[], &[live], &none());
        assert_eq!(turns[0].user_message_id, None);
        assert_eq!(turns[0].image_count, 3);
        assert!(turns[0].image_ids.is_empty());
    }

    #[test]
    fn live_turn_keeps_error_and_reason_and_drops_empty_ones() {
        let mut failed = run("s1", Some("r1"), AssistantRunStatus::Failed, Vec::new());
        failed.error = Some("boom".to_owned());
        failed.error_reason = Some("streamLost".to_owned());
        let turns = build_timeline("s1", &[], &[], &[failed.clone()], &none());
        let live = turns[0].live.as_ref();
        assert_eq!(live.and_then(|l| l.error.as_deref()), Some("boom"));
        assert_eq!(live.and_then(|l| l.reason.as_deref()), Some("streamLost"));

        failed.error = Some(String::new());
        failed.error_reason = Some(String::new());
        let turns = build_timeline("s1", &[], &[], &[failed], &none());
        let live = turns[0].live.as_ref();
        assert_eq!(live.and_then(|l| l.error.as_deref()), None);
        assert_eq!(live.and_then(|l| l.reason.as_deref()), None);
    }

    #[test]
    fn interrupted_answer_flag_comes_from_history_and_stopped_runs() {
        // 历史：assistant 消息自带 interrupted。
        let mut cut = message("a1", Some("r1"), 2, "assistant", "half an ans");
        cut.interrupted = true;
        let history = [message("u1", Some("r1"), 1, "user", "q"), cut];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert!(turns[0].answer_interrupted);
        let history = [
            message("u1", Some("r1"), 1, "user", "q"),
            message("a1", Some("r1"), 2, "assistant", "full answer"),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert!(!turns[0].answer_interrupted);

        // 实时：失败 / 取消且已有回答文本才算中断；正常结束、或没有任何文本都不是。
        let flag = |status, items: Vec<AssistantRunItemDto>| {
            let runs = [run("s1", Some("r1"), status, items)];
            build_timeline("s1", &[], &[], &runs, &none())[0].answer_interrupted
        };
        assert!(flag(AssistantRunStatus::Failed, vec![text("part")]));
        assert!(flag(AssistantRunStatus::Canceled, vec![text("part")]));
        assert!(!flag(AssistantRunStatus::Completed, vec![text("all")]));
        assert!(!flag(AssistantRunStatus::Running, vec![text("so far")]));
        assert!(!flag(AssistantRunStatus::Failed, Vec::new()));

        // 复制对话时带上「回答被中断」说明。
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Canceled,
            vec![text("part")],
        )];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        let out = turn_transcript(&turns[0], 0, &fake_t);
        assert!(out.contains("<assistantAnswerInterrupted>"));
        assert!(out.contains("<assistantStopped>"));
    }

    #[test]
    fn steps_open_does_not_jitter_while_the_run_is_active() {
        let now = 0;
        let call = || AssistantRunItemDto::Tool {
            call: AssistantToolCallDto::default(),
        };
        let turn = |status, items: Vec<AssistantRunItemDto>| {
            let runs = [run("s1", Some("r1"), status, items)];
            build_timeline("s1", &[], &[], &runs, &none()).remove(0)
        };
        // 进行中：中间文字、工具、再来文字，每个阶段展开状态都不变。
        let stages = [
            vec![text("plan")],
            vec![text("plan"), call()],
            vec![text("plan"), call(), text("partial answer")],
            vec![text("plan"), call(), text("partial answer"), call()],
        ];
        for items in stages {
            assert!(
                steps_open(&turn(AssistantRunStatus::Running, items), None, now),
                "running turn must stay open"
            );
        }
        // 结束后收起（含失败 / 取消）；用户的选择始终优先于默认值。
        for status in [
            AssistantRunStatus::Completed,
            AssistantRunStatus::Replied,
            AssistantRunStatus::Failed,
            AssistantRunStatus::Canceled,
        ] {
            let finished = turn(status, vec![call(), text("answer")]);
            assert!(!steps_open(&finished, None, now), "{status:?}");
            assert!(steps_open(&finished, Some(true), now), "{status:?}");
        }
        let running = turn(AssistantRunStatus::Running, vec![call()]);
        assert!(!steps_open(&running, Some(false), now));
        // 历史轮次（无实时状态）默认收起。
        let history = [
            message("u1", Some("r1"), 1, "user", "q"),
            tool_message("t1", "r1", 2, "tool_call", "c1", "x", false),
            message("a1", Some("r1"), 3, "assistant", "a"),
        ];
        let turns = build_timeline("s1", &history, &[], &[], &none());
        assert!(!steps_open(&turns[0], None, now));
    }

    #[test]
    fn tool_states_and_status_keys() {
        let call = |status, interrupted| AssistantToolCallDto {
            status,
            interrupted,
            ..AssistantToolCallDto::default()
        };
        assert!(matches!(
            tool_state(&call(AssistantToolStatus::Running, false), true),
            ToolState::Running
        ));
        assert!(matches!(
            tool_state(&call(AssistantToolStatus::Running, false), false),
            ToolState::Interrupted
        ));
        assert!(matches!(
            tool_state(&call(AssistantToolStatus::Succeeded, false), false),
            ToolState::Succeeded
        ));
        assert!(matches!(
            tool_state(&call(AssistantToolStatus::Failed, false), false),
            ToolState::Failed
        ));
        // 已批准的写操作被停止：不论状态与执行是否仍在进行，都是「结果未知」。
        for status in [
            AssistantToolStatus::Failed,
            AssistantToolStatus::Running,
            AssistantToolStatus::Succeeded,
        ] {
            for active in [true, false] {
                assert!(
                    matches!(
                        tool_state(&call(status, true), active),
                        ToolState::ResultUnknown
                    ),
                    "{status:?} active={active}"
                );
            }
        }

        let pending = Some(ConfirmView::Pending { remaining_ms: None });
        assert_eq!(
            tool_status_key(None, ToolState::ResultUnknown),
            "assistantToolResultUnknown"
        );
        assert_eq!(
            tool_status_key(Some(ConfirmView::Approved), ToolState::ResultUnknown),
            "assistantToolResultUnknown"
        );
        assert_eq!(
            tool_status_key(pending, ToolState::Running),
            "assistantToolWaiting"
        );
        assert_eq!(
            tool_status_key(Some(ConfirmView::Declined), ToolState::Failed),
            "assistantToolSkipped"
        );
        assert_eq!(
            tool_status_key(Some(ConfirmView::Expired), ToolState::Running),
            "assistantToolSkipped"
        );
        assert_eq!(
            tool_status_key(None, ToolState::Running),
            "assistantToolRunning"
        );
        assert_eq!(
            tool_status_key(None, ToolState::Succeeded),
            "assistantToolDone"
        );
        assert_eq!(
            tool_status_key(None, ToolState::Failed),
            "assistantToolFailed"
        );
        assert_eq!(
            tool_status_key(None, ToolState::Interrupted),
            "assistantToolInterrupted"
        );
    }

    #[test]
    fn interrupted_write_shows_result_unknown_in_the_transcript() {
        let runs = [run(
            "s1",
            Some("r1"),
            AssistantRunStatus::Canceled,
            vec![AssistantRunItemDto::Tool {
                call: AssistantToolCallDto {
                    status: AssistantToolStatus::Failed,
                    interrupted: true,
                    name: "tasks_delete".to_owned(),
                    ..AssistantToolCallDto::default()
                },
            }],
        )];
        let turns = build_timeline("s1", &[], &[], &runs, &none());
        let out = turn_transcript(&turns[0], 0, &fake_t);
        assert!(out.contains("`tasks_delete` · <assistantToolResultUnknown>"));
    }

    #[test]
    fn transcript_failed_run_uses_reason_text_and_raw_error() {
        let mut failed = run("s1", Some("r1"), AssistantRunStatus::Failed, Vec::new());
        failed.error = Some("  raw cloud error ".to_owned());
        failed.error_reason = Some("timeout".to_owned());
        let turns = build_timeline("s1", &[], &[], &[failed], &none());
        let out = turn_transcript(&turns[0], 0, &fake_t);
        assert!(out.contains("<assistantRunFailed>"));
        assert!(out.contains("<assistantRunErrTimeout>"));
        assert!(out.contains("\nraw cloud error\n"));
    }

    #[test]
    fn send_failures_follow_reason_before_code() {
        let error = |code| RpcErrorData::new(code, false);
        let reason = |code, reason| error(code).with_reason(reason);
        // 无 reason：按错误码。
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Unauthorized)),
            SendFailure::SignedOut
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Unavailable)),
            SendFailure::Unavailable
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Timeout)),
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
            classify_send_error(&error(ApplicationErrorCode::InvalidArgument)),
            SendFailure::Rejected
        );
        assert_eq!(
            classify_send_error(&error(ApplicationErrorCode::Internal)),
            SendFailure::Failed
        );
        // 额度用尽与限频是两回事，reason 压过错误码（含 Conflict / Unavailable）。
        assert_eq!(
            classify_send_error(&reason(
                ApplicationErrorCode::Unavailable,
                ErrorReason::AssistantQuotaExceeded
            )),
            SendFailure::QuotaExceeded
        );
        assert_eq!(
            classify_send_error(&reason(
                ApplicationErrorCode::Unavailable,
                ErrorReason::RateLimited
            )),
            SendFailure::RateLimited
        );
        assert_eq!(
            classify_send_error(&reason(
                ApplicationErrorCode::Conflict,
                ErrorReason::RateLimited
            )),
            SendFailure::RateLimited
        );
        assert_eq!(
            classify_send_error(&reason(
                ApplicationErrorCode::Internal,
                ErrorReason::SessionExpired
            )),
            SendFailure::SignedOut
        );
        assert_eq!(
            classify_send_error(&reason(
                ApplicationErrorCode::Internal,
                ErrorReason::CloudUnreachable
            )),
            SendFailure::Unavailable
        );
    }

    #[test]
    fn session_limit_reason_is_its_own_failure() {
        let error = RpcErrorData::new(ApplicationErrorCode::Conflict, false);
        // reason 压过 Conflict（否则会被归成 Busy）。
        assert_eq!(
            classify_send_error(
                &error
                    .clone()
                    .with_reason(ErrorReason::AssistantSessionLimit)
            ),
            SendFailure::SessionLimit
        );
        assert_eq!(classify_send_error(&error), SendFailure::Busy);
    }

    #[test]
    fn session_quota_follows_limit_and_used() {
        // 0 = 不限：无论用了多少都不变。
        assert_eq!(session_quota(0, 0), SessionQuota::Free);
        assert_eq!(session_quota(0, 999), SessionQuota::Free);
        // 剩余较多：不提示。
        assert_eq!(session_quota(10, 0), SessionQuota::Free);
        assert_eq!(session_quota(10, 6), SessionQuota::Free);
        // 剩余 3 / 2 / 1：提示。
        assert_eq!(session_quota(10, 7), SessionQuota::Low(3));
        assert_eq!(session_quota(10, 8), SessionQuota::Low(2));
        assert_eq!(session_quota(10, 9), SessionQuota::Low(1));
        // 剩余 0 与超出：已达上限。
        assert_eq!(session_quota(10, 10), SessionQuota::Reached);
        assert_eq!(session_quota(10, 11), SessionQuota::Reached);
        // 上限不超过提示阈值时一开始就提示。
        assert_eq!(session_quota(2, 0), SessionQuota::Low(2));
    }

    #[test]
    fn send_failure_keys_are_distinct_and_stable() {
        let all = [
            (SendFailure::SignedOut, "assistantErrSignedOut"),
            (SendFailure::NotOpen, "assistantErrNotOpen"),
            (SendFailure::QuotaExceeded, "assistantErrQuota"),
            (SendFailure::RateLimited, "assistantErrRateLimited"),
            (SendFailure::Busy, "assistantErrBusy"),
            (SendFailure::Unavailable, "assistantErrUnavailable"),
            (SendFailure::Rejected, "assistantErrRejected"),
            (SendFailure::Failed, "assistantErrFailed"),
            (SendFailure::SessionLimit, "assistantErrSessionLimit"),
        ];
        for (failure, key) in all {
            assert_eq!(failure.key(), key);
        }
        let mut keys: Vec<_> = all.iter().map(|(_, key)| *key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len());
    }

    #[test]
    fn action_errors_distinguish_expired_confirmations() {
        let base = RpcErrorData::new(ApplicationErrorCode::Conflict, false);
        assert_eq!(classify_action_error(&base), ActionError::Failed);
        assert_eq!(
            classify_action_error(&base.clone().with_reason(ErrorReason::RateLimited)),
            ActionError::Failed
        );
        let expired = base.with_reason(ErrorReason::AssistantConfirmExpired);
        assert_eq!(classify_action_error(&expired), ActionError::ConfirmExpired);
        assert_eq!(
            ActionError::ConfirmExpired.key(),
            "assistantErrConfirmExpired"
        );
        assert_eq!(ActionError::Failed.key(), "assistantActionFailed");
    }

    #[test]
    fn run_error_reasons_map_to_keys_and_know_themselves() {
        let known = [
            ("timeout", "assistantRunErrTimeout"),
            ("maxTurns", "assistantRunErrMaxTurns"),
            ("noAnswer", "assistantRunErrNoAnswer"),
            ("contextOverflow", "assistantRunErrContextOverflow"),
            ("imageUnsupported", "assistantRunErrImageUnsupported"),
            ("unavailable", "assistantRunErrUnavailable"),
            ("streamLost", "assistantRunErrStreamLost"),
            ("network", "assistantRunErrNetwork"),
        ];
        for (reason, key) in known {
            assert_eq!(run_error_key(Some(reason)), key);
            assert!(run_error_known(Some(reason)), "{reason}");
        }
        // `error` / `canceled`、未知值与缺省：通用文案，且界面要附带失败原文。
        for reason in [
            Some("error"),
            Some("canceled"),
            Some("Timeout"),
            Some(""),
            None,
        ] {
            assert_eq!(run_error_key(reason), "assistantRunErrError", "{reason:?}");
            assert!(!run_error_known(reason), "{reason:?}");
        }
    }

    #[test]
    fn clock_offset_corrects_confirmation_deadlines() {
        // 旧 agent 不带时钟：无偏差。
        assert_eq!(clock_offset_ms(1_000_000, 0), 0);
        // 本机比 agent 快 5 s：偏差为正；换算回 agent 时钟。
        let offset = clock_offset_ms(1_005_000, 1_000_000);
        assert_eq!(offset, 5_000);
        assert_eq!(agent_now_ms(1_005_000, offset), 1_000_000);
        // 本机比 agent 慢。
        let offset = clock_offset_ms(990_000, 1_000_000);
        assert_eq!(offset, -10_000);
        assert_eq!(agent_now_ms(990_000, offset), 1_000_000);
        assert_eq!(agent_now_ms(123, 0), 123);

        // 截止时间是 agent 时钟：不修正会把仍有效的确认当成过期。
        let local_now = 1_003_000;
        let corrected = agent_now_ms(local_now, 5_000);
        let call = pending_call(Some(1_000_500));
        assert_eq!(
            confirm_view(&call, true, corrected),
            Some(ConfirmView::Pending {
                remaining_ms: Some(2_500)
            })
        );
        assert_eq!(
            confirm_view(&call, true, local_now),
            Some(ConfirmView::Expired)
        );
    }

    #[test]
    fn units_and_countdown_use_the_translator() {
        assert_eq!(format_countdown(0, &fake_t), "<assistantUnitSeconds>{n=0}");
        assert_eq!(
            format_countdown(59, &fake_t),
            "<assistantUnitSeconds>{n=59}"
        );
        assert_eq!(format_countdown(-3, &fake_t), "<assistantUnitSeconds>{n=0}");
        // 一分钟以上用 M:SS，不依赖语言。
        assert_eq!(format_countdown(60, &fake_t), "1:00");
        assert_eq!(format_countdown(125, &fake_t), "2:05");
        assert_eq!(
            format_duration(-5, &fake_t),
            "<assistantUnitMilliseconds>{n=0}"
        );
        assert_eq!(
            format_duration(999, &fake_t),
            "<assistantUnitMilliseconds>{n=999}"
        );
        assert_eq!(
            format_duration(1_000, &fake_t),
            "<assistantUnitSeconds>{n=1.0}"
        );
        assert_eq!(
            format_duration(12_345, &fake_t),
            "<assistantUnitSeconds>{n=12.3}"
        );
    }

    #[test]
    fn pending_counts_use_the_agent_clock() {
        let item = || AssistantRunItemDto::Tool {
            call: pending_call(Some(1_000_500)),
        };
        let state = AssistantStateDto {
            runs: vec![run(
                "a",
                Some("r1"),
                AssistantRunStatus::Running,
                vec![item()],
            )],
            now_unix_ms: 1_000_000,
            ..AssistantStateDto::default()
        };
        let local_now = 1_003_000;
        let offset = clock_offset_ms(local_now, state.now_unix_ms);
        assert_eq!(
            pending_confirmations(&state, agent_now_ms(local_now, offset)),
            1
        );
        assert_eq!(pending_confirmations(&state, local_now), 0);
    }
}
