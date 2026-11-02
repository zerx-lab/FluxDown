//! 设置窗口内容：左侧分类导航 + 搜索，右侧「标题 + 描述 + 子 Tab」头部与白色内容区。
//!
//! 按分类与子 Tab 分区；宽视口使用双列分组卡片。

use std::collections::HashMap;

use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, nav_icon_color, segmented_tabs, sidebar_navigation_button,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Icon, WindowExt as _,
    input::{Input, InputState},
    notification::Notification,
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::search::SettingsTarget;
use crate::sections::{
    self, SectionContext, about, api, appearance, bt, doctor, download, ed2k, general, notify,
    proxy, shortcuts,
};
use crate::store::{SettingsErrorKind, SettingsStore};
use crate::ui::{
    CONTENT_PADDING_LEFT, CONTENT_PADDING_RIGHT, SettingsPage, meta_text, page_heading,
};

/// 分类导航列宽（与下载侧栏默认宽一致）。
const SIDEBAR_WIDTH: f32 = 200.;

/// 「推介有奖」页的可见性：app 依「已登录 + 设备本地偏好」维护，设置窗口与搜索索引共用。
#[derive(Debug, Default)]
pub struct SlotVisibility {
    visible: bool,
}

impl SlotVisibility {
    #[must_use]
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// 更新可见性；值未变时不通知，避免无谓重绘。
    pub fn set(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            cx.notify();
        }
    }
}

/// 动态显隐的整页插槽：视图本身 + 控制它是否出现在侧栏 / 搜索里的可见性。
pub struct VisibleSlot {
    pub view: AnyView,
    pub visible: Entity<SlotVisibility>,
}

/// 构建页面列表时「推介有奖」页的取舍：`Hidden` 整页不出现；`Shown(None)` 仅供搜索索引
/// （没有真实视图，显示分类描述）。
pub(crate) enum ReferralPage {
    Hidden,
    Shown(Option<AnyView>),
}

/// app 注入的外部内容：账户 / 推介页与扩展页由对应 capability 提供；活动栏开关由 app 的
/// 活动栏注册表派生。
#[derive(Default)]
pub struct SettingsContentSlots {
    pub account: Option<AnyView>,
    pub extensions: Option<AnyView>,
    /// 「推介有奖」页（账户之后）；不可见时整页不出现在侧栏、搜索与直达里。
    pub referral: Option<VisibleSlot>,
    /// 「通用 → 活动栏」分区的开关行，按活动栏自上而下顺序。
    pub activity_bar: Vec<ActivityBarToggle>,
}

/// 活动栏可选入口的可见性开关：一个布尔偏好（缺省视为显示）+ 标题 / 描述文案键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityBarToggle {
    pub pref_key: &'static str,
    pub title_key: &'static str,
    pub desc_key: &'static str,
}

/// 设置能力的顶层页面。
pub struct SettingsView {
    store: Entity<SettingsStore>,
    translator: Entity<Translator>,
    slots: SettingsContentSlots,
    search: Entity<InputState>,
    /// 当前选中分类的 key。
    selected: SharedString,
    /// 每个分类会话内记住的子 Tab id。
    tab_by_page: HashMap<SharedString, &'static str>,
}

impl SettingsView {
    /// 创建设置页面，并订阅共享翻译状态与设置存储。
    ///
    /// `store` 由 app 持有并跨窗口复用：设置窗口关闭后防抖中的写回仍会完成，
    /// 快照/事件也持续进入同一存储。
    pub fn new(
        translator: Entity<Translator>,
        store: Entity<SettingsStore>,
        slots: SettingsContentSlots,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 搜索框占位只在创建时取一次文案：直接唤起设置窗口时语言可能尚未从偏好恢复，
        // 语言变化后要同步重设。
        cx.observe_in(&translator, window, |this, translator, window, cx| {
            let placeholder = translator.read(cx).text("settingsSearchHint").to_owned();
            this.search.update(cx, |search, cx| {
                search.set_placeholder(placeholder, window, cx)
            });
            cx.notify();
        })
        .detach();
        cx.observe_in(&store, window, |this, store, window, cx| {
            let snapshot = store.read(cx);
            let translator = this.translator.read(cx);
            let notification = match snapshot.last_error() {
                Some(error)
                    if matches!(
                        error.kind,
                        SettingsErrorKind::GatewayPortInUse
                            | SettingsErrorKind::GatewayRestartFailed
                    ) =>
                {
                    Some(Notification::error(
                        translator.text(error.kind.i18n_key()).to_owned(),
                    ))
                }
                None if snapshot.last_notice() == Some("apiServiceRestarted") => {
                    Some(Notification::success(translator.text_with(
                        "apiServiceRestarted",
                        &[("port", &snapshot.gateway().port.to_string())],
                    )))
                }
                _ => None,
            };
            if let Some(notification) = notification {
                store.update(cx, |store, cx| store.clear_feedback(cx));
                window.push_notification(notification, cx);
            }
            cx.notify();
        })
        .detach();
        // 站点凭据由新建任务等流程在窗口外写入且无事件推送：每次打开窗口重新加载一次。
        // 系统集成状态上次失败的话，重新打开窗口也给一次重试机会。
        store.update(cx, |store, _| {
            store.reset_load("siteAuth");
            store.reset_load("integration");
        });
        let placeholder = translator.read(cx).text("settingsSearchHint").to_owned();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        if let Some(slot) = &slots.referral {
            cx.observe(&slot.visible, |_, _, cx| cx.notify()).detach();
        }
        Self {
            store,
            translator,
            slots,
            search,
            selected: SharedString::from("general"),
            tab_by_page: HashMap::new(),
        }
    }

    #[must_use]
    pub fn store(&self) -> Entity<SettingsStore> {
        self.store.clone()
    }

    fn feedback(&self, cx: &Context<Self>) -> Option<(SharedString, bool)> {
        let store = self.store.read(cx);
        let translator = self.translator.read(cx);
        if let Some(error) = store.last_error() {
            if matches!(
                error.kind,
                SettingsErrorKind::GatewayPortInUse | SettingsErrorKind::GatewayRestartFailed
            ) {
                return None;
            }
            let mut text = translator.text(error.kind.i18n_key()).to_owned();
            if error.kind == SettingsErrorKind::InvalidArgument && !error.detail.is_empty() {
                text.push_str(": ");
                text.push_str(&error.detail);
            }
            return Some((SharedString::from(text), true));
        }
        store
            .last_notice()
            .filter(|key| *key != "apiServiceRestarted")
            .map(|key| (SharedString::from(translator.text(key).to_owned()), false))
    }

    /// 「推介有奖」页当前是否可见（无插槽即不可见）。
    fn referral_visible(&self, cx: &App) -> bool {
        self.slots
            .referral
            .as_ref()
            .is_some_and(|slot| slot.visible.read(cx).visible())
    }

    fn pages(&self, cx: &mut Context<Self>) -> Vec<SettingsPage> {
        let translator = self.translator.read(cx).clone();
        let ctx = SectionContext {
            store: &self.store,
            translator: &translator,
        };
        build_pages(
            &ctx,
            &self.slots.activity_bar,
            self.slots.account.clone(),
            self.slots.extensions.clone(),
            match &self.slots.referral {
                Some(slot) if slot.visible.read(cx).visible() => {
                    ReferralPage::Shown(Some(slot.view.clone()))
                }
                _ => ReferralPage::Hidden,
            },
            cx,
        )
    }

    /// 定位到设置项：切到目标分类与子 Tab；指向具体行时以行标题填入搜索框筛出该行，
    /// 指向分类 / Tab 时清空搜索。
    pub fn reveal(&mut self, target: &SettingsTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = SharedString::from(target.page);
        if !target.tab.is_empty() {
            self.tab_by_page
                .insert(SharedString::from(target.page), target.tab);
        }
        let query = target.row.clone().unwrap_or_default();
        self.search
            .update(cx, |search, cx| search.set_value(query, window, cx));
        cx.notify();
    }

    fn render_nav_item(&self, page: &SettingsPage, cx: &mut Context<Self>) -> impl IntoElement {
        let icon_size = active_theme(cx).extended().icon.lg;
        let selected = self.selected == page.title || self.selected == page.key;
        let key = SharedString::from(page.key);
        let icon_color = nav_icon_color(selected, cx);

        sidebar_navigation_button(
            SharedString::from(format!("settings-nav-{}", page.key)),
            page.title.clone(),
            page.nav_icon().size(icon_size).text_color(icon_color),
            div(),
            selected,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.selected != key {
                this.selected = key.clone();
                cx.notify();
            }
        }))
    }

    fn render_sidebar(&self, pages: &[SettingsPage], cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let sidebar_width = theme.text_extent(SIDEBAR_WIDTH);
        let extended = theme.extended().clone();
        let items: Vec<_> = pages
            .iter()
            .map(|page| self.render_nav_item(page, cx).into_any_element())
            .collect();

        v_flex()
            .flex_none()
            .w(sidebar_width)
            .h_full()
            .min_h_0()
            .px(tokens.spacing.sm)
            .py(tokens.spacing.md)
            .gap(tokens.spacing.md)
            .bg(extended.colors.chrome)
            .border_r_1()
            .border_color(extended.colors.hairline)
            .child(
                Input::new(&self.search).control(cx).prefix(
                    Icon::new(FluxIcon::Search)
                        .size(extended.icon.md)
                        .text_color(extended.colors.text_tertiary),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap(tokens.spacing.xxs)
                    .children(items)
                    .overflow_y_scrollbar(),
            )
    }

    fn active_tab_id(&self, page: &SettingsPage) -> &'static str {
        let tabs = page.visible_tabs();
        if tabs.is_empty() {
            return "";
        }
        let saved = self.tab_by_page.get(&SharedString::from(page.key)).copied();
        match saved {
            Some(id) if tabs.iter().any(|tab| tab.id == id) => id,
            _ => tabs.first().map_or("", |tab| tab.id),
        }
    }

    fn render_tabs(
        &self,
        page: &SettingsPage,
        tab_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let tabs = page.visible_tabs();
        if tabs.is_empty() {
            return None;
        }
        let ids: Vec<&'static str> = tabs.iter().map(|tab| tab.id).collect();
        let selected = ids.iter().position(|id| *id == tab_id).unwrap_or(0);
        let labels: Vec<SharedString> = tabs.iter().map(|tab| tab.label.clone()).collect();
        let key = SharedString::from(page.key);
        let view = cx.entity().downgrade();
        Some(segmented_tabs(
            SharedString::from(format!("settings-tabs-{}", page.key)),
            labels,
            selected,
            move |index, _, cx| {
                let Some(id) = ids.get(index).copied() else {
                    return;
                };
                let key = key.clone();
                // 标签点击回调可能晚于设置窗口关闭。

                let Ok(()) = view.update(cx, |this, cx| {
                    this.tab_by_page.insert(key, id);
                    cx.notify();
                }) else {
                    // 设置视图或窗口已释放，结束回调，不再更新状态。
                    return;
                };
            },
            cx,
        ))
    }

    fn render_content(
        &self,
        page: &SettingsPage,
        content_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let tab_id = self.active_tab_id(page);
        let tabs = self.render_tabs(page, tab_id, cx);
        let fill = page.tab_fills_height(tab_id);

        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(tokens.colors.surface)
            .child(
                v_flex()
                    .w_full()
                    .flex_none()
                    .px(px(CONTENT_PADDING_LEFT))
                    .pt(tokens.spacing.lg)
                    .pb(tokens.spacing.md)
                    .gap(tokens.spacing.md)
                    .border_b_1()
                    .border_color(extended.colors.hairline)
                    .child(page_heading(
                        page.title.clone(),
                        page.description.clone(),
                        cx,
                    ))
                    .children(tabs),
            )
            .child({
                let body = v_flex()
                    .id(gpui::ElementId::from(SharedString::from(format!(
                        "settings-body-{}-{tab_id}",
                        page.key
                    ))))
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .pl(px(CONTENT_PADDING_LEFT))
                    .pr(px(CONTENT_PADDING_RIGHT))
                    .pt(tokens.spacing.lg + tokens.spacing.xs)
                    .pb(tokens.spacing.xl)
                    .child(page.render_tab(tab_id, content_width, window, cx));
                // 占满高度的注入视图自管滚动（固定头部 + 局部滚动），宿主不再包滚动容器。
                if fill {
                    body.into_any_element()
                } else {
                    body.overflow_y_scrollbar().into_any_element()
                }
            })
    }
}

/// 全部设置分类页（设置窗口渲染与搜索索引共用同一份构建）。
pub(crate) fn build_pages(
    ctx: &SectionContext,
    activity_bar: &[ActivityBarToggle],
    account: Option<AnyView>,
    extensions: Option<AnyView>,
    referral: ReferralPage,
    cx: &mut App,
) -> Vec<SettingsPage> {
    let mut pages = vec![
        general::page(ctx, activity_bar, cx),
        sections::slot_page(
            ctx,
            "account",
            "settingsCatAccount",
            "settingsCatAccountDesc",
            FluxIcon::User,
            account,
        ),
    ];
    if let ReferralPage::Shown(view) = referral {
        pages.push(
            sections::slot_page(
                ctx,
                "referral",
                "settingsCatReferral",
                "settingsCatReferralDesc",
                FluxIcon::Gift,
                view,
            )
            .fill_height(),
        );
    }
    pages.extend([
        appearance::page(ctx, cx),
        shortcuts::page(ctx, cx),
        download::page(ctx, cx),
        bt::page(ctx, cx),
        ed2k::page(ctx, cx),
        proxy::page(ctx, cx),
        api::page(ctx, cx),
        notify::page(ctx, cx),
        sections::slot_page(
            ctx,
            "extensions",
            "settingsCatExtensions",
            "settingsCatExtensionsDesc",
            FluxIcon::Package,
            extensions,
        ),
        doctor::page(ctx, cx),
        about::page(ctx, cx),
    ]);
    pages
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let query = self.search.read(cx).value().trim().to_lowercase();
        let pages: Vec<SettingsPage> = self
            .pages(cx)
            .into_iter()
            .filter_map(|page| page.filtered(&query))
            .collect();
        // 当前就在「推介有奖」页而它已不可见（登出 / 偏好关闭）：退回账户页。
        if self.selected == "referral" && !self.referral_visible(cx) {
            self.selected = SharedString::from("account");
        }
        if !pages.iter().any(|page| self.selected == page.key)
            && let Some(first) = pages.first()
        {
            self.selected = SharedString::from(first.key);
        }
        // 内容区可用宽度：窗口宽 - 侧栏 - 左右留白（列数判定用，不参与布局）。
        let content_width = f32::from(window.viewport_size().width)
            - f32::from(active_theme(cx).text_extent(SIDEBAR_WIDTH))
            - CONTENT_PADDING_LEFT
            - CONTENT_PADDING_RIGHT;
        let feedback = self.feedback(cx);
        let active = pages
            .iter()
            .find(|page| self.selected == page.key)
            .or_else(|| pages.first());

        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(tokens.colors.surface)
            .when_some(feedback, |this, (text, is_error)| {
                this.child(
                    meta_text(cx)
                        .w_full()
                        .px(px(CONTENT_PADDING_LEFT))
                        .py(tokens.spacing.sm)
                        .when(is_error, |this| this.text_color(tokens.colors.destructive))
                        .child(text),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_stretch()
                    .child(self.render_sidebar(&pages, cx))
                    .children(
                        active.map(|page| self.render_content(page, content_width, window, cx)),
                    ),
            )
    }
}
