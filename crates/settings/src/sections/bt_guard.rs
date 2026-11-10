//! BT 防吸血：设置页签（总开关 / 规则 / 阈值 / 处置 / 豁免 / 云名单 / 过滤文件 / 证据上报 / 今日统计），
//! 以及封禁名单与事件日志两个页签的入口行（列表面板见 `bt_guard_bans` / `bt_guard_events`）。
//!
//! 总览（`daemon.bt.guard.state`）存放在 [`SettingsStore`]：行在渲染期按需加载，`BtGuardChanged`
//! 事件后重新放行。所有键名、范围与枚举以 `fluxdown_protocol` 目录为准。

use fluxdown_protocol::{BtGuardCloudStatusDto, BtGuardFilterSourceDto};
use fluxdown_ui_components::{ButtonVariant, DialogIntent, dialog_title};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, Div, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{Disableable as _, WindowExt as _, h_flex, switch::Switch, v_flex};

use super::{
    SectionContext, bt_guard_bans, bt_guard_events,
    subscription::{self, ListFormat, format_unix},
};
use crate::model::bt_guard::{attribution_url, format_bytes};
use crate::store::{BT_GUARD_STATE_LOAD, SettingsStore};
use crate::ui::{
    Control, SettingsRow, SettingsSection, SettingsTab, body_text, meta_text, row_loading_button,
};

const REPORT_KEY: &str = "bt_guard_report_enabled";

/// 防吸血的三个页签：设置、封禁名单、事件日志。
pub(crate) fn tabs(ctx: &SectionContext, cx: &mut App) -> Vec<SettingsTab> {
    let enabled = ctx.store.read(cx).daemon_bool("bt_guard_enabled");
    let cloud_enabled = ctx.store.read(cx).daemon_bool("bt_guard_cloud_enabled");
    vec![
        SettingsTab::new("guard", ctx.t("btGuardTab")).sections([
            general_section(ctx),
            stats_section(ctx),
            rules_section(ctx, enabled),
            thresholds_section(ctx, enabled),
            actions_section(ctx, enabled),
            exemptions_section(ctx, enabled),
            cloud_section(ctx, enabled, cloud_enabled),
            filter_section(ctx, enabled),
            report_section(ctx),
        ]),
        SettingsTab::new("guardBans", ctx.t("btGuardTabBans")).section(bt_guard_bans::section(ctx)),
        SettingsTab::new("guardEvents", ctx.t("btGuardTabEvents"))
            .section(bt_guard_events::section(ctx)),
    ]
}

/// 渲染期按需加载总览：闸门由 [`SettingsStore::load_bt_guard_state`] 置位，只读判定后才写 store。
pub(crate) fn ensure_state(store: &Entity<SettingsStore>, cx: &mut App) {
    if !store.read(cx).load_attempted(BT_GUARD_STATE_LOAD) {
        store.update(cx, |store, cx| store.load_bt_guard_state(cx));
    }
}

fn general_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionGeneral"))
        .row(ctx.item(
            "btGuardEnabled",
            Some("btGuardEnabledDesc"),
            ctx.daemon_switch("bt_guard_enabled"),
        ))
        .row(status_row(ctx))
}

fn status_row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let on = ctx.t("btGuardSessionOn");
    let off = ctx.t("btGuardSessionOff");
    let disabled_text = ctx.t("btGuardStatusDisabled");
    let loading = ctx.t("btGuardLoading");
    let failed = ctx.t("btGuardLoadFailed");
    let reload = ctx.t("btGuardReload");
    SettingsRow::custom(move |_, _, _, cx: &mut App| {
        ensure_state(&store, cx);
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let snapshot = store.read(cx);
        let busy = snapshot.is_busy("btGuardState");
        let (dot, headline, detail) = match snapshot.bt_guard_state() {
            None => (
                extended.colors.text_tertiary,
                if busy {
                    loading.clone()
                } else {
                    failed.clone()
                },
                None,
            ),
            Some(state) => {
                let (dot, headline) = if !state.enabled {
                    (extended.colors.text_tertiary, disabled_text.clone())
                } else if state.session_active {
                    (extended.colors.success, on.clone())
                } else {
                    (extended.colors.warning, off.clone())
                };
                let counts = &state.counts;
                let summary = translator.text_with(
                    "btGuardCountsSummary",
                    &[
                        ("allow", &counts.allow.to_string()),
                        ("manual", &counts.manual.to_string()),
                        ("auto", &counts.auto.to_string()),
                        ("cloud", &counts.cloud.to_string()),
                        ("file", &counts.file.to_string()),
                    ],
                );
                (dot, headline, Some(SharedString::from(summary)))
            }
        };
        let click_store = store.clone();
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(tokens.spacing.md)
            .child(
                v_flex()
                    .min_w_0()
                    .gap(tokens.spacing.xxs)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(tokens.spacing.xs)
                            .child(
                                div()
                                    .flex_none()
                                    .size(tokens.spacing.sm)
                                    .rounded_full()
                                    .bg(dot),
                            )
                            .child(body_text(cx).child(headline)),
                    )
                    .children(detail.map(|text| meta_text(cx).child(text))),
            )
            .child(
                row_loading_button(
                    "bt-guard-reload",
                    reload.clone(),
                    ButtonVariant::Secondary,
                    busy,
                    cx,
                )
                .on_click(move |_, _, cx| {
                    click_store.update(cx, |store, cx| store.load_bt_guard_state(cx));
                }),
            )
            .into_any_element()
    })
    .keywords([ctx.t("btGuardTab"), ctx.t("btGuardKeywords")])
}

fn stats_section(ctx: &SectionContext) -> SettingsSection {
    let store = ctx.store();
    let labels = [
        ctx.t("btGuardStatObserved"),
        ctx.t("btGuardStatEnforced"),
        ctx.t("btGuardStatBlocked"),
        ctx.t("btGuardStatRefusedBytes"),
    ];
    SettingsSection::new()
        .title(ctx.t("btGuardSectionStats"))
        .row(SettingsRow::custom(move |_, _, _, cx: &mut App| {
            ensure_state(&store, cx);
            let tokens = active_theme(cx).tokens();
            let stats = store
                .read(cx)
                .bt_guard_state()
                .map(|state| state.stats)
                .unwrap_or_default();
            let values = [
                stats.today_observed.to_string(),
                stats.today_enforced.to_string(),
                stats.today_blocked_connections.to_string(),
                format_bytes(stats.today_refused_bytes),
            ];
            h_flex()
                .w_full()
                .gap(tokens.spacing.md)
                .children(labels.iter().zip(values).map(|(label, value)| {
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(tokens.spacing.xxs)
                        .child(
                            body_text(cx)
                                .truncate()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .font_features(fluxdown_ui_components::tabular_numbers())
                                .child(SharedString::from(value)),
                        )
                        .child(meta_text(cx).truncate().child(label.clone()))
                }))
                .into_any_element()
        }))
}

fn rules_section(ctx: &SectionContext, enabled: bool) -> SettingsSection {
    let rule = |title: &str, desc: &str, key: &'static str| {
        ctx.item(
            title,
            Some(desc),
            ctx.daemon_enum_dropdown(key, "btGuardMode"),
        )
        .disabled(!enabled)
    };
    SettingsSection::new()
        .title(ctx.t("btGuardSectionRules"))
        .subtitle(ctx.t("btGuardSectionRulesDesc"))
        .row(rule(
            "btGuardRuleExcessive",
            "btGuardRuleExcessiveDesc",
            "bt_guard_rule_excessive",
        ))
        .row(rule(
            "btGuardRuleRerequest",
            "btGuardRuleRerequestDesc",
            "bt_guard_rule_rerequest",
        ))
        .row(rule(
            "btGuardRuleRewind",
            "btGuardRuleRewindDesc",
            "bt_guard_rule_rewind",
        ))
        .row(rule(
            "btGuardRuleLag",
            "btGuardRuleLagDesc",
            "bt_guard_rule_lag",
        ))
        .row(rule(
            "btGuardRuleFingerprint",
            "btGuardRuleFingerprintDesc",
            "bt_guard_rule_fingerprint",
        ))
}

fn thresholds_section(ctx: &SectionContext, enabled: bool) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionThresholds"))
        .row(
            ctx.item(
                "btGuardExcessRatio",
                Some("btGuardExcessRatioDesc"),
                ctx.daemon_number("bt_guard_excess_ratio_pct")
                    .unit(ctx.t("btGuardUnitPercent")),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardMinTorrent",
                Some("btGuardMinTorrentDesc"),
                ctx.daemon_number("bt_guard_min_torrent_mib")
                    .unit(ctx.t("btGuardUnitMib")),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardRewindTolerance",
                Some("btGuardRewindToleranceDesc"),
                ctx.daemon_number("bt_guard_rewind_tolerance_pct")
                    .unit(ctx.t("btGuardUnitPercent")),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardIpv6Prefix",
                Some("btGuardIpv6PrefixDesc"),
                ctx.daemon_number("bt_guard_ipv6_prefix"),
            )
            .disabled(!enabled),
        )
}

fn actions_section(ctx: &SectionContext, enabled: bool) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionActions"))
        .row(
            ctx.item(
                "btGuardThrottleKib",
                Some("btGuardThrottleKibDesc"),
                ctx.daemon_number("bt_guard_throttle_kib")
                    .unit(ctx.t("btGuardUnitKibps")),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardBanBaseHours",
                Some("btGuardBanBaseHoursDesc"),
                ctx.daemon_number("bt_guard_ban_base_hours")
                    .unit(ctx.t("timeUnitHours")),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardBanMaxDays",
                None,
                ctx.daemon_number("bt_guard_ban_max_days")
                    .unit(ctx.t("timeUnitDays")),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardDownloadingMaxAction",
                Some("btGuardDownloadingMaxActionDesc"),
                ctx.daemon_enum_dropdown(
                    "bt_guard_downloading_max_action",
                    "btGuardDownloadingAction",
                ),
            )
            .disabled(!enabled),
        )
}

fn exemptions_section(ctx: &SectionContext, enabled: bool) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionExemptions"))
        .row(
            ctx.item(
                "btGuardSkipPrivate",
                Some("btGuardSkipPrivateDesc"),
                ctx.daemon_switch("bt_guard_skip_private"),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardSkipLan",
                Some("btGuardSkipLanDesc"),
                ctx.daemon_switch("bt_guard_skip_lan"),
            )
            .disabled(!enabled),
        )
}

// ───────────────────────── 云名单 ─────────────────────────

fn cloud_section(ctx: &SectionContext, enabled: bool, cloud_enabled: bool) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionCloud"))
        .row(
            ctx.item(
                "btGuardCloudEnabled",
                Some("btGuardCloudEnabledDesc"),
                ctx.daemon_switch("bt_guard_cloud_enabled"),
            )
            .disabled(!enabled),
        )
        .row(
            ctx.item(
                "btGuardCloudMode",
                None,
                ctx.daemon_enum_dropdown("bt_guard_cloud_mode", "btGuardMode"),
            )
            .disabled(!enabled || !cloud_enabled),
        )
        .row(cloud_status_row(ctx, cloud_enabled))
}

/// 云名单状态行的各行文案（纯数据，渲染与测试分离）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CloudLine {
    Plain(String),
    Warning(String),
    Error(String),
}

/// 状态卡内容：序号 / 条目数 / 更新与过期时间 / 过期 / 最近错误 / 被拒原因。
pub(crate) fn cloud_lines(
    cloud: &BtGuardCloudStatusDto,
    translator: &fluxdown_ui_i18n::Translator,
    time: impl Fn(i64) -> String,
) -> Vec<CloudLine> {
    let mut lines = Vec::new();
    if cloud.sequence == 0 && cloud.updated_at <= 0 {
        lines.push(CloudLine::Plain(
            translator.text("btGuardCloudNone").to_owned(),
        ));
    } else {
        lines.push(CloudLine::Plain(translator.text_with(
            "btGuardCloudSequence",
            &[("n", &cloud.sequence.to_string())],
        )));
        lines.push(CloudLine::Plain(translator.text_with(
            "btGuardCloudEntries",
            &[
                ("entries", &cloud.entries.to_string()),
                ("rules", &cloud.client_rules.to_string()),
            ],
        )));
        if cloud.updated_at > 0 {
            lines.push(CloudLine::Plain(translator.text_with(
                "btGuardCloudUpdatedAt",
                &[("time", &time(cloud.updated_at))],
            )));
        }
        if cloud.expires_at > 0 {
            lines.push(CloudLine::Plain(translator.text_with(
                "btGuardCloudExpiresAt",
                &[("time", &time(cloud.expires_at))],
            )));
        }
    }
    if cloud.stale {
        lines.push(CloudLine::Warning(
            translator.text("btGuardCloudStale").to_owned(),
        ));
    }
    if let Some(error) = cloud
        .last_error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        lines.push(CloudLine::Error(
            translator.text_with("btGuardCloudLastError", &[("error", error)]),
        ));
    }
    if let Some(reason) = cloud
        .rejected_reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        lines.push(CloudLine::Warning(
            translator.text_with("btGuardCloudRejected", &[("reason", reason)]),
        ));
    }
    lines
}

fn cloud_status_row(ctx: &SectionContext, cloud_enabled: bool) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let disabled_text = ctx.t("btGuardCloudDisabled");
    let attribution_title = ctx.t("btGuardCloudAttribution");
    let loading = ctx.t("btGuardLoading");
    SettingsRow::custom(move |_, _, _, cx: &mut App| {
        ensure_state(&store, cx);
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let extended = theme.extended();
        let Some(state) = store.read(cx).bt_guard_state().cloned() else {
            return meta_text(cx).child(loading.clone()).into_any_element();
        };
        if !cloud_enabled {
            return meta_text(cx)
                .child(disabled_text.clone())
                .into_any_element();
        }
        let mut column = v_flex().w_full().gap(tokens.spacing.xxs);
        for line in cloud_lines(&state.cloud, &translator, format_unix) {
            column = column.child(match line {
                CloudLine::Plain(text) => meta_text(cx).child(SharedString::from(text)),
                CloudLine::Warning(text) => meta_text(cx)
                    .text_color(extended.colors.warning)
                    .child(SharedString::from(text)),
                CloudLine::Error(text) => meta_text(cx)
                    .text_color(tokens.colors.destructive)
                    .child(SharedString::from(text)),
            });
        }
        if !state.cloud.attribution.is_empty() {
            column = column.child(
                v_flex()
                    .pt(tokens.spacing.xs)
                    .gap(tokens.spacing.xxs)
                    .child(
                        meta_text(cx)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(attribution_title.clone()),
                    )
                    .children(
                        state
                            .cloud
                            .attribution
                            .iter()
                            .enumerate()
                            .map(|(index, line)| attribution_line(index, line, cx)),
                    ),
            );
        }
        column.into_any_element()
    })
    .keywords([
        ctx.t("btGuardSectionCloud"),
        ctx.t("btGuardCloudAttribution"),
    ])
}

/// 署名行：含 http(s) 链接时整行可点击打开（署名文本来自已验签的文档）。
fn attribution_line(index: usize, line: &str, cx: &App) -> gpui::AnyElement {
    let accent = active_theme(cx).tokens().colors.primary;
    let text = SharedString::from(line.to_owned());
    match attribution_url(line).map(str::to_owned) {
        Some(url) => meta_text(cx)
            .id(SharedString::from(format!("bt-guard-attribution-{index}")))
            .cursor_pointer()
            .text_color(accent)
            .hover(|style| style.underline())
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(text)
            .into_any_element(),
        None => meta_text(cx).child(text).into_any_element(),
    }
}

// ───────────────────────── 过滤文件订阅 ─────────────────────────

fn filter_section(ctx: &SectionContext, enabled: bool) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionFilter"))
        .row(
            subscription::list_item(
                ctx,
                "btGuardFilterUrls",
                "btGuardFilterUrlsDesc",
                "btGuardFilterUrlsPlaceholder",
                "bt_guard_filter_urls",
                ListFormat::Lines,
            )
            .disabled(!enabled),
        )
        .row(filter_status_row(ctx))
}

/// 订阅状态一行的文案：`条目数 · 更新时间 / 尚未更新`，失败时追加错误。
pub(crate) fn filter_source_status(
    source: &BtGuardFilterSourceDto,
    translator: &fluxdown_ui_i18n::Translator,
    time: impl Fn(i64) -> String,
) -> (String, bool) {
    let entries = translator.text_with(
        "btGuardFilterEntries",
        &[("n", &source.entries.to_string())],
    );
    let updated = if source.updated_at > 0 {
        translator.text_with(
            "btGuardFilterUpdatedAt",
            &[("time", &time(source.updated_at))],
        )
    } else {
        translator.text("btGuardFilterNeverUpdated").to_owned()
    };
    let error = source
        .error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    match error {
        Some(error) => (
            format!(
                "{entries} · {updated} · {}",
                translator.text_with("btGuardFilterFailed", &[("error", error)])
            ),
            true,
        ),
        None if !source.ok && source.updated_at <= 0 => (updated, false),
        None => (format!("{entries} · {updated}"), false),
    }
}

fn filter_status_row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let empty = ctx.t("btGuardFilterEmpty");
    let refresh = ctx.t("btGuardFilterRefresh");
    let refreshing = ctx.t("btGuardFilterRefreshing");
    SettingsRow::custom(move |disabled, _, _, cx: &mut App| {
        ensure_state(&store, cx);
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let sources = store
            .read(cx)
            .bt_guard_state()
            .map(|state| state.filter_sources.clone())
            .unwrap_or_default();
        let busy = store.read(cx).is_busy("btGuardFilterRefresh");
        let click_store = store.clone();
        let mut list = v_flex().flex_1().min_w_0().gap(tokens.spacing.xs);
        if sources.is_empty() {
            list = list.child(meta_text(cx).child(empty.clone()));
        }
        for source in &sources {
            let (status, failed) = filter_source_status(source, &translator, format_unix);
            list = list.child(
                v_flex()
                    .min_w_0()
                    .gap(tokens.spacing.xxs)
                    .child(
                        body_text(cx)
                            .truncate()
                            .text_ellipsis_middle()
                            .font_family(tokens.typography.mono.clone())
                            .child(SharedString::from(source.url.clone())),
                    )
                    .child(
                        meta_text(cx)
                            .when(failed, |text| text.text_color(tokens.colors.destructive))
                            .child(SharedString::from(status)),
                    ),
            );
        }
        h_flex()
            .w_full()
            .items_start()
            .justify_between()
            .gap(tokens.spacing.md)
            .child(list)
            .child(
                row_loading_button(
                    "bt-guard-filter-refresh",
                    if busy {
                        refreshing.clone()
                    } else {
                        refresh.clone()
                    },
                    ButtonVariant::Secondary,
                    busy,
                    cx,
                )
                .disabled(disabled || busy || sources.is_empty())
                .on_click(move |_, _, cx| {
                    click_store.update(cx, |store, cx| store.refresh_bt_guard_filter_sources(cx));
                }),
            )
            .into_any_element()
    })
    .keywords([ctx.t("btGuardFilterUrls")])
}

// ───────────────────────── 证据上报 ─────────────────────────

fn report_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("btGuardSectionReport"))
        .row(ctx.item(
            "btGuardReportEnabled",
            Some("btGuardReportEnabledDesc"),
            report_switch(ctx),
        ))
        .row(report_notes_row(ctx))
}

/// 上报开关：开启需要已登录并先经显式同意；关闭随时可用。
fn report_switch(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    Control::custom(move |disabled, key, _window: &mut Window, cx: &mut App| {
        let snapshot = store.read(cx);
        let on = snapshot.daemon_bool(REPORT_KEY);
        let signed_in = snapshot.session().is_some();
        let click_store = store.clone();
        let translator = translator.clone();
        Switch::new(SharedString::from(format!("{key}-switch")))
            .checked(on)
            .disabled(disabled || (!on && !signed_in))
            .on_click(move |checked: &bool, window, cx| {
                if *checked {
                    confirm_report(click_store.clone(), &translator, window, cx);
                } else {
                    click_store.update(cx, |store, cx| {
                        store.set_daemon_bool(REPORT_KEY, false, cx);
                    });
                }
            })
    })
}

/// 开启上报前的显式同意：说明上报与不上报的内容，确认后才写入设置。
fn confirm_report(
    store: Entity<SettingsStore>,
    translator: &fluxdown_ui_i18n::Translator,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text("btGuardReportConsentTitle").to_owned());
    let body = SharedString::from(translator.text("btGuardReportConsentBody").to_owned());
    let cancel = SharedString::from(translator.text("cancel").to_owned());
    let confirm = SharedString::from(translator.text("btGuardReportConsentConfirm").to_owned());
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let store = store.clone();
        alert
            .title(dialog_title(title.clone(), cx))
            .description(body.clone())
            .footer(fluxdown_ui_components::dialog_footer(
                Some(cancel.clone()),
                confirm.clone(),
                DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, _, cx| {
                store.update(cx, |store, cx| store.set_daemon_bool(REPORT_KEY, true, cx));
                true
            })
    });
}

/// 登录提示（未登录时）与上报内容说明。
fn report_notes_row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let login = ctx.t("btGuardReportLoginRequired");
    let title = ctx.t("btGuardReportContentsTitle");
    let contents = ctx.t("btGuardReportContents");
    SettingsRow::custom(move |_, _, _, cx: &mut App| {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let signed_in = store.read(cx).session().is_some();
        notes(
            v_flex().w_full().gap(tokens.spacing.sm),
            signed_in,
            &login,
            &title,
            &contents,
            cx,
        )
        .into_any_element()
    })
    .keywords([ctx.t("btGuardReportContentsTitle")])
}

fn notes(
    column: Div,
    signed_in: bool,
    login: &SharedString,
    title: &SharedString,
    contents: &SharedString,
    cx: &App,
) -> Div {
    let warning = active_theme(cx).extended().colors.warning;
    column
        .when(!signed_in, |column| {
            column.child(meta_text(cx).text_color(warning).child(login.clone()))
        })
        .child(
            v_flex()
                .gap(active_theme(cx).tokens().spacing.xxs)
                .child(
                    meta_text(cx)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title.clone()),
                )
                .child(meta_text(cx).child(contents.clone())),
        )
}

/// `{prefix}{CamelValue}` 的翻译；目录里没有该值（旧客户端遇到新枚举）时显示原始值。
pub(crate) fn label_or_raw(
    translator: &fluxdown_ui_i18n::Translator,
    prefix: &str,
    value: &str,
) -> SharedString {
    let key = format!("{prefix}{}", super::camel(value));
    let text = translator.text(&key);
    SharedString::from(if text == key {
        value.to_owned()
    } else {
        text.to_owned()
    })
}

/// 动作小标签：封禁用危险色，限速 / 停止上传用警示色，放行用成功色，其余中性。
pub(crate) fn action_chip(action: &str, label: SharedString, cx: &App) -> Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens();
    let extended = theme.extended();
    let color = match action {
        "ban" | "banned" => tokens.colors.destructive,
        "choke" | "throttle" | "enforce" => extended.colors.warning,
        "allow" => extended.colors.success,
        _ => tokens.colors.muted_foreground,
    };
    div()
        .flex_none()
        .max_w_full()
        .truncate()
        .px(tokens.spacing.xs)
        .rounded(tokens.radius.sm)
        .border_1()
        .border_color(color)
        .text_color(color)
        .text_size(extended.caption.size)
        .line_height(extended.caption.line_height)
        .child(label)
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{BtGuardCloudStatusDto, BtGuardFilterSourceDto};
    use fluxdown_ui_i18n::I18nCatalog;
    use std::sync::Arc;

    use super::{CloudLine, cloud_lines, filter_source_status};

    fn translator() -> fluxdown_ui_i18n::Translator {
        let catalog = Arc::new(I18nCatalog::load_embedded().unwrap_or_else(|e| panic!("{e}")));
        catalog.translator("en")
    }

    fn time(secs: i64) -> String {
        format!("T{secs}")
    }

    #[test]
    fn empty_cloud_shows_only_the_placeholder() {
        let lines = cloud_lines(&BtGuardCloudStatusDto::default(), &translator(), time);
        assert_eq!(
            lines,
            vec![CloudLine::Plain("No blocklist has been loaded yet".into())]
        );
    }

    #[test]
    fn loaded_cloud_lists_version_counts_and_times() {
        let cloud = BtGuardCloudStatusDto {
            sequence: 42,
            updated_at: 100,
            expires_at: 200,
            entries: 1500,
            client_rules: 7,
            ..BtGuardCloudStatusDto::default()
        };
        let lines = cloud_lines(&cloud, &translator(), time);
        assert_eq!(
            lines,
            vec![
                CloudLine::Plain("Version 42".into()),
                CloudLine::Plain("1500 IP entries · 7 client rules".into()),
                CloudLine::Plain("Updated T100".into()),
                CloudLine::Plain("Valid until T200".into()),
            ]
        );
    }

    #[test]
    fn stale_error_and_rejection_are_flagged_separately() {
        let cloud = BtGuardCloudStatusDto {
            sequence: 1,
            updated_at: 5,
            stale: true,
            last_error: Some("  timeout ".into()),
            rejected_reason: Some("sequence rollback".into()),
            ..BtGuardCloudStatusDto::default()
        };
        let lines = cloud_lines(&cloud, &translator(), time);
        assert!(lines.contains(&CloudLine::Warning(
            "Expired — the cloud layer is paused until a new list arrives".into()
        )));
        assert!(lines.contains(&CloudLine::Error("Last refresh failed: timeout".into())));
        assert!(lines.contains(&CloudLine::Warning(
            "The latest list was rejected: sequence rollback".into()
        )));
        // 空白错误文本不产生空行。
        let blank = BtGuardCloudStatusDto {
            last_error: Some("   ".into()),
            ..cloud
        };
        assert!(
            !cloud_lines(&blank, &translator(), time)
                .iter()
                .any(|line| matches!(line, CloudLine::Error(_)))
        );
    }

    #[test]
    fn filter_status_covers_never_ok_and_failed() {
        let t = translator();
        let never = BtGuardFilterSourceDto {
            url: "https://a".into(),
            ..BtGuardFilterSourceDto::default()
        };
        assert_eq!(
            filter_source_status(&never, &t, time),
            ("not updated yet".to_owned(), false)
        );
        let ok = BtGuardFilterSourceDto {
            ok: true,
            entries: 12,
            updated_at: 9,
            ..never.clone()
        };
        assert_eq!(
            filter_source_status(&ok, &t, time),
            ("12 entries · updated T9".to_owned(), false)
        );
        let failed = BtGuardFilterSourceDto {
            error: Some("HTTP 500".into()),
            ..ok
        };
        assert_eq!(
            filter_source_status(&failed, &t, time),
            (
                "12 entries · updated T9 · failed: HTTP 500".to_owned(),
                true
            )
        );
    }
}
