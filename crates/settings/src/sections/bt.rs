//! BitTorrent：基础（DHT/UPnP/端口）、Tracker（列表 + 订阅）、做种。

use fluxdown_ui_components::FluxIcon;
use std::rc::Rc;

use fluxdown_ui_theme::active_theme;
use gpui::{App, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window, div};
use gpui_component::h_flex;

use super::{
    SectionContext, bt_guard,
    subscription::{self, ListFormat},
};
use crate::ui::{
    Control, Getter, Setter, SettingsPage, SettingsSection, SettingsTab, UNIT_DROPDOWN_WIDTH,
    dropdown_button, render_number,
};

pub(crate) fn page(ctx: &SectionContext, cx: &mut App) -> SettingsPage {
    let bt_enabled = ctx.store.read(cx).daemon_bool("bt_enabled");
    let mut page = SettingsPage::new(
        "bt",
        ctx.t("settingsCatBt"),
        ctx.t("settingsCatBtDesc"),
        FluxIcon::Magnet,
    )
    .tab(
        SettingsTab::new("basic", ctx.t("settingsTabGeneral"))
            .section(basic_section(ctx, bt_enabled)),
    );
    if bt_enabled {
        page = page
            .tab(
                SettingsTab::new("tracker", ctx.t("settingsTabTracker"))
                    .section(tracker_section(ctx, cx)),
            )
            .tab(
                SettingsTab::new("seeding", ctx.t("settingsTabSeeding"))
                    .section(seeding_section(ctx, cx)),
            );
        for tab in bt_guard::tabs(ctx, cx) {
            page = page.tab(tab);
        }
    }
    page
}

fn basic_section(ctx: &SectionContext, bt_enabled: bool) -> SettingsSection {
    let section = SettingsSection::new()
        .title(ctx.t("settingsTabGeneral"))
        .subtitle(ctx.t("btSettingsRestartHint"))
        .row(ctx.item(
            "btEnabled",
            Some("btEnabledDesc"),
            ctx.daemon_switch("bt_enabled"),
        ));
    if !bt_enabled {
        return section;
    }
    section
        .row(ctx.item(
            "btEnableDht",
            Some("btEnableDhtDesc"),
            ctx.daemon_switch("bt_enable_dht"),
        ))
        .row(ctx.item(
            "btEnableUpnp",
            Some("btEnableUpnpDesc"),
            ctx.daemon_switch("bt_enable_upnp"),
        ))
        .row(ctx.item(
            "btEnableUtp",
            Some("btEnableUtpDesc"),
            ctx.daemon_switch("bt_enable_utp"),
        ))
        .row(ctx.item(
            "btListenPortStart",
            Some("btListenPortDesc"),
            ctx.daemon_number("bt_port_start"),
        ))
        .row(ctx.item("btListenPortEnd", None, ctx.daemon_number("bt_port_end")))
        .row(ctx.item(
            "btMseMode",
            Some("btMseModeDesc"),
            ctx.daemon_enum_dropdown("bt_mse_mode", "btMseMode"),
        ))
}

fn tracker_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let sub_enabled = ctx.store.read(cx).daemon_bool("bt_tracker_sub_enabled");
    SettingsSection::new()
        .title(ctx.t("settingsTabTracker"))
        .row(subscription::list_item(
            ctx,
            "btTrackerList",
            "btTrackerListDesc",
            "btTrackerPlaceholder",
            "bt_custom_trackers",
            ListFormat::Lines,
        ))
        .row(ctx.item(
            "btTrackerSub",
            Some("btTrackerSubDesc"),
            ctx.daemon_switch("bt_tracker_sub_enabled"),
        ))
        .row(
            subscription::list_item(
                ctx,
                "btTrackerSubUrls",
                "btTrackerSubUrlsDesc",
                "btTrackerSubPlaceholder",
                "bt_tracker_sub_urls",
                ListFormat::Lines,
            )
            .disabled(!sub_enabled),
        )
        .row(subscription::status_item(
            ctx,
            subscription::SubscriptionKind::BtTrackers,
            cx,
        ))
}

fn seeding_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let store = ctx.store.read(cx);
    let seed_enabled = store.daemon_bool("bt_seed_enabled");
    let mut group = SettingsSection::new()
        .title(ctx.t("settingsTabSeeding"))
        .row(ctx.item(
            "btSeedEnabled",
            Some("btSeedEnabledDesc"),
            ctx.daemon_switch("bt_seed_enabled"),
        ));
    if !seed_enabled {
        return group;
    }
    group = group
        .row(ctx.item(
            "btSeedMaxActive",
            Some("btSeedMaxActiveDesc"),
            ctx.daemon_number("bt_seed_max_active"),
        ))
        .row(ctx.item(
            "btAutoReseed",
            Some("btAutoReseedDesc"),
            ctx.daemon_switch("bt_auto_reseed"),
        ))
        .row(
            ctx.item(
                "btSeedRatioLimit",
                None,
                ctx.daemon_number_with("bt_seed_ratio_limit", 0.1)
                    .unit(ctx.t("btSeedRatioUnit")),
            ),
        )
        .row(
            ctx.item(
                "btSeedPostRatioLimit",
                None,
                ctx.daemon_number_with("bt_seed_post_ratio_limit", 0.1)
                    .unit(ctx.t("btSeedRatioUnit")),
            ),
        )
        .row(ctx.item(
            "btSeedTimeLimit",
            None,
            duration_control(ctx, "bt_seed_time_limit_minutes", "bt_seed_time_limit_unit"),
        ))
        .row(ctx.item(
            "btSeedInactiveTimeLimit",
            None,
            duration_control(
                ctx,
                "bt_seed_inactive_time_limit_minutes",
                "bt_seed_inactive_time_limit_unit",
            ),
        ))
        .row(ctx.item(
            "btSeedConditionsOperator",
            None,
            ctx.daemon_dropdown(
                "bt_seed_limit_operator",
                vec![
                    (SharedString::from("or"), ctx.t("btSeedOperatorOr")),
                    (SharedString::from("and"), ctx.t("btSeedOperatorAnd")),
                ],
            ),
        ))
        .row(ctx.item(
            "btSeedThenAction",
            None,
            ctx.daemon_dropdown(
                "bt_seed_then_action",
                vec![
                    (SharedString::from("stop"), ctx.t("btSeedStopSeeding")),
                    (SharedString::from("delete"), ctx.t("btSeedDeleteTask")),
                    (
                        SharedString::from("delete_files"),
                        ctx.t("btSeedDeleteTaskAndFiles"),
                    ),
                ],
            ),
        ));
    group
}

fn time_units(ctx: &SectionContext) -> Vec<(SharedString, SharedString)> {
    vec![
        (SharedString::from("minutes"), ctx.t("timeUnitMinutes")),
        (SharedString::from("hours"), ctx.t("timeUnitHours")),
        (SharedString::from("days"), ctx.t("timeUnitDays")),
    ]
}

/// 时长限制：数字 + 单位下拉并排（0 = 不限）。数字按所选单位解读，单位与数字同行，
/// 用户一眼可见量纲。
fn duration_control(
    ctx: &SectionContext,
    value_key: &'static str,
    unit_key: &'static str,
) -> Control {
    let store = ctx.store();
    let options = time_units(ctx);
    Control::custom(
        move |disabled, row_key, window: &mut Window, cx: &mut App| {
            let gap = active_theme(cx).tokens().spacing.sm;
            let get_store = store.clone();
            let set_store = store.clone();
            let get: Getter<f64> =
                Rc::new(move |cx: &App| get_store.read(cx).daemon_i64(value_key) as f64);
            let set: Setter<f64> = Rc::new(move |value: f64, cx: &mut App| {
                set_store.update(cx, |store, cx| {
                    store.set_daemon_i64(value_key, value.round() as i64, cx);
                });
            });
            let number = render_number(
                0.0,
                f64::from(i32::MAX),
                1.0,
                &get,
                &set,
                disabled,
                &SharedString::from(format!("{row_key}-{value_key}")),
                false,
                window,
                cx,
            );
            let unit_store = store.clone();
            let dropdown = dropdown_button(
                format!("{row_key}-unit"),
                &options,
                SharedString::from(store.read(cx).daemon_str(unit_key)),
                disabled,
                true,
                Rc::new(move |value: SharedString, cx: &mut App| {
                    unit_store.update(cx, |store, cx| {
                        store.set_daemon(unit_key, value.to_string(), cx);
                    });
                }),
                cx,
            );
            h_flex()
                .gap(gap)
                .items_center()
                .child(number)
                .child(
                    div()
                        .w(active_theme(cx).text_extent(UNIT_DROPDOWN_WIDTH))
                        .child(dropdown),
                )
                .into_any_element()
        },
    )
}
