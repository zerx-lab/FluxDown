//! 主窗口活动栏注册表：活动栏条目的唯一登记处。
//!
//! 主窗口按 [`ActivityEntry::ALL`] 注册按钮，偏好回流按它切换可见性，设置页「通用 → 活动栏」
//! 按它生成开关——新增可选入口只需在这里加变体并给出 [`ActivityEntry::toggle`]，
//! 开关自动出现在设置页，无需另行维护。

use std::collections::BTreeMap;

use fluxdown_ui_settings::ActivityBarToggle;
use fluxdown_ui_shell::{RouteId, ShellView};
use fluxdown_ui_theme::{THEME_MODE_KEY, ThemePreference, active_theme};
use gpui::{App, Context, WeakEntity, Window};

use crate::app::Desktop;
use crate::windows::{WindowKey, WindowRegistry};

/// 主窗口路由标识：主窗口装配与命令面板导航共用。
const DOWNLOADS_ID: &str = "downloads";
const RSS_ID: &str = "rss";
const WEBHOOKS_ID: &str = "webhooks";
pub(crate) const DOWNLOADS_ROUTE: RouteId = RouteId::new(DOWNLOADS_ID);
pub(crate) const RSS_ROUTE: RouteId = RouteId::new(RSS_ID);
pub(crate) const WEBHOOKS_ROUTE: RouteId = RouteId::new(WEBHOOKS_ID);

/// chrome 条目，按自上而下顺序（路由在上，动作在下）。AI 助手不是路由：它是窗口右侧面板的
/// 开关，默认摆在状态栏（见 `chrome_layout::catalog`）。
///
/// [`Self::ALL`] 是唯一构造点：新增变体未登记进 `ALL` 会触发 `dead_code`（`clippy -D warnings` 拦截），
/// 而 [`Self::button_id`] / [`Self::toggle`] 与主窗口装配的穷尽 `match` 迫使补齐元数据与内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivityEntry {
    Downloads,
    Rss,
    Webhooks,
    Assistant,
    Account,
    Settings,
}

impl ActivityEntry {
    pub(crate) const ALL: [Self; 6] = [
        Self::Downloads,
        Self::Rss,
        Self::Webhooks,
        Self::Assistant,
        Self::Account,
        Self::Settings,
    ];

    /// 活动栏按钮 id：shell 以它定位路由 / 动作。
    pub(crate) const fn button_id(self) -> &'static str {
        match self {
            Self::Downloads => "activity-downloads",
            Self::Rss => "activity-rss",
            Self::Webhooks => "activity-webhooks",
            Self::Assistant => "activity-assistant",
            Self::Account => "activity-account",
            Self::Settings => "activity-settings",
        }
    }

    /// chrome 布局里的条目 id：路由沿用路由标识，动作取稳定短名（与 Web 同名）。
    /// AI 助手条目由活动栏路由 `assistant` 改为面板开关时换了 id，旧布局里的位置随之作废。
    pub(crate) const fn layout_id(self) -> &'static str {
        match self {
            Self::Downloads => DOWNLOADS_ID,
            Self::Rss => RSS_ID,
            Self::Webhooks => WEBHOOKS_ID,
            Self::Assistant => "assistantPanel",
            Self::Account => "account",
            Self::Settings => "settings",
        }
    }

    /// 活动栏按钮与命令面板共用的标题文案键。
    pub(crate) const fn label_key(self) -> &'static str {
        match self {
            Self::Downloads => fluxdown_ui_i18n::keys::MOBILE_NAV_DOWNLOADS,
            Self::Rss => "sidebarRss",
            Self::Webhooks => "pushNavTitle",
            Self::Assistant => "assistantNavTitle",
            Self::Account => "settingsCatAccount",
            Self::Settings => fluxdown_ui_i18n::keys::SETTINGS,
        }
    }

    /// 路由条目的 shell 路由标识；动作条目为 `None`。
    pub(crate) const fn route(self) -> Option<RouteId> {
        match self {
            Self::Downloads => Some(DOWNLOADS_ROUTE),
            Self::Rss => Some(RSS_ROUTE),
            Self::Webhooks => Some(WEBHOOKS_ROUTE),
            Self::Assistant | Self::Account | Self::Settings => None,
        }
    }

    /// 可选入口的可见性开关（偏好缺省视为显示）；`None` = 固定显示，不参与活动栏整体收起判定。
    pub(crate) const fn toggle(self) -> Option<ActivityBarToggle> {
        match self {
            Self::Downloads | Self::Settings => None,
            Self::Rss => Some(ActivityBarToggle {
                pref_key: "ui.show_activity_rss",
                title_key: "showActivityRss",
                desc_key: "showActivityRssDesc",
            }),
            Self::Webhooks => Some(ActivityBarToggle {
                pref_key: "ui.show_activity_webhooks",
                title_key: "showActivityWebhooks",
                desc_key: "showActivityWebhooksDesc",
            }),
            Self::Assistant => Some(ActivityBarToggle {
                pref_key: "ui.show_activity_assistant",
                title_key: "showActivityAssistant",
                desc_key: "showActivityAssistantDesc",
            }),
            Self::Account => Some(ActivityBarToggle {
                pref_key: "ui.show_activity_account",
                title_key: "showActivityAccount",
                desc_key: "showActivityAccountDesc",
            }),
        }
    }
}

/// 设置页「活动栏」分区的开关行，与活动栏同序。
pub(crate) fn toggles() -> Vec<ActivityBarToggle> {
    ActivityEntry::ALL
        .into_iter()
        .filter_map(ActivityEntry::toggle)
        .collect()
}

/// 偏好 → 活动栏可选入口可见性；幂等，窗口创建与每次偏好快照/事件都走这里。
pub(crate) fn apply_visibility(
    shell: &mut ShellView,
    preferences: &BTreeMap<String, serde_json::Value>,
    cx: &mut Context<ShellView>,
) {
    for entry in ActivityEntry::ALL {
        let Some(toggle) = entry.toggle() else {
            continue;
        };
        let visible = preferences
            .get(toggle.pref_key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        shell.set_entry_visible(entry.button_id(), visible, cx);
    }
}

/// 活动栏账户按钮 / 命令面板：打开设置窗口并定位到账户页（登录、资料与套餐都在这里）。
pub(crate) fn open_account(cx: &mut App) {
    crate::windows::settings::reveal(
        cx,
        fluxdown_ui_settings::SettingsTarget {
            page: "account",
            tab: "",
            row: None,
        },
    );
}

/// 状态栏 AI 助手按钮之外的入口（命令面板）：主窗口到前台并开关右侧 AI 助手面板。
pub(crate) fn toggle_assistant(cx: &mut App) {
    cx.defer(|cx| {
        crate::windows::main::reveal(cx);
        if let Some(shell) = Desktop::global(cx)
            .main_shell
            .as_ref()
            .and_then(WeakEntity::upgrade)
        {
            shell.update(cx, |shell, cx| shell.toggle_side_panel(cx));
        }
    });
}

/// 命令面板「问 AI 助手」：主窗口到前台、打开右侧 AI 助手面板，开新对话发出这段问题。
pub(crate) fn ask_assistant(cx: &mut App, query: String) {
    cx.defer(move |cx| {
        crate::windows::main::reveal(cx);
        let desktop = Desktop::global(cx);
        let shell = desktop.main_shell.as_ref().and_then(WeakEntity::upgrade);
        let assistant = desktop
            .main_assistant
            .as_ref()
            .and_then(WeakEntity::upgrade);
        let (Some(shell), Some(assistant)) = (shell, assistant) else {
            return;
        };
        let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
            return;
        };
        shell.update(cx, |shell, cx| shell.set_side_panel_open(true, cx));
        if let Err(error) = handle.update(cx, |_, window, cx| {
            assistant.update(cx, |assistant, cx| assistant.ask(query, window, cx));
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
}

/// 命令面板的明暗切换：只写 [`THEME_MODE_KEY`]，换肤由偏好投影统一完成——
/// 直接改内存主题会在下一次偏好快照 / 事件回流时被旧偏好覆盖。
pub(crate) fn toggle_theme(_window: &mut Window, cx: &mut App) {
    let target = if active_theme(cx).mode().is_dark() {
        ThemePreference::Light
    } else {
        ThemePreference::Dark
    };
    Desktop::set_pref(cx, THEME_MODE_KEY, target.wire_name().into());
}
