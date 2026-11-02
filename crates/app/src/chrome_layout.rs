//! 主窗口 chrome 布局的装配：布局目录（活动栏条目 + 下载控件 + 下载页顶栏控件）、
//! 控件注册与偏好回流。
//!
//! 布局存设备本地偏好 [`CHROME_LAYOUT_PREF`]（不在云同步目录，不随账号同步），含各条目的
//! 摆放与隐藏；Web 各自存在浏览器本地，两端互不影响。条目 id 与 Web
//! `web/src/shell/chromeCatalog.ts` 一致（下载页顶栏控件 `search` / `viewMenu` 为桌面专属）。

use std::collections::BTreeMap;

use fluxdown_ui_downloads::{
    DownloadPageWidget, DownloadSearchBox, DownloadView, DownloadViewMenu, DownloadWidget,
    WidgetPlacement,
};
use fluxdown_ui_shell::{
    ChromeFamilies, ChromeFamily, ChromeItemSpec, ChromeZone, ShellView, ShellWidget,
};
use gpui::{Anchor, Context, Entity, IntoElement as _, WeakEntity};

use crate::activity::{ActivityEntry, DOWNLOADS_ROUTE};

/// 主窗口 chrome 布局偏好键（设备本地）。
pub(crate) const CHROME_LAYOUT_PREF: &str = "ui.chrome_layout";

/// 布局目录：活动栏条目在前（路由在上、动作在下），随后是下载控件（状态栏自左向右），
/// 最后是下载页顶栏控件。下载页与设置是固定入口：可移动、不可隐藏。
pub(crate) fn catalog() -> Vec<ChromeItemSpec> {
    let activity = ActivityEntry::ALL.into_iter().map(|entry| {
        let (zone, families) = match entry {
            ActivityEntry::Downloads | ActivityEntry::Rss | ActivityEntry::Webhooks => {
                (ChromeZone::RailTop, ChromeFamilies::ALL)
            }
            // 账户头像是固定 32px 的自绘视图，高于状态栏。
            ActivityEntry::Account => (ChromeZone::RailBottom, ChromeFamilies::RAIL_TITLE),
            ActivityEntry::Settings => (ChromeZone::RailBottom, ChromeFamilies::ALL),
        };
        let spec = ChromeItemSpec::new(entry.layout_id(), zone, families);
        match entry {
            ActivityEntry::Downloads | ActivityEntry::Settings => spec.fixed(),
            ActivityEntry::Rss | ActivityEntry::Webhooks | ActivityEntry::Account => spec,
        }
    });
    let widgets = DownloadWidget::ALL.into_iter().map(|widget| {
        let zone = match widget {
            DownloadWidget::DownloadSpeed
            | DownloadWidget::UploadSpeed
            | DownloadWidget::PauseAll
            | DownloadWidget::ResumeAll => ChromeZone::StatusLeading,
            DownloadWidget::DownloadLimit
            | DownloadWidget::UploadLimit
            | DownloadWidget::Proxy
            | DownloadWidget::Shutdown
            | DownloadWidget::DiskFree => ChromeZone::StatusTrailing,
        };
        let families = if widget.is_text_readout() {
            ChromeFamilies::TITLE_STATUS
        } else {
            ChromeFamilies::ALL
        };
        ChromeItemSpec::new(widget.id(), zone, families)
    });
    let page_widgets = DownloadPageWidget::ALL.into_iter().map(|widget| {
        ChromeItemSpec::new(
            widget.id(),
            ChromeZone::TitleTrailing,
            ChromeFamilies::TITLE,
        )
    });
    activity.chain(widgets).chain(page_widgets).collect()
}

/// 下载控件与下载页顶栏控件注册为 shell 控件：在 shell 的渲染里读取下载页状态绘制。
pub(crate) fn widgets(downloads: &Entity<DownloadView>, cx: &mut gpui::App) -> Vec<ShellWidget> {
    let weak = downloads.downgrade();
    let status = DownloadWidget::ALL.into_iter().map(|widget| {
        let downloads = weak.clone();
        ShellWidget::new(widget.id(), widget.label_key(), move |zone, _, cx| {
            let view = downloads.upgrade()?;
            view.read(cx)
                .render_widget(widget, placement(zone), &downloads, cx)
        })
    });
    let search = downloads.update(cx, |view, cx| view.new_search_box(cx));
    let view_menu = downloads.update(cx, |view, cx| view.new_view_menu(cx));
    status
        .chain([
            search_widget(search, weak.clone()),
            view_menu_widget(view_menu),
        ])
        .collect()
}

/// 搜索框：被布局隐藏时，快捷键唤出或仍有查询词则临时显示。
fn search_widget(
    search: Entity<DownloadSearchBox>,
    downloads: WeakEntity<DownloadView>,
) -> ShellWidget {
    let widget = DownloadPageWidget::Search;
    ShellWidget::new(widget.id(), widget.label_key(), move |_, _, _| {
        Some(search.clone().into_any_element())
    })
    .on_route(DOWNLOADS_ROUTE)
    .revealed_when(move |cx| {
        downloads
            .upgrade()
            .is_some_and(|view| view.read(cx).search_revealed(cx))
    })
}

/// 视图菜单：弹层锚点随所在顶栏区域切换。
fn view_menu_widget(view_menu: Entity<DownloadViewMenu>) -> ShellWidget {
    let widget = DownloadPageWidget::ViewMenu;
    ShellWidget::new(widget.id(), widget.label_key(), move |zone, _, cx| {
        let anchor = placement(zone).menu_anchor;
        view_menu.update(cx, |menu, cx| menu.set_anchor(anchor, cx));
        Some(view_menu.clone().into_any_element())
    })
    .on_route(DOWNLOADS_ROUTE)
}

/// 区域 → 控件呈现：活动栏紧凑；底部区域菜单向上弹，顶部区域向下弹，右侧区域右对齐。
fn placement(zone: ChromeZone) -> WidgetPlacement {
    let menu_anchor = match zone {
        ChromeZone::TitleLeading | ChromeZone::RailTop => Anchor::TopLeft,
        ChromeZone::TitleTrailing => Anchor::TopRight,
        ChromeZone::RailBottom | ChromeZone::StatusLeading => Anchor::BottomLeft,
        ChromeZone::StatusTrailing => Anchor::BottomRight,
    };
    WidgetPlacement {
        compact: zone.family() == ChromeFamily::Rail,
        menu_anchor,
    }
}

/// 偏好 → 布局；幂等，窗口创建与每次偏好快照 / 事件都走这里。
pub(crate) fn apply(
    shell: &mut ShellView,
    preferences: &BTreeMap<String, serde_json::Value>,
    cx: &mut Context<ShellView>,
) {
    shell.apply_layout(preferences.get(CHROME_LAYOUT_PREF), cx);
}

#[cfg(test)]
mod tests {
    use fluxdown_ui_shell::{ChromeLayout, ChromeZone};

    use super::catalog;

    /// 默认布局必须还原改造前的界面：活动栏路由在上、动作在下，状态栏左 = 速度 + 暂停 / 开始，
    /// 右 = 限速、代理、关机、剩余空间，顶栏右 = 下载页搜索框 + 视图菜单；默认不隐藏任何条目。
    #[test]
    fn default_layout_matches_classic_chrome() {
        let catalog = catalog();
        let layout = ChromeLayout::default_for(&catalog);
        let ids =
            |zone| -> Vec<String> { layout.items(zone).iter().map(ToString::to_string).collect() };
        assert!(ids(ChromeZone::TitleLeading).is_empty());
        assert_eq!(ids(ChromeZone::TitleTrailing), ["search", "viewMenu"]);
        assert!(catalog.iter().all(|spec| !layout.is_hidden(&spec.id)));
        assert_eq!(ids(ChromeZone::RailTop), ["downloads", "rss", "webhooks"]);
        assert_eq!(ids(ChromeZone::RailBottom), ["account", "settings"]);
        assert_eq!(
            ids(ChromeZone::StatusLeading),
            ["downloadSpeed", "uploadSpeed", "pauseAll", "resumeAll"]
        );
        assert_eq!(
            ids(ChromeZone::StatusTrailing),
            [
                "downloadLimit",
                "uploadLimit",
                "proxy",
                "shutdown",
                "diskFree"
            ]
        );
    }

    /// 固定入口（下载页、设置）不可隐藏，保证始终有路回到它们；其余条目都可隐藏。
    #[test]
    fn only_downloads_and_settings_are_fixed() {
        let fixed: Vec<String> = catalog()
            .into_iter()
            .filter(|spec| !spec.hideable)
            .map(|spec| spec.id.to_string())
            .collect();
        assert_eq!(fixed, ["downloads", "settings"]);
    }
}
