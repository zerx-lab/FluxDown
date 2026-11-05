//! 专属插件（FluxCloud 私有商店）：目录状态、分区渲染与加载。
//!
//! 行组件、安装 / 更新链路都复用公开市场（`plugins.rs`）；这里只负责「拉哪份目录、何时显示」。
//! 目录仅存内存，不持久化；打开插件页时懒加载一次，登录态变化后重拉，不轮询；
//! 「插件市场」的刷新按钮同时强制重拉本目录（见 [`PrivateUi::begin_load`]）。

use std::{collections::HashMap, fmt::Display};

use fluxdown_protocol::{
    ApplicationErrorCode, MarketEntryDto, PluginDto, PrivatePluginListDto, RpcErrorData,
};
use fluxdown_ui_components::{ControlExt as _, FluxIcon, card};
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Styled, Window, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Icon,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use super::{
    Frame,
    plugins::{MARKET_PAGE_SIZE, list_card},
};
use crate::{
    ExtensionsEvent, ExtensionsView,
    market::{filter_private, latest_per_plugin, market_action, page_of, private_search_visible},
    ui,
};

/// 私有目录的加载结果；`Idle` = 尚未加载或需要重新加载（下次渲染触发）。
/// 「正在请求」不在此枚举里，由 [`PrivateUi`] 的 `in_flight` 单独表达：
/// 刷新时已加载的条目留在 `Loaded` 里继续显示，完成后整体替换。
pub(crate) enum PrivateLoad {
    Idle,
    Failed,
    Loaded {
        signed_in: bool,
        /// agent 返回的原始条目（每插件每版本一条，含撤回历史）；用于已装版本的撤回标记。
        entries: Vec<MarketEntryDto>,
        /// 每插件一条可装版本（与公开市场同一规则，见 [`latest_per_plugin`]）。
        catalog: Vec<MarketEntryDto>,
    },
}

/// 专属插件分区的呈现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateView {
    /// 不展示整区（未请求，或已登录但没有任何授权：不暗示存在私有商店）。
    Hidden,
    Loading,
    Error,
    SignIn,
    List,
}

/// [`PrivateUi::finish_load`] 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadDone {
    /// 发起后登录态已变，结果作废，状态未动。
    Ignored,
    /// 结果已写入。
    Applied,
    /// 结果已写入；在途期间又有刷新请求，调用方应立即再拉一次。
    Refetch,
}

pub(crate) struct PrivateUi {
    pub load: PrivateLoad,
    /// agent 快照里的登录态；变化时丢弃旧目录并重新加载。
    session: bool,
    /// 登录态变化计数：在途请求带着发起时的值，返回时不一致即作废，避免旧账号目录回写。
    epoch: u64,
    /// 当前 epoch 下有请求在途；在途时不并发发第二个。
    in_flight: bool,
    /// 在途期间收到了刷新请求：在途结果写入后再拉一次，刷新意图不丢。
    rerun: bool,
    /// 已展开的条数：搜索关键字变化与换账号 / 加载失败时回到一页；成功刷新（含安装后重拉）
    /// 保持已展开的页数，用户刚装的那一条不会被收回折叠区。
    pub limit: usize,
    /// 搜索输入：目录条数达到 [`PRIVATE_SEARCH_MIN_ENTRIES`](crate::market::PRIVATE_SEARCH_MIN_ENTRIES)
    /// 才懒创建（见 [`ExtensionsView::private_query`]）。
    pub search: Option<Entity<InputState>>,
}

impl Default for PrivateUi {
    fn default() -> Self {
        Self {
            load: PrivateLoad::Idle,
            session: false,
            epoch: 0,
            in_flight: false,
            rerun: false,
            limit: MARKET_PAGE_SIZE,
            search: None,
        }
    }
}

impl PrivateUi {
    /// 记录 agent 的登录态；变化时立即丢弃旧目录（登出 / 换号不残留）并标记重新加载，
    /// 同时作废在途请求（连同其待补发的刷新）。返回是否变化（调用方据此重绘）。
    pub fn observe_session(&mut self, signed_in: bool) -> bool {
        if self.session == signed_in {
            return false;
        }
        self.session = signed_in;
        self.epoch += 1;
        self.load = PrivateLoad::Idle;
        self.in_flight = false;
        self.rerun = false;
        self.limit = MARKET_PAGE_SIZE;
        true
    }

    /// 是否该由渲染触发懒加载：待加载且无请求在途。
    pub fn wants_auto_load(&self) -> bool {
        !self.in_flight && matches!(self.load, PrivateLoad::Idle)
    }

    /// 发起一次拉取（自动加载、重试、刷新共用，不论当前是 Idle / Failed / Loaded）。
    /// 返回发起时的 epoch，调用方带着它发请求并回写 [`Self::finish_load`]；
    /// 已有请求在途则不并发，只记下「结果写入后再拉一次」并返回 `None`。
    /// 已加载的条目保持显示直到新结果替换（不闪空）。
    pub fn begin_load(&mut self) -> Option<u64> {
        if self.in_flight {
            self.rerun = true;
            return None;
        }
        self.in_flight = true;
        Some(self.epoch)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn view(&self) -> PrivateView {
        match &self.load {
            PrivateLoad::Idle | PrivateLoad::Failed if self.in_flight => PrivateView::Loading,
            PrivateLoad::Idle => PrivateView::Hidden,
            PrivateLoad::Failed => PrivateView::Error,
            PrivateLoad::Loaded {
                signed_in: false, ..
            } => PrivateView::SignIn,
            PrivateLoad::Loaded { catalog, .. } if catalog.is_empty() => PrivateView::Hidden,
            PrivateLoad::Loaded { .. } => PrivateView::List,
        }
    }

    /// 原始条目（含撤回历史）；未加载为空。
    pub fn entries(&self) -> &[MarketEntryDto] {
        match &self.load {
            PrivateLoad::Loaded { entries, .. } => entries,
            _ => &[],
        }
    }

    /// 每插件一条可装版本；未加载为空。
    pub fn catalog(&self) -> &[MarketEntryDto] {
        match &self.load {
            PrivateLoad::Loaded { catalog, .. } => catalog,
            _ => &[],
        }
    }

    /// 写入一次由 [`Self::begin_load`] 发起的拉取结果。`epoch` 与发起时不一致（登录态已变）
    /// 则丢弃并返回 [`LoadDone::Ignored`]。
    /// `disconnected` = 本地服务当前断连：断连类失败回到 `Idle`，重连后自动重试一次
    /// （其他失败停在 `Failed`，由「重试」恢复，避免重绘循环）。
    pub fn finish_load(
        &mut self,
        epoch: u64,
        result: Result<PrivatePluginListDto, RpcErrorData>,
        disconnected: bool,
    ) -> LoadDone {
        if epoch != self.epoch {
            return LoadDone::Ignored;
        }
        self.in_flight = false;
        self.load = match result {
            Ok(dto) => Self::loaded(dto),
            Err(error)
                if disconnected
                    && matches!(
                        error.code,
                        ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout
                    ) =>
            {
                PrivateLoad::Idle
            }
            Err(_) => PrivateLoad::Failed,
        };
        if !matches!(self.load, PrivateLoad::Loaded { .. }) {
            self.limit = MARKET_PAGE_SIZE;
        }
        if std::mem::take(&mut self.rerun) {
            LoadDone::Refetch
        } else {
            LoadDone::Applied
        }
    }

    /// 写入安装流程里顺带重拉的目录（不是 `begin_load` 发起的，不动在途 / 补发标记）。
    /// 登录态已变（`epoch` 不一致）则丢弃并返回 `false`。
    pub fn absorb(&mut self, epoch: u64, dto: PrivatePluginListDto) -> bool {
        if epoch != self.epoch {
            return false;
        }
        self.load = Self::loaded(dto);
        true
    }

    fn loaded(dto: PrivatePluginListDto) -> PrivateLoad {
        if dto.signed_in {
            PrivateLoad::Loaded {
                signed_in: true,
                catalog: latest_per_plugin(&dto.entries),
                entries: dto.entries,
            }
        } else {
            PrivateLoad::Loaded {
                signed_in: false,
                entries: Vec::new(),
                catalog: Vec::new(),
            }
        }
    }
}

pub(crate) fn parse_private_list(
    value: serde_json::Value,
) -> Result<PrivatePluginListDto, RpcErrorData> {
    serde_json::from_value::<PrivatePluginListDto>(value)
        .map_err(|_| RpcErrorData::new(ApplicationErrorCode::ProtocolIncompatible, false))
}

/// 授权到期日的展示文本：缺省 / 空串 → `None`；RFC 3339 解析成功 → `tz` 下的 `YYYY-MM-DD`；
/// 解析失败 → 原样字符串。
pub(crate) fn expiry_date_in<Tz>(raw: Option<&str>, tz: &Tz) -> Option<String>
where
    Tz: chrono::TimeZone,
    Tz::Offset: Display,
{
    let raw = raw.filter(|raw| !raw.is_empty())?;
    Some(match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(at) => at.with_timezone(tz).format("%Y-%m-%d").to_string(),
        Err(_) => raw.to_owned(),
    })
}

/// [`expiry_date_in`] 取本机时区。
pub(crate) fn expiry_date(raw: Option<&str>) -> Option<String> {
    expiry_date_in(raw, &chrono::Local)
}

impl ExtensionsView {
    /// 进入插件页后（或登录态变化后）懒加载一次；断连期间不发起。
    pub(crate) fn ensure_private_loaded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.controller.is_stale() || !self.plugins.private.wants_auto_load() {
            return;
        }
        self.load_private(window, cx);
    }

    /// 拉取专属目录（懒加载 / 重试 / 「刷新」共用）：在途时不并发，结果写入后补拉一次。
    pub(crate) fn load_private(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(epoch) = self.plugins.private.begin_load() else {
            return;
        };
        let future = self.controller.private_plugins();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(parse_private_list);
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                let disconnected = this.controller.is_stale();
                match this
                    .plugins
                    .private
                    .finish_load(epoch, result, disconnected)
                {
                    LoadDone::Ignored => return,
                    LoadDone::Applied => {}
                    // 断连时不补拉：失败结果已回到 Idle，重连后由懒加载接手。
                    LoadDone::Refetch if !disconnected => this.load_private(window, cx),
                    LoadDone::Refetch => {}
                }
                cx.notify();
            }) else {
                // 扩展视图已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
        // 已有条目保持显示，只有首次 / 失败后的加载才切到「加载中」，两者都要重绘。
        cx.notify();
    }

    /// 搜索关键字（渲染前调用）：目录长到出现搜索框时懒创建输入并订阅，关键字变化回到第一页；
    /// 搜索框不可见时返回空串。
    pub(crate) fn private_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        if !private_search_visible(self.plugins.private.catalog().len()) {
            return String::new();
        }
        let search = match &self.plugins.private.search {
            Some(search) => search.clone(),
            None => {
                let placeholder = self
                    .translator
                    .read(cx)
                    .text("marketSearchPlaceholder")
                    .to_owned();
                let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.plugins.private.limit = MARKET_PAGE_SIZE;
                        cx.notify();
                    }
                })
                .detach();
                self.plugins.private.search = Some(search.clone());
                search
            }
        };
        search.read(cx).value().to_string()
    }

    /// 「专属插件」分区：夹在已安装列表与公开市场之间；`Hidden` 时不占位。
    pub(crate) fn render_private_plugins(
        &self,
        installed_by_id: &HashMap<&str, &PluginDto>,
        query: &str,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let private = &self.plugins.private;
        let view = private.view();
        if view == PrivateView::Hidden {
            return None;
        }
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;
        let header = v_flex()
            .w_full()
            .pt(tokens.spacing.lg)
            .gap(tokens.spacing.xxs)
            .child(ui::title_text(
                translator.text("privatePluginsTitle").to_owned(),
                frame,
            ))
            .when(view == PrivateView::List, |this| {
                this.child(ui::meta_text(
                    translator.text("privatePluginsDesc").to_owned(),
                    frame,
                ))
            });
        let body = match view {
            PrivateView::Hidden => return None,
            PrivateView::Loading => {
                ui::meta_text(translator.text("privatePluginsLoading").to_owned(), frame)
                    .into_any_element()
            }
            PrivateView::Error => h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(ui::status_line(
                    FluxIcon::CircleAlert,
                    tokens.colors.destructive,
                    translator.text("privatePluginsLoadFailed").to_owned(),
                    frame,
                ))
                .child(
                    Button::new("private-plugins-retry")
                        .ghost()
                        .control(cx)
                        .label(translator.text("privatePluginsRetry").to_owned())
                        .disabled(stale)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.load_private(window, cx);
                            cx.notify();
                        })),
                )
                .into_any_element(),
            PrivateView::SignIn => card(cx)
                .w_full()
                .p(tokens.spacing.md)
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(tokens.spacing.md)
                        .child(ui::meta_text(
                            translator.text("privatePluginsSignInHint").to_owned(),
                            frame,
                        ))
                        .child(
                            Button::new("private-plugins-sign-in")
                                .primary()
                                .control(cx)
                                .label(translator.text("accountLogin").to_owned())
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(ExtensionsEvent::OpenAccount);
                                })),
                        ),
                )
                .into_any_element(),
            PrivateView::List => self.render_private_rows(installed_by_id, query, frame, cx),
        };
        Some(
            v_flex()
                .w_full()
                .gap(tokens.spacing.md)
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    fn render_private_rows(
        &self,
        installed_by_id: &HashMap<&str, &PluginDto>,
        query: &str,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Frame {
            translator, tokens, ..
        } = frame;
        let private = &self.plugins.private;
        let catalog = private.catalog();
        let filtered = filter_private(catalog, query);
        let (visible, remaining) = page_of(&filtered, private.limit);
        let search = private
            .search
            .as_ref()
            .filter(|_| private_search_visible(catalog.len()));
        let rows = visible
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let installed = installed_by_id.get(entry.plugin_id.as_str()).copied();
                let action = market_action(entry, installed);
                let pending = self.plugins.market.pending.contains(&entry.plugin_id);
                self.render_market_row(index, entry, action, pending, frame, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let list = if visible.is_empty() {
            ui::empty_state(
                FluxIcon::Search,
                translator.text("marketSearchNoResult").to_owned(),
                None,
                frame,
            )
            .into_any_element()
        } else {
            // 行内按钮 id 与公开市场行同名（按下标）：包一层带 id 的容器让两处各自成域。
            list_card(rows, frame, cx)
                .id("private-plugin-rows")
                .into_any_element()
        };
        v_flex()
            .w_full()
            .gap(tokens.spacing.sm)
            .when_some(search, |this, search| {
                this.child(
                    h_flex().w_full().child(
                        Input::new(search)
                            .control(cx)
                            .flex_1()
                            .prefix(
                                Icon::new(FluxIcon::Search)
                                    .size(frame.extended.icon.md)
                                    .text_color(tokens.colors.muted_foreground),
                            )
                            .cleanable(true),
                    ),
                )
            })
            .child(list)
            .when(remaining > 0, |this| {
                this.child(
                    h_flex().justify_center().child(
                        Button::new("private-show-more")
                            .ghost()
                            .control(cx)
                            .label(
                                translator.text_with(
                                    "marketShowMore",
                                    &[("count", &remaining.to_string())],
                                ),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.plugins.private.limit += MARKET_PAGE_SIZE;
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};
    use fluxdown_protocol::{
        ApplicationErrorCode, MarketEntryDto, PrivatePluginListDto, RpcErrorData,
    };

    use super::{
        LoadDone, PrivateLoad, PrivateUi, PrivateView, expiry_date_in, parse_private_list,
    };

    fn entry(id: &str, sequence: u64, yanked: &str) -> MarketEntryDto {
        MarketEntryDto {
            plugin_id: id.to_owned(),
            version: format!("1.0.{sequence}"),
            sequence,
            content_hash: String::new(),
            min_app_version: String::new(),
            name: String::new(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            mirrors: Vec::new(),
            publish_time: String::new(),
            yanked: yanked.to_owned(),
            tags: Vec::new(),
            permissions: Vec::new(),
            network_hosts: Vec::new(),
            channel: Some("private".to_owned()),
            expires_at: None,
        }
    }

    fn list(signed_in: bool, entries: Vec<MarketEntryDto>) -> PrivatePluginListDto {
        PrivatePluginListDto { signed_in, entries }
    }

    fn loaded(ui: &mut PrivateUi, dto: PrivatePluginListDto) {
        let epoch = ui.epoch();
        assert_eq!(ui.finish_load(epoch, Ok(dto), false), LoadDone::Applied);
    }

    fn error(code: ApplicationErrorCode) -> RpcErrorData {
        RpcErrorData::new(code, false)
    }

    #[test]
    fn section_view_follows_the_load_state() {
        let mut ui = PrivateUi::default();
        // 尚未请求：整区不展示。
        assert_eq!(ui.view(), PrivateView::Hidden);
        ui.begin_load();
        assert_eq!(ui.view(), PrivateView::Loading);
        ui.load = PrivateLoad::Failed;
        // 失败后重试在途：显示加载中，而不是停在错误条。
        assert_eq!(ui.view(), PrivateView::Loading);
        ui.in_flight = false;
        assert_eq!(ui.view(), PrivateView::Error);

        // 未登录：登录提示，不报错。
        loaded(&mut ui, list(false, Vec::new()));
        assert_eq!(ui.view(), PrivateView::SignIn);

        // 已登录无授权：整区不展示（不暗示存在私有商店）。
        loaded(&mut ui, list(true, Vec::new()));
        assert_eq!(ui.view(), PrivateView::Hidden);

        // 只剩撤回版本：等同于没有授权。
        loaded(&mut ui, list(true, vec![entry("a@acme", 1, "malicious")]));
        assert_eq!(ui.view(), PrivateView::Hidden);

        loaded(&mut ui, list(true, vec![entry("a@acme", 1, "none")]));
        assert_eq!(ui.view(), PrivateView::List);
    }

    #[test]
    fn catalog_keeps_the_installable_latest_and_entries_keep_history() {
        let mut ui = PrivateUi::default();
        loaded(
            &mut ui,
            list(
                true,
                vec![
                    entry("a@acme", 1, "none"),
                    entry("a@acme", 2, "none"),
                    entry("a@acme", 3, "vulnerable"),
                ],
            ),
        );
        let versions = ui
            .catalog()
            .iter()
            .map(|entry| entry.version.as_str())
            .collect::<Vec<_>>();
        assert_eq!(versions, vec!["1.0.2"]);
        assert_eq!(ui.entries().len(), 3);
    }

    #[test]
    fn signed_out_result_carries_no_entries() {
        let mut ui = PrivateUi::default();
        // 即使 agent 误带条目，未登录也不展示。
        loaded(&mut ui, list(false, vec![entry("a@acme", 1, "none")]));
        assert_eq!(ui.view(), PrivateView::SignIn);
        assert!(ui.catalog().is_empty());
        assert!(ui.entries().is_empty());
    }

    #[test]
    fn session_change_drops_the_catalog_and_voids_in_flight_loads() {
        let mut ui = PrivateUi::default();
        assert!(!ui.observe_session(false));
        assert!(ui.observe_session(true));
        loaded(&mut ui, list(true, vec![entry("a@acme", 1, "none")]));
        assert_eq!(ui.view(), PrivateView::List);

        // 资料刷新等仍为已登录的会话事件不触发重载。
        assert!(!ui.observe_session(true));
        assert_eq!(ui.view(), PrivateView::List);

        // 登出：旧目录立即消失，等待重新加载。
        let in_flight = ui.begin_load().expect("a load starts");
        assert!(ui.observe_session(false));
        assert!(matches!(ui.load, PrivateLoad::Idle));
        assert!(ui.catalog().is_empty());
        // 登录态变化同时清掉在途标记：新会话可立即再发请求。
        assert!(ui.wants_auto_load());

        // 旧会话的在途结果作废，不能把旧账号目录写回来。
        let stale = list(true, vec![entry("a@acme", 1, "none")]);
        assert_eq!(
            ui.finish_load(in_flight, Ok(stale), false),
            LoadDone::Ignored
        );
        assert!(matches!(ui.load, PrivateLoad::Idle));
    }

    #[test]
    fn refresh_refetches_from_loaded_failed_and_idle() {
        let mut ui = PrivateUi::default();
        // 懒加载：Idle 且无在途。
        assert!(ui.wants_auto_load());
        let epoch = ui.begin_load().expect("first load starts");
        assert!(!ui.wants_auto_load());
        assert_eq!(ui.view(), PrivateView::Loading);
        assert_eq!(
            ui.finish_load(
                epoch,
                Ok(list(true, vec![entry("a@acme", 1, "none")])),
                false
            ),
            LoadDone::Applied
        );

        // Loaded：刷新照常发请求；在途期间已有条目继续显示，不闪空。
        let epoch = ui.begin_load().expect("refresh from loaded starts");
        assert_eq!(ui.view(), PrivateView::List);
        assert_eq!(ui.catalog().len(), 1);
        // 新授权出现：结果整体替换。
        let granted = vec![entry("a@acme", 1, "none"), entry("b@acme", 1, "none")];
        assert_eq!(
            ui.finish_load(epoch, Ok(list(true, granted)), false),
            LoadDone::Applied
        );
        assert_eq!(ui.catalog().len(), 2);

        // Loaded → 刷新失败：落到错误条。
        let epoch = ui.begin_load().expect("refresh starts");
        ui.finish_load(epoch, Err(error(ApplicationErrorCode::Internal)), false);
        assert_eq!(ui.view(), PrivateView::Error);

        // Failed：刷新（或重试）再次发请求，期间显示加载中，成功后恢复列表。
        let epoch = ui.begin_load().expect("refresh from failed starts");
        assert_eq!(ui.view(), PrivateView::Loading);
        ui.finish_load(
            epoch,
            Ok(list(true, vec![entry("a@acme", 1, "none")])),
            false,
        );
        assert_eq!(ui.view(), PrivateView::List);

        // 已登录无授权 → 刷新后出现授权：整区立即出现。
        let epoch = ui.begin_load().expect("refresh starts");
        ui.finish_load(epoch, Ok(list(true, Vec::new())), false);
        assert_eq!(ui.view(), PrivateView::Hidden);
        let epoch = ui.begin_load().expect("refresh from empty starts");
        assert_eq!(ui.view(), PrivateView::Hidden);
        ui.finish_load(
            epoch,
            Ok(list(true, vec![entry("c@acme", 1, "none")])),
            false,
        );
        assert_eq!(ui.view(), PrivateView::List);

        // Idle（断连类失败后）：刷新同样发请求。
        let epoch = ui.begin_load().expect("refresh starts");
        ui.finish_load(epoch, Err(error(ApplicationErrorCode::Timeout)), true);
        assert!(matches!(ui.load, PrivateLoad::Idle));
        assert!(ui.begin_load().is_some());
    }

    #[test]
    fn refresh_while_loading_does_not_overlap_but_is_not_lost() {
        let mut ui = PrivateUi::default();
        let epoch = ui.begin_load().expect("first load starts");
        // 在途时再刷新：不并发，只记下补拉。
        assert_eq!(ui.begin_load(), None);
        assert_eq!(ui.begin_load(), None);
        assert_eq!(
            ui.finish_load(
                epoch,
                Ok(list(true, vec![entry("a@acme", 1, "none")])),
                false
            ),
            LoadDone::Refetch
        );
        // 在途结果先落地（不闪空），随后补拉一次，且只补一次。
        assert_eq!(ui.view(), PrivateView::List);
        let epoch = ui.begin_load().expect("queued refresh starts");
        assert_eq!(
            ui.finish_load(
                epoch,
                Ok(list(true, vec![entry("a@acme", 1, "none")])),
                false
            ),
            LoadDone::Applied
        );
    }

    #[test]
    fn session_change_cancels_a_queued_refresh() {
        let mut ui = PrivateUi::default();
        let old = ui.begin_load().expect("load starts");
        assert_eq!(ui.begin_load(), None);
        assert!(ui.observe_session(true));
        // 旧请求作废；排队的补拉随之取消，新会话的第一次请求不会被误判为在途。
        assert_eq!(
            ui.finish_load(old, Err(error(ApplicationErrorCode::Internal)), false),
            LoadDone::Ignored
        );
        let epoch = ui.begin_load().expect("new session loads");
        assert_eq!(
            ui.finish_load(epoch, Ok(list(true, Vec::new())), false),
            LoadDone::Applied
        );
    }

    #[test]
    fn absorb_writes_the_catalog_without_touching_the_in_flight_load() {
        let mut ui = PrivateUi::default();
        let epoch = ui.begin_load().expect("load starts");
        assert_eq!(ui.begin_load(), None);
        assert!(ui.absorb(epoch, list(true, vec![entry("a@acme", 1, "none")])));
        assert_eq!(ui.catalog().len(), 1);
        // 在途请求与补拉标记不受影响。
        assert_eq!(
            ui.finish_load(
                epoch,
                Ok(list(true, vec![entry("a@acme", 1, "none")])),
                false
            ),
            LoadDone::Refetch
        );
        // 登录态变化后的写回被丢弃。
        assert!(ui.observe_session(true));
        assert!(!ui.absorb(epoch, list(true, vec![entry("b@acme", 1, "none")])));
        assert!(ui.catalog().is_empty());
    }

    #[test]
    fn expanded_pages_survive_refreshes_but_not_account_changes_or_failures() {
        use super::MARKET_PAGE_SIZE;

        let entries = (0..120)
            .map(|index| entry(&format!("p{index}@acme"), 1, "none"))
            .collect::<Vec<_>>();
        let mut ui = PrivateUi::default();
        assert!(ui.observe_session(true));
        loaded(&mut ui, list(true, entries.clone()));
        assert_eq!(ui.limit, MARKET_PAGE_SIZE);
        assert_eq!(ui.catalog().len(), 120);

        // 用户点了两次「显示更多」；刷新 / 安装后重拉都保持，刚装的条目不会被收回折叠区。
        ui.limit = MARKET_PAGE_SIZE * 3;
        loaded(&mut ui, list(true, entries.clone()));
        assert_eq!(ui.limit, MARKET_PAGE_SIZE * 3);
        let epoch = ui.epoch();
        assert!(ui.absorb(epoch, list(true, entries.clone())));
        assert_eq!(ui.limit, MARKET_PAGE_SIZE * 3);
        // 目录变短后 limit 仍大于条数：全部可见，没有「显示更多」。
        loaded(&mut ui, list(true, entries[..3].to_vec()));
        let (visible, remaining) = crate::market::page_of(ui.catalog(), ui.limit);
        assert_eq!((visible.len(), remaining), (3, 0));

        // 加载失败：目录已无可展示条目，回到一页。
        ui.limit = MARKET_PAGE_SIZE * 3;
        let epoch = ui.epoch();
        ui.finish_load(epoch, Err(error(ApplicationErrorCode::Internal)), false);
        assert_eq!(ui.limit, MARKET_PAGE_SIZE);

        // 换账号：新目录从第一页开始。
        loaded(&mut ui, list(true, entries));
        ui.limit = MARKET_PAGE_SIZE * 2;
        assert!(ui.observe_session(false));
        assert_eq!(ui.limit, MARKET_PAGE_SIZE);
    }

    #[test]
    fn failures_stop_at_failed_unless_disconnected() {
        let mut ui = PrivateUi::default();
        let epoch = ui.epoch();
        assert_eq!(
            ui.finish_load(epoch, Err(error(ApplicationErrorCode::Internal)), false),
            LoadDone::Applied
        );
        assert!(matches!(ui.load, PrivateLoad::Failed));
        // 未断连的 Unavailable 也停在失败，避免重绘循环。
        ui.finish_load(epoch, Err(error(ApplicationErrorCode::Unavailable)), false);
        assert!(matches!(ui.load, PrivateLoad::Failed));
        // 断连类失败：回到待加载，重连后自动重试。
        ui.finish_load(epoch, Err(error(ApplicationErrorCode::Timeout)), true);
        assert!(matches!(ui.load, PrivateLoad::Idle));
        ui.finish_load(epoch, Err(error(ApplicationErrorCode::Internal)), true);
        assert!(matches!(ui.load, PrivateLoad::Failed));
    }

    #[test]
    fn list_payload_parses_camel_case_and_rejects_garbage() {
        let value = serde_json::json!({
            "signedIn": true,
            "entries": [{
                "pluginId": "a@acme",
                "version": "1.0.0",
                "sequence": 1,
                "contentHash": "",
                "minAppVersion": "",
                "name": "A",
                "description": "",
                "author": "",
                "homepage": "",
                "mirrors": [],
                "publishTime": "",
                "yanked": "none",
                "tags": [],
                "permissions": [],
                "networkHosts": [],
                "channel": "private",
                "expiresAt": "2026-12-31T23:30:00Z"
            }]
        });
        let parsed = parse_private_list(value);
        assert!(parsed.is_ok());
        let dto = parsed.expect("list payload parses");
        assert!(dto.signed_in);
        assert_eq!(dto.entries.len(), 1);
        assert_eq!(
            dto.entries[0].expires_at.as_deref(),
            Some("2026-12-31T23:30:00Z")
        );
        assert!(parse_private_list(serde_json::json!({ "entries": 3 })).is_err());
    }

    #[test]
    fn expiry_is_formatted_in_the_given_zone_and_falls_back_verbatim() {
        assert_eq!(expiry_date_in(None, &Utc), None);
        assert_eq!(expiry_date_in(Some(""), &Utc), None);
        assert_eq!(
            expiry_date_in(Some("2026-12-31T23:30:00Z"), &Utc),
            Some("2026-12-31".to_owned())
        );
        // 跨日：UTC 23:30 在 +08:00 已是次日。
        let east8 = FixedOffset::east_opt(8 * 3600).expect("+08:00");
        assert_eq!(
            expiry_date_in(Some("2026-12-31T23:30:00Z"), &east8),
            Some("2027-01-01".to_owned())
        );
        assert_eq!(
            expiry_date_in(Some("2026-12-31T23:30:00+08:00"), &Utc),
            Some("2026-12-31".to_owned())
        );
        // 解析失败：原样字符串，不 panic。
        assert_eq!(
            expiry_date_in(Some("next tuesday"), &Utc),
            Some("next tuesday".to_owned())
        );
    }
}
