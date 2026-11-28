//! 提问流程：发送、停止、重新生成与编辑重发。
//!
//! 发送在途时用户可以切会话 / 继续输入：回写只在「发起时的会话仍是当前会话」时做，输入框只在文本
//! 与发送时一致时清空。提问被 agent 推送为实时执行之前，用 [`Outgoing`] 在本地显示提问气泡与停止按钮。

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use fluxdown_protocol::{
    AssistantAskParams, AssistantAskResult, AssistantChannel, AssistantMessageDto, RpcErrorData,
};
use fluxdown_ui_components::toast;
use gpui::{Context, Image, Window};
use gpui_component::WindowExt as _;

use crate::{
    draft::{Attachment, SentImages},
    history::Load,
    model::{Gate, SendFailure, classify_send_error},
    view::AssistantView,
};

/// 实时执行的起始时间与本地发送时间比较时容许的时钟误差。
const RUN_START_SLACK_MS: i64 = 5_000;
/// 提问被接受后仍没等到对应的实时执行推送时，本地气泡的保留时长。
const OUTGOING_TIMEOUT: Duration = Duration::from_secs(15);
/// 被限频后本地暂停发送的时长：云端只返回「限频」不带重试时刻，按固定冷却处理。
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(10);

/// 已发出、尚未由实时执行接管显示的提问：本地气泡 + 可用的停止按钮。
pub(crate) struct Outgoing {
    id: u64,
    /// 发起时的会话；新对话在提问被接受前为 `None`。
    pub(crate) session_id: Option<String>,
    pub(crate) text: String,
    pub(crate) thumbnails: Vec<Arc<Image>>,
    pub(crate) image_count: usize,
    /// 提问已被 agent 接受（`ask` 已返回）。
    pub(crate) accepted: bool,
    /// 用户在提问被接受前点了停止：接受后立即取消。
    stop_requested: bool,
    /// 发送时刻（agent 时钟），与实时执行的 `started_at_unix_ms` 比较。
    sent_at_agent_ms: i64,
}

/// 正在编辑重发的历史提问：输入框里是被编辑的文本，原草稿暂存，取消或发完后还原。
pub(crate) struct Editing {
    pub(crate) user_message_id: String,
    pub(crate) session_id: String,
    /// 被编辑提问的附图张数；用户没附新图时随 `keepImages` 沿用。
    pub(crate) image_count: usize,
    stash_text: String,
    stash_attachments: Vec<Attachment>,
}

/// 一次提问请求。
struct Submission {
    text: String,
    images: Vec<String>,
    thumbnails: Vec<Arc<Image>>,
    image_count: usize,
    replace_from: Option<String>,
    keep_images: bool,
    /// 来自输入框：提问被接受后若输入框文本仍是它就清空。
    input_snapshot: Option<String>,
}

/// 提问返回时回调需要的上下文。
struct SubmitDone {
    id: u64,
    /// 发起时的会话。
    started: Option<String>,
    text: String,
    thumbnails: Vec<Arc<Image>>,
    snapshot: Option<String>,
    replaced: Option<String>,
}

/// 丢掉 `from_id` 这条消息及之后的全部历史（云端已把它们标为已替换）。
fn truncate_from(messages: &mut Vec<AssistantMessageDto>, from_id: &str) {
    let Some(seq) = messages
        .iter()
        .find(|message| message.id == from_id)
        .map(|message| message.seq)
    else {
        return;
    };
    messages.retain(|message| message.seq < seq);
}

impl AssistantView {
    /// 提问请求在途（还没被 agent 接受）。
    pub(crate) fn sending(&self) -> bool {
        self.outgoing
            .as_ref()
            .is_some_and(|outgoing| !outgoing.accepted)
    }

    /// 对应的实时执行是否已出现在状态里（出现后由实时轮次显示提问）。
    fn run_started_since(&self, outgoing: &Outgoing) -> bool {
        let Some(session_id) = outgoing.session_id.as_deref() else {
            return false;
        };
        self.state.runs.iter().any(|run| {
            run.session_id == session_id
                && run.started_at_unix_ms + RUN_START_SLACK_MS >= outgoing.sent_at_agent_ms
        })
    }

    /// 需要在当前对话里显示的本地提问气泡。
    pub(crate) fn visible_outgoing(&self) -> Option<&Outgoing> {
        self.outgoing.as_ref().filter(|outgoing| {
            outgoing.session_id == self.current && !self.run_started_since(outgoing)
        })
    }

    /// 实时执行出现后收起本地气泡。
    pub(crate) fn settle_outgoing(&mut self) {
        let Some(outgoing) = self.outgoing.as_ref() else {
            return;
        };
        if outgoing.accepted && self.run_started_since(outgoing) {
            self.outgoing = None;
        }
    }

    pub(crate) fn can_send(&self, cx: &gpui::App) -> bool {
        let Gate::Open {
            quota_exhausted: false,
        } = self.gate()
        else {
            return false;
        };
        if self.stale
            || self.session_limit_reached()
            || self.outgoing.is_some()
            || self.send_cooldown_until.is_some()
            || self.attaching > 0
            || self.current_run().is_some()
        {
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
        let snapshot = self.input.read(cx).value().to_string();
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
        // 编辑重发只在仍停留在被编辑提问所在的会话时有效。
        let edit = self
            .editing
            .as_ref()
            .filter(|editing| self.current.as_deref() == Some(editing.session_id.as_str()))
            .map(|editing| (editing.user_message_id.clone(), editing.image_count));
        let (replace_from, keep_images, image_count) = match edit {
            Some((id, old_images)) if images.is_empty() && old_images > 0 => {
                (Some(id), true, old_images)
            }
            Some((id, _)) => (Some(id), false, images.len()),
            None => (None, false, images.len()),
        };
        self.submit(
            Submission {
                text,
                images,
                thumbnails,
                image_count,
                replace_from,
                keep_images,
                input_snapshot: Some(snapshot),
            },
            window,
            cx,
        );
    }

    fn submit(&mut self, sub: Submission, window: &mut Window, cx: &mut Context<Self>) {
        let Submission {
            text,
            images,
            thumbnails,
            image_count,
            replace_from,
            keep_images,
            input_snapshot,
        } = sub;
        let started = self.current.clone();
        self.outgoing_gen = self.outgoing_gen.wrapping_add(1);
        let id = self.outgoing_gen;
        self.outgoing = Some(Outgoing {
            id,
            session_id: started.clone(),
            text: text.clone(),
            thumbnails: thumbnails.clone(),
            image_count,
            accepted: false,
            stop_requested: false,
            sent_at_agent_ms: self.agent_now(),
        });
        // 上一次的失败提示保留到这次有结果：发送期间提示行不消失，输入区不跳动。
        self.action_error = None;
        self.scroll.scroll_to_bottom();
        let params = AssistantAskParams {
            session_id: started.clone(),
            kind: self.current_kind(),
            channel: AssistantChannel::Desktop,
            text: text.clone(),
            images,
            locale: Some(self.translator.read(cx).locale().to_owned()),
            replace_from: replace_from.clone(),
            keep_images,
        };
        let future = self.port.ask(params);
        let done = SubmitDone {
            id,
            started,
            text,
            thumbnails,
            snapshot: input_snapshot,
            replaced: replace_from,
        };
        self.run_in(window, future, cx, move |this, result, window, cx| {
            this.finish_submit(done, result, window, cx);
        });
        cx.notify();
    }

    fn finish_submit(
        &mut self,
        done: SubmitDone,
        result: Result<AssistantAskResult, RpcErrorData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tracked = self
            .outgoing
            .as_ref()
            .is_some_and(|outgoing| outgoing.id == done.id);
        let same_view = self.current == done.started;
        match result {
            Ok(answer) => {
                let stop_requested = self
                    .outgoing
                    .as_ref()
                    .is_some_and(|outgoing| outgoing.id == done.id && outgoing.stop_requested);
                if same_view {
                    self.send_failure = None;
                    if done.started.is_none() {
                        self.current = Some(answer.session_id.clone());
                        // 新会话在云端还没有任何已落库的历史：直接以空历史起步，
                        // 提问内容由推送的实时执行显示。
                        self.reset_detail(Load::Ready);
                    }
                    if let Some(outgoing) = self
                        .outgoing
                        .as_mut()
                        .filter(|outgoing| outgoing.id == done.id)
                    {
                        outgoing.session_id = Some(answer.session_id.clone());
                        outgoing.accepted = true;
                    }
                    // 本地缩略图记在这次提问所属的会话下，气泡据此显示。
                    self.sent_images = (!done.thumbnails.is_empty()).then(|| SentImages {
                        session_id: answer.session_id.clone(),
                        text: done.text.clone(),
                        thumbnails: done.thumbnails.clone(),
                    });
                    if let Some(from) = &done.replaced {
                        truncate_from(&mut self.detail.messages, from);
                    }
                    self.scroll.scroll_to_bottom();
                } else if tracked {
                    // 用户已经去了别的会话：这次执行在后台继续，不动当前页面。
                    self.outgoing = None;
                }
                if let Some(snapshot) = &done.snapshot {
                    self.clear_sent_draft(snapshot, window, cx);
                }
                self.settle_outgoing();
                self.rebuild_timeline(cx);
                self.expire_outgoing(done.id, cx);
                self.reload_sessions(cx);
                // 今日用量在云端随提问增长：重新取配置，顶栏用量与额度卡片才会跟上。
                self.fetch_config(true, cx);
                if stop_requested {
                    self.cancel_session(answer.session_id, cx);
                }
            }
            Err(error) => {
                if tracked {
                    self.outgoing = None;
                    self.cancelling = false;
                }
                let failure = classify_send_error(&error);
                log::warn!("assistant ask failed: {:?} ({failure:?})", error.code);
                if same_view {
                    self.send_failure = Some(failure);
                } else {
                    let message = self.t(failure.key(), cx);
                    window.push_notification(toast::error(message), cx);
                }
                // 额度 / 开放状态可能已变：重新取配置让页面门禁跟上；限频只是稍后再试，无需重取。
                if matches!(failure, SendFailure::QuotaExceeded | SendFailure::NotOpen) {
                    self.fetch_config(true, cx);
                }
                if failure == SendFailure::RateLimited {
                    self.start_send_cooldown(cx);
                }
                // 本会话提问数已满：本地视为已达上限，输入区换成提示卡（输入框内容保留）。
                if same_view && failure == SendFailure::SessionLimit {
                    self.mark_session_limit_reached();
                }
            }
        }
        cx.notify();
    }

    /// 限频冷却：到点恢复发送（期间再次限频会把截止时刻往后推，旧计时器到点不提前解除）。
    fn start_send_cooldown(&mut self, cx: &mut Context<Self>) {
        self.send_cooldown_until = Some(Instant::now() + RATE_LIMIT_COOLDOWN);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RATE_LIMIT_COOLDOWN).await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this
                    .send_cooldown_until
                    .is_some_and(|until| until <= Instant::now())
                {
                    this.send_cooldown_until = None;
                    cx.notify();
                }
            }) else {
                return;
            };
        })
        .detach();
    }

    /// 提问发出后输入框的收尾：文本仍与发送时一致才清空（用户发送期间新输入的、⌘K 带进来的
    /// 文字都要保留）；编辑重发在此还原被暂存的原草稿。
    fn clear_sent_draft(&mut self, snapshot: &str, window: &mut Window, cx: &mut Context<Self>) {
        let editing = self.editing.take();
        if self.input.read(cx).value().as_ref() != snapshot {
            return;
        }
        self.attachments.clear();
        let (text, attachments) = editing.map_or_else(
            || (String::new(), Vec::new()),
            |editing| (editing.stash_text, editing.stash_attachments),
        );
        self.attachments = attachments;
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.sync_input_text(cx);
    }

    /// 提问被接受后一直没等到对应的实时执行：到点收起本地气泡。
    fn expire_outgoing(&mut self, id: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(OUTGOING_TIMEOUT).await;
            let Ok(()) = this.update(cx, move |this, cx| {
                if this
                    .outgoing
                    .as_ref()
                    .is_some_and(|outgoing| outgoing.id == id && outgoing.accepted)
                {
                    this.outgoing = None;
                    cx.notify();
                }
            }) else {
                // 助手页已释放，结束超时回调。
                return;
            };
        })
        .detach();
    }

    // ───────────────────────── 停止 ─────────────────────────

    pub(crate) fn can_stop(&self) -> bool {
        !self.stale
            && !self.cancelling
            && (self.current_run().is_some() || self.visible_outgoing().is_some())
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.can_stop() {
            return;
        }
        if let Some(session_id) = self.current_run().map(|run| run.session_id.clone()) {
            self.cancel_session(session_id, cx);
            return;
        }
        let Some(outgoing) = self.outgoing.as_mut() else {
            return;
        };
        match (outgoing.accepted, outgoing.session_id.clone()) {
            (true, Some(session_id)) => self.cancel_session(session_id, cx),
            _ => {
                // 提问还在途中，没有可取消的执行：接受后立即取消。
                outgoing.stop_requested = true;
                self.cancelling = true;
                cx.notify();
            }
        }
    }

    fn cancel_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.cancelling = true;
        let future = self.port.cancel(session_id);
        self.run(future, cx, |this, result, cx| {
            this.cancelling = false;
            if let Err(error) = result {
                this.report_failure("cancel", &error, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    // ───────────────────────── 重新生成 / 编辑重发 ─────────────────────────

    /// 现在能否替换历史提问重新作答：可以提问、没有进行中的执行、与 agent 保持连接。
    pub(crate) fn can_resend(&self) -> bool {
        matches!(
            self.gate(),
            Gate::Open {
                quota_exhausted: false
            }
        ) && !self.stale
            && self.current.is_some()
            && self.outgoing.is_none()
            && self.send_cooldown_until.is_none()
            && self.current_run().is_none()
            && !self.session_limit_reached()
    }

    /// 重新生成最后一轮的回答：以原提问（沿用原附图）替换它。
    pub(crate) fn regenerate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_resend() {
            return;
        }
        let Some(turn) = self.timeline.last().filter(|turn| !turn.run_active()) else {
            return;
        };
        let Some(message_id) = turn.user_message_id.clone() else {
            return;
        };
        let sub = Submission {
            text: turn.question.trim().to_owned(),
            images: Vec::new(),
            thumbnails: Vec::new(),
            image_count: turn.image_count,
            replace_from: Some(message_id),
            keep_images: turn.image_count > 0,
            input_snapshot: None,
        };
        if sub.text.is_empty() {
            return;
        }
        self.submit(sub, window, cx);
    }

    /// 把一条历史提问载入输入框重新编辑；发送时替换它及其后的全部对话。
    pub(crate) fn start_edit(
        &mut self,
        turn_key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_resend() {
            return;
        }
        let Some(turn) = self.timeline.iter().find(|turn| turn.key == turn_key) else {
            return;
        };
        let (Some(message_id), Some(session_id)) =
            (turn.user_message_id.clone(), self.current.clone())
        else {
            return;
        };
        let question = turn.question.clone();
        let image_count = turn.image_count;
        let (stash_text, stash_attachments) = match self.editing.take() {
            // 已在编辑别的提问：保留最早暂存的原草稿，编辑中新附的图作废。
            Some(previous) => {
                self.attachments.clear();
                (previous.stash_text, previous.stash_attachments)
            }
            None => (
                self.input.read(cx).value().to_string(),
                std::mem::take(&mut self.attachments),
            ),
        };
        self.editing = Some(Editing {
            user_message_id: message_id,
            session_id,
            image_count,
            stash_text,
            stash_attachments,
        });
        self.send_failure = None;
        self.input
            .update(cx, |input, cx| input.set_value(question, window, cx));
        self.sync_input_text(cx);
        self.focus_input(window, cx);
        cx.notify();
    }

    /// 放弃编辑：还原编辑前的草稿（文本与附件）。
    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.attachments = editing.stash_attachments;
        self.input.update(cx, |input, cx| {
            input.set_value(editing.stash_text, window, cx)
        });
        self.sync_input_text(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, seq: i64) -> AssistantMessageDto {
        AssistantMessageDto {
            id: id.to_owned(),
            seq,
            ..AssistantMessageDto::default()
        }
    }

    #[test]
    fn replaced_history_is_cut_at_the_edited_question() {
        let mut messages = vec![
            message("u1", 1),
            message("a1", 2),
            message("u2", 3),
            message("a2", 4),
        ];
        truncate_from(&mut messages, "u2");
        let ids: Vec<_> = messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["u1", "a1"]);
    }

    #[test]
    fn unknown_replace_target_keeps_history() {
        let mut messages = vec![message("u1", 1), message("a1", 2)];
        truncate_from(&mut messages, "missing");
        assert_eq!(messages.len(), 2);
    }
}
