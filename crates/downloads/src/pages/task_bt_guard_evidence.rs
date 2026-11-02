//! 连接页「查看证据」对话框：该 IP 组的当前状态、名单条目（含证据摘要）与近期事件。
//!
//! 数据由宿主按 IP 组查询（`DownloadsCommand::BtGuardEvidence`），事件列表没有 IP 过滤，
//! 在这里按 IP 组筛选；匹配与字段展开见 [`crate::model::bt_guard`]。

use std::sync::Arc;

use chrono::{Local, TimeZone as _};
use fluxdown_protocol::{BtGuardEntryDto, BtGuardEventDto, BtPeerDto};
use fluxdown_ui_components::{
    ButtonVariant, ControlExt as _, dialog_scroll_body, dialog_title, field_error, loading_button,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight, IntoElement, ParentElement,
    Render, SharedString, Styled, Window, div, px,
};
use gpui_component::{WindowExt as _, button::Button, h_flex, v_flex};

use crate::{
    controller::{BtGuardEvidence, DownloadsCommand, DownloadsPort, DownloadsResult},
    model::bt_guard::{
        EvidenceLine, PeerGuard, event_detail_lines, evidence_lines, label_or_raw, tooltip_text,
    },
    strings::error_text,
};

/// 对话框内最多列出的近期事件数。
const MAX_EVENTS: usize = 30;

enum Load {
    Loading,
    Failed(SharedString),
    Ready(BtGuardEvidence),
}

struct EvidenceDialog {
    translator: Entity<Translator>,
    port: Arc<dyn DownloadsPort>,
    peer: BtPeerDto,
    group: String,
    load: Load,
    /// 请求代次：重试后旧响应作废。
    request: u64,
}

/// 打开某个连接所属 IP 组的证据对话框；没有 IP 组（防吸血未启用）时不做任何事。
pub(crate) fn open(
    translator: Entity<Translator>,
    port: Arc<dyn DownloadsPort>,
    peer: BtPeerDto,
    window: &mut Window,
    cx: &mut App,
) {
    let group = peer.ip_group.trim().to_owned();
    if group.is_empty() {
        return;
    }
    let title = SharedString::from(
        translator
            .read(cx)
            .text_with("btGuardEvidenceTitle", &[("group", &group)]),
    );
    let view = cx.new(|cx| {
        let mut dialog = EvidenceDialog {
            translator,
            port,
            peer,
            group,
            load: Load::Loading,
            request: 0,
        };
        dialog.fetch(cx);
        dialog
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(620.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
}

/// Unix 秒 → 本地时间 `YYYY-MM-DD HH:MM:SS`；0 / 非法值为 `—`。
fn format_time(secs: i64) -> String {
    if secs <= 0 {
        return "—".to_owned();
    }
    Local.timestamp_opt(secs, 0).single().map_or_else(
        || "—".to_owned(),
        |time| time.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

fn line_text(translator: &Translator, line: &EvidenceLine) -> String {
    let label = line.label_key.map_or_else(
        || line.raw_key.clone(),
        |key| translator.text(key).to_owned(),
    );
    format!("{label}: {}", line.value)
}

impl EvidenceDialog {
    fn fetch(&mut self, cx: &mut Context<Self>) {
        self.request = self.request.wrapping_add(1);
        let token = self.request;
        self.load = Load::Loading;
        let pending = self.port.execute(DownloadsCommand::BtGuardEvidence {
            ip_group: self.group.clone(),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this.request != token {
                    return;
                }
                this.load = match result {
                    Ok(DownloadsResult::BtGuardEvidence(evidence)) => Load::Ready(evidence),
                    Ok(_) => Load::Failed(SharedString::from(
                        this.translator
                            .read(cx)
                            .text("btGuardEvidenceFailed")
                            .to_owned(),
                    )),
                    Err(error) => Load::Failed(SharedString::from(error_text(
                        this.translator.read(cx),
                        &error,
                    ))),
                };
                cx.notify();
            }) else {
                // 对话框已关闭。
                return;
            };
        })
        .detach();
    }

    fn heading(&self, key: &str, cx: &App) -> Div {
        let theme = active_theme(cx);
        let extended = theme.extended();
        div()
            .text_size(extended.caption.size)
            .line_height(extended.caption.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(extended.colors.text_tertiary)
            .child(SharedString::from(
                self.translator.read(cx).text(key).to_owned(),
            ))
    }

    fn text_line(&self, text: impl Into<SharedString>, cx: &App) -> Div {
        let tokens = active_theme(cx).tokens();
        div()
            .min_w_0()
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
            .text_color(tokens.colors.foreground)
            .child(text.into())
    }

    fn muted_line(&self, text: impl Into<SharedString>, cx: &App) -> Div {
        let tokens = active_theme(cx).tokens();
        div()
            .min_w_0()
            .text_size(tokens.typography.xs.size)
            .line_height(tokens.typography.xs.line_height)
            .text_color(tokens.colors.muted_foreground)
            .child(text.into())
    }

    /// 当前状态：IP 组 / 累计上传 / 嫌疑 / 命中规则 / 处置。
    fn current_state(&self, cx: &App) -> Div {
        let translator = self.translator.read(cx);
        let tokens = active_theme(cx).tokens();
        let summary = match PeerGuard::of(&self.peer) {
            Some(guard) => tooltip_text(translator, &guard),
            None => format!("{}: {}", translator.text("btGuardPeerGroup"), self.group),
        };
        v_flex()
            .gap(tokens.spacing.xxs)
            .child(self.heading("btGuardEvidenceSummary", cx))
            .children(
                summary
                    .lines()
                    .map(|line| self.text_line(line.to_owned(), cx)),
            )
    }

    fn entry_block(&self, entry: &BtGuardEntryDto, cx: &App) -> Div {
        let translator = self.translator.read(cx);
        let tokens = active_theme(cx).tokens();
        let mut head = vec![
            entry.cidr.clone(),
            label_or_raw(translator, "btGuardLayer", &entry.layer),
            label_or_raw(translator, "btGuardAction", &entry.action),
        ];
        if !entry.rule.is_empty() {
            head.push(label_or_raw(translator, "btGuardRule", &entry.rule));
        }
        let mut block = v_flex()
            .gap(tokens.spacing.xxs)
            .child(self.text_line(head.join(" · "), cx));
        let reason = entry.reason.trim();
        if !reason.is_empty() {
            block = block.child(self.muted_line(reason.to_owned(), cx));
        }
        if let Some(evidence) = &entry.evidence {
            for line in evidence_lines(evidence) {
                block = block.child(self.muted_line(line_text(translator, &line), cx));
            }
        }
        block
    }

    fn event_block(&self, event: &BtGuardEventDto, cx: &App) -> Div {
        let translator = self.translator.read(cx);
        let tokens = active_theme(cx).tokens();
        let mut head = vec![
            format_time(event.at),
            label_or_raw(translator, "btGuardEventKind", &event.kind),
        ];
        if !event.rule.is_empty() {
            head.push(label_or_raw(translator, "btGuardRule", &event.rule));
        }
        if !event.mode.is_empty() {
            head.push(label_or_raw(translator, "btGuardMode", &event.mode));
        }
        let mut block = v_flex()
            .gap(tokens.spacing.xxs)
            .child(self.text_line(head.join(" · "), cx));
        for line in event_detail_lines(&event.detail) {
            block = block.child(self.muted_line(line_text(translator, &line), cx));
        }
        block
    }

    fn evidence_body(&self, evidence: &BtGuardEvidence, cx: &App) -> Div {
        let tokens = active_theme(cx).tokens();
        let (entries, events) = (&evidence.entries, &evidence.events);
        let mut column = v_flex()
            .gap(tokens.spacing.md)
            .child(self.current_state(cx));
        if entries.is_empty() && events.is_empty() {
            return column.child(
                self.muted_line(
                    self.translator
                        .read(cx)
                        .text("btGuardEvidenceNone")
                        .to_owned(),
                    cx,
                ),
            );
        }
        if !entries.is_empty() {
            column = column.child(
                v_flex()
                    .gap(tokens.spacing.sm)
                    .child(self.heading("btGuardEvidenceEntries", cx))
                    .children(entries.iter().map(|entry| self.entry_block(entry, cx))),
            );
        }
        if !events.is_empty() {
            column = column.child(
                v_flex()
                    .gap(tokens.spacing.sm)
                    .child(self.heading("btGuardEvidenceEvents", cx))
                    .children(
                        events
                            .iter()
                            .take(MAX_EVENTS)
                            .map(|event| self.event_block(event, cx)),
                    ),
            );
        }
        column
    }
}

impl Render for EvidenceDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let translator = self.translator.read(cx).clone();
        let body = match &self.load {
            Load::Loading => self
                .muted_line(translator.text("btGuardLoading").to_owned(), cx)
                .into_any_element(),
            Load::Failed(message) => v_flex()
                .gap(tokens.spacing.sm)
                .child(field_error(message.clone(), cx))
                .child(
                    h_flex().child(
                        loading_button(
                            "bt-guard-evidence-retry",
                            SharedString::from(translator.text("btGuardReload").to_owned()),
                            ButtonVariant::Secondary,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.fetch(cx))),
                    ),
                )
                .into_any_element(),
            Load::Ready(evidence) => self.evidence_body(evidence, cx).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body(
                "bt-guard-evidence-body",
                Some(px(520.)),
                body,
                cx,
            ))
            .child(
                h_flex().w_full().justify_end().child(
                    Button::new("bt-guard-evidence-close")
                        .outline()
                        .label(SharedString::from(translator.text("close").to_owned()))
                        .control(cx)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::format_time;

    #[test]
    fn times_render_as_dashes_when_unset() {
        assert_eq!(format_time(0), "—");
        assert_eq!(format_time(-1), "—");
        assert_eq!(
            format_time(1_700_000_000).len(),
            "2023-11-14 22:13:20".len()
        );
    }
}
