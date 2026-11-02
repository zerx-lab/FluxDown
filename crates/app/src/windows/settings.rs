//! 设置窗口：账户 / 扩展视图各自 attach 会话；边界持久化。

use std::sync::Arc;

use fluxdown_ui_account::{AccountView, ReferralView};
use fluxdown_ui_extensions::{ExtensionsEvent, ExtensionsView};
use fluxdown_ui_i18n::keys;
use fluxdown_ui_settings::{SettingsContentSlots, SettingsTarget, SettingsView, VisibleSlot};
use fluxdown_ui_shell::{AuxiliaryWindowView, auxiliary_window_options};
use gpui::{App, AppContext as _, px, size};
use gpui_component::Root;

use crate::{
    app::Desktop,
    capability_ports::AgentExtensionsPort,
    session::attach,
    windows::{RememberedWindow, WindowKey, WindowRegistry},
};

const SETTINGS_WINDOW_SIZE: gpui::Size<gpui::Pixels> = size(px(1240.), px(760.));

pub fn open(cx: &mut App) {
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let session = desktop.session.clone();
    let client = desktop.client.clone();
    let settings_store = desktop.settings_store.clone();
    let account_host = desktop.account_host.clone();
    let referral_visibility = desktop.referral_visibility.clone();
    let title = translator.read(cx).text(keys::SETTINGS).to_owned();
    let mut options = auxiliary_window_options(title);
    options.window_min_size = Some(size(px(1000.), px(600.)));
    WindowRegistry::restore_bounds(
        RememberedWindow::Settings,
        &mut options,
        SETTINGS_WINDOW_SIZE,
        cx,
    );

    WindowRegistry::open_or_focus(cx, WindowKey::Settings, options, move |window, cx| {
        let extensions_port = Arc::new(AgentExtensionsPort::new(client.clone()));
        let account =
            cx.new(|cx| AccountView::new(translator.clone(), account_host.clone(), window, cx));
        let extensions = cx.new(|cx| ExtensionsView::new(translator.clone(), extensions_port, cx));
        let referral = cx.new(|cx| ReferralView::new(account_host.clone(), window, cx));
        let settings = cx.new(|cx| {
            SettingsView::new(
                translator.clone(),
                settings_store,
                SettingsContentSlots {
                    account: Some(account.clone().into()),
                    extensions: Some(extensions.clone().into()),
                    referral: Some(VisibleSlot {
                        view: referral.clone().into(),
                        visible: referral_visibility,
                    }),
                    activity_bar: crate::activity::toggles(),
                },
                window,
                cx,
            )
        });
        Desktop::global_mut(cx).settings_view = Some(settings.downgrade());
        attach(&session, &extensions, cx);
        extensions.update(cx, |extensions, cx| {
            extensions
                .set_plugin_view_factory(crate::plugin_contributions::dialog_view_factory(), cx);
        });
        cx.subscribe(&extensions, |_, event, cx| match event {
            ExtensionsEvent::OpenPluginView { plugin, view } => {
                let plugin = plugin.clone();
                let view = view.clone();
                // 事件在设置窗口的 update 栈内分发；开新窗口延后到栈外。
                cx.defer(move |cx| crate::windows::plugin::open(cx, &plugin, &view));
            }
        })
        .detach();
        let window_view =
            cx.new(|cx| AuxiliaryWindowView::new(translator, keys::SETTINGS, settings.into(), cx));
        let root = cx.new(|cx| Root::new(window_view, window, cx));
        WindowRegistry::persist_bounds(RememberedWindow::Settings, client, &root, window, cx);
        root
    });
}

/// 打开（或聚焦）设置窗口并定位到 `target`。
///
/// 一律 defer：菜单 / 快捷键 / 命令面板触发时都处于活动窗口的 update 栈内
/// （`App::dispatch_action` 经活动窗口分发），同步 `handle.update` 会静默失败。
pub fn reveal(cx: &mut App, target: SettingsTarget) {
    cx.defer(move |cx| {
        open(cx);
        let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Settings) else {
            return;
        };
        let Some(view) = Desktop::global(cx)
            .settings_view
            .as_ref()
            .and_then(gpui::WeakEntity::upgrade)
        else {
            return;
        };
        if let Err(error) = handle.update(cx, |_, window, cx| {
            view.update(cx, |view, cx| view.reveal(&target, window, cx));
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
}
