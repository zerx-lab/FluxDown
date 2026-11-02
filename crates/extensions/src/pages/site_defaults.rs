//! 站点默认插件子页：多个插件的 resolver 匹配同一站点时，用户记住的（或无人值守下载自动选用、
//! 待确认的）插件。读 / 改均走 `daemon.plugin.siteDefaults` / `daemon.plugin.setSiteDefault`；
//! 每次进入子页重新读取。

use std::collections::HashSet;

use fluxdown_protocol::{ApplicationErrorCode, PluginDto, PluginSiteDefaultDto, RpcErrorData};
use fluxdown_ui_components::{BusyExt as _, ControlExt as _, FluxIcon};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, Context, IntoElement, ParentElement, SharedString, Styled, Window,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex,
};

use super::{
    Frame,
    plugins::{list_card, list_row},
};
use crate::{ExtensionsView, error_text, ui};

/// 站点默认插件子页的视图状态。
#[derive(Default)]
pub(crate) struct SiteDefaultsUi {
    /// 本次进入子页是否已发起读取。
    pub requested: bool,
    pub loading: bool,
    pub entries: Vec<PluginSiteDefaultDto>,
    /// 至少成功读到过一次（含 daemon 不支持）；此前列表为空不代表没有记录，不显示空状态。
    pub loaded: bool,
    /// 有写操作在途的站点（禁用该行控件）。
    pub busy: HashSet<String>,
    /// `busy` 中由「移除」发起的站点（仅「移除」按钮转圈，其余写操作转「确认」）。
    pub removing: HashSet<String>,
    /// 上次读取因断连失败；重连后重新读取一次。
    pub retry_on_reconnect: bool,
}

/// 候选插件展示名：优先插件列表中的名称，缺失（已卸载 / 未加载）时回退 identity。
pub(crate) fn plugin_display_name<'a>(plugins: &'a [PluginDto], identity: &'a str) -> &'a str {
    plugins
        .iter()
        .find(|plugin| plugin.identity == identity)
        .map_or(identity, |plugin| plugin.name.as_str())
}

fn parse_site_defaults(
    value: serde_json::Value,
) -> Result<Vec<PluginSiteDefaultDto>, RpcErrorData> {
    serde_json::from_value::<Vec<PluginSiteDefaultDto>>(value)
        .map_err(|_| RpcErrorData::new(ApplicationErrorCode::ProtocolIncompatible, false))
}

impl ExtensionsView {
    pub(crate) fn ensure_site_defaults_loaded(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = &self.plugins.site_defaults;
        if self.controller.is_stale() || state.loading {
            return;
        }
        if state.requested && !state.retry_on_reconnect {
            return;
        }
        self.load_site_defaults(window, cx);
    }

    fn load_site_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.plugins.site_defaults;
        state.requested = true;
        state.loading = true;
        state.retry_on_reconnect = false;
        let future = self.controller.site_defaults();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await.and_then(parse_site_defaults);

            let Ok(()) = this.update(cx, |this, cx| {
                let stale = this.controller.is_stale();
                let state = &mut this.plugins.site_defaults;
                state.loading = false;
                match result {
                    Ok(entries) => {
                        state.entries = entries;
                        state.loaded = true;
                    }
                    // 不带插件的 daemon：没有该功能，按空列表显示空状态。
                    Err(error) if error.code == ApplicationErrorCode::Unsupported => {
                        state.entries.clear();
                        state.loaded = true;
                    }
                    // 其余失败保留已有列表；断连类失败在 daemon 恢复后自动重试一次。
                    Err(error) => {
                        state.retry_on_reconnect = stale
                            && matches!(
                                error.code,
                                ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout
                            );
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

    /// 站点默认被外部或本视图改动（`PluginsChanged` / 快照替换 / 写操作结束）：
    /// 下次渲染重新读取；读取在途时待其结束后再读一次，避免旧结果覆盖。
    pub(crate) fn invalidate_site_defaults(&mut self) {
        self.plugins.site_defaults.requested = false;
    }

    /// `identity = None` 移除该站点记录；`Some` 确认 / 更改默认插件。无论成败都重新读取。
    fn set_site_default(
        &mut self,
        site: String,
        identity: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.plugins.site_defaults.busy.insert(site.clone()) {
            return;
        }
        if identity.is_none() {
            self.plugins.site_defaults.removing.insert(site.clone());
        }
        cx.notify();
        let future = self.controller.set_site_default(site.clone(), identity);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.plugins.site_defaults.busy.remove(&site);
                this.plugins.site_defaults.removing.remove(&site);
                if let Err(error) = result {
                    let translator = this.translator.read(cx);
                    let detail = error_text(translator, &error);
                    let message =
                        translator.text_with("pluginSiteDefaultSaveFailed", &[("error", &detail)]);
                    this.toast_error(message, window, cx);
                }
                // 守护进程写入后会广播 `PluginsChanged`；失败时没有事件，因此无论成败都标记过期，
                // 由渲染入口串行重读（读取在途时不会并发再发一次）。
                this.invalidate_site_defaults();
                cx.notify();
            }) else {
                // 视图或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 「站点默认插件」子页：标题说明 + 记录列表（为空时空状态）。
    pub(crate) fn render_site_defaults(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_site_defaults_loaded(window, cx);
        let theme = active_theme(cx);
        let frame = Frame {
            translator: self.translator.read(cx),
            tokens: theme.tokens(),
            extended: theme.extended(),
            components: theme.components(),
            stale: self.controller.is_stale(),
        };
        let translator = frame.translator;
        let state = &self.plugins.site_defaults;
        let body = if !state.entries.is_empty() {
            Some(list_card(self.site_default_rows(frame, cx), frame, cx).into_any_element())
        } else if !state.loaded {
            // 尚未成功读取（在途 / 断连 / 失败）：不谎称「暂无记录」。
            None
        } else {
            Some(
                ui::empty_state(
                    FluxIcon::Package,
                    translator.text("pluginSiteDefaultsEmpty").to_owned(),
                    None,
                    frame,
                )
                .into_any_element(),
            )
        };
        v_flex()
            .w_full()
            .gap(frame.tokens.spacing.md)
            .child(
                v_flex()
                    .w_full()
                    .gap(frame.tokens.spacing.xxs)
                    .child(ui::title_text(
                        translator.text("pluginSiteDefaultsTitle").to_owned(),
                        frame,
                    ))
                    .child(ui::meta_text(
                        translator.text("pluginSiteDefaultsDesc").to_owned(),
                        frame,
                    )),
            )
            .children(body)
    }

    fn site_default_rows(&self, frame: Frame<'_>, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let entries = &self.plugins.site_defaults.entries;
        let Frame {
            translator,
            tokens,
            stale,
            ..
        } = frame;
        let plugins = self.controller.plugins();
        let view = cx.entity();
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let site_busy = self.plugins.site_defaults.busy.contains(&entry.site);
                let removing = self.plugins.site_defaults.removing.contains(&entry.site);
                let busy = stale || site_busy;
                let current_name =
                    SharedString::from(plugin_display_name(plugins, &entry.identity).to_owned());
                let options = entry
                    .candidates
                    .iter()
                    .map(|identity| {
                        (
                            identity.clone(),
                            SharedString::from(plugin_display_name(plugins, identity).to_owned()),
                        )
                    })
                    .collect::<Vec<_>>();
                let site = entry.site.clone();
                let current = entry.identity.clone();
                let automatic = entry.automatic;
                let picker = Button::new(("site-default-pick", index))
                    .outline()
                    .label(current_name)
                    .dropdown_caret(true)
                    .control(cx)
                    .disabled(busy)
                    .dropdown_menu_with_anchor(Anchor::TopRight, {
                        let view = view.clone();
                        let site = site.clone();
                        move |menu, _, _| {
                            options
                                .iter()
                                .fold(menu, |menu, (identity, name)| {
                                    let view = view.clone();
                                    let site = site.clone();
                                    let target = identity.clone();
                                    let selected = *identity == current;
                                    menu.item(
                                        PopupMenuItem::new(name.clone())
                                            .checked(selected)
                                            .on_click(move |_, window, cx| {
                                                // 已确认的默认项再选一次是空操作；自动项选同一个等同于确认。
                                                if selected && !automatic {
                                                    return;
                                                }
                                                let site = site.clone();
                                                let target = target.clone();
                                                view.update(cx, |this, cx| {
                                                    this.set_site_default(
                                                        site,
                                                        Some(target),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                })
                                .scrollable(true)
                                .max_h(px(320.))
                        }
                    });
                list_row(frame)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(tokens.spacing.xxs)
                            .child(
                                h_flex()
                                    .gap(tokens.spacing.sm)
                                    .items_center()
                                    .flex_wrap()
                                    .child(ui::title_text(entry.site.clone(), frame))
                                    .when(automatic, |this| {
                                        this.child(ui::tone_pill(
                                            translator
                                                .text("pluginSiteDefaultAutomatic")
                                                .to_owned(),
                                            frame.extended.colors.warning,
                                            frame,
                                        ))
                                    }),
                            )
                            .when(automatic, |this| {
                                this.child(ui::meta_text(
                                    translator.text("pluginSiteDefaultAutomaticHint").to_owned(),
                                    frame,
                                ))
                            }),
                    )
                    .child(picker)
                    .when(automatic, |this| {
                        let site = site.clone();
                        let identity = entry.identity.clone();
                        this.child(
                            Button::new(("site-default-confirm", index))
                                .primary()
                                .control(cx)
                                .label(translator.text("pluginSiteDefaultConfirm").to_owned())
                                .busy(site_busy && !removing)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_site_default(
                                        site.clone(),
                                        Some(identity.clone()),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child({
                        let site = site.clone();
                        Button::new(("site-default-remove", index))
                            .ghost()
                            .control(cx)
                            .label(translator.text("pluginSiteDefaultRemove").to_owned())
                            .busy(removing)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_site_default(site.clone(), None, window, cx);
                            }))
                    })
                    .into_any_element()
            })
            .collect()
    }
}
