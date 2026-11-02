//! 插件详情对话框：已安装插件与市场条目共用（manifest 级信息 + 权限 + 页面脚本 + 使用须知）。

use std::{rc::Rc, sync::Arc};

use fluxdown_protocol::{
    MarketEntryDto, PluginArchiveRulesDto, PluginDto, PluginPageScriptDeclDto,
};
use fluxdown_ui_components::{BusyExt as _, ControlExt as _, dialog_scroll_body, tabular_numbers};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _, button::Button, dialog::DialogFooter, h_flex, link::Link,
    v_flex,
};

use super::plugin_live::LivePlugins;
use crate::{pages::Frame, ui};

/// 详情对话框的数据；调用侧从各自 DTO 拆字段。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginDetail {
    pub name: String,
    pub version: String,
    pub identity: String,
    pub description: String,
    pub homepage: String,
    pub author: String,
    pub tags: Vec<String>,
    pub publish_time: String,
    pub min_app_version: String,
    pub settings_count: usize,
    pub permissions: Vec<String>,
    pub yanked_label: Option<String>,
    /// `standard|elevated|full`；空串按 standard。
    pub risk: String,
    pub network_hosts: Vec<String>,
    /// 插件声明的页面脚本（标题 + 匹配站点）；市场条目不携带。
    pub page_scripts: Vec<PluginPageScriptDeclDto>,
    pub archive_rules: bool,
    pub enabled: bool,
    pub stale: bool,
}

impl PluginDetail {
    pub fn from_plugin(plugin: &PluginDto) -> Self {
        Self {
            name: plugin.name.clone(),
            version: plugin.version.clone(),
            identity: plugin.identity.clone(),
            description: plugin.description.clone(),
            homepage: plugin.homepage.clone(),
            archive_rules: plugin.contributions.archive_rules,
            enabled: plugin.enabled,
            settings_count: plugin.settings.len(),
            permissions: plugin.permissions.clone(),
            risk: plugin.risk.clone(),
            network_hosts: plugin.network_hosts.clone(),
            page_scripts: plugin.contributions.page_scripts.clone(),
            ..Self::default()
        }
    }

    pub fn from_market(entry: &MarketEntryDto, translator: &Translator) -> Self {
        Self {
            name: if entry.name.is_empty() {
                entry.plugin_id.clone()
            } else {
                entry.name.clone()
            },
            version: entry.version.clone(),
            identity: entry.plugin_id.clone(),
            description: entry.description.clone(),
            homepage: entry.homepage.clone(),
            author: entry.author.clone(),
            tags: entry.tags.clone(),
            publish_time: entry.publish_time.clone(),
            min_app_version: entry.min_app_version.clone(),
            settings_count: 0,
            permissions: entry.permissions.clone(),
            yanked_label: yanked_label(translator, &entry.yanked),
            risk: crate::market::risk_of(&entry.permissions).to_owned(),
            network_hosts: entry.network_hosts.clone(),
            page_scripts: Vec::new(),
            ..Self::default()
        }
    }
}

/// 市场 `yanked` 标记 → 文案；空 / 未知值不展示。
pub fn yanked_label(translator: &Translator, yanked: &str) -> Option<String> {
    let key = match yanked {
        "deprecated" => "marketYankedDeprecated",
        "vulnerable" => "marketYankedVulnerable",
        "malicious" => "marketYankedMalicious",
        _ => return None,
    };
    Some(translator.text(key).to_owned())
}

/// 权限 → （展示名，说明）；未知权限降级展示原始名。
pub fn permission_label(translator: &Translator, permission: &str) -> (String, String) {
    match permission {
        "ffmpeg" => (
            translator.text("pluginPermFfmpegName").to_owned(),
            translator.text("pluginPermFfmpegDesc").to_owned(),
        ),
        "ytdlp" => (
            translator.text("pluginPermYtdlpName").to_owned(),
            translator.text("pluginPermYtdlpDesc").to_owned(),
        ),
        "auth" => (
            translator.text("pluginPermAuthName").to_owned(),
            translator.text("pluginPermAuthDesc").to_owned(),
        ),
        "tasks" => (
            translator.text("pluginPermTasksName").to_owned(),
            translator.text("pluginPermTasksDesc").to_owned(),
        ),
        "browserRules" => (
            translator.text("pluginPermBrowserRulesName").to_owned(),
            translator.text("pluginPermBrowserRulesDesc").to_owned(),
        ),
        "desktop" => (
            translator.text("pluginPermDesktopName").to_owned(),
            translator.text("pluginPermDesktopDesc").to_owned(),
        ),
        "background" => (
            translator.text("pluginPermBackgroundName").to_owned(),
            translator.text("pluginPermBackgroundDesc").to_owned(),
        ),
        "intercept" => (
            translator.text("pluginPermInterceptName").to_owned(),
            translator.text("pluginPermInterceptDesc").to_owned(),
        ),
        "pageScripts" => (
            translator.text("pluginPermPageScriptsName").to_owned(),
            translator.text("pluginPermPageScriptsDesc").to_owned(),
        ),
        "archive" => (
            translator.text("pluginPermArchiveName").to_owned(),
            translator.text("pluginPermArchiveDesc").to_owned(),
        ),
        other => (
            other.to_owned(),
            translator.text("pluginPermUnknownDesc").to_owned(),
        ),
    }
}

/// 详情对话框「审计记录」入口回调；仅已安装插件提供。
pub type OpenAudit = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchiveRulesNotice {
    Success,
    Error,
}

struct DetailBody {
    detail: PluginDetail,
    translator: Translator,
    port: Option<Arc<dyn crate::ExtensionsPort>>,
    archive_rules_status: Option<PluginArchiveRulesDto>,
    archive_rules_error: Option<String>,
    archive_rules_notice: Option<ArchiveRulesNotice>,
    archive_rules_loading: bool,
    copied: bool,
    closed: bool,
    _live: Option<Subscription>,
}

impl DetailBody {
    fn load_archive_rules(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if self.archive_rules_loading
            || !self.detail.archive_rules
            || !self.detail.enabled
            || self.detail.stale
        {
            return;
        }
        let Some(port) = self.port.clone() else {
            return;
        };
        self.archive_rules_loading = true;
        self.archive_rules_error = None;
        self.archive_rules_notice = None;
        cx.notify();
        let identity = self.detail.identity.clone();
        let future = port.call(
            fluxdown_protocol::method::DAEMON_PLUGIN_ARCHIVE_RULES,
            serde_json::json!({ "identity": identity, "refresh": refresh }),
        );
        cx.spawn(async move |this, cx| {
            let result = future.await.and_then(|value| {
                serde_json::from_value::<PluginArchiveRulesDto>(value).map_err(|_| {
                    fluxdown_protocol::RpcErrorData::new(
                        fluxdown_protocol::ApplicationErrorCode::Internal,
                        false,
                    )
                })
            });
            let Ok(()) = this.update(cx, |this, cx| {
                this.archive_rules_loading = false;
                match result {
                    Ok(status) => {
                        this.archive_rules_status = Some(status);
                        this.archive_rules_notice = refresh.then_some(ArchiveRulesNotice::Success);
                    }
                    Err(error) => {
                        this.archive_rules_error =
                            Some(crate::error_text(&this.translator, &error).to_string());
                        if refresh {
                            this.archive_rules_notice = Some(ArchiveRulesNotice::Error);
                        }
                    }
                }
                cx.notify();
            }) else {
                return;
            };
        })
        .detach();
    }
}

impl Render for DetailBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_detail(self, cx)
    }
}

/// 打开详情对话框。`live` 仅已安装插件提供：详情随最新列表刷新，插件被卸载时关闭对话框。
pub fn open_plugin_detail(
    detail: PluginDetail,
    translator: Translator,
    open_audit: Option<OpenAudit>,
    live: Option<Entity<LivePlugins>>,
    port: Option<Arc<dyn crate::ExtensionsPort>>,
    window: &mut Window,
    cx: &mut App,
) {
    let audit_label = SharedString::from(translator.text("pluginAuditOpenForPlugin").to_owned());
    let has_archive_rules =
        detail.archive_rules && detail.enabled && !detail.stale && port.is_some();
    let body = cx.new(|_| DetailBody {
        detail,
        translator,
        port,
        archive_rules_status: None,
        archive_rules_error: None,
        archive_rules_notice: None,
        archive_rules_loading: false,
        copied: false,
        closed: false,
        _live: None,
    });
    if has_archive_rules {
        body.update(cx, |this, cx| this.load_archive_rules(false, cx));
    }
    if let Some(live) = live {
        body.update(cx, |this, cx| {
            this._live = Some(cx.observe_in(&live, window, |this, live, window, cx| {
                let Some(plugin) = live.read(cx).plugin(&this.detail.identity) else {
                    if !this.closed {
                        this.closed = true;
                        window.close_dialog(cx);
                    }
                    return;
                };
                let mut next = PluginDetail::from_plugin(plugin);
                next.stale = live.read(cx).is_stale();
                let should_load_archive_rules =
                    next.enabled && !next.stale && (!this.detail.enabled || this.detail.stale);
                if next == this.detail {
                    return;
                }
                // 标题在对话框构造器里每帧重建：改名时整窗重绘才能带上新标题。
                if next.name != this.detail.name {
                    window.refresh();
                }
                this.detail = next;
                if should_load_archive_rules {
                    this.load_archive_rules(false, cx);
                }
                cx.notify();
            }));
        });
    }
    window.open_dialog(cx, move |dialog, _, cx| {
        let name = body.read(cx).detail.name.clone();
        let content = body.clone();
        let open_audit = open_audit.clone();
        dialog
            .title(fluxdown_ui_components::dialog_title(name, cx))
            .w(active_theme(cx).text_extent(480.))
            .content(move |root, _, cx| {
                root.min_h_0().child(dialog_scroll_body(
                    "plugin-detail-body",
                    None,
                    content.clone(),
                    cx,
                ))
            })
            .when_some(open_audit, |dialog, open_audit| {
                dialog.footer(
                    DialogFooter::new().child(
                        Button::new("plugin-detail-audit")
                            .outline()
                            .control(cx)
                            .label(audit_label.clone())
                            .on_click(move |_, window, cx| open_audit(window, cx)),
                    ),
                )
            })
    });
}

/// 信息行标签列宽：容纳「最低应用版本」等最长标签。
const LABEL_WIDTH: f32 = 96.;

fn render_detail(body: &DetailBody, cx: &mut Context<DetailBody>) -> impl IntoElement {
    let detail = &body.detail;
    let translator = &body.translator;
    let theme = active_theme(cx);
    let frame = Frame {
        translator,
        tokens: theme.tokens(),
        extended: theme.extended(),
        components: theme.components(),
        stale: false,
    };
    let tokens = frame.tokens;
    let text = |key: &str| SharedString::from(translator.text(key).to_owned());
    let label_cell = |label: SharedString| {
        ui::meta_text(label, frame)
            .w(theme.text_extent(LABEL_WIDTH))
            .flex_shrink_0()
    };
    let info_row = |label: SharedString, value: String| {
        h_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .items_baseline()
            .child(label_cell(label))
            .child(ui::body_text(value, frame).flex_1().min_w_0())
    };
    // 分区标题：caption MEDIUM + 三级文字色。
    let section_title = |label: SharedString| {
        div()
            .pt(tokens.spacing.sm)
            .text_size(frame.extended.caption.size)
            .line_height(frame.extended.caption.line_height)
            .font_weight(FontWeight::MEDIUM)
            .text_color(frame.extended.colors.text_tertiary)
            .child(label)
    };
    let permissions = detail
        .permissions
        .iter()
        .map(|permission| {
            let (name, description) = permission_label(translator, permission);
            v_flex()
                .w_full()
                .gap(tokens.spacing.xxs)
                .child(ui::title_text(name, frame))
                .child(ui::meta_text(description, frame))
        })
        .collect::<Vec<_>>();
    let mut content = v_flex().w_full().gap(tokens.spacing.sm).child(
        h_flex()
            .gap(tokens.spacing.sm)
            .items_center()
            .flex_wrap()
            .child(
                ui::meta_text(format!("v{}", detail.version), frame)
                    .font_features(tabular_numbers()),
            )
            .children(
                detail
                    .yanked_label
                    .clone()
                    .map(|label| ui::tone_pill(label, tokens.colors.destructive, frame)),
            )
            .children(ui::risk_pill("plugin-detail-risk", &detail.risk, frame))
            .children(
                detail
                    .tags
                    .iter()
                    .cloned()
                    .map(|tag| ui::neutral_pill(tag, frame)),
            ),
    );
    content = content.child(info_row(
        text("pluginDetailIdentity"),
        detail.identity.clone(),
    ));
    if !detail.author.is_empty() {
        content = content.child(info_row(text("pluginDetailAuthor"), detail.author.clone()));
    }
    if ui::is_web_url(&detail.homepage) {
        content = content.child(
            h_flex()
                .w_full()
                .gap(tokens.spacing.md)
                .items_baseline()
                .child(label_cell(text("pluginDetailHomepage")))
                .child(
                    Link::new("plugin-detail-homepage")
                        .href(detail.homepage.clone())
                        .text_size(tokens.typography.sm.size)
                        .child(detail.homepage.clone()),
                ),
        );
    } else if !detail.homepage.is_empty() {
        content = content.child(info_row(
            text("pluginDetailHomepage"),
            detail.homepage.clone(),
        ));
    }
    if !detail.publish_time.is_empty() {
        content = content.child(info_row(
            text("pluginDetailPublishTime"),
            detail.publish_time.clone(),
        ));
    }
    if !detail.min_app_version.is_empty() {
        content = content.child(info_row(
            text("pluginDetailMinAppVersion"),
            detail.min_app_version.clone(),
        ));
    }
    if detail.settings_count > 0 {
        content = content.child(info_row(
            text("pluginDetailSettings"),
            translator.text_with(
                "pluginDetailSettingsCount",
                &[("count", &detail.settings_count.to_string())],
            ),
        ));
    }
    if !detail.description.is_empty() {
        content = content
            .child(section_title(text("pluginDetailDescription")))
            .child(ui::body_text(detail.description.clone(), frame));
    }
    if !permissions.is_empty() {
        content = content
            .child(section_title(text("pluginDetailPermissions")))
            .children(permissions);
    }
    if !detail.page_scripts.is_empty() {
        content = content
            .child(section_title(text("pluginConsentPageScripts")))
            .children(page_script_rows(&detail.page_scripts, frame));
    }
    if !detail.network_hosts.is_empty() {
        content = content
            .child(section_title(text("pluginConsentNetworkHosts")))
            .children(
                detail
                    .network_hosts
                    .iter()
                    .map(|host| ui::body_text(host.clone(), frame)),
            );
    }
    if detail.archive_rules {
        content = content.child(section_title(text("pluginArchiveRulesTitle")));
        if body.archive_rules_loading && body.archive_rules_status.is_none() {
            content = content.child(ui::meta_text(text("pluginArchiveRulesRefreshing"), frame));
        }
        if let Some(status) = &body.archive_rules_status {
            content = content
                .child(info_row(
                    text("pluginArchiveRulesSequence"),
                    status.sequence.to_string(),
                ))
                .child(info_row(
                    text("pluginArchiveRulesCount"),
                    status.rule_count.to_string(),
                ))
                .child(info_row(
                    text("pluginArchiveRulesLastChecked"),
                    format_archive_timestamp(status.last_checked_at, translator),
                ))
                .child(info_row(
                    text("pluginArchiveRulesLastUpdated"),
                    format_archive_timestamp(status.last_updated_at, translator),
                ))
                .child(info_row(
                    text("pluginArchiveRulesUpdateUrl"),
                    status
                        .update_url
                        .clone()
                        .unwrap_or_else(|| translator.text("pluginArchiveRulesNone").to_owned()),
                ))
                .child(info_row(
                    text("pluginArchiveRulesAutomatic"),
                    translator
                        .text(if status.automatic_updates {
                            "pluginArchiveRulesEnabled"
                        } else {
                            "pluginArchiveRulesDisabled"
                        })
                        .to_owned(),
                ));
            if let Some(error) = &status.last_error {
                content = content.child(info_row(text("pluginArchiveRulesError"), error.clone()));
            }
            let key = status.public_key.clone();
            content = content.child(
                h_flex()
                    .w_full()
                    .gap(tokens.spacing.md)
                    .items_center()
                    .child(label_cell(text("pluginArchiveRulesPublicKey")))
                    .child(ui::body_text(key, frame).flex_1().min_w_0())
                    .child(
                        Button::new("plugin-archive-rules-copy")
                            .outline()
                            .control(cx)
                            .label(text(if body.copied {
                                "pluginArchiveRulesCopied"
                            } else {
                                "pluginArchiveRulesCopy"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let Some(status) = &this.archive_rules_status else {
                                    return;
                                };
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    status.public_key.clone(),
                                ));
                                this.copied = true;
                                cx.notify();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_millis(1500))
                                        .await;
                                    let Ok(()) = this.update(cx, |this, cx| {
                                        this.copied = false;
                                        cx.notify();
                                    }) else {
                                        return;
                                    };
                                })
                                .detach();
                            })),
                    ),
            );
        }
        if let Some(error) = &body.archive_rules_error {
            content = content.child(ui::body_text(error.clone(), frame));
        }
        if let Some(notice) = body.archive_rules_notice {
            let key = match notice {
                ArchiveRulesNotice::Success => "pluginArchiveRulesRefreshSuccess",
                ArchiveRulesNotice::Error => "pluginArchiveRulesRefreshError",
            };
            content = content.child(ui::meta_text(text(key), frame));
        }
        content = content.child(
            Button::new("plugin-archive-rules-refresh")
                .outline()
                .control(cx)
                .busy(body.archive_rules_loading)
                .disabled(!detail.enabled || detail.stale || body.archive_rules_loading)
                .label(text(if body.archive_rules_loading {
                    "pluginArchiveRulesRefreshing"
                } else {
                    "pluginArchiveRulesRefresh"
                }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.load_archive_rules(true, cx);
                })),
        );
    }
    content
        .child(section_title(text("pluginDetailUsage")))
        .child(ui::meta_text(text("pluginDetailUsageBody"), frame))
}

fn format_archive_timestamp(timestamp: Option<u64>, translator: &Translator) -> String {
    timestamp
        .and_then(|value| i64::try_from(value).ok())
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| translator.text("pluginArchiveRulesNone").to_owned())
}

/// 页面脚本行：标题 + 匹配站点（逗号分隔）；详情与授权对话框共用。
pub fn page_script_rows(scripts: &[PluginPageScriptDeclDto], frame: Frame<'_>) -> Vec<gpui::Div> {
    scripts
        .iter()
        .map(|script| {
            v_flex()
                .w_full()
                .gap(frame.tokens.spacing.xxs)
                .child(ui::title_text(script.title.clone(), frame))
                .child(ui::meta_text(script.matches.join(", "), frame))
        })
        .collect()
}
