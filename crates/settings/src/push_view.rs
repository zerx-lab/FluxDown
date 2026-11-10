//! 活动栏「推送通知」页：右上角分段标签在「云端推送」（FluxCloud 代发）与「自托管 Webhook」之间切换。
//!
//! 复用 app 常驻的 `SettingsStore` 与既有 agent 会话，不挂载到设置分类或下载页；页面自身只持有
//! 当前标签与「已为哪个账号拉取过云端数据」的记录，其余状态全部在 store 里。

use std::rc::Rc;

use fluxdown_ui_components::segmented_tabs;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
};
use gpui_component::{h_flex, scroll::ScrollableElement as _, v_flex};

use crate::model::cloud_notify::{
    PushTab, cloud_enabled, default_tab, effective_tab, page_desc_key, show_try_cloud, tabs_visible,
};
use crate::sections::cloud_push::{self, CloudPage};
use crate::sections::{SectionContext, webhook};
use crate::store::{SettingsErrorKind, SettingsStore};
use crate::ui::{
    CONTENT_PADDING_LEFT, CONTENT_PADDING_RIGHT, field_width_of, meta_text, page_heading,
};

/// 宿主 / 页面内的窗口动作回调。
pub type WindowAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// app 注入的宿主动作（跨能力的跳转由 app 装配，本 crate 不认识其他页面）。
#[derive(Clone)]
pub struct PushHost {
    /// 打开账户页（登录 / 升级套餐都在那里）。
    pub open_account: WindowAction,
}

/// 主窗口中的独立「推送通知」页面。
pub struct PushView {
    store: Entity<SettingsStore>,
    translator: Entity<Translator>,
    host: PushHost,
    /// 已锁定的标签；`None` = 尚未确定（云端开放且 daemon 快照到达后按默认规则确定一次，
    /// 之后只跟随用户选择）。
    tab: Option<PushTab>,
    /// 用户是否手动选过标签（选过则云端关闭再开放也保留其选择）。
    user_chose: bool,
    /// 本轮连接内已为「目录未知」请求过一次，避免每次 store 变化重发。
    catalog_requested: bool,
    /// 已为其拉取过概览的账号；离线 / 登出时清空，重连或换号后重新拉取。
    synced_account: Option<String>,
}

impl PushView {
    /// 复用 app 持有的配置存储；关闭设置窗口不影响事件订阅和待保存的修改。
    pub fn new(
        translator: Entity<Translator>,
        store: Entity<SettingsStore>,
        host: PushHost,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&translator, |_, _, cx| cx.notify()).detach();
        cx.observe(&store, |this, _, cx| {
            this.on_store_changed(cx);
            cx.notify();
        })
        .detach();
        let mut this = Self {
            store,
            translator,
            host,
            tab: None,
            user_chose: false,
            catalog_requested: false,
            synced_account: None,
        };
        this.on_store_changed(cx);
        this
    }

    fn feedback(&self, cx: &Context<Self>) -> Option<SharedString> {
        let store = self.store.read(cx);
        let translator = self.translator.read(cx);
        if !store.daemon_connected() {
            return Some(
                translator
                    .text(SettingsErrorKind::Disconnected.i18n_key())
                    .to_owned()
                    .into(),
            );
        }
        store.last_error().map(|error| {
            let mut text = translator.text(error.kind.i18n_key()).to_owned();
            if error.kind == SettingsErrorKind::InvalidArgument && !error.detail.is_empty() {
                text.push_str(": ");
                text.push_str(&error.detail);
            }
            text.into()
        })
    }

    fn self_hosted_endpoints(&self, cx: &App) -> usize {
        webhook::read_endpoints(self.store.read(cx)).len()
    }

    /// 云端推送是否对本客户端开放（服务端目录非空）。
    fn cloud_on(&self, cx: &App) -> bool {
        cloud_enabled(self.store.read(cx).cloud_notify().catalog.as_deref())
    }

    /// 当前生效的标签：云端关闭恒为自托管；否则用户选择 > 已锁定的默认 > 实时默认规则。
    fn effective_tab(&self, cx: &App) -> PushTab {
        effective_tab(self.cloud_on(cx), self.tab, self.self_hosted_endpoints(cx))
    }

    /// 用户点击标签：此后默认规则不再参与。
    fn set_tab(&mut self, tab: PushTab, cx: &mut Context<Self>) {
        self.user_chose = true;
        if self.tab == Some(tab) {
            return;
        }
        self.tab = Some(tab);
        if tab == PushTab::Cloud {
            self.sync_cloud(cx);
        }
        cx.notify();
    }

    /// store 变化（快照 / 事件 / 请求完成）：维护默认标签，并在云端标签下按需拉取云端数据。
    /// 全部在事件回调里完成，render 只读。
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let (connected, read_only, catalog_known) = {
            let store = self.store.read(cx);
            (
                store.daemon_connected(),
                store.is_read_only(),
                store.cloud_notify().catalog.is_some(),
            )
        };
        if read_only {
            self.catalog_requested = false;
        }
        // 目录未知时请求一次（agent 启动拉取失败 / 尚未完成时的兜底；匿名即可）。
        if connected && !read_only && !catalog_known && !self.catalog_requested {
            self.catalog_requested = true;
            self.store
                .update(cx, |store, cx| store.refresh_cloud_notify(false, cx));
        }
        if !self.cloud_on(cx) {
            // 云端关闭：未手动选过标签则丢弃已锁定的默认，重新开放后按默认规则重新判定。
            if !self.user_chose {
                self.tab = None;
            }
        } else if self.tab.is_none() && connected {
            self.tab = Some(default_tab(self.self_hosted_endpoints(cx)));
        }
        if self.effective_tab(cx) == PushTab::Cloud {
            self.sync_cloud(cx);
        }
    }

    /// 首次 / 换号 / 重连后读取一次概览（agent 缓存，过期时它会后台刷新并推送）。
    /// 之后概览、投递记录都由 agent 经 `CloudNotifyChanged` 实时推送，页面不再主动拉取。
    fn sync_cloud(&mut self, cx: &mut Context<Self>) {
        let (account, read_only) = {
            let store = self.store.read(cx);
            (
                store.session().map(|session| session.user.id.clone()),
                store.is_read_only(),
            )
        };
        // 断线期间不发请求；恢复后以「未同步」状态重新拉取。
        let account = account.filter(|_| !read_only);
        let Some(account) = account else {
            self.synced_account = None;
            return;
        };
        if self.synced_account.as_deref() != Some(account.as_str()) {
            self.synced_account = Some(account);
            self.store
                .update(cx, |store, cx| store.refresh_cloud_notify(false, cx));
        }
    }
}

impl Render for PushView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let hairline = active_theme(cx).extended().colors.hairline;
        let translator = self.translator.read(cx).clone();
        let cloud_on = self.cloud_on(cx);
        let tab = self.effective_tab(cx);
        let feedback = self.feedback(cx);

        let view = cx.entity().downgrade();
        // 云端未开放时没有分段标签，页面就是自托管内容。
        let tabs = tabs_visible(cloud_on).then(|| {
            let labels: Vec<SharedString> = PushTab::ALL
                .iter()
                .map(|tab| SharedString::from(translator.text(tab.label_key()).to_owned()))
                .collect();
            let selected = PushTab::ALL
                .iter()
                .position(|candidate| *candidate == tab)
                .unwrap_or(0);
            segmented_tabs(
                "push-tabs",
                labels,
                selected,
                {
                    let view = view.clone();
                    move |index, _, cx| {
                        let Some(tab) = PushTab::ALL.get(index).copied() else {
                            return;
                        };
                        let Ok(()) = view.update(cx, |this, cx| this.set_tab(tab, cx)) else {
                            // 页面已释放，结束回调，不再更新状态。
                            return;
                        };
                    }
                },
                cx,
            )
        });

        let body = match tab {
            PushTab::Cloud => cloud_push::render(
                &CloudPage {
                    store: &self.store,
                    translator: &translator,
                    host: &self.host,
                },
                window,
                cx,
            ),
            PushTab::SelfHosted => {
                let ctx = SectionContext {
                    store: &self.store,
                    translator: &translator,
                };
                // 云端关闭时没有可切换的目标，空态不显示「改用云端推送」。
                let try_cloud: Option<WindowAction> = show_try_cloud(cloud_on).then(|| {
                    Rc::new(move |_: &mut Window, cx: &mut App| {
                        let Ok(()) = view.update(cx, |this, cx| this.set_tab(PushTab::Cloud, cx))
                        else {
                            // 页面已释放，结束回调，不再更新状态。
                            return;
                        };
                    }) as WindowAction
                });
                let sections = [
                    webhook::endpoints_group(&ctx, try_cloud, cx),
                    webhook::delivery_log_group(&ctx, cx),
                ];
                let field_width = field_width_of(&sections, cx);
                v_flex()
                    .w_full()
                    .gap(tokens.spacing.lg)
                    .children(sections.iter().enumerate().map(|(index, section)| {
                        section
                            .render("webhooks", index, field_width, window, cx)
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        };

        let body_id = match tab {
            PushTab::Cloud => "push-body-cloud",
            PushTab::SelfHosted => "push-body-self-hosted",
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(tokens.colors.surface)
            .text_color(tokens.colors.foreground)
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(tokens.spacing.lg)
                    .px(px(CONTENT_PADDING_LEFT))
                    .pt(tokens.spacing.lg)
                    .pb(tokens.spacing.md)
                    .border_b_1()
                    .border_color(hairline)
                    .child(div().flex_1().min_w_0().child(page_heading(
                        translator.text("pushNavTitle").to_owned(),
                        translator.text(page_desc_key(cloud_on)).to_owned(),
                        cx,
                    )))
                    .children(tabs),
            )
            .children(feedback.map(|text| {
                meta_text(cx)
                    .px(px(CONTENT_PADDING_LEFT))
                    .py(tokens.spacing.sm)
                    .text_color(tokens.colors.destructive)
                    .child(text)
            }))
            .child(
                v_flex()
                    .id(SharedString::from(body_id))
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .pl(px(CONTENT_PADDING_LEFT))
                    .pr(px(CONTENT_PADDING_RIGHT))
                    .pt(tokens.spacing.lg + tokens.spacing.xs)
                    .pb(tokens.spacing.xl)
                    .overflow_y_scrollbar()
                    .child(body),
            )
    }
}
