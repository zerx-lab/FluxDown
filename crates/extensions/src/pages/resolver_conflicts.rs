//! 插件页「多个插件可处理同一链接」卡：把 `PluginDto.resolverUrlConflicts` 按站点分组，
//! 卡内直接为站点选定默认插件（`daemon.plugin.setSiteDefault` 携带候选，尚无记录也能写）。
//! 用户确认过、且覆盖全部冲突插件的站点移出本卡，改由「站点默认」子页管理——卡片只列
//! 还需要处理的事项。两侧主机都是通配（或 `magnet:` 等无主机链接）的重叠无法按站点
//! 解决，只说明后果；同插件重复声明属于插件作者的问题，只在开发模式显示；待授权插件
//! 尚未参与匹配，不进卡片，只在安装 / 启用后的提示里预告。

use fluxdown_protocol::{PluginDto, PluginSiteDefaultDto};
use fluxdown_ui_components::{BusyExt as _, ControlExt as _, FluxIcon, tooltip_view};
use fluxdown_ui_i18n::Translator;
use gpui::{
    Context, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use super::{Frame, plugins::list_row, site_defaults::plugin_display_name};
use crate::{ExtensionsView, ui};

/// 两个已启用插件在同一站点上的重叠（按站点合并）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SiteConflict<'a> {
    /// 站点键（同 `PluginSiteDefaultDto.site`）。
    pub site: &'a str,
    /// 能处理该站点链接的插件，按插件列表顺序。
    pub plugins: Vec<&'a str>,
    /// 参与重叠的（插件，模式），去重。
    pub patterns: Vec<(&'a str, &'a str)>,
}

/// 一对插件的单条重叠示例。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PairConflict<'a> {
    pub owner: &'a str,
    pub pattern: &'a str,
    pub other: &'a str,
    pub other_pattern: &'a str,
    pub site: Option<&'a str>,
}

/// 按处理方式分好类的冲突。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ConflictReport<'a> {
    /// 双方都已启用、可用站点默认插件解决。
    pub sites: Vec<SiteConflict<'a>>,
    /// 双方都已启用、但无法确定站点（下载时只能询问）。
    pub unscoped: Vec<PairConflict<'a>>,
    /// 一方待授权：目前不会同时参与匹配。
    pub pending: Vec<PairConflict<'a>>,
    /// 同一插件内重复声明的模式。
    pub duplicates: Vec<PairConflict<'a>>,
}

/// 站点冲突当前的处理状态（对照站点默认记录）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SiteResolution<'a> {
    /// 用户确认过、覆盖全部冲突插件的默认插件：下载不再询问。
    Confirmed,
    /// 无人值守下载自动选用、尚未确认的插件。
    Automatic(&'a str),
    /// 没有可用记录：下载时询问。
    Unset,
}

/// 分类冲突；`identity` 为 `Some` 时只取与该插件相关的配对（操作后提示）。
pub(crate) fn conflict_report<'a>(
    plugins: &'a [PluginDto],
    identity: Option<&str>,
) -> ConflictReport<'a> {
    let enabled = |id: &str| plugins.iter().any(|p| p.enabled && p.identity == id);
    let order = |id: &str| plugins.iter().position(|p| p.identity == id);
    let mut report = ConflictReport::default();
    for owner in plugins {
        for conflict in &owner.resolver_url_conflicts {
            if identity.is_some_and(|id| id != owner.identity && id != conflict.other_identity) {
                continue;
            }
            let pair = PairConflict {
                owner: &owner.identity,
                pattern: &conflict.pattern,
                other: &conflict.other_identity,
                other_pattern: &conflict.other_pattern,
                site: conflict.site.as_deref(),
            };
            if pair.owner == pair.other {
                report.duplicates.push(pair);
            } else if !owner.enabled || !enabled(pair.other) {
                report.pending.push(pair);
            } else if let Some(site) = pair.site {
                let index = match report.sites.iter().position(|s| s.site == site) {
                    Some(index) => index,
                    None => {
                        report.sites.push(SiteConflict {
                            site,
                            plugins: Vec::new(),
                            patterns: Vec::new(),
                        });
                        report.sites.len() - 1
                    }
                };
                let Some(group) = report.sites.get_mut(index) else {
                    continue;
                };
                for (plugin, pattern) in
                    [(pair.owner, pair.pattern), (pair.other, pair.other_pattern)]
                {
                    if !group.plugins.contains(&plugin) {
                        group.plugins.push(plugin);
                    }
                    if !group.patterns.contains(&(plugin, pattern)) {
                        group.patterns.push((plugin, pattern));
                    }
                }
            } else {
                report.unscoped.push(pair);
            }
        }
    }
    for group in &mut report.sites {
        group.plugins.sort_by_key(|id| order(id));
        group.patterns.sort_by_key(|(id, _)| order(id));
    }
    report.sites.sort_by(|a, b| a.site.cmp(b.site));
    report
}

/// 对照站点默认记录判断该站点是否已处理。记录须选中冲突插件之一；已确认的记录还须
/// 覆盖全部冲突插件，否则引擎遇到集合外插件仍会询问，按未处理显示。
pub(crate) fn site_resolution<'a>(
    conflict: &SiteConflict<'_>,
    defaults: &'a [PluginSiteDefaultDto],
) -> SiteResolution<'a> {
    let Some(entry) = defaults.iter().find(|d| d.site == conflict.site) else {
        return SiteResolution::Unset;
    };
    if !conflict.plugins.contains(&entry.identity.as_str()) {
        return SiteResolution::Unset;
    }
    if entry.automatic {
        return SiteResolution::Automatic(&entry.identity);
    }
    if conflict
        .plugins
        .iter()
        .all(|plugin| entry.candidates.iter().any(|c| c == plugin))
    {
        SiteResolution::Confirmed
    } else {
        SiteResolution::Unset
    }
}

fn join_names(plugins: &[PluginDto], ids: &[&str], translator: &Translator) -> String {
    ids.iter()
        .map(|id| plugin_display_name(plugins, id))
        .collect::<Vec<_>>()
        .join(translator.text("pluginResolverConflictJoin"))
}

fn push_unique<'a>(list: &mut Vec<&'a str>, items: impl IntoIterator<Item = &'a str>) {
    for item in items {
        if !list.contains(&item) {
            list.push(item);
        }
    }
}

/// 安装 / 启用成功后的提示：用一两句话说明影响与去处，不罗列模式。
/// 已确认站点默认插件的站点不再提示；没有需要处理的事项时返回 `None`。
pub(crate) fn conflict_notice(
    plugins: &[PluginDto],
    defaults: &[PluginSiteDefaultDto],
    identity: &str,
    translator: &Translator,
) -> Option<String> {
    let report = conflict_report(plugins, Some(identity));
    let mut lines = Vec::new();

    let open: Vec<&SiteConflict<'_>> = report
        .sites
        .iter()
        .filter(|site| site_resolution(site, defaults) != SiteResolution::Confirmed)
        .collect();
    if !open.is_empty() {
        let mut ids = Vec::new();
        for site in &open {
            push_unique(&mut ids, site.plugins.iter().copied());
        }
        let sites = open
            .iter()
            .map(|site| site.site)
            .collect::<Vec<_>>()
            .join(translator.text("pluginResolverConflictJoin"));
        lines.push(translator.text_with(
            "pluginResolverConflictNoticeSites",
            &[
                ("plugins", &join_names(plugins, &ids, translator)),
                ("sites", &sites),
            ],
        ));
    }
    if !report.unscoped.is_empty() {
        let mut ids = Vec::new();
        for pair in &report.unscoped {
            push_unique(&mut ids, [pair.owner, pair.other]);
        }
        lines.push(translator.text_with(
            "pluginResolverConflictNoticeAny",
            &[("plugins", &join_names(plugins, &ids, translator))],
        ));
    }
    if !report.pending.is_empty() {
        let mut ids = Vec::new();
        for pair in &report.pending {
            push_unique(&mut ids, [pair.owner, pair.other]);
        }
        lines.push(translator.text_with(
            "pluginResolverConflictNoticePending",
            &[("plugins", &join_names(plugins, &ids, translator))],
        ));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// 悬停显示参与重叠的匹配规则（插件名：模式），行内只放图标，不让技术细节占版面。
fn patterns_hint(
    id: impl Into<ElementId>,
    plugins: &[PluginDto],
    patterns: &[(&str, &str)],
    frame: Frame<'_>,
) -> impl IntoElement {
    let mut text = frame
        .translator
        .text("pluginResolverConflictPatterns")
        .to_owned();
    for (plugin, pattern) in patterns {
        text.push('\n');
        text.push_str(plugin_display_name(plugins, plugin));
        text.push_str(": ");
        text.push_str(pattern);
    }
    let text = SharedString::from(text);
    div()
        .id(id)
        .flex_none()
        .child(
            Icon::new(FluxIcon::Info)
                .size(frame.extended.icon.md)
                .text_color(frame.tokens.colors.muted_foreground),
        )
        .tooltip(move |window, cx| tooltip_view(text.clone(), window, cx))
}

impl ExtensionsView {
    /// 插件列表上方的冲突卡；没有需要处理的事项时不渲染。
    pub(crate) fn render_resolver_conflicts(
        &self,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let plugins = self.controller.plugins();
        let report = conflict_report(plugins, None);
        let defaults_state = &self.plugins.site_defaults;
        // 首次读取站点默认在途：先不渲染，避免已处理的站点闪现。
        let defaults_ready = defaults_state.loaded || !defaults_state.loading;
        let open_sites: Vec<(&SiteConflict<'_>, SiteResolution<'_>)> = if defaults_ready {
            report
                .sites
                .iter()
                .map(|site| (site, site_resolution(site, &defaults_state.entries)))
                .filter(|(_, resolution)| *resolution != SiteResolution::Confirmed)
                .collect()
        } else {
            Vec::new()
        };
        let duplicates = if self.plugins.dev_mode {
            report.duplicates.as_slice()
        } else {
            &[]
        };
        if open_sites.is_empty() && report.unscoped.is_empty() && duplicates.is_empty() {
            return None;
        }
        let translator = frame.translator;
        let tokens = frame.tokens;

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if !open_sites.is_empty() || !report.unscoped.is_empty() {
            rows.push(
                h_flex()
                    .w_full()
                    .gap(tokens.spacing.sm)
                    .items_start()
                    .px(tokens.spacing.md)
                    .py(tokens.spacing.sm)
                    .child(
                        div().flex_none().pt(px(2.)).child(
                            Icon::new(FluxIcon::TriangleAlert)
                                .size(frame.extended.icon.md)
                                .text_color(frame.extended.colors.warning),
                        ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(tokens.spacing.xxs)
                            .child(ui::title_text(
                                translator.text("pluginResolverConflictsHeading").to_owned(),
                                frame,
                            ))
                            .child(ui::meta_text(
                                translator.text("pluginResolverConflictsDesc").to_owned(),
                                frame,
                            )),
                    )
                    .into_any_element(),
            );
        }
        for (index, (site, resolution)) in open_sites.iter().enumerate() {
            rows.push(self.site_conflict_row(index, site, resolution, frame, cx));
        }
        for (index, pair) in report.unscoped.iter().enumerate() {
            let first = plugin_display_name(plugins, pair.owner);
            let second = plugin_display_name(plugins, pair.other);
            rows.push(
                list_row(frame)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(tokens.spacing.xxs)
                            .child(ui::title_text(
                                translator.text_with(
                                    "pluginResolverConflictPair",
                                    &[("first", first), ("second", second)],
                                ),
                                frame,
                            ))
                            .child(ui::meta_text(
                                translator.text("pluginResolverConflictAnyHint").to_owned(),
                                frame,
                            )),
                    )
                    .child(patterns_hint(
                        ("resolver-conflict-any-patterns", index),
                        plugins,
                        &[(pair.owner, pair.pattern), (pair.other, pair.other_pattern)],
                        frame,
                    ))
                    .into_any_element(),
            );
        }
        if !duplicates.is_empty() {
            let mut block = v_flex()
                .w_full()
                .gap(tokens.spacing.xxs)
                .px(tokens.spacing.md)
                .py(tokens.spacing.sm)
                .child(ui::title_text(
                    translator
                        .text("pluginResolverConflictDuplicates")
                        .to_owned(),
                    frame,
                ));
            for pair in duplicates {
                block = block.child(ui::meta_text(
                    format!(
                        "{}: {} = {}",
                        plugin_display_name(plugins, pair.owner),
                        pair.pattern,
                        pair.other_pattern
                    ),
                    frame,
                ));
            }
            rows.push(block.into_any_element());
        }
        Some(super::plugins::list_card(rows, frame, cx))
    }

    fn site_conflict_row(
        &self,
        index: usize,
        conflict: &SiteConflict<'_>,
        resolution: &SiteResolution<'_>,
        frame: Frame<'_>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let translator = frame.translator;
        let tokens = frame.tokens;
        let plugins = self.controller.plugins();
        let view = cx.entity();
        let site = conflict.site.to_owned();
        let site_busy = self.plugins.site_defaults.busy.contains(&site);
        let busy = frame.stale || site_busy;
        let candidates: Vec<String> = conflict.plugins.iter().map(|id| (*id).to_owned()).collect();
        let automatic = match resolution {
            SiteResolution::Automatic(identity) => Some((*identity).to_owned()),
            SiteResolution::Confirmed | SiteResolution::Unset => None,
        };
        let names = join_names(plugins, &conflict.plugins, translator);
        let label = match &automatic {
            Some(identity) => plugin_display_name(plugins, identity).to_owned(),
            None => translator.text("pluginResolverConflictPick").to_owned(),
        };
        let options: Vec<(String, SharedString)> = conflict
            .plugins
            .iter()
            .map(|id| {
                (
                    (*id).to_owned(),
                    SharedString::from(plugin_display_name(plugins, id).to_owned()),
                )
            })
            .collect();
        let picker = ui::select_button(
            Button::new(("resolver-conflict-pick", index))
                .label(label)
                .busy(site_busy)
                .disabled(busy),
            options,
            automatic.clone(),
            {
                let site = site.clone();
                let candidates = candidates.clone();
                move |target, window, cx| {
                    let site = site.clone();
                    let candidates = candidates.clone();
                    view.update(cx, |this, cx| {
                        this.set_site_default(site, Some(target), candidates, window, cx);
                    });
                }
            },
            cx,
        );
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
                            .child(ui::title_text(site.clone(), frame))
                            .when(automatic.is_some(), |this| {
                                this.child(ui::tone_pill(
                                    translator.text("pluginSiteDefaultAutomatic").to_owned(),
                                    frame.extended.colors.warning,
                                    frame,
                                ))
                            }),
                    )
                    .child(ui::meta_text(
                        translator
                            .text_with("pluginResolverConflictHandledBy", &[("plugins", &names)]),
                        frame,
                    )),
            )
            .child(patterns_hint(
                ("resolver-conflict-patterns", index),
                plugins,
                &conflict.patterns,
                frame,
            ))
            .child(picker)
            .when_some(automatic, |this, identity| {
                let site = site.clone();
                this.child(
                    Button::new(("resolver-conflict-confirm", index))
                        .primary()
                        .control(cx)
                        .label(translator.text("pluginSiteDefaultConfirm").to_owned())
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_site_default(
                                site.clone(),
                                Some(identity.clone()),
                                candidates.clone(),
                                window,
                                cx,
                            );
                        })),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{PluginDto, PluginSiteDefaultDto};
    use fluxdown_ui_i18n::I18nCatalog;
    use serde_json::json;

    use super::{SiteResolution, conflict_notice, conflict_report, site_resolution};

    fn plugins() -> Vec<PluginDto> {
        serde_json::from_value(json!([
            {
                "identity": "yt@test", "name": "YouTube", "version": "1", "enabled": true,
                "disabledReason": "None", "devMode": false, "settings": [], "settingsValues": {},
                "resolverUrlConflicts": [
                    {"pattern": "*://www.youtube.com/watch*", "otherIdentity": "any@test",
                     "otherPattern": "*://*/*", "site": "youtube.com"},
                    {"pattern": "*://www.youtube.com/watch*", "otherIdentity": "yt@test",
                     "otherPattern": "*://WWW.youtube.com/watch*"},
                    {"pattern": "*://www.youtube.com/watch*", "otherIdentity": "new@test",
                     "otherPattern": "*://*.youtube.com/*", "site": "youtube.com"}
                ]
            },
            {
                "identity": "any@test", "name": "Any", "version": "1", "enabled": true,
                "disabledReason": "None", "devMode": false, "settings": [], "settingsValues": {},
                "resolverUrlConflicts": [
                    {"pattern": "*://*/*", "otherIdentity": "all@test", "otherPattern": "*"},
                    {"pattern": "*://*/*", "otherIdentity": "bili@test",
                     "otherPattern": "*://www.bilibili.com/*", "site": "bilibili.com"}
                ]
            },
            {
                "identity": "all@test", "name": "All", "version": "1", "enabled": true,
                "disabledReason": "None", "devMode": false, "settings": [], "settingsValues": {}
            },
            {
                "identity": "bili@test", "name": "Bili", "version": "1", "enabled": true,
                "disabledReason": "None", "devMode": false, "settings": [], "settingsValues": {}
            },
            {
                "identity": "new@test", "name": "New", "version": "1", "enabled": false,
                "disabledReason": "ConsentRequired", "devMode": false, "settings": [],
                "settingsValues": {}
            }
        ]))
        .unwrap()
    }

    fn record(
        site: &str,
        identity: &str,
        candidates: &[&str],
        automatic: bool,
    ) -> PluginSiteDefaultDto {
        PluginSiteDefaultDto {
            site: site.into(),
            identity: identity.into(),
            candidates: candidates.iter().map(|c| (*c).to_owned()).collect(),
            automatic,
        }
    }

    #[test]
    fn groups_active_pairs_by_site_and_separates_the_rest() {
        let plugins = plugins();
        let report = conflict_report(&plugins, None);
        let sites: Vec<_> = report
            .sites
            .iter()
            .map(|s| (s.site, s.plugins.clone()))
            .collect();
        assert_eq!(
            sites,
            [
                ("bilibili.com", vec!["any@test", "bili@test"]),
                ("youtube.com", vec!["yt@test", "any@test"]),
            ]
        );
        assert_eq!(report.unscoped.len(), 1);
        assert_eq!(report.unscoped[0].other, "all@test");
        assert_eq!(report.pending.len(), 1);
        assert_eq!(report.pending[0].other, "new@test");
        assert_eq!(report.duplicates.len(), 1);

        let only_bili = conflict_report(&plugins, Some("bili@test"));
        assert_eq!(only_bili.sites.len(), 1);
        assert!(only_bili.unscoped.is_empty() && only_bili.pending.is_empty());
    }

    #[test]
    fn site_is_resolved_only_by_a_confirmed_record_covering_every_plugin() {
        let plugins = plugins();
        let report = conflict_report(&plugins, None);
        let youtube = &report.sites[1];
        assert_eq!(site_resolution(youtube, &[]), SiteResolution::Unset);
        let confirmed = [record(
            "youtube.com",
            "yt@test",
            &["any@test", "yt@test"],
            false,
        )];
        assert_eq!(
            site_resolution(youtube, &confirmed),
            SiteResolution::Confirmed
        );
        let partial = [record(
            "youtube.com",
            "yt@test",
            &["yt@test", "x@test"],
            false,
        )];
        assert_eq!(site_resolution(youtube, &partial), SiteResolution::Unset);
        let automatic = [record(
            "youtube.com",
            "any@test",
            &["any@test", "yt@test"],
            true,
        )];
        assert_eq!(
            site_resolution(youtube, &automatic),
            SiteResolution::Automatic("any@test")
        );
        let other = [record(
            "youtube.com",
            "gone@test",
            &["gone@test", "yt@test"],
            false,
        )];
        assert_eq!(site_resolution(youtube, &other), SiteResolution::Unset);
    }

    #[test]
    fn notice_names_plugins_and_sites_and_skips_confirmed_sites() {
        let plugins = plugins();
        let translator = I18nCatalog::load_embedded().unwrap().translator("en");
        let notice = conflict_notice(&plugins, &[], "any@test", &translator).unwrap();
        let sites = translator.text_with(
            "pluginResolverConflictNoticeSites",
            &[
                ("plugins", "Any, Bili, YouTube"),
                ("sites", "bilibili.com, youtube.com"),
            ],
        );
        let any = translator.text_with(
            "pluginResolverConflictNoticeAny",
            &[("plugins", "Any, All")],
        );
        assert_eq!(notice, format!("{sites}\n{any}"));

        let confirmed = [record(
            "bilibili.com",
            "bili@test",
            &["any@test", "bili@test"],
            false,
        )];
        assert!(conflict_notice(&plugins, &confirmed, "bili@test", &translator).is_none());

        let pending = conflict_notice(&plugins, &[], "new@test", &translator).unwrap();
        assert_eq!(
            pending,
            translator.text_with(
                "pluginResolverConflictNoticePending",
                &[("plugins", "YouTube, New")]
            )
        );
    }
}
