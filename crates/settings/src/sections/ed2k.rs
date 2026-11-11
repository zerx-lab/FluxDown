//! eD2K：基础（Kad/UPnP/端口/ID 检测）、服务器（列表 + server.met 订阅）、Kad 节点（nodes.dat
//! 订阅 + 引导）。ID 检测与引导面板见 [`super::ed2k_probe`]。

use fluxdown_ui_components::FluxIcon;
use gpui::App;

use super::{
    SectionContext,
    ed2k_probe::{bootstrap_item, id_check_item},
    subscription::{self, ListFormat},
};
use crate::ui::{SettingsPage, SettingsSection, SettingsTab};

pub(crate) fn page(ctx: &SectionContext, cx: &mut App) -> SettingsPage {
    SettingsPage::new(
        "ed2k",
        ctx.t("settingsCatEd2k"),
        ctx.t("settingsCatEd2kDesc"),
        FluxIcon::HardDrive,
    )
    .tab(SettingsTab::new("basic", ctx.t("settingsTabGeneral")).section(basic_section(ctx)))
    .tab(SettingsTab::new("servers", ctx.t("settingsTabServers")).section(servers_section(ctx, cx)))
    .tab(SettingsTab::new("kad", ctx.t("settingsTabKadNodes")).section(kad_section(ctx, cx)))
}

fn basic_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("settingsTabGeneral"))
        .row(ctx.item(
            "ed2kEnableKad",
            Some("ed2kEnableKadDesc"),
            ctx.daemon_switch("ed2k_enable_kad"),
        ))
        .row(ctx.item(
            "ed2kEnableUpnp",
            Some("ed2kEnableUpnpDesc"),
            ctx.daemon_switch("ed2k_enable_upnp"),
        ))
        .row(ctx.item(
            "ed2kListenPort",
            Some("ed2kListenPortDesc"),
            ctx.daemon_number("ed2k_listen_port"),
        ))
        .row(id_check_item(ctx))
}

fn servers_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let sub_enabled = ctx.store.read(cx).daemon_bool("ed2k_server_sub_enabled");
    SettingsSection::new()
        .title(ctx.t("settingsTabServers"))
        .row(subscription::list_item(
            ctx,
            "ed2kServerList",
            "ed2kServerListDesc",
            "ed2kServerPlaceholder",
            "ed2k_server_list",
            ListFormat::Comma,
        ))
        .row(ctx.item(
            "ed2kServerSub",
            Some("ed2kServerSubDesc"),
            ctx.daemon_switch("ed2k_server_sub_enabled"),
        ))
        .row(
            subscription::list_item(
                ctx,
                "ed2kServerSubUrls",
                "ed2kServerSubUrlsDesc",
                "ed2kServerSubPlaceholder",
                "ed2k_server_sub_urls",
                ListFormat::Lines,
            )
            .disabled(!sub_enabled),
        )
        .row(subscription::status_item(
            ctx,
            subscription::SubscriptionKind::Ed2kServers,
            cx,
        ))
}

fn kad_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let kad_enabled = ctx.store.read(cx).daemon_bool("ed2k_enable_kad");
    SettingsSection::new()
        .title(ctx.t("settingsTabKadNodes"))
        .row(
            subscription::list_item(
                ctx,
                "ed2kNodesSubUrls",
                "ed2kNodesSubUrlsDesc",
                "ed2kNodesSubPlaceholder",
                "ed2k_nodes_dat_url",
                ListFormat::Lines,
            )
            .disabled(!kad_enabled),
        )
        .row(subscription::status_item(
            ctx,
            subscription::SubscriptionKind::Ed2kNodes,
            cx,
        ))
        .row(bootstrap_item(ctx).disabled(!kad_enabled))
}
