//! 封禁名单页签：分层 / 规则筛选、搜索、剩余时长、批量解除、手动添加、加白与导出。
//!
//! 列表是一个保留视图（[`BansPanel`]）：自带筛选 / 选择 / 分页状态，观察 [`SettingsStore`] 的
//! `bt_guard_revision`（`BtGuardChanged` 事件或本地动作后递增）与连接状态来重新拉取。

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use fluxdown_protocol::{
    BT_GUARD_LAYERS, BtGuardEntriesAddParams, BtGuardEntriesListDto, BtGuardEntriesRemoveDto,
    BtGuardEntryDto, BtGuardExportDto, RpcErrorData, method,
};
use fluxdown_ui_components::{
    ButtonVariant, CheckState, ControlExt as _, FluxIcon, card, check_mark, loading_button,
    tabular_numbers, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    scroll::ScrollableElement as _,
    v_flex,
};
use serde_json::{Value, json};

use super::bt_guard::{action_chip, label_or_raw};
use super::subscription::format_unix;
use super::{SectionContext, bt_guard_add_dialog};
use crate::model::bt_guard::{
    CLOUD_LAYER, DurationChoice, EvidenceField, RULE_FILTERS, duration_units, entries_params,
    evidence_fields, live_remaining, pick_static, reload_limit, removable_ids, retain_selected,
};
use crate::port::PortFuture;
use crate::store::{SettingsStore, rpc_error_text};
use crate::ui::{
    SettingsRow, SettingsSection, body_text, dropdown_button, empty_state, meta_text, row_button,
    row_loading_button,
};

/// 搜索框输入后等待多久再请求，避免逐字符打 RPC。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// 导出默认文件名。
const EXPORT_FILE_NAME: &str = "fluxdown-bt-guard.txt";
/// 表格最小宽度（界面缩放前的 px）；内容区更窄时整表水平滚动，操作列不会被裁掉。
const MIN_TABLE_WIDTH: f32 = 800.;

// 列宽（界面缩放前的 px，经 `text_extent` 伸缩）。
const COL_CHECK: f32 = 22.;
const COL_ADDRESS: f32 = 172.;
const COL_LAYER: f32 = 84.;
const COL_ACTION: f32 = 72.;
const COL_RULE: f32 = 116.;
const COL_REMAINING: f32 = 96.;

pub(crate) fn section(ctx: &SectionContext) -> SettingsSection {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    SettingsSection::new().row(
        SettingsRow::custom(move |_, key, window: &mut Window, cx: &mut App| {
            let panel =
                window.use_keyed_state(SharedString::from(format!("{key}-bt-guard-bans")), cx, {
                    let store = store.clone();
                    let translator = translator.clone();
                    move |window, cx| BansPanel::new(store, translator, window, cx)
                });
            panel.update(cx, |panel, cx| panel.sync_translator(&translator, cx));
            panel.into_any_element()
        })
        .keywords([ctx.t("btGuardTabBans"), ctx.t("btGuardKeywords")]),
    )
}

pub(crate) struct BansPanel {
    store: Entity<SettingsStore>,
    translator: Translator,
    layer: Option<&'static str>,
    rule: Option<&'static str>,
    query: Entity<InputState>,
    items: Vec<BtGuardEntryDto>,
    total: u64,
    fetched_at: Instant,
    loading: bool,
    failed: bool,
    /// 最近一次请求的令牌：旧响应（筛选已变 / 已被新请求取代）直接丢弃。
    request: u64,
    /// 搜索框编辑计数，防抖回调据此判断是否仍是最新输入。
    query_edit: u64,
    selected: HashSet<i64>,
    seen_revision: u64,
    connected: bool,
    releasing: bool,
    /// 加白请求在途：期间禁用所有「加白」按钮，避免重复提交。
    allowing: bool,
    exporting: bool,
    _subscriptions: Vec<Subscription>,
}

impl BansPanel {
    fn new(
        store: Entity<SettingsStore>,
        translator: Translator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder =
            SharedString::from(translator.text("btGuardSearchPlaceholder").to_owned());
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let query_events =
            cx.subscribe_in(
                &query,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.schedule_search(cx),
                    InputEvent::PressEnter { .. } => {
                        this.query_edit = this.query_edit.wrapping_add(1);
                        this.items.clear();
                        this.reload(cx);
                    }
                    _ => {}
                },
            );
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
            // 连接状态影响按钮可用性。
            cx.notify();
        });
        let snapshot = store.read(cx);
        let connected = snapshot.daemon_connected();
        let seen_revision = snapshot.bt_guard_revision();
        let mut panel = Self {
            store,
            translator,
            layer: None,
            rule: None,
            query,
            items: Vec::new(),
            total: 0,
            fetched_at: Instant::now(),
            loading: false,
            failed: false,
            request: 0,
            query_edit: 0,
            selected: HashSet::new(),
            seen_revision,
            connected,
            releasing: false,
            allowing: false,
            exporting: false,
            _subscriptions: vec![query_events, store_events],
        };
        if connected {
            panel.reload(cx);
        }
        panel
    }

    /// 语言切换后同步翻译快照（保留视图不会随父视图重建）。
    fn sync_translator(&mut self, translator: &Translator, cx: &mut Context<Self>) {
        if self.translator.locale() != translator.locale() {
            self.translator = translator.clone();
            cx.notify();
        }
    }

    fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.translator.text(key).to_owned())
    }

    // ───────────────────────── 拉取 ─────────────────────────

    /// 从头重新拉取，保持已加载的条数。
    fn reload(&mut self, cx: &mut Context<Self>) {
        let limit = reload_limit(self.items.len());
        self.fetch(0, limit, false, cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let offset = u32::try_from(self.items.len()).unwrap_or(u32::MAX);
        self.fetch(
            offset,
            fluxdown_protocol::BT_GUARD_LIST_DEFAULT_LIMIT,
            true,
            cx,
        );
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.query_edit = self.query_edit.wrapping_add(1);
        let edit = self.query_edit;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let Ok(()) = this.update(cx, |this, cx| {
                if this.query_edit == edit {
                    this.items.clear();
                    this.reload(cx);
                }
            }) else {
                // 面板已释放。
                return;
            };
        })
        .detach();
    }

    fn fetch(&mut self, offset: u32, limit: u32, append: bool, cx: &mut Context<Self>) {
        self.request = self.request.wrapping_add(1);
        let token = self.request;
        let query = self.query.read(cx).value().to_string();
        let params = entries_params(self.layer, self.rule, &query, offset, limit);
        let params = match serde_json::to_value(&params) {
            Ok(params) => params,
            Err(_) => {
                self.failed = true;
                cx.notify();
                return;
            }
        };
        let future = self
            .store
            .read(cx)
            .raw_call(method::DAEMON_BT_GUARD_ENTRIES_LIST, params);
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update(cx, |this, cx| {
                this.finish_fetch(token, append, result, cx);
            }) else {
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
            .and_then(|value| serde_json::from_value::<BtGuardEntriesListDto>(value).ok());
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
                self.fetched_at = Instant::now();
                retain_selected(&mut self.selected, &self.items);
            }
            None => self.failed = true,
        }
        cx.notify();
    }

    // ───────────────────────── 筛选 ─────────────────────────

    fn set_layer(&mut self, value: &str, cx: &mut Context<Self>) {
        let layer = pick_static(BT_GUARD_LAYERS, value);
        if layer != self.layer {
            self.layer = layer;
            self.items.clear();
            self.selected.clear();
            self.reload(cx);
        }
    }

    fn set_rule(&mut self, value: &str, cx: &mut Context<Self>) {
        let rule = pick_static(RULE_FILTERS, value);
        if rule != self.rule {
            self.rule = rule;
            self.items.clear();
            self.selected.clear();
            self.reload(cx);
        }
    }

    fn filtered(&self, cx: &App) -> bool {
        self.layer.is_some()
            || self.rule.is_some()
            || !self.query.read(cx).value().trim().is_empty()
    }

    // ───────────────────────── 选择 ─────────────────────────

    fn toggle(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        cx.notify();
    }

    fn selectable(&self) -> impl Iterator<Item = i64> + '_ {
        self.items
            .iter()
            .filter(|item| item.layer != CLOUD_LAYER)
            .map(|item| item.id)
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let all: Vec<i64> = self.selectable().collect();
        if !all.is_empty() && all.iter().all(|id| self.selected.contains(id)) {
            self.selected.clear();
        } else {
            self.selected.extend(all);
        }
        cx.notify();
    }

    // ───────────────────────── 动作 ─────────────────────────

    fn notify(&self, notification: Notification, window: &mut Window, cx: &mut App) {
        window.push_notification(notification, cx);
    }

    fn release(&mut self, ids: Vec<i64>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() || self.releasing || self.allowing {
            return;
        }
        let future = self.store.read(cx).raw_call(
            method::DAEMON_BT_GUARD_ENTRIES_REMOVE,
            json!({ "ids": ids }),
        );
        self.releasing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.releasing = false;
                let translator = this.translator.clone();
                match result {
                    Ok(value) => {
                        let removed = serde_json::from_value::<BtGuardEntriesRemoveDto>(value)
                            .map_or(0, |dto| dto.removed);
                        this.selected.clear();
                        this.notify(
                            Notification::success(
                                translator
                                    .text_with("btGuardReleased", &[("n", &removed.to_string())]),
                            ),
                            window,
                            cx,
                        );
                        this.store
                            .update(cx, |store, cx| store.bump_bt_guard_revision(cx));
                    }
                    Err(error) => {
                        this.notify(
                            Notification::error(format!(
                                "{}: {}",
                                translator.text("btGuardReleaseFailed"),
                                rpc_error_text(&translator, &error)
                            )),
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            }) else {
                // 面板或窗口已释放。
                return;
            };
        })
        .detach();
    }

    fn allow(&mut self, cidr: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.allowing || self.releasing {
            return;
        }
        let params = BtGuardEntriesAddParams {
            cidr: cidr.clone(),
            action: "allow".to_owned(),
            duration_secs: DurationChoice::Forever.secs(),
            reason: String::new(),
        };
        let params = match serde_json::to_value(&params) {
            Ok(params) => params,
            Err(_) => {
                let translator = self.translator.clone();
                self.notify(
                    Notification::error(translator.text("btGuardAddFailed").to_owned()),
                    window,
                    cx,
                );
                return;
            }
        };
        let future = self
            .store
            .read(cx)
            .raw_call(method::DAEMON_BT_GUARD_ENTRIES_ADD, params);
        self.allowing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.allowing = false;
                let translator = this.translator.clone();
                match result {
                    Ok(_) => {
                        this.notify(
                            Notification::success(
                                translator.text_with("btGuardAllowed", &[("cidr", &cidr)]),
                            ),
                            window,
                            cx,
                        );
                        this.store
                            .update(cx, |store, cx| store.bump_bt_guard_revision(cx));
                    }
                    Err(error) => {
                        this.notify(
                            Notification::error(format!(
                                "{}: {}",
                                translator.text("btGuardAddFailed"),
                                rpc_error_text(&translator, &error)
                            )),
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            }) else {
                // 面板或窗口已释放。
                return;
            };
        })
        .detach();
    }

    /// 导出当前分层筛选下的条目：先选保存位置，再取文本写盘。
    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.exporting {
            return;
        }
        let layer = self.layer;
        let directory = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
        let receiver = cx.prompt_for_new_path(&directory, Some(EXPORT_FILE_NAME));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let Ok(future) = this.update(cx, |this, cx| -> PortFuture<Value> {
                this.exporting = true;
                cx.notify();
                this.store
                    .read(cx)
                    .raw_call(method::DAEMON_BT_GUARD_EXPORT, json!({ "layer": layer }))
            }) else {
                // 面板已释放。
                return;
            };
            let exported = future
                .await
                .ok()
                .and_then(|value| serde_json::from_value::<BtGuardExportDto>(value).ok());
            let written = match exported {
                Some(dto) => {
                    let lines = dto.lines;
                    cx.background_executor()
                        .spawn(async move { std::fs::write(&path, dto.text).map(|()| lines) })
                        .await
                        .ok()
                }
                None => None,
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.exporting = false;
                let translator = this.translator.clone();
                let notification = match written {
                    Some(lines) => Notification::success(
                        translator.text_with("btGuardExportDone", &[("n", &lines.to_string())]),
                    ),
                    None => Notification::error(translator.text("btGuardExportFailed").to_owned()),
                };
                this.notify(notification, window, cx);
                cx.notify();
            }) else {
                // 面板或窗口已释放。
                return;
            };
        })
        .detach();
    }

    // ───────────────────────── 渲染 ─────────────────────────

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
        let mut layers = vec![(SharedString::default(), self.t("btGuardLayerAll"))];
        layers.extend(BT_GUARD_LAYERS.iter().map(|layer| {
            (
                SharedString::from(*layer),
                label_or_raw(&self.translator, "btGuardLayer", layer),
            )
        }));
        let mut rules = vec![(SharedString::default(), self.t("btGuardRuleAll"))];
        rules.extend(RULE_FILTERS.iter().map(|rule| {
            (
                SharedString::from(*rule),
                label_or_raw(&self.translator, "btGuardRule", rule),
            )
        }));
        let (selected_ids, _) = removable_ids(&self.items, &self.selected);
        let selected_count = selected_ids.len();
        let connected = self.connected;
        let store = self.store.clone();
        let translator = self.translator.clone();
        h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .gap(tokens.spacing.sm)
            .child(self.filter_dropdown("bt-guard-layer", layers, self.layer, Self::set_layer, cx))
            .child(self.filter_dropdown("bt-guard-rule", rules, self.rule, Self::set_rule, cx))
            .child(
                div()
                    .w(active_theme(cx).text_extent(220.))
                    .child(Input::new(&self.query).control(cx).w_full().cleanable(true)),
            )
            .child(div().flex_1())
            .when(selected_count > 0, |bar| {
                bar.child(meta_text(cx).child(SharedString::from(translator.text_with(
                    "btGuardSelectedCount",
                    &[("n", &selected_count.to_string())],
                ))))
                .child(
                    row_loading_button(
                        "bt-guard-release-selected",
                        SharedString::from(translator.text("btGuardReleaseSelected").to_owned()),
                        ButtonVariant::Secondary,
                        self.releasing,
                        cx,
                    )
                    .disabled(!connected || self.allowing)
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            let (ids, _) = removable_ids(&this.items, &this.selected);
                            this.release(ids, window, cx);
                        },
                    )),
                )
            })
            .child(
                row_button(
                    "bt-guard-add",
                    self.t("btGuardAddEntry"),
                    ButtonVariant::Primary,
                    cx,
                )
                .disabled(!connected)
                .on_click(move |_, window, cx| {
                    bt_guard_add_dialog::open(store.clone(), translator.clone(), window, cx);
                }),
            )
            .child(
                row_loading_button(
                    "bt-guard-export",
                    self.t("btGuardExport"),
                    ButtonVariant::Secondary,
                    self.exporting,
                    cx,
                )
                .disabled(!connected)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.export(window, cx);
                })),
            )
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let all: Vec<i64> = self.selectable().collect();
        let state = if all.is_empty() || self.selected.is_empty() {
            CheckState::Unchecked
        } else if all.iter().all(|id| self.selected.contains(id)) {
            CheckState::Checked
        } else {
            CheckState::Indeterminate
        };
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
            .child(
                div()
                    .id("bt-guard-check-all")
                    .flex_none()
                    .w(theme.text_extent(COL_CHECK))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_all(cx)))
                    .child(check_mark(state, cx)),
            )
            .child(cell(self.t("btGuardColAddress"), COL_ADDRESS))
            .child(cell(self.t("btGuardColLayer"), COL_LAYER))
            .child(cell(self.t("btGuardColAction"), COL_ACTION))
            .child(cell(self.t("btGuardColRule"), COL_RULE))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.t("btGuardColReason")),
            )
            .child(cell(self.t("btGuardColRemaining"), COL_REMAINING))
            // 行内操作列：与按钮同宽留位。
            .child(div().flex_none().w(theme.text_extent(160.)))
    }

    fn remaining_text(&self, item: &BtGuardEntryDto) -> SharedString {
        let elapsed = self.fetched_at.elapsed().as_secs();
        match live_remaining(item.remaining_secs, elapsed) {
            None => self.t("btGuardPermanent"),
            Some(secs) => SharedString::from(
                duration_units(secs)
                    .into_iter()
                    .map(|(unit, n)| {
                        self.translator
                            .text_with(unit.label_key(), &[("n", &n.to_string())])
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        }
    }

    /// 悬停提示：来源 / 添加时间 / 证据摘要。
    fn entry_tooltip(&self, item: &BtGuardEntryDto) -> SharedString {
        let mut lines = vec![item.cidr.clone()];
        if !item.reason.trim().is_empty() {
            lines.push(item.reason.trim().to_owned());
        }
        if !item.source.trim().is_empty() {
            lines.push(item.source.trim().to_owned());
        }
        if item.created_at > 0 {
            lines.push(format_unix(item.created_at));
        }
        if let Some(evidence) = &item.evidence {
            lines.extend(
                evidence_fields(evidence)
                    .iter()
                    .map(|field| self.evidence_line(field)),
            );
        }
        if item.layer == CLOUD_LAYER {
            lines.push(self.translator.text("btGuardCloudEntryHint").to_owned());
        }
        SharedString::from(lines.join("\n"))
    }

    fn evidence_line(&self, field: &EvidenceField) -> String {
        let label = field.label_key.map_or_else(
            || field.raw_key.clone(),
            |key| self.translator.text(key).to_owned(),
        );
        format!("{label}: {}", field.value.text())
    }

    fn entry_row(
        &self,
        index: usize,
        item: &BtGuardEntryDto,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let id = item.id;
        let cloud = item.layer == CLOUD_LAYER;
        let checked = self.selected.contains(&id);
        let tooltip = self.entry_tooltip(item);
        let cidr = item.cidr.clone();
        let release_ids = vec![id];
        let can_allow = item.layer != "allow";
        let connected = self.connected;
        let hint = self.t("btGuardCloudEntryHint");
        let layer_label = label_or_raw(&self.translator, "btGuardLayer", &item.layer);
        let action_label = label_or_raw(&self.translator, "btGuardAction", &item.action);
        let rule_label = if item.rule.is_empty() {
            SharedString::from("—")
        } else {
            label_or_raw(&self.translator, "btGuardRule", &item.rule)
        };
        let reason = SharedString::from(item.reason.trim().to_owned());
        let allow_cidr = cidr.clone();
        h_flex()
            .id(("bt-guard-entry", index))
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
                    .id(("bt-guard-entry-check", index))
                    .flex_none()
                    .w(theme.text_extent(COL_CHECK))
                    .when(!cloud, |cell| {
                        cell.cursor_pointer().on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle(id, cx)),
                        )
                    })
                    .when(cloud, |cell| cell.opacity(0.35))
                    .child(check_mark(
                        if checked {
                            CheckState::Checked
                        } else {
                            CheckState::Unchecked
                        },
                        cx,
                    )),
            )
            .child(
                div()
                    .id(("bt-guard-entry-addr", index))
                    .flex_none()
                    .w(theme.text_extent(COL_ADDRESS))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_middle()
                    .font_family(tokens.typography.mono.clone())
                    .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx))
                    .child(SharedString::from(cidr)),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_LAYER))
                    .truncate()
                    .text_color(tokens.colors.muted_foreground)
                    .child(layer_label),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_ACTION))
                    .child(h_flex().child(action_chip(&item.action, action_label, cx))),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_RULE))
                    .truncate()
                    .text_color(tokens.colors.muted_foreground)
                    .child(rule_label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(tokens.colors.muted_foreground)
                    .child(reason),
            )
            .child(
                div()
                    .flex_none()
                    .w(theme.text_extent(COL_REMAINING))
                    .truncate()
                    .child(self.remaining_text(item)),
            )
            .child(
                h_flex()
                    .flex_none()
                    .w(theme.text_extent(160.))
                    .justify_end()
                    .gap(tokens.spacing.xs)
                    .when(can_allow, |actions| {
                        actions.child(
                            row_button(
                                ("bt-guard-entry-allow", index),
                                self.t("btGuardAllowEntry"),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .disabled(!connected || self.allowing || self.releasing)
                            .when(cloud, |button| {
                                button.tooltip({
                                    let hint = hint.clone();
                                    move |window, cx| tooltip_view(hint.clone(), window, cx)
                                })
                            })
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.allow(allow_cidr.clone(), window, cx);
                                },
                            )),
                        )
                    })
                    .when(!cloud, |actions| {
                        actions.child(
                            row_button(
                                ("bt-guard-entry-release", index),
                                self.t("btGuardRelease"),
                                ButtonVariant::Secondary,
                                cx,
                            )
                            .disabled(!connected || self.releasing || self.allowing)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.release(release_ids.clone(), window, cx);
                                },
                            )),
                        )
                    }),
            )
            .into_any_element()
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
                            "bt-guard-bans-reload",
                            self.t("btGuardReload"),
                            ButtonVariant::Secondary,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload(cx))),
                    )
                    .into_any_element();
            }
            let key = if self.filtered(cx) {
                "btGuardEntriesFilterEmpty"
            } else {
                "btGuardEntriesEmpty"
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
        for (index, item) in self.items.iter().enumerate() {
            table = table.child(self.entry_row(index, item, cx));
        }
        let loaded = self.items.len();
        let more = u64::try_from(loaded).unwrap_or(u64::MAX) < self.total;
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .child(
                div()
                    .id("bt-guard-bans-scroll")
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
                                "bt-guard-load-more",
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

impl Render for BansPanel {
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
