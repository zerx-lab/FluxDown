//! 通用：启动与托盘、系统集成、侧边栏与活动栏可见性。

use fluxdown_protocol::capture_link::OpenAssociation;
use fluxdown_protocol::{PlatformIntegrationDto, ShellStatusDto, TrayUnavailableReason};
use fluxdown_ui_components::FluxIcon;
use gpui::App;

use super::SectionContext;
use crate::ActivityBarToggle;
use crate::ui::{Control, SettingsPage, SettingsSection};

pub(crate) fn page(
    ctx: &SectionContext,
    activity_bar: &[ActivityBarToggle],
    cx: &mut App,
) -> SettingsPage {
    // 首次进入拉取系统集成状态（自启 / 文件关联 / URL scheme）。断线只读时不发；失败后
    // 本轮不再重发，重连 / 新快照后重新放行。
    let store = ctx.store.read(cx);
    if store.integration().is_none()
        && !store.is_read_only()
        && !store.in_flight("integration")
        && !store.load_attempted("integration")
        && ctx
            .store
            .update(cx, |store, _| store.begin_load("integration"))
    {
        ctx.store.update(cx, |store, cx| store.load_integration(cx));
    }

    let page = SettingsPage::new(
        "general",
        ctx.t("settingsCatGeneral"),
        ctx.t("settingsCatGeneralDesc"),
        FluxIcon::Settings,
    );
    let mut sections = vec![startup_section(ctx, cx), system_section(ctx, cx)];
    // macOS 应用菜单恒在系统菜单栏，开关只对 Windows / Linux 标题栏菜单栏有意义。
    if !cfg!(target_os = "macos") {
        sections.push(title_bar_section(ctx));
    }
    sections.extend([
        sidebar_section(ctx),
        task_list_section(ctx),
        activity_bar_section(ctx, activity_bar),
    ]);
    page.sections(sections)
}

/// 设备本地偏好 `ui.hide_task_source`（缺省关，不进同步目录；GPUI 与 Web 共用）：下载列表 /
/// 详情副标题不展示来源站点。与下载能力的 `HIDE_TASK_SOURCE_PREF` 同名（设置能力不依赖下载能力）。
const HIDE_TASK_SOURCE_PREF: &str = "ui.hide_task_source";

/// 标题栏应用菜单栏显隐：设备本地偏好 `ui.show_menu_bar`（不进同步目录，缺省隐藏），
/// 由 app 投影到 shell。
fn title_bar_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("settingsGroupInterface"))
        .row(ctx.item(
            "showMenuBar",
            Some("showMenuBarDesc"),
            ctx.pref_switch("ui.show_menu_bar", false),
        ))
}

fn startup_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    // 托盘由 agent 承载：可用性在运行期探测（Linux 取决于 StatusNotifier 宿主与 appindicator）。
    let shell = ctx.store.read(cx).shell().clone();
    SettingsSection::new()
        .title(ctx.t("settingsGroupStartupTray"))
        .row(
            ctx.item(
                "autoStartup",
                Some("autoStartupDesc"),
                integration_switch(ctx, IntegrationKind::Autostart),
            )
            .disabled(!integration_supported(ctx, IntegrationKind::Autostart, cx)),
        )
        .row(
            ctx.item(
                "closeToTray",
                Some(tray_desc(&shell, "closeToTrayDesc")),
                ctx.pref_switch("close_to_tray", true),
            )
            .disabled(!shell.tray_available),
        )
        .row(
            ctx.item(
                "startMinimizedToTray",
                Some(tray_desc(&shell, "startMinimizedToTrayDesc")),
                ctx.pref_switch("start_minimized_to_tray", false),
            )
            .disabled(!shell.tray_available),
        )
}

/// 托盘不可用时用原因替换说明文案，告诉用户关闭窗口后会发生什么。
fn tray_desc(shell: &ShellStatusDto, key: &'static str) -> &'static str {
    match shell.tray_unavailable_reason {
        None => key,
        Some(TrayUnavailableReason::NotBuilt) => "trayUnavailableNotBuilt",
        Some(TrayUnavailableReason::NoDisplay) => "trayUnavailableNoDisplay",
        Some(TrayUnavailableReason::NoHost) => "trayUnavailableNoHost",
        Some(TrayUnavailableReason::InitFailed) => "trayUnavailableInitFailed",
    }
}

fn system_section(ctx: &SectionContext, cx: &mut App) -> SettingsSection {
    let watching = ctx
        .store
        .read(cx)
        .pref_bool("general.clipboard_watch", false);
    SettingsSection::new()
        .title(ctx.t("settingsGroupSystem"))
        .row(ctx.item(
            "clipboardWatch",
            Some("clipboardWatchDesc"),
            ctx.pref_switch("general.clipboard_watch", false),
        ))
        .row(
            ctx.item(
                "clipboardWatchProbe",
                Some("clipboardWatchProbeDesc"),
                ctx.pref_switch("general.clipboard_watch_probe", true),
            )
            .disabled(!watching),
        )
        .row(
            ctx.item(
                "clipboardWatchExtensions",
                Some("clipboardWatchExtensionsDesc"),
                ctx.pref_input("general.clipboard_watch_extensions", ""),
            )
            .disabled(!watching),
        )
        .row(
            ctx.item(
                "torrentFileAssociation",
                Some("torrentFileAssociationDesc"),
                integration_switch(ctx, IntegrationKind::Torrent),
            )
            .disabled(!integration_supported(ctx, IntegrationKind::Torrent, cx)),
        )
        .row(
            ctx.item(
                "magnetLinkAssociation",
                Some("magnetLinkAssociationDesc"),
                integration_switch(ctx, IntegrationKind::Scheme("magnet")),
            )
            .disabled(!integration_supported(
                ctx,
                IntegrationKind::Scheme("magnet"),
                cx,
            )),
        )
        .row(
            ctx.item(
                "ed2kLinkAssociation",
                Some("ed2kLinkAssociationDesc"),
                integration_switch(ctx, IntegrationKind::Scheme("ed2k")),
            )
            .disabled(!integration_supported(
                ctx,
                IntegrationKind::Scheme("ed2k"),
                cx,
            )),
        )
        .row(ctx.item(
            "keepAwakeWhileDownloading",
            Some("keepAwakeWhileDownloadingDesc"),
            ctx.pref_switch("download.keep_awake", false),
        ))
        .row(ctx.item(
            "analyticsEnabled",
            Some("analyticsEnabledDesc"),
            ctx.pref_switch("analytics_enabled", true),
        ))
}

fn sidebar_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("sidebarVisibility"))
        .subtitle(ctx.t("sidebarVisibilityDesc"))
        .row(ctx.item(
            "showSidebarStatus",
            Some("showSidebarStatusDesc"),
            ctx.pref_switch("ui.show_sidebar_status", true),
        ))
        .row(ctx.item(
            "showSidebarQueues",
            Some("showSidebarQueuesDesc"),
            ctx.pref_switch("ui.show_sidebar_queues", true),
        ))
        .row(ctx.item(
            "showSidebarCategory",
            Some("showSidebarCategoryNestedDesc"),
            ctx.pref_switch("ui.show_sidebar_category", true),
        ))
        .row(ctx.item(
            "showSidebarDevice",
            Some("showSidebarDeviceDesc"),
            show_sidebar_device_field(ctx),
        ))
}

fn task_list_section(ctx: &SectionContext) -> SettingsSection {
    SettingsSection::new()
        .title(ctx.t("taskListDisplay"))
        .row(ctx.item(
            "hideTaskSource",
            Some("hideTaskSourceDesc"),
            ctx.pref_switch(HIDE_TASK_SOURCE_PREF, false),
        ))
}

/// `ui.show_sidebar_devices` 三态：未设置 = 有其他设备（云账号其他设备 / 已配对设备）时
/// 自动显示，与侧栏的判定一致。开关显示有效值。
///
/// 该键不在云同步目录内：写入走设备本地路径（`sync:false`），隐藏设备区只影响本机。
fn show_sidebar_device_field(ctx: &SectionContext) -> Control {
    let get = ctx.store();
    let set = ctx.store();
    Control::switch(
        move |cx: &App| {
            let store = get.read(cx);
            store
                .pref("ui.show_sidebar_devices")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_else(|| store.has_other_devices())
        },
        move |value, cx: &mut App| {
            set.update(cx, |store, cx| {
                store.set_pref_bool("ui.show_sidebar_devices", value, cx)
            });
        },
    )
}

/// 活动栏开关由 app 的活动栏注册表派生：注册了可选入口即自动出现在这里。
fn activity_bar_section(ctx: &SectionContext, toggles: &[ActivityBarToggle]) -> SettingsSection {
    toggles.iter().fold(
        SettingsSection::new()
            .title(ctx.t("activityBarSection"))
            .subtitle(ctx.t("activityBarSectionDesc")),
        |section, toggle| {
            section.row(ctx.item(
                toggle.title_key,
                Some(toggle.desc_key),
                ctx.pref_switch(toggle.pref_key, true),
            ))
        },
    )
}

#[derive(Clone, Copy)]
enum IntegrationKind {
    Autostart,
    Torrent,
    Scheme(&'static str),
}

fn integration_supported(ctx: &SectionContext, kind: IntegrationKind, cx: &App) -> bool {
    let store = ctx.store.read(cx);
    if store.is_read_only() {
        return false;
    }
    store.integration().is_some_and(|dto| match kind {
        IntegrationKind::Autostart => dto.autostart_supported,
        IntegrationKind::Torrent => dto.file_association_supported,
        IntegrationKind::Scheme(_) => dto.url_protocol_supported,
    })
}

/// 用户手动关闭关联时持久化的 opt-out 键（与 agent 捕获拦截同一键）。
///
/// macOS Launch Services 没有「无默认处理程序」：FluxDown 是唯一候选时，关闭后系统仍
/// 回落到 FluxDown，探测值恒为 true。opt-out 让用户的「关闭」压过探测值，否则开关会被
/// 立即顶回开启；agent 也据此拦截系统交来的链接 / 文件。
fn opt_out_key(kind: IntegrationKind) -> Option<&'static str> {
    let association = match kind {
        IntegrationKind::Autostart => return None,
        IntegrationKind::Torrent => OpenAssociation::Torrent,
        IntegrationKind::Scheme("magnet") => OpenAssociation::Magnet,
        IntegrationKind::Scheme(_) => OpenAssociation::Ed2k,
    };
    Some(association.opt_out_pref_key())
}

/// 开关显示值：系统探测为已关联，且用户未手动关闭。
fn integration_enabled(
    dto: &PlatformIntegrationDto,
    kind: IntegrationKind,
    user_disabled: bool,
) -> bool {
    let probed = match kind {
        IntegrationKind::Autostart => dto.autostart_enabled,
        IntegrationKind::Torrent => dto.torrent_associated,
        IntegrationKind::Scheme(scheme) => dto.url_protocols.get(scheme).copied().unwrap_or(false),
    };
    probed && !user_disabled
}

/// 系统集成开关：值来自 agent 探测结果（叠加用户 opt-out），切换即调用 agent 注册/注销。
fn integration_switch(ctx: &SectionContext, kind: IntegrationKind) -> Control {
    let get = ctx.store();
    let set = ctx.store();
    Control::switch(
        move |cx: &App| {
            let store = get.read(cx);
            let user_disabled = opt_out_key(kind).is_some_and(|key| store.pref_bool(key, false));
            store
                .integration()
                .is_some_and(|dto| integration_enabled(dto, kind, user_disabled))
        },
        move |value, cx: &mut App| {
            set.update(cx, |store, cx| {
                if let Some(key) = opt_out_key(kind) {
                    store.set_pref_bool(key, !value, cx);
                }
                match kind {
                    IntegrationKind::Autostart => store.set_autostart(value, cx),
                    IntegrationKind::Torrent => store.set_file_association(value, cx),
                    IntegrationKind::Scheme(scheme) => store.set_url_protocol(scheme, value, cx),
                }
            });
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn associated_everywhere() -> PlatformIntegrationDto {
        PlatformIntegrationDto {
            torrent_associated: true,
            url_protocols: BTreeMap::from([("magnet".to_owned(), true), ("ed2k".to_owned(), true)]),
            ..PlatformIntegrationDto::default()
        }
    }

    #[test]
    fn user_opt_out_wins_over_sticky_system_probe() {
        // macOS：唯一候选时清空默认处理程序无效，探测仍报告已关联。
        let dto = associated_everywhere();
        for kind in [
            IntegrationKind::Torrent,
            IntegrationKind::Scheme("magnet"),
            IntegrationKind::Scheme("ed2k"),
        ] {
            assert!(!integration_enabled(&dto, kind, true));
            assert!(integration_enabled(&dto, kind, false));
        }
    }

    #[test]
    fn opt_out_cannot_turn_unassociated_on() {
        let dto = PlatformIntegrationDto::default();
        assert!(!integration_enabled(&dto, IntegrationKind::Torrent, false));
        assert!(!integration_enabled(
            &dto,
            IntegrationKind::Scheme("magnet"),
            false
        ));
    }
}
