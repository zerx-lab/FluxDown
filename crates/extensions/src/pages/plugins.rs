//! 插件子页：已安装插件管理（启用 / 设置 / 更新 / 卸载）+ 安装区（zip / 开发目录）
//! + 插件市场浏览与安装。

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Instant,
};

use fluxdown_protocol::{InstalledPlugin, MarketEntryDto, PluginDto};
use fluxdown_ui_components::{
    BusyExt as _, BusyHold, ControlExt as _, FluxIcon, IconControlExt as _, card, form, form_field,
    input_with_action, tabular_numbers, tooltip_view,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    h_flex,
    input::{InputEvent, InputState},
    link::Link,
    switch::Switch,
    v_flex,
};

use super::Frame;
use crate::{
    ExtensionsTab, ExtensionsView, PluginViewSlot,
    components::{
        plugin_audit::open_plugin_audit,
        plugin_auth::PluginAuthDialog,
        plugin_consent::{ConsentAction, ConsentRequest, open_consent_dialog},
        plugin_detail::{OpenAudit, PluginDetail, open_plugin_detail, yanked_label},
        plugin_live::CloseWhenRemoved,
        plugin_settings::{PluginSettingsForm, PluginSettingsTabs},
    },
    controller::{COMPONENT_KINDS, component_wire_name, grant_plugin_consent},
    error_text,
    market::{
        ConsentFlow, MarketAction, MarketSort, arrange_market, confirmation_holds, consent_flow,
        filter_market, hosts_to_confirm, installed_version_yanked, latest_per_plugin,
        market_action, market_needs_load, permissions_to_confirm, risk_of,
    },
    ui,
};

/// 市场列表每次展开的条数。
pub const MARKET_PAGE_SIZE: usize = 50;

/// 市场行「安装 / 更新 / 已安装」按钮的最小宽度：不同文案的按钮列保持同宽对齐。
const MARKET_ACTION_MIN_WIDTH: f32 = 76.;

/// 插件写操作类型；结果提示按此分流。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginOp {
    Install,
    Update,
    Uninstall,
    SetEnabled,
    Reload,
}

/// 用户安装前确认过的（权限，主机）。
pub(crate) type ConfirmedSets = (Vec<String>, Vec<String>);

pub(crate) struct PluginsUi {
    pub dev_mode: bool,
    pub dev_dir: String,
    pub installing_file: bool,
    pub installing_dir: bool,
    /// 有写操作在途的插件标识（禁用其卡片上的控件）。
    pub busy: HashSet<String>,
    /// 「重新加载」在途的 dev 插件（按钮显示转圈）；是 `busy` 的子集。
    pub reloading: HashSet<String>,
    /// 「卸载」在途的插件（仅该按钮转圈）；是 `busy` 的子集。
    pub uninstalling: HashSet<String>,
    /// 点击「重新加载」后的最短转圈保持，按插件标识。
    pub reload_holds: HashMap<String, BusyHold>,
    /// 点击「刷新市场」后的最短转圈保持（自动加载不启动）。
    pub market_hold: BusyHold,
    pub market: MarketUi,
    pub site_defaults: super::site_defaults::SiteDefaultsUi,
    /// 安装 / 更新 / 重载成功、等待插件列表追上后再判定授权流程的插件；
    /// 值 = 市场路径下用户安装前确认过的（权限，主机）。
    pub awaiting_consent: HashMap<String, Option<ConfirmedSets>>,
    /// 市场安装在途时用户已确认的（权限，主机），按插件 id。
    pub market_confirmed: HashMap<String, ConfirmedSets>,
}

impl Default for PluginsUi {
    fn default() -> Self {
        Self {
            dev_mode: true,
            dev_dir: String::new(),
            installing_file: false,
            installing_dir: false,
            busy: HashSet::new(),
            reloading: HashSet::new(),
            uninstalling: HashSet::new(),
            reload_holds: HashMap::new(),
            market_hold: BusyHold::default(),
            market: MarketUi::default(),
            site_defaults: super::site_defaults::SiteDefaultsUi::default(),
            awaiting_consent: HashMap::new(),
            market_confirmed: HashMap::new(),
        }
    }
}

pub(crate) struct MarketUi {
    pub requested: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// 索引原始条目（每插件每版本一条）；用于查询已安装版本的撤回标记。
    pub entries: Vec<MarketEntryDto>,
    /// 每插件一条：引擎实际会安装的版本（见 [`latest_per_plugin`]）。
    pub catalog: Vec<MarketEntryDto>,
    pub search: Option<Entity<InputState>>,
    pub sort: MarketSort,
    pub favorites_only: bool,
    pub favorites: super::market_favorites::FavoritesUi,
    pub limit: usize,
    /// 安装 / 更新在途的市场插件 id。
    pub pending: HashSet<String>,
    /// 上次加载因断连失败；重连后重新加载一次。
    pub retry_on_reconnect: bool,
    /// 上次成功加载完成的时刻；超过 `MARKET_MAX_AGE` 后再次渲染插件页会重新拉取。
    pub loaded_at: Option<Instant>,
}

impl Default for MarketUi {
    fn default() -> Self {
        Self {
            requested: false,
            loading: false,
            error: None,
            entries: Vec::new(),
            catalog: Vec::new(),
            search: None,
            sort: MarketSort::default(),
            favorites_only: false,
            favorites: super::market_favorites::FavoritesUi::default(),
            limit: MARKET_PAGE_SIZE,
            pending: HashSet::new(),
            retry_on_reconnect: false,
            loaded_at: None,
        }
    }
}

impl ExtensionsView {
    pub(crate) fn render_plugins(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_market_loaded(window, cx);
        self.ensure_favorites_loaded(window, cx);
        let search = self.ensure_market_search(window, cx);
        self.schedule_ready_consents(window, cx);
        let query = search.read(cx).value().to_string();
        let theme = active_theme(cx);
        let frame = Frame {
            translator: self.translator.read(cx),
            tokens: theme.tokens(),
            extended: theme.extended(),
            components: theme.components(),
            stale: self.controller.is_stale(),
        };
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;

        let plugins = self.controller.plugins();
        let market = &self.plugins.market;
        let catalog_by_id = market
            .catalog
            .iter()
            .map(|entry| (entry.plugin_id.as_str(), entry))
            .collect::<HashMap<_, _>>();
        let installed = plugins
            .iter()
            .enumerate()
            .map(|(index, plugin)| {
                let update = catalog_by_id
                    .get(plugin.identity.as_str())
                    .copied()
                    .filter(|entry| market_action(entry, Some(plugin)) == MarketAction::Update);
                let yanked = installed_version_yanked(&market.entries, plugin);
                self.render_plugin_row(index, plugin, update, yanked, frame, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let installed_by_id = plugins
            .iter()
            .map(|plugin| (plugin.identity.as_str(), plugin))
            .collect::<HashMap<_, _>>();
        let market_refreshing = market.loading || self.plugins.market_hold.is_held();

        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(ui::title_text(
                        translator.text("pluginsSectionTitle").to_owned(),
                        frame,
                    ))
                    .child(
                        h_flex()
                            .gap(tokens.spacing.sm)
                            .items_center()
                            .child(
                                Button::new("plugin-audit-open")
                                    .ghost()
                                    .control(cx)
                                    .label(translator.text("pluginAuditOpenAll").to_owned())
                                    .disabled(stale)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_audit_dialog(None, window, cx);
                                    })),
                            )
                            .child(ui::meta_text(
                                translator.text("pluginDevModeSwitch").to_owned(),
                                frame,
                            ))
                            .child(
                                Switch::new("plugin-dev-mode")
                                    .checked(self.plugins.dev_mode)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.plugins.dev_mode = *checked;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(self.render_install_area(frame, cx))
            .child(if installed.is_empty() {
                ui::empty_state(
                    FluxIcon::Package,
                    translator.text("pluginsEmpty").to_owned(),
                    None,
                    frame,
                )
            } else {
                list_card(installed, frame, cx)
            })
            .child(
                v_flex()
                    .w_full()
                    .pt(tokens.spacing.lg)
                    .gap(tokens.spacing.xxs)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(ui::title_text(
                                translator.text("marketSectionTitle").to_owned(),
                                frame,
                            ))
                            .child(
                                Button::new("market-refresh")
                                    .ghost()
                                    .control(cx)
                                    .label(translator.text("marketRefreshTooltip").to_owned())
                                    .busy(market_refreshing)
                                    .disabled(market_refreshing || stale)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.plugins.market_hold.start(cx);
                                        this.load_market(window, cx);
                                    })),
                            ),
                    )
                    .child(ui::meta_text(
                        translator.text("marketSectionDesc").to_owned(),
                        frame,
                    )),
            )
            .child(self.render_market(&search, &query, &installed_by_id, frame, cx))
    }

    fn ensure_market_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(search) = &self.plugins.market.search {
            return search.clone();
        }
        let placeholder = self
            .translator
            .read(cx)
            .text("marketSearchPlaceholder")
            .to_owned();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.plugins.market.limit = MARKET_PAGE_SIZE;
                cx.notify();
            }
        })
        .detach();
        self.plugins.market.search = Some(search.clone());
        search
    }

    fn render_install_area(&self, frame: Frame<'_>, cx: &Context<Self>) -> impl IntoElement {
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;
        let dev_dir = self.plugins.dev_dir.clone();
        let dev_dir_empty = dev_dir.is_empty();
        // 安装区：zip 安装按钮 + （开发模式下）「从目录安装」字段：只读路径框 +
        // 选择目录 / 安装 同行按钮，全部统一控件档。
        card(cx).w_full().p(tokens.spacing.md).child(
            form(cx)
                .child(
                    h_flex().child(
                        Button::new("plugin-install-zip")
                            .outline()
                            .icon(FluxIcon::FolderOpen)
                            .label(translator.text("pluginInstallZipButton").to_owned())
                            .control(cx)
                            .busy(self.plugins.installing_file)
                            .disabled(stale || self.plugins.installing_file)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.pick_plugin_zip(window, cx);
                            })),
                    ),
                )
                .when(self.plugins.dev_mode, |this| {
                    let display = if dev_dir_empty {
                        translator.text("pluginInstallDirPlaceholder").to_owned()
                    } else {
                        dev_dir
                    };
                    this.child(form_field(
                        translator.text("pluginInstallDirLabel").to_owned(),
                        input_with_action(
                            ui::path_box(display, dev_dir_empty, active_theme(cx)),
                            h_flex()
                                .gap(tokens.spacing.sm)
                                .items_center()
                                .child(
                                    Button::new("plugin-pick-dev-dir")
                                        .outline()
                                        .icon(FluxIcon::FolderOpen)
                                        .control_icon(cx)
                                        .tooltip(
                                            translator
                                                .text("pluginInstallDirPlaceholder")
                                                .to_owned(),
                                        )
                                        .disabled(self.plugins.installing_dir)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.pick_dev_dir(window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("plugin-install-dev-dir")
                                        .primary()
                                        .label(translator.text("pluginInstallDirButton").to_owned())
                                        .control(cx)
                                        .busy(self.plugins.installing_dir)
                                        .disabled(
                                            stale || dev_dir_empty || self.plugins.installing_dir,
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.install_dev_dir(window, cx);
                                        })),
                                ),
                            cx,
                        ),
                        None,
                        cx,
                    ))
                }),
        )
    }

    fn render_plugin_row(
        &self,
        index: usize,
        plugin: &PluginDto,
        update: Option<&MarketEntryDto>,
        yanked: Option<&str>,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator, tokens, ..
        } = frame;
        let destructive = tokens.colors.destructive;
        let load_failed = plugin.load_status == "Failed";
        // 颜色是信号：只有加载失败 / 熔断停用着 destructive，其余状态中性。
        let badges = [
            plugin
                .dev_mode
                .then(|| ui::neutral_pill(translator.text("pluginDevModeBadge").to_owned(), frame)),
            Some(if load_failed {
                ui::tone_pill(
                    translator.text("pluginLoadStatusFailed").to_owned(),
                    destructive,
                    frame,
                )
            } else {
                ui::neutral_pill(translator.text("pluginLoadStatusLoaded").to_owned(), frame)
            }),
            (plugin.disabled_reason == "Manual").then(|| {
                ui::neutral_pill(translator.text("pluginDisabledManual").to_owned(), frame)
            }),
            (plugin.disabled_reason == "CircuitBreaker").then(|| {
                ui::tone_pill(
                    translator.text("pluginDisabledCircuitBreaker").to_owned(),
                    destructive,
                    frame,
                )
            }),
            (plugin.disabled_reason == "Revoked").then(|| {
                ui::tone_pill(
                    translator.text("pluginDisabledRevoked").to_owned(),
                    destructive,
                    frame,
                )
            }),
            (plugin.disabled_reason == "ConsentRequired").then(|| {
                ui::tone_pill(
                    translator.text("pluginDisabledConsentRequired").to_owned(),
                    frame.extended.colors.warning,
                    frame,
                )
            }),
            yanked
                .and_then(|yanked| yanked_label(translator, yanked))
                .map(|label| {
                    ui::tone_pill(
                        translator.text_with("pluginInstalledVersionYanked", &[("label", &label)]),
                        destructive,
                        frame,
                    )
                }),
            update.map(|entry| {
                ui::tone_pill(
                    translator.text_with("pluginUpdateAvailable", &[("version", &entry.version)]),
                    frame.extended.colors.info,
                    frame,
                )
            }),
        ];
        list_row(frame)
            .child(
                row_text(
                    row_title(plugin.name.clone(), &plugin.version, frame)
                        .children(badges.into_iter().flatten())
                        .children(ui::risk_pill(("plugin-risk", index), &plugin.risk, frame)),
                    row_source(("plugin-homepage", index), "", &plugin.homepage, frame),
                    row_description(("plugin-desc", index), &plugin.description, frame),
                    frame,
                )
                .when(load_failed && !plugin.load_error.is_empty(), |this| {
                    let load_error = plugin.load_error.clone();
                    let load_error_tooltip = plugin.load_error.clone();
                    this.child(
                        ui::meta_text(load_error, frame)
                            .id(("plugin-load-error", index))
                            .w_full()
                            .truncate()
                            .text_color(destructive)
                            .tooltip(move |window, cx| {
                                tooltip_view(load_error_tooltip.clone(), window, cx)
                            }),
                    )
                }),
            )
            .child(self.render_plugin_actions(index, plugin, update, frame, cx))
    }

    /// 已安装行的操作区：文字按钮与按插件出现的图标在左，「详情 / 卸载 / 启用开关」
    /// 固定在最右，保证各行尾部三列对齐。
    fn render_plugin_actions(
        &self,
        index: usize,
        plugin: &PluginDto,
        update: Option<&MarketEntryDto>,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;
        let identity = plugin.identity.clone();
        let busy = stale || self.plugins.busy.contains(&plugin.identity);
        let load_failed = plugin.load_status == "Failed";
        let detail_identity = identity.clone();
        let consent_pending = has_pending_consent(plugin);
        h_flex()
            .flex_none()
            .pl(tokens.spacing.md)
            .gap(tokens.spacing.xs)
            .items_center()
            .when(consent_pending, |this| {
                let identity = identity.clone();
                this.child(
                    Button::new(("plugin-consent", index))
                        .primary()
                        .control(cx)
                        .label(translator.text("pluginConsentButton").to_owned())
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(plugin) = this.controller.plugin(&identity).cloned() {
                                this.open_consent(plugin, window, cx);
                            }
                        })),
                )
            })
            .when_some(update, |this, entry| {
                let pending = self.plugins.market.pending.contains(&entry.plugin_id);
                let entry = entry.clone();
                this.child(
                    Button::new(("plugin-update", index))
                        .outline()
                        .control(cx)
                        .label(
                            translator
                                .text(if pending {
                                    "marketUpdatingButton"
                                } else {
                                    "marketUpdateButton"
                                })
                                .to_owned(),
                        )
                        .busy(pending)
                        .disabled(busy || pending)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.request_market_install(entry.clone(), window, cx);
                        })),
                )
            })
            .when(!load_failed && plugin.auth_supported, |this| {
                let identity = identity.clone();
                this.child(
                    Button::new(("plugin-auth", index))
                        .outline()
                        .control(cx)
                        .label(translator.text("pluginAuthButton").to_owned())
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_plugin_auth(&identity, window, cx);
                        })),
                )
            })
            .when(plugin.dev_mode, |this| {
                let identity = identity.clone();
                let reloading = self.plugins.reloading.contains(&plugin.identity)
                    || self
                        .plugins
                        .reload_holds
                        .get(&plugin.identity)
                        .is_some_and(BusyHold::is_held);
                this.child(
                    Button::new(("plugin-reload", index))
                        .ghost()
                        .control_icon(cx)
                        .icon(FluxIcon::RotateCw)
                        .tooltip(translator.text("pluginReloadTooltip").to_owned())
                        .busy(reloading)
                        .disabled(busy || reloading)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.reload_plugin(identity.clone(), window, cx);
                        })),
                )
            })
            .children(
                plugin
                    .contributions
                    .views
                    .iter()
                    .enumerate()
                    .filter(|(_, view)| {
                        !load_failed
                            && plugin.enabled
                            && plugin.package_dir.is_some()
                            && view.location == "window"
                            && view.desktop.is_some()
                    })
                    .map(|(view_index, view)| {
                        let target = plugin.clone();
                        let view_id = view.id.clone();
                        // views ≤ 16（manifest 校验），index * 16 + view_index 唯一。
                        Button::new(("plugin-window", index * 16 + view_index))
                            .ghost()
                            .control_icon(cx)
                            .icon(FluxIcon::AppWindow)
                            .tooltip(view.title.clone())
                            .disabled(busy)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(crate::ExtensionsEvent::OpenPluginView {
                                    plugin: target.clone(),
                                    view: view_id.clone(),
                                });
                            }))
                    })
                    .collect::<Vec<_>>(),
            )
            .when(
                !load_failed
                    && (!plugin.settings.is_empty() || self.settings_view(plugin).is_some()),
                |this| {
                    let identity = identity.clone();
                    this.child(
                        Button::new(("plugin-settings", index))
                            .ghost()
                            .control_icon(cx)
                            .icon(FluxIcon::Settings)
                            .tooltip(translator.text("pluginSettingsTooltip").to_owned())
                            .disabled(busy || load_failed)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_plugin_settings(&identity, window, cx);
                            })),
                    )
                },
            )
            .child(
                Button::new(("plugin-detail", index))
                    .ghost()
                    .control_icon(cx)
                    .icon(FluxIcon::Info)
                    .tooltip(translator.text("pluginDetailDescription").to_owned())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_plugin_detail(&detail_identity, window, cx);
                    })),
            )
            .child(
                Button::new(("plugin-uninstall", index))
                    .ghost()
                    .control_icon(cx)
                    .icon(FluxIcon::Trash2)
                    .tooltip(translator.text("pluginUninstallTooltip").to_owned())
                    .busy(self.plugins.uninstalling.contains(&plugin.identity))
                    .disabled(busy)
                    .on_click({
                        let name = plugin.name.clone();
                        let identity = identity.clone();
                        cx.listener(move |this, _, window, cx| {
                            this.confirm_uninstall_plugin(
                                identity.clone(),
                                name.clone(),
                                window,
                                cx,
                            );
                        })
                    }),
            )
            .child(div().w(tokens.spacing.xs).flex_none())
            .child(
                Switch::new(("plugin-enabled", index))
                    .checked(plugin.enabled && !load_failed)
                    .disabled(busy || load_failed || consent_pending)
                    .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                        let future = this
                            .controller
                            .set_plugin_enabled(identity.clone(), *checked);
                        this.run_plugin_op(
                            identity.clone(),
                            PluginOp::SetEnabled,
                            future,
                            window,
                            cx,
                        );
                    })),
            )
    }

    fn render_market(
        &self,
        search: &Entity<InputState>,
        query: &str,
        installed_by_id: &HashMap<&str, &PluginDto>,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator, tokens, ..
        } = frame;
        let market = &self.plugins.market;
        let mut root = v_flex().w_full().gap(tokens.spacing.sm);
        if market.loading && market.entries.is_empty() {
            return root.child(ui::meta_text(
                translator.text("pluginCommonLoading").to_owned(),
                frame,
            ));
        }
        if let Some(error) = &market.error {
            return root.child(ui::status_line(
                FluxIcon::CircleAlert,
                tokens.colors.destructive,
                translator.text_with("marketLoadFailed", &[("message", error)]),
                frame,
            ));
        }
        if market.catalog.is_empty() {
            return root.child(ui::empty_state(
                FluxIcon::Package,
                translator.text("marketEmpty").to_owned(),
                None,
                frame,
            ));
        }
        root = root
            .child(self.render_market_toolbar(search, frame, cx))
            .children(self.favorites_load_failed_hint(frame));
        let favorites = &market.favorites;
        let favorites_only = market.favorites_only && favorites.signed_in && !favorites.load_failed;
        let filtered = arrange_market(
            filter_market(&market.catalog, query),
            market.sort,
            &favorites.counts,
            &favorites.mine,
            favorites_only,
        );
        if filtered.is_empty() {
            let (icon, key) = if favorites_only {
                (FluxIcon::Star, "marketFavoritesEmpty")
            } else {
                (FluxIcon::Search, "marketSearchNoResult")
            };
            return root.child(ui::empty_state(
                icon,
                translator.text(key).to_owned(),
                None,
                frame,
            ));
        }
        let remaining = filtered.len().saturating_sub(market.limit);
        let rows = filtered
            .iter()
            .take(market.limit)
            .enumerate()
            .map(|(index, entry)| {
                let installed = installed_by_id.get(entry.plugin_id.as_str()).copied();
                let action = market_action(entry, installed);
                let pending = market.pending.contains(&entry.plugin_id);
                self.render_market_row(index, entry, action, pending, frame, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        root.child(list_card(rows, frame, cx))
            .when(remaining > 0, |this| {
                this.child(
                    h_flex().justify_center().child(
                        Button::new("market-show-more")
                            .ghost()
                            .control(cx)
                            .label(
                                translator.text_with(
                                    "marketShowMore",
                                    &[("count", &remaining.to_string())],
                                ),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.plugins.market.limit += MARKET_PAGE_SIZE;
                                cx.notify();
                            })),
                    ),
                )
            })
    }

    fn render_market_row(
        &self,
        index: usize,
        entry: &MarketEntryDto,
        action: MarketAction,
        pending: bool,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;
        let name = if entry.name.is_empty() {
            entry.plugin_id.clone()
        } else {
            entry.name.clone()
        };
        let yanked = yanked_label(translator, &entry.yanked);
        let detail = PluginDetail::from_market(entry, translator);
        let detail_translator = translator.clone();
        let install_key = match (action, pending) {
            (MarketAction::Install, false) => "marketInstallButton",
            (MarketAction::Install, true) => "marketInstallingButton",
            (MarketAction::Update, false) => "marketUpdateButton",
            (MarketAction::Update, true) => "marketUpdatingButton",
            (MarketAction::Installed, _) => "marketInstalledButton",
            (MarketAction::Unavailable, _) => "marketUnavailableButton",
        };
        let install_label = translator.text(install_key).to_owned();
        let actionable = matches!(action, MarketAction::Install | MarketAction::Update);
        let target = entry.clone();
        list_row(frame)
            .child(row_text(
                row_title(name, &entry.version, frame)
                    .children(
                        yanked.map(|label| ui::tone_pill(label, tokens.colors.destructive, frame)),
                    )
                    .children(ui::risk_pill(
                        ("market-risk", index),
                        risk_of(&entry.permissions),
                        frame,
                    )),
                row_source(
                    ("market-homepage", index),
                    &entry.author,
                    &entry.homepage,
                    frame,
                ),
                row_description(("market-desc", index), &entry.description, frame),
                frame,
            ))
            .child(
                h_flex()
                    .flex_none()
                    .pl(tokens.spacing.md)
                    .gap(tokens.spacing.xs)
                    .items_center()
                    .child(self.render_favorite_button(index, &entry.plugin_id, frame, cx))
                    .child(
                        Button::new(("market-detail", index))
                            .ghost()
                            .control_icon(cx)
                            .icon(FluxIcon::Info)
                            .tooltip(translator.text("pluginDetailDescription").to_owned())
                            .on_click(move |_, window, cx| {
                                open_plugin_detail(
                                    detail.clone(),
                                    detail_translator.clone(),
                                    None,
                                    None,
                                    None,
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new(("market-install", index))
                            .outline()
                            .control(cx)
                            .min_w(px(MARKET_ACTION_MIN_WIDTH))
                            .label(install_label)
                            .busy(pending)
                            .disabled(!actionable || pending || stale)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request_market_install(target.clone(), window, cx);
                            })),
                    ),
            )
    }

    fn ensure_market_loaded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.controller.is_stale() || self.plugins.market.loading {
            return;
        }
        let market = &self.plugins.market;
        if !market_needs_load(
            market.requested,
            market.retry_on_reconnect,
            market.error.is_some(),
            market.loaded_at,
            Instant::now(),
        ) {
            return;
        }
        self.load_market(window, cx);
    }

    fn load_market(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let market = &mut self.plugins.market;
        market.requested = true;
        market.loading = true;
        market.error = None;
        market.retry_on_reconnect = false;
        self.load_favorites(window, cx);
        let future = self.controller.market_list();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update(cx, |this, cx| {
                let market = &mut this.plugins.market;
                market.loading = false;
                match result.and_then(parse_market_entries) {
                    Ok(entries) => {
                        market.catalog = latest_per_plugin(&entries);
                        market.entries = entries;
                        market.error = None;
                        market.limit = MARKET_PAGE_SIZE;
                        market.loaded_at = Some(Instant::now());
                    }
                    Err(error) => {
                        // 断连类失败：daemon 恢复后自动重试一次（其他失败保持手动刷新，避免重绘循环）。
                        market.retry_on_reconnect = this.controller.is_stale()
                            && matches!(
                                error.code,
                                fluxdown_protocol::ApplicationErrorCode::Unavailable
                                    | fluxdown_protocol::ApplicationErrorCode::Timeout
                            );
                        market.error = Some(error_text(this.translator.read(cx), &error));
                    }
                }
                cx.notify();
            }) else {
                // 扩展视图已释放，结束回调，不再更新状态。
                return;
            };
        })
        .detach();
    }

    fn pick_plugin_zip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.installing_file {
            return;
        }
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(SharedString::from(
                self.translator
                    .read(cx)
                    .text("pluginInstallZipButton")
                    .to_owned(),
            )),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.plugins.installing_file = true;
                let future = this
                    .controller
                    .install_plugin_file(path.display().to_string());
                cx.spawn_in(window, async move |this, cx| {
                    let result = future.await;
                    let Ok(()) = this.update_in(cx, |this, window, cx| {
                        this.plugins.installing_file = false;
                        this.finish_plugin_op(PluginOp::Install, result, window, cx);
                        cx.notify();
                    }) else {
                        // 扩展页或窗口已释放，停止回写安装结果。
                        return;
                    };
                })
                .detach();
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn pick_dev_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.first()
            {
                let text = path.display().to_string();

                let Ok(()) = this.update(cx, |this, cx| {
                    this.plugins.dev_dir = text;
                    cx.notify();
                }) else {
                    // 扩展视图已释放，结束回调，不再更新状态。
                    return;
                };
            }
        })
        .detach();
    }

    fn install_dev_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.dev_dir.is_empty() || self.plugins.installing_dir {
            return;
        }
        self.plugins.installing_dir = true;
        let future = self
            .controller
            .install_plugin_dev(self.plugins.dev_dir.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.plugins.installing_dir = false;
                if result.is_ok() {
                    this.plugins.dev_dir.clear();
                }
                this.finish_plugin_op(PluginOp::Install, result, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 市场安装 / 更新入口：daemon 会重新拉取索引并安装当时最新条目，因此先拉取最新索引，
    /// 按最新条目确认权限（新装确认全部权限，更新只确认新增权限）；无需确认时直接执行。
    fn request_market_install(
        &mut self,
        entry: MarketEntryDto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.plugins.market.pending.insert(entry.plugin_id.clone()) {
            return;
        }
        cx.notify();
        self.recheck_market_install(entry.plugin_id, None, window, cx);
    }

    /// 重新拉取索引后继续安装流程。`confirmed` = 用户已确认过的（版本，权限，主机）：
    /// 仍覆盖最新条目才直接安装，否则按最新条目重新确认。`pending` 由调用方置位。
    fn recheck_market_install(
        &mut self,
        plugin_id: String,
        confirmed: Option<(String, Vec<String>, Vec<String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let future = self.controller.market_list();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(parse_market_entries);

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                let op = if this.controller.plugin(&plugin_id).is_some() {
                    PluginOp::Update
                } else {
                    PluginOp::Install
                };
                let entries = match result {
                    Ok(entries) => entries,
                    Err(error) => {
                        this.plugins.market.pending.remove(&plugin_id);
                        this.finish_plugin_op(op, Err(error), window, cx);
                        cx.notify();
                        return;
                    }
                };
                let catalog = latest_per_plugin(&entries);
                let latest = catalog.iter().find(|e| e.plugin_id == plugin_id).cloned();
                let market = &mut this.plugins.market;
                market.catalog = catalog;
                market.entries = entries;
                market.error = None;
                let Some(latest) = latest else {
                    market.pending.remove(&plugin_id);
                    let error = fluxdown_protocol::RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::NotFound,
                        false,
                    )
                    .with_reason(fluxdown_protocol::ErrorReason::PluginNotInMarket);
                    this.finish_plugin_op(op, Err(error), window, cx);
                    cx.notify();
                    return;
                };
                let installed = this.controller.plugin(&plugin_id);
                let permissions = permissions_to_confirm(&latest, installed);
                let hosts = hosts_to_confirm(&latest, installed);
                let holds = match &confirmed {
                    Some((version, granted_permissions, granted_hosts)) => confirmation_holds(
                        version,
                        granted_permissions,
                        granted_hosts,
                        &latest,
                        installed,
                    ),
                    None => permissions.is_empty() && hosts.is_empty(),
                };
                if holds {
                    let version = latest.version.clone();
                    let confirmed_sets = match confirmed {
                        Some((_, granted_permissions, granted_hosts)) => {
                            (granted_permissions, granted_hosts)
                        }
                        None => (permissions, hosts),
                    };
                    this.start_market_install(plugin_id, version, op, confirmed_sets, window, cx);
                } else {
                    this.plugins.market.pending.remove(&plugin_id);
                    this.confirm_market_permissions(latest, op, permissions, hosts, window, cx);
                    cx.notify();
                }
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 市场安装 / 更新前的权限确认：与授权对话框同一组件，展示权限、网络主机与风险等级。
    fn confirm_market_permissions(
        &self,
        entry: MarketEntryDto,
        op: PluginOp,
        permissions: Vec<String>,
        hosts: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        let name = if entry.name.is_empty() {
            entry.plugin_id.clone()
        } else {
            entry.name.clone()
        };
        let (title_key, body_key, ok_key) = if op == PluginOp::Update {
            (
                "pluginPermConfirmUpdateTitle",
                "pluginPermConfirmUpdateBody",
                "pluginPermConfirmUpdateOk",
            )
        } else {
            (
                "pluginPermConfirmInstallTitle",
                "pluginPermConfirmInstallBody",
                "pluginPermConfirmInstallOk",
            )
        };
        let request = ConsentRequest {
            title: translator.text_with(title_key, &[("name", &name)]).into(),
            intro: translator
                .text_with(body_key, &[("version", &entry.version)])
                .into(),
            ok_label: translator.text(ok_key).to_owned().into(),
            risk: risk_of(&entry.permissions).to_owned(),
            permissions: permissions.clone(),
            network_hosts: hosts.clone(),
            page_scripts: Vec::new(),
        };
        let view = cx.entity().downgrade();
        let plugin_id = entry.plugin_id;
        let version = entry.version;
        let on_confirm = Rc::new(move |window: &mut Window, cx: &mut gpui::App| {
            // 页面已释放时未提交安装，不能报告确认成功。
            view.update(cx, |this, cx| {
                // 确认后重新核对索引：daemon 安装的是当时最新条目。
                this.plugins.market.pending.insert(plugin_id.clone());
                cx.notify();
                this.recheck_market_install(
                    plugin_id.clone(),
                    Some((version.clone(), permissions.clone(), hosts.clone())),
                    window,
                    cx,
                );
            })
            .is_ok()
        });
        open_consent_dialog(
            request,
            ConsentAction::Confirm(on_confirm),
            self.translator.clone(),
            window,
            cx,
        );
    }

    /// 调用方已把 `plugin_id` 放入 `pending`；安装结束（成功或失败）时移除。
    /// `confirmed` = 用户安装前确认的（权限，主机），安装成功后用于判定能否直接授权。
    fn start_market_install(
        &mut self,
        plugin_id: String,
        version: String,
        op: PluginOp,
        confirmed: (Vec<String>, Vec<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.plugins.market.pending.insert(plugin_id.clone());
        self.plugins
            .market_confirmed
            .insert(plugin_id.clone(), confirmed);
        cx.notify();
        let future = self.controller.market_install(plugin_id.clone(), version);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                // 市场在确认之后发布了新版本：提示后刷新索引并按最新条目重新确认权限。
                if let Err(error) = &result
                    && error.reason == Some(fluxdown_protocol::ErrorReason::MarketVersionChanged)
                {
                    this.plugins.market_confirmed.remove(&plugin_id);
                    this.finish_plugin_op(op, result, window, cx);
                    this.recheck_market_install(plugin_id, None, window, cx);
                    cx.notify();
                    return;
                }
                this.plugins.market.pending.remove(&plugin_id);
                this.finish_plugin_op(op, result, window, cx);
                // 失败时清理残留；成功时 `finish_plugin_op` 已取走。
                this.plugins.market_confirmed.remove(&plugin_id);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn run_plugin_op(
        &mut self,
        identity: String,
        op: PluginOp,
        future: crate::PortFuture<serde_json::Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.plugins.busy.insert(identity.clone()) {
            return;
        }
        if op == PluginOp::Uninstall {
            self.plugins.uninstalling.insert(identity.clone());
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.plugins.busy.remove(&identity);
                this.plugins.reloading.remove(&identity);
                this.plugins.uninstalling.remove(&identity);
                this.finish_plugin_op(op, result, window, cx);
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn reload_plugin(&mut self, identity: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.busy.contains(&identity) {
            return;
        }
        self.plugins.reloading.insert(identity.clone());
        self.plugins
            .reload_holds
            .entry(identity.clone())
            .or_default()
            .start(cx);
        let future = self.controller.reload_plugin_dev(identity.clone());
        self.run_plugin_op(identity, PluginOp::Reload, future, window, cx);
    }

    /// 写操作结果的全局提示；安装成功且缺少基础组件时追加依赖提醒。
    fn finish_plugin_op(
        &mut self,
        op: PluginOp,
        result: Result<serde_json::Value, fluxdown_protocol::RpcErrorData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        match (op, result) {
            (op @ (PluginOp::Install | PluginOp::Update), Ok(value)) => {
                let key = if op == PluginOp::Update {
                    "pluginOpUpdateSuccess"
                } else {
                    "pluginOpInstallSuccess"
                };
                let message = translator.text(key).to_owned();
                let installed = serde_json::from_value::<InstalledPlugin>(value).ok();
                let missing = installed
                    .as_ref()
                    .map(|installed| installed.missing_components.clone())
                    .unwrap_or_default();
                if let Some(installed) = installed {
                    self.track_consent(installed.identity);
                }
                self.toast_success(message, window, cx);
                if !missing.is_empty() {
                    self.show_missing_components(&missing, window, cx);
                }
            }
            (PluginOp::Uninstall, Ok(_)) => {
                let message = translator.text("pluginOpUninstallSuccess").to_owned();
                self.toast_success(message, window, cx);
            }
            (PluginOp::Reload, Ok(value)) => {
                let message = translator.text("pluginOpReloadSuccess").to_owned();
                let installed = serde_json::from_value::<InstalledPlugin>(value).ok();
                let missing = installed
                    .as_ref()
                    .map(|installed| installed.missing_components.clone())
                    .unwrap_or_default();
                if let Some(installed) = installed {
                    self.track_consent(installed.identity);
                }
                self.toast_success(message, window, cx);
                if !missing.is_empty() {
                    self.show_missing_components(&missing, window, cx);
                }
            }
            (PluginOp::SetEnabled, Ok(_)) => {}
            (op, Err(error)) => {
                let detail = error_text(translator, &error);
                let key = match op {
                    PluginOp::Install => "pluginOpInstallFailed",
                    PluginOp::Update => "pluginOpUpdateFailed",
                    PluginOp::Uninstall => "pluginOpUninstallFailed",
                    PluginOp::SetEnabled => "pluginOpEnabledFailed",
                    PluginOp::Reload => "pluginOpReloadFailed",
                };
                let message = translator.text_with(key, &[("message", &detail)]);
                self.toast_error(message, window, cx);
            }
        }
    }

    fn show_missing_components(
        &self,
        missing: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        let names = missing
            .iter()
            .map(|component| {
                COMPONENT_KINDS
                    .into_iter()
                    .find(|kind| component_wire_name(*kind) == component)
                    .map(|kind| {
                        translator
                            .text(super::managed_components::title_key(kind))
                            .to_owned()
                    })
                    .unwrap_or_else(|| component.clone())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let title = SharedString::from(translator.text("pluginDepsMissingTitle").to_owned());
        let body = SharedString::from(
            translator.text_with("pluginDepsMissingBody", &[("components", &names)]),
        );
        let later = SharedString::from(translator.text("pluginDepsLater").to_owned());
        let go = SharedString::from(translator.text("pluginDepsGoToComponents").to_owned());
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let view = view.clone();
            alert
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(body.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(later.clone()),
                    go.clone(),
                    fluxdown_ui_components::DialogIntent::Confirm,
                    cx,
                ))
                .on_ok(move |_, _, cx| {
                    // 页面已释放时无法跳转，不能报告确认成功。
                    view.update(cx, |this, cx| this.show_tab(ExtensionsTab::Components, cx))
                        .is_ok()
                })
        });
    }

    fn confirm_uninstall_plugin(
        &self,
        identity: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translator = self.translator.read(cx);
        let title = SharedString::from(translator.text("pluginUninstallTitle").to_owned());
        let body =
            SharedString::from(translator.text_with("pluginUninstallMsg", &[("name", &name)]));
        let ok = SharedString::from(translator.text("pluginUninstallTooltip").to_owned());
        let cancel = SharedString::from(translator.text("cancel").to_owned());
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let view = view.clone();
            let identity = identity.clone();
            alert
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .description(body.clone())
                .footer(fluxdown_ui_components::dialog_footer(
                    Some(cancel.clone()),
                    ok.clone(),
                    fluxdown_ui_components::DialogIntent::Destructive,
                    cx,
                ))
                .on_ok(move |_, window, cx| {
                    // 页面已释放时未提交卸载，不能报告确认成功。
                    view.update(cx, |this, cx| {
                        let future = this.controller.uninstall_plugin(identity.clone());
                        this.run_plugin_op(
                            identity.clone(),
                            PluginOp::Uninstall,
                            future,
                            window,
                            cx,
                        );
                    })
                    .is_ok()
                })
        });
    }

    fn open_plugin_settings(&self, identity: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plugin) = self.controller.plugin(identity) else {
            return;
        };
        let translator = self.translator.read(cx);
        let title = SharedString::from(
            translator.text_with("pluginSettingsDialogTitle", &[("name", &plugin.name)]),
        );
        let save = SharedString::from(translator.text("pluginSettingsSaveButton").to_owned());
        let saving = SharedString::from(translator.text("pluginSettingsSaving").to_owned());
        let cancel = SharedString::from(translator.text("cancel").to_owned());
        let translator_entity = self.translator.clone();
        let port = self.controller.port().clone();
        let view = match (self.settings_view(plugin), &self.plugin_view_factory) {
            (Some(decl), Some(factory)) => Some(factory(
                plugin,
                decl,
                None,
                PluginViewSlot::Dialog,
                window,
                cx,
            )),
            _ => None,
        };
        let form = (!plugin.settings.is_empty()).then(|| {
            cx.new(|cx| {
                PluginSettingsForm::new(translator_entity.clone(), port, plugin, window, cx)
            })
        });
        let tabs = cx.new(|_| PluginSettingsTabs::new(translator_entity.clone(), view, form));
        // 设置表单刻意保留打开时的快照（避免编辑中途被覆盖），但插件被卸载时关闭对话框。
        let guard = CloseWhenRemoved::wrap(
            tabs.clone(),
            plugin.identity.clone(),
            &self.live_plugins,
            window,
            cx,
        );
        window.open_dialog(cx, move |dialog, _, cx| {
            let (is_saving, active_form) = {
                let tabs = tabs.read(cx);
                (tabs.is_saving(cx), tabs.active_form())
            };
            let tabs_for_content = guard.clone();
            let footer = DialogFooter::new()
                .gap(active_theme(cx).tokens().spacing.sm)
                .child(
                    Button::new("plugin-settings-cancel")
                        .outline()
                        .control(cx)
                        .label(cancel.clone())
                        .disabled(is_saving)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .when_some(active_form, |footer, form| {
                    footer.child(
                        Button::new("plugin-settings-save")
                            .primary()
                            .control(cx)
                            .label(if is_saving {
                                saving.clone()
                            } else {
                                save.clone()
                            })
                            .busy(is_saving)
                            .disabled(is_saving)
                            .on_click(move |_, window, cx| {
                                form.update(cx, |form, cx| form.submit(window, cx));
                            }),
                    )
                });
            dialog
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .w(active_theme(cx).text_extent(560.))
                .overlay_closable(!is_saving)
                // min_h_0：窗口矮于对话框时让内容区收缩，交给表单内部滚动。
                .content(move |content, _, _| content.min_h_0().child(tabs_for_content.clone()))
                .footer(footer)
        });
    }

    fn open_plugin_auth(&self, identity: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plugin) = self.controller.plugin(identity) else {
            return;
        };
        let title = SharedString::from(
            self.translator
                .read(cx)
                .text_with("pluginAuthDialogTitle", &[("name", &plugin.name)]),
        );
        // 插件自绘登录界面：弹窗只承载该视图（插件经 `ctx.close()` 自行关闭弹窗）。
        if let (Some(decl), Some(factory)) = (self.auth_view(plugin), &self.plugin_view_factory) {
            let view = factory(plugin, decl, None, PluginViewSlot::Dialog, window, cx);
            let view = CloseWhenRemoved::wrap(
                view,
                plugin.identity.clone(),
                &self.live_plugins,
                window,
                cx,
            );
            window.open_dialog(cx, move |dialog_view, _, cx| {
                let view = view.clone();
                dialog_view
                    .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                    .w(active_theme(cx).text_extent(520.))
                    .content(move |content, _, cx| {
                        content.min_h_0().child(
                            div()
                                .w_full()
                                .h(active_theme(cx).text_extent(420.))
                                .overflow_hidden()
                                .child(view.clone()),
                        )
                    })
            });
            return;
        }
        let translator = self.translator.clone();
        let port = self.controller.port().clone();
        let identity = plugin.identity.clone();
        let dialog = cx.new(|cx| PluginAuthDialog::new(translator, port, identity, window, cx));
        let guard = CloseWhenRemoved::wrap(
            dialog.clone(),
            plugin.identity.clone(),
            &self.live_plugins,
            window,
            cx,
        );
        let dialog_for_cancel = dialog.clone();
        window.open_dialog(cx, move |dialog_view, _, cx| {
            let dialog_for_content = guard.clone();
            let dialog_for_cancel = dialog_for_cancel.clone();
            dialog_view
                .title(fluxdown_ui_components::dialog_title(title.clone(), cx))
                .w(active_theme(cx).text_extent(520.))
                .on_cancel(move |_, _, cx| {
                    dialog_for_cancel.update(cx, |this, cx| this.cancel_session(cx));
                    true
                })
                .content(move |content, _, _| content.min_h_0().child(dialog_for_content.clone()))
        });
    }
}

impl ExtensionsView {
    /// 插件的 settings 位置视图（需要 app 已注入视图工厂且插件已启用）。
    fn settings_view<'a>(
        &self,
        plugin: &'a PluginDto,
    ) -> Option<&'a fluxdown_protocol::PluginViewDeclDto> {
        if self.plugin_view_factory.is_none() || !plugin.enabled {
            return None;
        }
        plugin
            .contributions
            .views
            .iter()
            .find(|view| view.location == "settings")
    }

    /// 插件的 auth 位置视图：插件可用（已启用、已加载、无待授权）、视图工厂已注入，且视图在
    /// 本机可渲染（可移植，或桌面脚本包在本机）时才接管登录弹窗；否则沿用固定登录流程。
    fn auth_view<'a>(
        &self,
        plugin: &'a PluginDto,
    ) -> Option<&'a fluxdown_protocol::PluginViewDeclDto> {
        if self.plugin_view_factory.is_none()
            || !plugin.enabled
            || plugin.load_status != "Loaded"
            || !plugin.consent_pending.is_empty()
            || !plugin.consent_pending_hosts.is_empty()
        {
            return None;
        }
        plugin
            .contributions
            .views
            .iter()
            .find(|view| view.location == "auth")
            .filter(|view| {
                view.portable
                    || view
                        .desktop
                        .as_ref()
                        .zip(plugin.package_dir.as_ref())
                        .is_some_and(|(entry, dir)| {
                            std::path::Path::new(dir).join(&entry.entry).is_file()
                        })
            })
    }

    fn open_consent(&self, plugin: PluginDto, window: &mut Window, cx: &mut Context<Self>) {
        let request = ConsentRequest::for_plugin(&plugin, self.translator.read(cx));
        let action = ConsentAction::Grant {
            port: self.controller.port().clone(),
            plugin: Box::new(plugin),
            live: self.live_plugins.clone(),
        };
        open_consent_dialog(request, action, self.translator.clone(), window, cx);
    }

    /// 打开审计对话框；`filter` = 预选插件。
    fn open_audit_dialog(
        &self,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_plugin_audit(
            plugin_choices(self.controller.plugins()),
            filter,
            self.translator.clone(),
            self.controller.port().clone(),
            window,
            cx,
        );
    }

    fn show_plugin_detail(&self, identity: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plugin) = self.controller.plugin(identity) else {
            return;
        };
        let mut detail = PluginDetail::from_plugin(plugin);
        detail.stale = self.controller.is_stale();
        let translator = self.translator.read(cx).clone();
        let choices = plugin_choices(self.controller.plugins());
        let audit_translator = self.translator.clone();
        let port = self.controller.port().clone();
        let archive_port = port.clone();
        let identity = identity.to_owned();
        let open_audit: OpenAudit = Rc::new(move |window, cx| {
            open_plugin_audit(
                choices.clone(),
                Some(identity.clone()),
                audit_translator.clone(),
                port.clone(),
                window,
                cx,
            );
        });
        open_plugin_detail(
            detail,
            translator,
            Some(open_audit),
            Some(self.live_plugins.clone()),
            Some(archive_port),
            window,
            cx,
        );
    }

    /// 安装 / 更新 / 重载成功：等插件列表追上后再判定授权流程。
    fn track_consent(&mut self, identity: String) {
        let confirmed = self.plugins.market_confirmed.remove(&identity);
        self.plugins.awaiting_consent.insert(identity, confirmed);
    }

    /// 列表中已追上、且确有待授权项的插件。
    fn ready_consents(&self) -> Vec<String> {
        self.plugins
            .awaiting_consent
            .keys()
            .filter(|identity| {
                self.controller
                    .plugin(identity)
                    .is_some_and(has_pending_consent)
            })
            .cloned()
            .collect()
    }

    /// 列表整体替换后：已追上且无待授权项（或已不存在）的记录不再等待。
    pub(crate) fn prune_awaiting_consent(&mut self) {
        let controller = &self.controller;
        self.plugins
            .awaiting_consent
            .retain(|identity, _| controller.plugin(identity).is_some_and(has_pending_consent));
    }

    /// 渲染中只读判定；有待处理项时经 `defer_in` 在渲染之外弹窗 / 调用授权。
    fn schedule_ready_consents(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugins.awaiting_consent.is_empty()
            || self.controller.is_stale()
            || self.ready_consents().is_empty()
        {
            return;
        }
        cx.defer_in(window, |this, window, cx| {
            this.run_consent_flows(window, cx)
        });
    }

    fn run_consent_flows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for identity in self.ready_consents() {
            let Some(confirmed) = self.plugins.awaiting_consent.remove(&identity) else {
                continue;
            };
            let Some(plugin) = self.controller.plugin(&identity).cloned() else {
                continue;
            };
            let flow = consent_flow(
                &plugin,
                confirmed
                    .as_ref()
                    .map(|(permissions, hosts)| (permissions.as_slice(), hosts.as_slice())),
            );
            match flow {
                ConsentFlow::None => {}
                ConsentFlow::Dialog => self.open_consent(plugin, window, cx),
                ConsentFlow::Direct => self.grant_consent_directly(plugin, window, cx),
            }
        }
        cx.notify();
    }

    /// 用户安装前已确认的集合正好等于待授权项：不再弹窗，直接授权；失败则回落到对话框。
    fn grant_consent_directly(
        &self,
        plugin: PluginDto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let future = grant_plugin_consent(self.controller.port(), &plugin);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result {
                    let translator = this.translator.read(cx);
                    let detail = error_text(translator, &error);
                    let message =
                        translator.text_with("pluginConsentFailed", &[("message", &detail)]);
                    this.toast_error(message, window, cx);
                    this.open_consent(plugin, window, cx);
                }
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }
}

/// 插件有待授权的权限或网络主机。
fn has_pending_consent(plugin: &PluginDto) -> bool {
    !plugin.consent_pending.is_empty() || !plugin.consent_pending_hosts.is_empty()
}

/// 审计筛选候选：（identity，展示名）。
fn plugin_choices(plugins: &[PluginDto]) -> Vec<(String, String)> {
    plugins
        .iter()
        .map(|plugin| (plugin.identity.clone(), plugin.name.clone()))
        .collect()
}

fn parse_market_entries(
    value: serde_json::Value,
) -> Result<Vec<MarketEntryDto>, fluxdown_protocol::RpcErrorData> {
    serde_json::from_value::<Vec<MarketEntryDto>>(value).map_err(|_| {
        fluxdown_protocol::RpcErrorData::new(
            fluxdown_protocol::ApplicationErrorCode::ProtocolIncompatible,
            false,
        )
    })
}

/// 列表卡片：一张 surface 卡片内纵向排列各行，行间 hairline 分隔（与下载表格同一「少线」风格）。
pub(super) fn list_card(rows: Vec<gpui::AnyElement>, frame: Frame<'_>, cx: &gpui::App) -> Div {
    let count = rows.len();
    card(cx)
        .w_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .children(rows.into_iter().enumerate().flat_map(move |(index, row)| {
            let divider = (index + 1 < count).then(|| ui::divider(frame).into_any_element());
            std::iter::once(row).chain(divider)
        }))
}

/// 列表行：横向排布、行内控件间距 `spacing.sm`，内边距 md / sm，悬停 `row_hover`。
pub(super) fn list_row(frame: Frame<'_>) -> Div {
    let hover = frame.extended.colors.row_hover;
    h_flex()
        .w_full()
        .gap(frame.tokens.spacing.sm)
        .items_center()
        .px(frame.tokens.spacing.md)
        .py(frame.tokens.spacing.sm)
        .hover(move |style| style.bg(hover))
}

/// 插件行文字区：标题行 / 来源行 / 描述行纵向排列，占满操作区以外的宽度。
fn row_text(
    title: Div,
    source: Option<Div>,
    description: Option<gpui::Stateful<Div>>,
    frame: Frame<'_>,
) -> Div {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(frame.tokens.spacing.xxs)
        .child(title)
        .children(source)
        .children(description)
}

/// 标题行：名称 + 版本 + 徽标单行排布；空间不足时只截断名称，徽标与版本保持完整。
fn row_title(name: impl Into<SharedString>, version: &str, frame: Frame<'_>) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .gap(frame.tokens.spacing.sm)
        .items_center()
        .child(ui::title_text(name, frame).min_w_0().truncate())
        .child(
            ui::meta_text(format!("v{version}"), frame)
                .flex_none()
                .font_features(tabular_numbers()),
        )
}

/// 来源行：作者 · 主页（去协议的短地址，过长截断）；两者皆空时不渲染。
/// 只有 http(s) 主页可点击（见 [`ui::is_web_url`]），平时保持二级文字色，悬停才显示链接色。
fn row_source(
    id: impl Into<gpui::ElementId>,
    author: &str,
    homepage: &str,
    frame: Frame<'_>,
) -> Option<Div> {
    let homepage = homepage.trim();
    if author.is_empty() && homepage.is_empty() {
        return None;
    }
    let xs = frame.tokens.typography.xs;
    let muted = frame.tokens.colors.muted_foreground;
    let link = (!homepage.is_empty()).then(|| {
        let text = ui::display_url(homepage).to_owned();
        if ui::is_web_url(homepage) {
            Link::new(id)
                .href(homepage.to_owned())
                .min_w_0()
                .truncate()
                .text_size(xs.size)
                .line_height(xs.line_height)
                .text_color(muted)
                .text_decoration_color(muted.opacity(0.4))
                .child(text)
                .into_any_element()
        } else {
            ui::meta_text(text, frame)
                .min_w_0()
                .truncate()
                .into_any_element()
        }
    });
    let has_author = !author.is_empty();
    Some(
        h_flex()
            .w_full()
            .min_w_0()
            .gap(frame.tokens.spacing.xs)
            .items_center()
            .when(has_author, |this| {
                this.child(ui::meta_text(author.to_owned(), frame).flex_none())
            })
            .when(has_author && link.is_some(), |this| {
                this.child(ui::meta_text("·", frame).flex_none())
            })
            .children(link),
    )
}

/// 描述行：单行截断，悬停显示全文。
fn row_description(
    id: impl Into<gpui::ElementId>,
    description: &str,
    frame: Frame<'_>,
) -> Option<gpui::Stateful<Div>> {
    if description.is_empty() {
        return None;
    }
    let tooltip = SharedString::from(description.to_owned());
    Some(
        ui::meta_text(tooltip.clone(), frame)
            .id(id)
            .w_full()
            .truncate()
            .tooltip(move |window, cx| tooltip_view(tooltip.clone(), window, cx)),
    )
}
