//! 通知设置：系统通知、提示音与下载进度 / 完成窗口；Webhook 使用活动栏独立页面。

use fluxdown_ui_components::FluxIcon;
use gpui::App;

use super::SectionContext;
use crate::ui::{SettingsPage, SettingsSection};

/// 设备本地偏好键，与下载能力的 `PROGRESS_WINDOW_PREF` / `COMPLETION_WINDOW_PREF` 一致
/// （设置能力不依赖下载能力，键名在此重复声明）。
const PROGRESS_WINDOW_PREF: &str = "desktop.progress_window";
const COMPLETION_WINDOW_PREF: &str = "desktop.completion_window";
/// 设备本地偏好：下载失败时发系统通知（缺省开启；agent 侧
/// `failure_notice::NOTIFY_ON_FAIL_PREF` 同名）。不进云同步目录。
const NOTIFY_ON_FAIL_PREF: &str = "desktop.notify_on_fail";

pub(crate) fn page(ctx: &SectionContext, _cx: &mut App) -> SettingsPage {
    SettingsPage::new(
        "notify",
        ctx.t("settingsCatNotify"),
        ctx.t("notifyGroupSystem"),
        FluxIcon::Bell,
    )
    .sections([
        SettingsSection::new()
            .title(ctx.t("notifyGroupSystem"))
            .row(ctx.item(
                "notifyOnComplete",
                Some("notifyOnCompleteDesc"),
                ctx.pref_switch("download.notify_on_complete", true),
            ))
            .row(ctx.item(
                "notifyOnFailure",
                Some("notifyOnFailureDesc"),
                ctx.pref_switch(NOTIFY_ON_FAIL_PREF, true),
            )),
        super::sound::section(ctx),
        SettingsSection::new()
            .title(ctx.t("progressWindowGroup"))
            .row(ctx.item(
                "showProgressWindow",
                Some("showProgressWindowDesc"),
                ctx.pref_switch(PROGRESS_WINDOW_PREF, true),
            ))
            .row(ctx.item(
                "showCompletionWindow",
                Some("showCompletionWindowDesc"),
                ctx.pref_switch(COMPLETION_WINDOW_PREF, true),
            )),
    ])
}
