//! 关于：版本、软件更新、日志导出、浏览器扩展与捐赠链接。

use fluxdown_protocol::{ReleaseNoteDto, UpdatePhase, method};
use fluxdown_ui_components::{ButtonVariant, FluxIcon, button, loading_button};
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, FontWeight, IntoElement as _, ParentElement, SharedString, Styled, div,
    prelude::FluentBuilder as _, relative,
};
use gpui_component::text::TextView;
use gpui_component::{h_flex, v_flex};
use serde_json::json;

use super::SectionContext;
use crate::ui::{Control, SettingsPage, SettingsRow, SettingsSection, body_text, meta_text};
use crate::update_view;

pub(crate) const APP_VERSION: &str = fluxdown_protocol::APP_VERSION;
const WEBSITE: &str = "https://fluxdown.com";
const CHROME_STORE: &str = "https://chromewebstore.google.com/search/FluxDown";
const FIREFOX_STORE: &str = "https://addons.mozilla.org/firefox/addon/fluxdown/";
const EDGE_STORE: &str = "https://microsoftedge.microsoft.com/addons/search/FluxDown";
const DONATE: &str = "https://fluxdown.com/sponsor";
const DB_IP: &str = "https://db-ip.com";
const FLAG_ICONS: &str = "https://github.com/lipis/flag-icons";
/// 更新说明正文的行高（字号倍数）：多行列表项与中文长句比控件文字需要更宽松的行距。
const RELEASE_NOTES_LINE_HEIGHT: f32 = 1.6;

pub(crate) fn page(ctx: &SectionContext, _cx: &mut App) -> SettingsPage {
    SettingsPage::new(
        "about",
        ctx.t("settingsCatAbout"),
        ctx.t("settingsCatAboutDesc"),
        FluxIcon::Info,
    )
    .sections([
        version_section(ctx),
        update_section(ctx),
        logs_section(ctx),
        links_section(ctx),
        open_source_section(ctx),
    ])
}

fn version_section(ctx: &SectionContext) -> SettingsSection {
    let version = SharedString::from(format!("v{APP_VERSION}"));
    let protocol = SharedString::from(fluxdown_protocol::PROTOCOL_VERSION.to_string());
    SettingsSection::new()
        .title(SharedString::from("FluxDown"))
        .row(ctx.item(
            "currentVersion",
            None,
            Control::custom(move |_, _, _, cx: &mut App| body_text(cx).child(version.clone())),
        ))
        .row(ctx.item(
            "protocolVersionLabel",
            None,
            Control::custom(move |_, _, _, cx: &mut App| body_text(cx).child(protocol.clone())),
        ))
}

fn update_section(ctx: &SectionContext) -> SettingsSection {
    let channels = vec![
        (SharedString::from("stable"), ctx.t("updateChannelStable")),
        (
            SharedString::from("frontier"),
            ctx.t("updateChannelFrontier"),
        ),
    ];
    SettingsSection::new()
        .title(ctx.t("softwareUpdate"))
        .row(ctx.item(
            "updateChannel",
            Some("updateChannelDesc"),
            ctx.pref_dropdown("general.update_channel", "stable", channels),
        ))
        .row(ctx.item(
            "autoCheckUpdate",
            Some("autoCheckUpdateBackgroundDesc"),
            ctx.pref_switch("general.auto_check_update", true),
        ))
        .row(ctx.item(
            "checkUpdate",
            Some("checkUpdateDesc"),
            check_update_control(ctx),
        ))
        .row(release_notes_item(ctx))
}

fn check_update_control(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    Control::custom(move |disabled, _key, _window, cx: &mut App| {
        let tokens = active_theme(cx).tokens();
        let (status, busy_checking, installing, cancelling) = {
            let store = store.read(cx);
            (
                store.update_status().clone(),
                store.is_busy("update"),
                store.is_busy("updateInstall"),
                store.is_busy("updateCancel"),
            )
        };
        // 后台自动检查也让按钮转圈；「检查中」状态行由按钮 loading 承担，不再单独显示。
        let checking = busy_checking || status.phase == UpdatePhase::Checking;
        let lines: Vec<SharedString> = [
            update_view::status_line(&status).filter(|_| status.phase != UpdatePhase::Checking),
            update_view::manual_line(&status),
        ]
        .into_iter()
        .flatten()
        .map(|line| SharedString::from(line.text(&translator)))
        .collect();
        let manual_url = update_view::manual_url(&status).map(str::to_owned);
        let mut buttons = h_flex().gap(tokens.spacing.sm).items_center();
        if update_view::can_cancel(&status) {
            let cancel_store = store.clone();
            buttons = buttons.child(
                loading_button(
                    "about-update-cancel",
                    SharedString::from(translator.text("cancel").to_owned()),
                    ButtonVariant::Secondary,
                    cancelling,
                    cx,
                )
                .disabled(disabled || cancelling)
                .on_click(move |_, _, cx| {
                    cancel_store.update(cx, |store, cx| store.cancel_update(cx));
                }),
            );
        }
        if update_view::can_install(&status) {
            let install_store = store.clone();
            buttons = buttons.child(
                loading_button(
                    "about-update-install",
                    SharedString::from(translator.text("updateRestartNow").to_owned()),
                    ButtonVariant::Primary,
                    installing,
                    cx,
                )
                .disabled(disabled || installing)
                .on_click(move |_, _, cx| {
                    install_store.update(cx, |store, cx| store.install_update(cx));
                }),
            );
        }
        if let Some(url) = manual_url {
            buttons = buttons.child(
                button(
                    "about-update-open-site",
                    SharedString::from(translator.text("updateFailedOpenSite").to_owned()),
                    ButtonVariant::Primary,
                    cx,
                )
                .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        let check_store = store.clone();
        buttons = buttons.child(
            loading_button(
                "about-check-update",
                SharedString::from(translator.text("checkUpdate").to_owned()),
                ButtonVariant::Secondary,
                checking,
                cx,
            )
            .disabled(disabled || checking)
            .on_click(move |_, _, cx| {
                check_store.update(cx, |store, cx| {
                    let channel = store.pref_str("general.update_channel", "stable");
                    store.check_update(Some(channel), cx);
                });
            }),
        );
        v_flex()
            .gap(tokens.spacing.xs)
            .items_end()
            .children(lines.into_iter().map(|line| meta_text(cx).child(line)))
            .child(buttons)
    })
}

fn release_notes_item(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    // 跟随应用界面语言（设置里可手动切换），而不是系统 locale。
    let locale = ctx.translator.locale().to_owned();
    SettingsRow::custom(move |_, _, _, cx: &mut App| {
        let notes = store.read(cx).update_status().notes.clone();
        if notes.is_empty() {
            return div().into_any_element();
        }
        release_notes(notes, &locale, cx).into_any_element()
    })
}

/// 更新说明列表（最多 10 个版本，按界面语言取对应段落）；关于页与新版本提醒窗口共用。
/// 每个版本的标题统一为「v版本 + 发布日期」，正文开头与之重复的版本标题会被去掉。
pub(crate) fn release_notes(notes: Vec<ReleaseNoteDto>, locale: &str, cx: &App) -> gpui::Div {
    let theme = active_theme(cx);
    let tokens = theme.tokens().clone();
    let extended = theme.extended().clone();
    v_flex()
        .w_full()
        .gap(tokens.spacing.lg)
        .children(notes.into_iter().take(10).enumerate().map(|(index, note)| {
            let body =
                strip_version_heading(localized_release_body(&note.body, locale), &note.version)
                    .to_owned();
            let date =
                release_date(&note.published_at).map(|date| SharedString::from(date.to_owned()));
            let markdown = (!body.is_empty()).then(|| {
                div()
                    .w_full()
                    .text_size(tokens.typography.sm.size)
                    .line_height(relative(RELEASE_NOTES_LINE_HEIGHT))
                    .text_color(tokens.colors.foreground)
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("release-notes-{}", note.version)),
                            body,
                        )
                        .selectable(true)
                        .w_full(),
                    )
            });
            v_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .when(index > 0, |this| {
                    this.pt(tokens.spacing.lg)
                        .border_t(extended.stroke.thin)
                        .border_color(extended.colors.hairline)
                })
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .items_center()
                        .child(
                            body_text(cx)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(format!("v{}", note.version))),
                        )
                        .children(date.map(|date| meta_text(cx).child(date))),
                )
                .children(markdown)
        }))
}

/// 发布时间（ISO 8601）的日期部分 `YYYY-MM-DD`；不是该格式时为 `None`，标题只显示版本。
fn release_date(published_at: &str) -> Option<&str> {
    let published_at = published_at.trim();
    let date = published_at.get(..10)?;
    let rest = published_at.get(10..)?;
    let digits = date.bytes().enumerate().all(|(index, byte)| match index {
        4 | 7 => byte == b'-',
        _ => byte.is_ascii_digit(),
    });
    (digits && (rest.is_empty() || rest.starts_with(['T', 't', ' ']))).then_some(date)
}

/// 去掉正文开头与标题重复的版本标题（`## 0.5.7 - 2026-10-15`、`## [0.5.6] - …`、
/// `## v0.4.8 - …`）。只认首个非空行、且标题文字以该版本号开头（其后不是版本号字符）；
/// 其它开头（如 `## 问题修复`、别的版本）原样保留。
fn strip_version_heading<'a>(body: &'a str, version: &str) -> &'a str {
    let body = body.trim_start();
    let version = version.trim().trim_start_matches(['v', 'V']);
    let (first, rest) = body.split_once('\n').unwrap_or((body, ""));
    let Some(title) = atx_heading_text(first.trim_end()) else {
        return body;
    };
    let title = title.trim_start_matches('[').trim_start_matches(['v', 'V']);
    match title.strip_prefix(version) {
        Some(after)
            if !version.is_empty()
                && after.chars().next().is_none_or(|next| {
                    !(next.is_alphanumeric() || matches!(next, '.' | '-' | '+' | '_'))
                }) =>
        {
            rest.trim_start()
        }
        _ => body,
    }
}

/// ATX 标题（1–6 个 `#` 后接空白或行尾）的标题文字。
fn atx_heading_text(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('#');
    let level = line.len() - text.len();
    ((1..=6).contains(&level) && (text.is_empty() || text.starts_with([' ', '\t'])))
        .then(|| text.trim())
}

fn localized_release_body<'a>(body: &'a str, locale: &str) -> &'a str {
    let language = locale
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("en");
    let chinese = language.eq_ignore_ascii_case("zh");
    let mut sections = [None, None];
    let mut previous = None;
    let mut cursor = 0;
    while let Some(offset) = body[cursor..].find("<!--") {
        let start = cursor + offset;
        let content_start = start + "<!--".len();
        let Some(offset) = body[content_start..].find("-->") else {
            break;
        };
        let content_end = content_start + offset;
        cursor = content_end + "-->".len();
        let index = match body[content_start..content_end].trim() {
            "fluxdown:lang:zh" => 0,
            "fluxdown:lang:en" => 1,
            _ => continue,
        };
        if let Some((index, section_start)) = previous {
            sections[index] = Some(body[section_start..start].trim());
        }
        previous = Some((index, cursor));
    }
    if let Some((index, section_start)) = previous {
        sections[index] = Some(body[section_start..].trim());
    }
    sections[usize::from(!chinese)]
        .or(sections[0])
        .or(sections[1])
        .unwrap_or(body)
}

fn logs_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("logExport"))
        .subtitle(ctx.t("logExportDesc"))
        .row(ctx.item(
            "logMaxSize",
            Some("logMaxSizeDesc"),
            ctx.daemon_number("log_max_size_mb").unit("MB"),
        ))
        .row(ctx.item("logExportButton", None, export_control(ctx)))
}

fn export_control(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let export = ctx.t("logExportButton");
    let open = ctx.t("doctorActionOpenLogDir");
    Control::custom(move |disabled, _key, _window, cx: &mut App| {
        let tokens = active_theme(cx).tokens();
        let busy = store.read(cx).is_busy("logExport");
        let opening = store.read(cx).is_busy_tagged("logExport", "openLogDir");
        let exporting = store.read(cx).is_busy_tagged("logExport", "exportLogs");
        let export_store = store.clone();
        let open_store = store.clone();
        h_flex()
            .gap(tokens.spacing.sm)
            .child(
                loading_button(
                    "about-open-log-dir",
                    open.clone(),
                    ButtonVariant::Secondary,
                    opening,
                    cx,
                )
                .disabled(disabled || opening)
                .on_click(move |_, _, cx| {
                    open_store.update(cx, |store, cx| {
                        store.call_with(
                            "logExport",
                            method::AGENT_DIAGNOSTICS_LOG_PATHS,
                            json!({}),
                            cx,
                            |store, result, cx| {
                                if let Ok(value) = result
                                    && let Some(dir) = value
                                        .get("agentLogDir")
                                        .and_then(serde_json::Value::as_str)
                                        .filter(|dir| !dir.is_empty())
                                {
                                    store.call_simple(
                                        "logExport",
                                        method::AGENT_PLATFORM_OPEN_PATH,
                                        json!({ "path": dir, "reveal": false }),
                                        None,
                                        cx,
                                    );
                                    store.tag_busy("logExport", "openLogDir");
                                }
                            },
                        );
                        store.tag_busy("logExport", "openLogDir");
                    });
                }),
            )
            .child(
                loading_button(
                    "about-export-logs",
                    export.clone(),
                    ButtonVariant::Primary,
                    exporting,
                    cx,
                )
                .disabled(disabled || busy)
                .on_click(move |_, _, cx| {
                    let store = export_store.clone();
                    let receiver =
                        cx.prompt_for_new_path(&std::env::temp_dir(), Some("fluxdown-logs.zip"));
                    cx.spawn(async move |cx| {
                        if let Ok(Ok(Some(path))) = receiver.await {
                            let target = path.display().to_string();
                            store.update(cx, |store, cx| {
                                store.call_simple(
                                    "logExport",
                                    method::AGENT_DIAGNOSTICS_EXPORT_LOGS,
                                    json!({ "targetPath": target }),
                                    Some("logExportSuccessNotice"),
                                    cx,
                                );
                                store.tag_busy("logExport", "exportLogs");
                            });
                        }
                    })
                    .detach();
                }),
            )
    })
}

fn links_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("extensionCardTitle"))
        .subtitle(ctx.t("extensionCardDesc"))
        .row(link_item(
            ctx,
            "extensionCardTitle",
            &[
                ("Chrome", CHROME_STORE),
                ("Firefox", FIREFOX_STORE),
                ("Edge", EDGE_STORE),
            ],
        ))
        .row(link_item(
            ctx,
            "donateTitle",
            &[("donateButton", DONATE), ("officialWebsite", WEBSITE)],
        ))
}

/// 第三方数据 / 素材署名：对端国家来自 DB-IP lite（CC BY 4.0，要求署名并链接），旗帜为 flag-icons（MIT）。
/// 两者都是专有名词署名行，按原文显示，不走翻译（`link_item` 对非目录键回落为键名本身）。
fn open_source_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("mobileOpenSource"))
        .row(link_item(
            ctx,
            "IP Geolocation by DB-IP",
            &[("db-ip.com", DB_IP)],
        ))
        .row(link_item(
            ctx,
            "flag-icons (MIT)",
            &[("GitHub", FLAG_ICONS)],
        ))
}

fn link_item(
    ctx: &SectionContext,
    title_key: &str,
    links: &[(&'static str, &'static str)],
) -> SettingsRow {
    let links: Vec<(SharedString, &'static str)> = links
        .iter()
        .map(|(label, url)| {
            let text = ctx.translator.text(label);
            (SharedString::from(text.to_owned()), *url)
        })
        .collect();
    let title = ctx.t(title_key);
    SettingsRow::custom(move |_, _, _, cx: &mut App| {
        let tokens = active_theme(cx).tokens();
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(tokens.spacing.md)
            .child(body_text(cx).child(title.clone()))
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .children(links.iter().map(|(label, url)| {
                        let url = *url;
                        button(
                            SharedString::from(format!("about-link-{url}")),
                            label.clone(),
                            ButtonVariant::Secondary,
                            cx,
                        )
                        .on_click(move |_, _, cx| cx.open_url(url))
                    })),
            )
            .into_any_element()
    })
    .keywords([title_key.to_owned()])
}

#[cfg(test)]
mod tests {
    use super::{localized_release_body, release_date, strip_version_heading};

    #[test]
    fn release_notes_follow_ui_language() {
        let body = "前言\n<!-- fluxdown:lang:zh -->\n## 问题修复\n- 修复下载\n\
                    <!-- fluxdown:lang:en -->\n## Bug Fixes\n- Fix downloads\n";
        for locale in ["zh", "zh_CN", "zh-Hans-CN", "zh-Hant-TW", "ZH_hk"] {
            assert_eq!(
                localized_release_body(body, locale),
                "## 问题修复\n- 修复下载"
            );
        }
        for locale in ["en", "en-US", "fr_FR", "ja", ""] {
            assert_eq!(
                localized_release_body(body, locale),
                "## Bug Fixes\n- Fix downloads"
            );
        }
    }

    #[test]
    fn release_notes_handle_marker_order_and_whitespace() {
        let body = "<!--fluxdown:lang:en-->\r\nEnglish\r\n\
                    <!--\tfluxdown:lang:zh\n-->\r\n中文\r\n";
        assert_eq!(localized_release_body(body, "en"), "English");
        assert_eq!(localized_release_body(body, "zh"), "中文");
    }

    #[test]
    fn release_notes_preserve_legacy_and_use_available_translation() {
        let legacy = "## 旧版本\nUntranslated notes\n<!-- unrelated -->";
        assert_eq!(localized_release_body(legacy, "zh"), legacy);
        assert_eq!(localized_release_body(legacy, "en"), legacy);
        let chinese = "<!-- fluxdown:lang:zh -->\n中文";
        assert_eq!(localized_release_body(chinese, "en"), "中文");
        let english = "<!-- fluxdown:lang:en -->\nEnglish";
        assert_eq!(localized_release_body(english, "zh"), "English");
    }

    #[test]
    fn release_notes_drop_the_heading_that_repeats_the_version() {
        let body = "## 0.5.7 - 2026-10-15\n\n### 🚀 新功能\n- 批量导入";
        assert_eq!(
            strip_version_heading(body, "0.5.7"),
            "### 🚀 新功能\n- 批量导入"
        );
        // 已发布说明里出现过的标题写法都按同一版本识别。
        for heading in [
            "## [0.5.6] - 2026-10-12",
            "## v0.5.6 - 2026-10-12",
            "# 0.5.6",
        ] {
            let body = format!("{heading}\r\n\r\n- fix\r\n");
            assert_eq!(
                strip_version_heading(&body, "0.5.6"),
                "- fix\r\n",
                "{heading}"
            );
            assert_eq!(
                strip_version_heading(&body, "v0.5.6"),
                "- fix\r\n",
                "{heading}"
            );
        }
        assert_eq!(strip_version_heading("\n  ## 0.5.7\n", "0.5.7"), "");
    }

    #[test]
    fn release_notes_keep_headings_of_other_versions_and_sections() {
        for (body, version) in [
            ("## 0.5.10 - 2026-11-01\n- a", "0.5.1"),
            ("## 0.5.7-rc.1 - 2026-10-14\n- a", "0.5.7"),
            ("## 问题修复\n- a", "0.5.7"),
            ("##0.5.7\n- a", "0.5.7"),
            ("####### 0.5.7\n- a", "0.5.7"),
            ("- 0.5.7 修复\n## 0.5.7", "0.5.7"),
            ("## 0.5.7\n- a", ""),
        ] {
            assert_eq!(
                strip_version_heading(body, version),
                body,
                "{body:?} / {version}"
            );
        }
    }

    #[test]
    fn release_dates_take_the_calendar_day_of_iso_timestamps() {
        assert_eq!(release_date("2026-10-15T06:14:16Z"), Some("2026-10-15"));
        assert_eq!(release_date(" 2026-10-15 "), Some("2026-10-15"));
        assert_eq!(release_date("2026-10-15 06:14:16"), Some("2026-10-15"));
        for invalid in [
            "",
            "2026-10",
            "2026/10/15T00:00:00Z",
            "2026-10-150",
            "十月十五日之后",
        ] {
            assert_eq!(release_date(invalid), None, "{invalid}");
        }
    }
}
