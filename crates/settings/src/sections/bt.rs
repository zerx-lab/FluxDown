//! BitTorrent：常规（开关 + 新任务默认）、连接（监听 / 传输 / 来源 / 上限 / I2P）、
//! Tracker（列表 + 订阅 + 汇报策略）、做种。

use fluxdown_protocol::BtPortStatusDto;
use fluxdown_ui_components::{FluxIcon, tooltip_view};
use std::rc::Rc;

use fluxdown_ui_theme::active_theme;
use gpui::{
    App, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_component::h_flex;

use super::{
    SectionContext, bt_guard,
    subscription::{self, ListFormat},
};
use crate::ui::{
    Control, Getter, NUMBER_WIDTH, Setter, SettingsPage, SettingsSection, SettingsTab,
    UNIT_DROPDOWN_WIDTH, dropdown_button, meta_text, render_number,
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
            .section(basic_section(ctx))
            .section(new_tasks_section(ctx, bt_enabled)),
    );
    if bt_enabled {
        page = page
            .tab(
                SettingsTab::new("connection", ctx.t("settingsTabConnection"))
                    .section(listen_section(ctx, cx))
                    .section(transport_section(ctx, cx))
                    .section(sources_section(ctx))
                    .section(limits_section(ctx))
                    .section(i2p_section(ctx, cx)),
            )
            .tab(
                SettingsTab::new("tracker", ctx.t("settingsTabTracker"))
                    .section(tracker_section(ctx, cx))
                    .section(announce_section(ctx)),
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

fn basic_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("settingsTabGeneral"))
        .row(ctx.item(
            "btEnabled",
            Some("btEnabledDesc"),
            ctx.daemon_switch("bt_enabled"),
        ))
}

/// 监听端口与端口映射。本页除「连接上限」外的改动都会重建 BT 会话，提示挂在第一组。
fn listen_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let section = SettingsSection::new()
        .title(ctx.t("btSectionListen"))
        .subtitle(ctx.t("btSettingsRestartHint"))
        .row(ctx.item(
            "btListenPortStart",
            Some("btListenPortDesc"),
            ctx.daemon_number("bt_port_start"),
        ))
        .row(ctx.item("btListenPortEnd", None, ctx.daemon_number("bt_port_end")))
        .row(ctx.item(
            "btEnableUpnp",
            Some("btEnableUpnpDesc"),
            ctx.daemon_switch("bt_enable_upnp"),
        ));
    if !ctx.store.read(cx).daemon_bool("bt_enable_upnp") {
        return section;
    }
    // 快照由设置视图在本页签可见时轮询（`SettingsView::sync_port_status_poll`）；视图观察 store，
    // 新快照到达即重建本行。尚无快照（首轮在途 / 旧 daemon 无此方法）时行内留空。
    let segments = ctx
        .store
        .read(cx)
        .bt_port_status()
        .map(port_mapping_segments)
        .unwrap_or_default();
    let chips: Vec<(SharedString, Option<SharedString>)> = segments
        .iter()
        .map(|segment| port_segment_text(ctx, segment))
        .collect();
    section.row(
        ctx.item(
            "btPortMappingStatus",
            None,
            Control::custom(move |_disabled, row_key, _window, cx: &mut App| {
                let theme = active_theme(cx);
                let gap_x = theme.tokens().spacing.md;
                let gap_y = theme.tokens().spacing.xxs;
                let max_w = theme.text_extent(PORT_SEGMENT_MAX_WIDTH);
                h_flex()
                    .flex_wrap()
                    .min_w_0()
                    .gap_x(gap_x)
                    .gap_y(gap_y)
                    .children(chips.iter().enumerate().map(|(index, (text, full))| {
                        let label = meta_text(cx).truncate().child(text.clone());
                        let chip = div()
                            .id(SharedString::from(format!("{row_key}-segment-{index}")))
                            .min_w_0()
                            .max_w(max_w);
                        match full {
                            Some(full) => {
                                let full = full.clone();
                                chip.tooltip(move |window, cx| {
                                    tooltip_view(full.clone(), window, cx)
                                })
                                .child(label)
                            }
                            None => chip.child(label),
                        }
                    }))
                    .into_any_element()
            }),
        )
        .vertical(),
    )
}

/// 单段状态文字的最大宽度（100% 字号基准）；超出截断，失败原因悬停看全文。
const PORT_SEGMENT_MAX_WIDTH: f32 = 320.;

/// 端口映射状态行的一段（纯数据，文案在渲染时按语言查表）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum PortSegment {
    /// BT 会话未运行：尚未请求映射。
    Idle,
    Mapping {
        /// `TCP 6881` / `UDP 6881 → 50123`。
        port: String,
        state: PortSegmentState,
    },
    ExternalIp(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PortSegmentState {
    /// 附显示用方法名（`UPnP` / `NAT-PMP` / `PCP`，未知取原值）。
    Mapped(String),
    Pending,
    Failed(String),
    /// 未识别的 wire 状态，原样显示。
    Other(String),
}

/// `daemon.bt.portStatus` → 状态行各段：未运行一段「空闲」；否则每个映射一段，
/// 任一映射带外网地址时末尾追加一段（取第一个非空地址）。
fn port_mapping_segments(status: &BtPortStatusDto) -> Vec<PortSegment> {
    if !status.running {
        return vec![PortSegment::Idle];
    }
    let mut segments: Vec<PortSegment> = status
        .mappings
        .iter()
        .map(|mapping| {
            let mut port = format!(
                "{} {}",
                mapping.protocol.to_uppercase(),
                mapping.internal_port
            );
            if mapping.state == "mapped"
                && mapping.external_port != 0
                && mapping.external_port != mapping.internal_port
            {
                port.push_str(&format!(" → {}", mapping.external_port));
            }
            let state = match mapping.state.as_str() {
                "mapped" => PortSegmentState::Mapped(method_label(&mapping.method)),
                "pending" => PortSegmentState::Pending,
                "failed" => PortSegmentState::Failed(mapping.message.trim().to_owned()),
                other => PortSegmentState::Other(other.to_owned()),
            };
            PortSegment::Mapping { port, state }
        })
        .collect();
    if let Some(ip) = status
        .mappings
        .iter()
        .map(|mapping| mapping.external_ip.trim())
        .find(|ip| !ip.is_empty())
    {
        segments.push(PortSegment::ExternalIp(ip.to_owned()));
    }
    segments
}

/// wire 方法名 → 显示名；未知值原样返回。
fn method_label(method: &str) -> String {
    match method {
        "upnp" => "UPnP".to_owned(),
        "natPmp" => "NAT-PMP".to_owned(),
        "pcp" => "PCP".to_owned(),
        other => other.to_owned(),
    }
}

/// 一段的显示文字 + 悬停全文（仅失败原因可能被截断，带全文）。失败原因压成单行。
fn port_segment_text(
    ctx: &SectionContext,
    segment: &PortSegment,
) -> (SharedString, Option<SharedString>) {
    match segment {
        PortSegment::Idle => (ctx.t("btPortMappingIdle"), None),
        PortSegment::ExternalIp(ip) => (
            SharedString::from(
                ctx.translator
                    .text_with("btPortMappingExternalIp", &[("ip", ip)]),
            ),
            None,
        ),
        PortSegment::Mapping { port, state } => {
            let (status, full) = match state {
                PortSegmentState::Mapped(method) => (
                    ctx.translator
                        .text_with("btPortMappingMapped", &[("method", method)]),
                    None,
                ),
                PortSegmentState::Pending => (ctx.t("btPortMappingPending").to_string(), None),
                PortSegmentState::Failed(message) => {
                    let failed = ctx.t("btPortMappingFailed").to_string();
                    if message.is_empty() {
                        (failed, None)
                    } else {
                        let line = message.split_whitespace().collect::<Vec<_>>().join(" ");
                        let full = format!("{port} {failed}: {message}");
                        (format!("{failed}: {line}"), Some(full))
                    }
                }
                PortSegmentState::Other(raw) => (raw.clone(), None),
            };
            (
                SharedString::from(format!("{port} {status}")),
                full.map(SharedString::from),
            )
        }
    }
}

/// 传输：uTP 及其拥塞控制、协议加密、上传槽。拥塞控制只在 uTP 开启时有意义。
fn transport_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let utp_enabled = ctx.store.read(cx).daemon_bool("bt_enable_utp");
    let mut section = SettingsSection::new()
        .title(ctx.t("btSectionTransport"))
        .row(ctx.item(
            "btEnableUtp",
            Some("btEnableUtpDesc"),
            ctx.daemon_switch("bt_enable_utp"),
        ));
    if utp_enabled {
        section = section.row(ctx.item(
            "btUtpCongestion",
            Some("btUtpCongestionDesc"),
            ctx.daemon_enum_dropdown("bt_utp_congestion", "btUtpCongestion"),
        ));
    }
    section
        .row(ctx.item(
            "btMseMode",
            Some("btMseModeDesc"),
            ctx.daemon_enum_dropdown("bt_mse_mode", "btMseMode"),
        ))
        .row(ctx.item(
            "btUploadSlots",
            Some("btUploadSlotsDesc"),
            ctx.daemon_number("bt_upload_slots"),
        ))
}

/// 节点与数据从哪里来：DHT、局域网发现、种子自带的 HTTP 源。
fn sources_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btSectionSources"))
        .row(ctx.item(
            "btEnableDht",
            Some("btEnableDhtDesc"),
            ctx.daemon_switch("bt_enable_dht"),
        ))
        .row(ctx.item(
            "btEnableLsd",
            Some("btEnableLsdDesc"),
            ctx.daemon_switch("bt_enable_lsd"),
        ))
        .row(ctx.item(
            "btEnableWebseed",
            Some("btEnableWebseedDesc"),
            ctx.daemon_switch("bt_enable_webseed"),
        ))
}

/// 连接数上限：热生效，不重建 BT 会话。
fn limits_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btSectionLimits"))
        .row(ctx.item(
            "btMaxConnections",
            Some("btMaxConnectionsDesc"),
            ctx.daemon_number("bt_max_connections"),
        ))
        .row(ctx.item(
            "btMaxConnectionsPerTorrent",
            Some("btMaxConnectionsPerTorrentDesc"),
            ctx.daemon_number("bt_max_connections_per_torrent"),
        ))
}

/// I2P：SAM 桥地址与混合模式只在启用后显示。
fn i2p_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let enabled = ctx.store.read(cx).daemon_bool("bt_i2p_enabled");
    let section = SettingsSection::new()
        .title(ctx.t("btSectionI2p"))
        .row(ctx.item(
            "btI2pEnabled",
            Some("btI2pEnabledDesc"),
            ctx.daemon_switch("bt_i2p_enabled"),
        ));
    if !enabled {
        return section;
    }
    section
        .row(ctx.item("btI2pSamHost", None, ctx.daemon_input("bt_i2p_sam_host")))
        .row(ctx.item("btI2pSamPort", None, ctx.daemon_number("bt_i2p_sam_port")))
        .row(ctx.item(
            "btI2pMixed",
            Some("btI2pMixedDesc"),
            ctx.daemon_switch("bt_i2p_mixed"),
        ))
}

/// tracker 汇报策略（BEP 12 的两个开关，对齐 qBittorrent 的同名高级设置）。
fn announce_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btSectionAnnounce"))
        .row(ctx.item(
            "btAnnounceAllTiers",
            Some("btAnnounceAllTiersDesc"),
            ctx.daemon_switch("bt_announce_all_tiers"),
        ))
        .row(ctx.item(
            "btAnnounceAllTrackers",
            Some("btAnnounceAllTrackersDesc"),
            ctx.daemon_switch("bt_announce_all_trackers"),
        ))
}

/// 新建任务的默认选片。热更新，不重建 BT 会话，因此不带重启提示。
/// BT 关闭时整组隐藏（与基础页其余行同一门槛）。
fn new_tasks_section(ctx: &SectionContext, bt_enabled: bool) -> SettingsSection {
    if !bt_enabled {
        return SettingsSection::new();
    }
    SettingsSection::new()
        .title(ctx.t("btNewTasksSection"))
        .row(ctx.item(
            "btPieceOrder",
            Some("btPieceOrderDesc"),
            ctx.daemon_enum_dropdown("bt_piece_order", "btPieceOrder"),
        ))
        .row(ctx.item(
            "btFirstLastPiecesFirstDefault",
            Some("btFirstLastPiecesFirstDefaultDesc"),
            ctx.daemon_switch("bt_first_last_pieces_first"),
        ))
        .row(ctx.item(
            "btSelectionAutoDefault",
            Some("btSelectionAutoDefaultDesc"),
            ctx.daemon_switch("bt_selection_auto_default"),
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
/// 用户一眼可见量纲。数字框取 Tab 统一字段宽度，单位下拉跟在右侧。
fn duration_control(
    ctx: &SectionContext,
    value_key: &'static str,
    unit_key: &'static str,
) -> Control {
    let store = ctx.store();
    let options = time_units(ctx);
    Control::field(
        NUMBER_WIDTH,
        move |disabled, row_key, width, window: &mut Window, cx: &mut App| {
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
                width,
                window,
                cx,
            );
            let unit_store = store.clone();
            let dropdown = dropdown_button(
                format!("{row_key}-unit"),
                &options,
                SharedString::from(store.read(cx).daemon_str(unit_key)),
                disabled,
                None,
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

#[cfg(test)]
mod tests {
    use super::{PortSegment, PortSegmentState, port_mapping_segments};
    use fluxdown_protocol::{BtPortMappingDto, BtPortStatusDto};

    fn mapping(
        protocol: &str,
        internal: u32,
        external: u32,
        method: &str,
        state: &str,
    ) -> BtPortMappingDto {
        BtPortMappingDto {
            protocol: protocol.to_owned(),
            internal_port: internal,
            external_port: external,
            method: method.to_owned(),
            external_ip: String::new(),
            state: state.to_owned(),
            message: String::new(),
        }
    }

    fn running(mappings: Vec<BtPortMappingDto>) -> BtPortStatusDto {
        BtPortStatusDto {
            running: true,
            listen_port: 6881,
            mappings,
        }
    }

    fn segment(port: &str, state: PortSegmentState) -> PortSegment {
        PortSegment::Mapping {
            port: port.to_owned(),
            state,
        }
    }

    #[test]
    fn not_running_is_idle() {
        let status = BtPortStatusDto {
            running: false,
            listen_port: 0,
            mappings: vec![mapping("tcp", 6881, 6881, "upnp", "mapped")],
        };
        assert_eq!(port_mapping_segments(&status), vec![PortSegment::Idle]);
    }

    #[test]
    fn mapped_same_port_shows_single_port() {
        let status = running(vec![mapping("tcp", 6881, 6881, "upnp", "mapped")]);
        assert_eq!(
            port_mapping_segments(&status),
            vec![segment(
                "TCP 6881",
                PortSegmentState::Mapped("UPnP".to_owned())
            )]
        );
    }

    #[test]
    fn mapped_different_external_port_shows_arrow() {
        let status = running(vec![
            mapping("udp", 6881, 50123, "natPmp", "mapped"),
            mapping("tcp", 6881, 0, "pcp", "mapped"),
        ]);
        assert_eq!(
            port_mapping_segments(&status),
            vec![
                segment(
                    "UDP 6881 → 50123",
                    PortSegmentState::Mapped("NAT-PMP".to_owned())
                ),
                segment("TCP 6881", PortSegmentState::Mapped("PCP".to_owned())),
            ]
        );
    }

    #[test]
    fn pending_mapping() {
        let status = running(vec![mapping("tcp", 6881, 0, "", "pending")]);
        assert_eq!(
            port_mapping_segments(&status),
            vec![segment("TCP 6881", PortSegmentState::Pending)]
        );
    }

    #[test]
    fn failed_mapping_keeps_message() {
        let mut failed = mapping("udp", 6881, 0, "", "failed");
        failed.message = "  router refused\nconflict  ".to_owned();
        assert_eq!(
            port_mapping_segments(&running(vec![failed])),
            vec![segment(
                "UDP 6881",
                PortSegmentState::Failed("router refused\nconflict".to_owned())
            )]
        );
    }

    #[test]
    fn external_ip_appended_once_at_end() {
        let mut tcp = mapping("tcp", 6881, 6881, "upnp", "mapped");
        tcp.external_ip = "203.0.113.7".to_owned();
        let mut udp = mapping("udp", 6881, 6881, "upnp", "mapped");
        udp.external_ip = "203.0.113.7".to_owned();
        let segments = port_mapping_segments(&running(vec![tcp, udp]));
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments.last(),
            Some(&PortSegment::ExternalIp("203.0.113.7".to_owned()))
        );
    }

    #[test]
    fn running_without_mappings_is_empty() {
        assert!(port_mapping_segments(&running(Vec::new())).is_empty());
    }

    #[test]
    fn unknown_method_and_state_shown_raw() {
        let status = running(vec![
            mapping("tcp", 6881, 6881, "igdv3", "mapped"),
            mapping("udp", 6881, 0, "", "renewing"),
        ]);
        assert_eq!(
            port_mapping_segments(&status),
            vec![
                segment("TCP 6881", PortSegmentState::Mapped("igdv3".to_owned())),
                segment("UDP 6881", PortSegmentState::Other("renewing".to_owned())),
            ]
        );
    }
}
