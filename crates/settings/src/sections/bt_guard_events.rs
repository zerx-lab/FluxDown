//! 事件日志页签：按规则 / 事件类型筛选的只读列表（最新在前），分页加载。
//!
//! 与封禁名单同构：保留视图观察 `bt_guard_revision` 与连接状态，变化后重新拉取已加载的条数。

use std::collections::HashSet;
use std::rc::Rc;

use fluxdown_protocol::{
    BT_GUARD_LIST_DEFAULT_LIMIT, BtGuardEventDto, BtGuardEventsListDto, RpcErrorData, method,
};
use fluxdown_ui_components::{
    ButtonVariant, FluxIcon, card, loading_button, tabular_numbers, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{h_flex, scroll::ScrollableElement as _, v_flex};
use serde_json::Value;

use super::SectionContext;
use super::bt_guard::{action_chip, label_or_raw};
use super::subscription::format_unix;
use crate::model::bt_guard::{
    EVENT_KINDS, EvidenceField, RULE_FILTERS, event_detail_fields, events_params, pick_static,
    reload_limit,
};
use crate::store::SettingsStore;
use crate::ui::{SettingsRow, SettingsSection, body_text, dropdown_button, empty_state, meta_text};

// 列宽（界面缩放前的 px，经 `text_extent` 伸缩）。
const COL_TIME: f32 = 132.;
const COL_KIND: f32 = 150.;
const COL_RULE: f32 = 116.;
const COL_MODE: f32 = 84.;
const COL_GROUP: f32 = 172.;
/// 表格最小宽度（界面缩放前的 px）；内容区更窄时整表水平滚动。
const MIN_TABLE_WIDTH: f32 = 860.;

pub(crate) fn section(ctx: &SectionContext) -> SettingsSection {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    SettingsSection::new().row(
        SettingsRow::custom(move |_, key, window: &mut Window, cx: &mut App| {
            let panel =
                window.use_keyed_state(SharedString::from(format!("{key}-bt-guard-events")), cx, {
                    let store = store.clone();
                    let translator = translator.clone();
                    move |_, cx| EventsPanel::new(store, translator, cx)
                });
            panel.update(cx, |panel, cx| panel.sync_translator(&translator, cx));
            panel.into_any_element()
        })
        .keywords([ctx.t("btGuardTabEvents"), ctx.t("btGuardKeywords")]),
    )
}

pub(crate) struct EventsPanel {
    store: Entity<SettingsStore>,
    translator: Translator,
    kind: Option<&'static str>,
    rule: Option<&'static str>,
    items: Vec<BtGuardEventDto>,
    total: u64,
    loading: bool,
    failed: bool,
    /// 最近一次请求的令牌：旧响应直接丢弃。
    request: u64,
    seen_revision: u64,
    connected: bool,
    _store_events: Subscription,
}

impl EventsPanel {
    fn new(store: Entity<SettingsStore>, translator: Translator, cx: &mut Context<Self>) -> Self {
        let store_events = cx.observe(&store, |this, store, cx| {
            let snapshot = store.read(cx);
            let connected = snapshot.daemon_connected();
            let revision = snapshot.bt_guard_revision();
            let changed = revision != this.seen_revision;
            let reconnected = connected && !this.connected;
            this.connected = connected;
            this.seen_revision = revision;
            if connected && (changed || reconnected) {
                this.reload(cx);
            }
            cx.notify();
        });
        let snapshot = store.read(cx);
        let connected = snapshot.daemon_connected();
        let seen_revision = snapshot.bt_guard_revision();
        let mut panel = Self {
            store,
            translator,
            kind: None,
            rule: None,
            items: Vec::new(),
            total: 0,
            loading: false,
            failed: false,
            request: 0,
            seen_revision,
            connected,
            _store_events: store_events,
        };
        if connected {
            panel.reload(cx);
        }
        panel
    }

    fn sync_translator(&mut self, translator: &Translator, cx: &mut Context<Self>) {
        if self.translator.locale() != translator.locale() {
            self.translator = translator.clone();
            cx.notify();
        }
    }

    fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.translator.text(key).to_owned())
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let limit = reload_limit(self.items.len());
        self.fetch(0, limit, false, cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let offset = u32::try_from(self.items.len()).unwrap_or(u32::MAX);
        self.fetch(offset, BT_GUARD_LIST_DEFAULT_LIMIT, true, cx);
    }

    fn fetch(&mut self, offset: u32, limit: u32, append: bool, cx: &mut Context<Self>) {
        self.request = self.request.wrapping_add(1);
        let token = self.request;
        let params = events_params(self.kind, self.rule, offset, limit);
        let Ok(params) = serde_json::to_value(&params) else {
            self.failed = true;
            cx.notify();
            return;
        };
        let future = self
            .store
            .read(cx)
            .raw_call(method::DAEMON_BT_GUARD_EVENTS_LIST, params);
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| this.finish_fetch(token, append, result, cx))
            else {
                // 面板已释放。
                return;
            };
        })
        .detach();
    }

    fn finish_fetch(
        &mut self,
        token: u64,
        append: bool,
        result: Result<Value, RpcErrorData>,
        cx: &mut Context<Self>,
    ) {
        if token != self.request {
            return;
        }
        self.loading = false;
        let page = result
            .ok()
            .and_then(|value| serde_json::from_value::<BtGuardEventsListDto>(value).ok());
        match page {
            Some(page) => {
                self.failed = false;
                self.total = page.total;
                if append {
                    let known: HashSet<i64> = self.items.iter().map(|item| item.id).collect();
                    self.items.extend(
                        page.items
                            .into_iter()
                            .filter(|item| !known.contains(&item.id)),
                    );
                } else {
                    self.items = page.items;
                }
            }
            None => self.failed = true,
        }
        cx.notify();
    }

    fn set_kind(&mut self, value: &str, cx: &mut Context<Self>) {
        let kind = pick_static(EVENT_KINDS, value);
        if kind != self.kind {
            self.kind = kind;
            self.items.clear();
            self.reload(cx);
        }
    }

    fn set_rule(&mut self, value: &str, cx: &mut Context<Self>) {
        let rule = pick_static(RULE_FILTERS, value);
        if rule != self.rule {
            self.rule = rule;
            self.items.clear();
            self.reload(cx);
        }
    }

    fn filter_dropdown(
        &self,
        id: &'static str,
        options: Vec<(SharedString, SharedString)>,
        current: Option<&'static str>,
        apply: fn(&mut Self, &str, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        dropdown_button(
            id,
            &options,
            SharedString::from(current.unwrap_or_default()),
            false,
            false,
            Rc::new(move |value: SharedString, cx: &mut App| {
                view.update(cx, |this, cx| apply(this, &value, cx));
            }),
            cx,
        )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let tokens = active_theme(cx).tokens();
        let mut kinds = vec![(SharedString::default(), self.t("btGuardEventKindAll"))];
        kinds.extend(EVENT_KINDS.iter().map(|kind| {
            (
                SharedString::from(*kind),
                label_or_raw(&self.translator, "btGuardEventKind", kind),
            )
        }));
        let mut rules = vec![(SharedString::default(), self.t("btGuardRuleAll"))];
        rules.extend(RULE_FILTERS.iter().map(|rule| {
            (
                SharedString::from(*rule),
                label_or_raw(&self.translator, "btGuardRule", rule),
            )
        }));
        h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(self.filter_dropdown(
                "bt-guard-event-kind",
                kinds,
                self.kind,
                Self::set_kind,
                cx,
            ))
            .child(self.filter_dropdown(
                "bt-guard-event-rule",
                rules,
                self.rule,
                Self::set_rule,
                cx,
            ))
    }

    fn field_line(&self, field: &EvidenceField) -> String {
        let label = field.label_key.map_or_else(
            || field.raw_key.clone(),
            |key| self.translator.text(key).to_owned(),
        );
        format!("{label}: {}", field.value.text())
    }

    /// 一行摘要（行内）与完整多行说明（悬停）。
    fn detail_texts(&self, event: &BtGuardEventDto) -> (SharedString, SharedString) {
        let fields = event_detail_fields(&event.detail);
        let inline = fields
            .iter()
            .map(|field| self.field_line(field))
            .collect::<Vec<_>>()
            .join(" · ");
        let mut tooltip: Vec<String> = Vec::new();
        if !event.info_hash.is_empty() {
            tooltip.push(format!(
                "{}: {}",
                self.translator.text("btGuardEvInfoHash"),
                event.info_hash
            ));
        }
        tooltip.extend(fields.iter().map(|field| self.field_line(field)));
        (
            SharedString::from(inline),
            SharedString::from(tooltip.join("\n")),
        )
    }

    fn event_row(
        &self,
        index: usize,
        event: &BtGuardEventDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let (inline, tooltip) = self.detail_texts(event);
        let time = if event.at > 0 {
            SharedString::from(format_unix(event.at))
        } else {
            SharedString::from("—")
        };
        let kind = label_or_raw(&self.translator, "btGuardEventKind", &event.kind);
        let rule = if event.rule.is_empty() {
            SharedString::from("—")
        } else {
            label_or_raw(&self.translator, "btGuardRule", &event.rule)
        };
        let mode = (!event.mode.is_empty())
            .then(|| label_or_raw(&self.translator, "btGuardMode", &event.mode));
        let group = SharedString::from(event.ip_group.clone());
        let has_tooltip = !tooltip.is_empty();
        h_flex()
            .id(("bt-guard-event", index))
            .w_full()
            .items_center()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs)
            .border_b_1()
            .border_color(extended.colors.hairline)
            .hover(|style| style.bg(extended.colors.row_hover))
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
            .text_color(tokens.colors.foreground)
            .font_features(tabular_numbers())
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_TIME))
                    .truncate()
                    .text_color(tokens.colors.muted_foreground)
                    .child(time),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_KIND))
                    .truncate()
                    .child(kind),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_RULE))
                    .truncate()
                    .text_color(tokens.colors.muted_foreground)
                    .child(rule),
            )
            .child(
                div().flex_none().w(theme.text_extent(COL_MODE)).child(
                    h_flex().children(mode.map(|label| action_chip(&event.mode, label, cx))),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_GROUP))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .font_family(tokens.typography.mono.clone())
                    .child(group),
            )
            .child(
                div()
                    .id(("bt-guard-event-detail", index))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(tokens.colors.muted_foreground)
                    .when(has_tooltip, |cell| {
                        cell.tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                    })
                    .child(inline),
            )
            .into_any_element()
    }

    fn header(&self, cx: &App) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let cell = |label: SharedString, width: f32| {
            div()
                .flex_none()
                .w(theme.text_extent(width))
                .truncate()
                .child(label)
        };
        h_flex()
            .w_full()
            .items_center()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.sm)
            .py(tokens.spacing.xs)
            .border_b_1()
            .border_color(extended.colors.hairline)
            .text_size(extended.caption.size)
            .line_height(extended.caption.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(extended.colors.text_tertiary)
            .child(cell(self.t("btGuardColTime"), COL_TIME))
            .child(cell(self.t("btGuardColKind"), COL_KIND))
            .child(cell(self.t("btGuardColRule"), COL_RULE))
            .child(cell(self.t("btGuardColMode"), COL_MODE))
            .child(cell(self.t("btGuardPeerGroup"), COL_GROUP))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.t("btGuardColDetail")),
            )
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = active_theme(cx).tokens().clone();
        if self.items.is_empty() {
            if self.loading {
                return meta_text(cx)
                    .child(self.t("btGuardLoading"))
                    .into_any_element();
            }
            if self.failed {
                return v_flex()
                    .w_full()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .child(meta_text(cx).child(self.t("btGuardLoadFailed")))
                    .child(
                        loading_button(
                            "bt-guard-events-reload",
                            self.t("btGuardReload"),
                            ButtonVariant::Secondary,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload(cx))),
                    )
                    .into_any_element();
            }
            let key = if self.kind.is_some() || self.rule.is_some() {
                "btGuardEventsFilterEmpty"
            } else {
                "btGuardEventsEmpty"
            };
            return empty_state(FluxIcon::Shield, self.t(key), SharedString::default(), cx)
                .into_any_element();
        }
        let mut table = card(cx)
            .flex()
            .flex_col()
            .w_full()
            .min_w(active_theme(cx).text_extent(MIN_TABLE_WIDTH))
            .overflow_hidden();
        table = table.child(self.header(cx));
        for (index, event) in self.items.iter().enumerate() {
            table = table.child(self.event_row(index, event, cx));
        }
        let loaded = self.items.len();
        let more = u64::try_from(loaded).unwrap_or(u64::MAX) < self.total;
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                div()
                    .id("bt-guard-events-scroll")
                    .w_full()
                    .overflow_x_scrollbar()
                    .child(table),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(meta_text(cx).child(SharedString::from(
                        self.translator.text_with(
                            "btGuardEntriesTotal",
                            &[("total", &self.total.to_string())],
                        ),
                    )))
                    .when(more, |footer| {
                        footer.child(
                            loading_button(
                                "bt-guard-events-more",
                                SharedString::from(self.translator.text_with(
                                    "btGuardLoadMore",
                                    &[
                                        ("shown", &loaded.to_string()),
                                        ("total", &self.total.to_string()),
                                    ],
                                )),
                                ButtonVariant::Secondary,
                                self.loading,
                                cx,
                            )
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.load_more(cx)),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Render for EventsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(self.toolbar(cx))
            .child(self.body(cx))
            .when(self.failed && !self.items.is_empty(), |column| {
                column.child(
                    body_text(cx)
                        .text_color(tokens.colors.destructive)
                        .child(self.t("btGuardLoadFailed")),
                )
            })
    }
}
