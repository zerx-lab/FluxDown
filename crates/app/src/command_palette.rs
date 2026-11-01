//! 全局命令面板（⌘K / Ctrl+K）的装配：宿主窗口选择、命令与设置项注册、执行路由、使用记录持久化。
//!
//! 面板只在主窗口或设置窗口内打开；焦点在其他窗口（进度、新建下载等小窗）或没有窗口时，
//! 先把主窗口调到前台再在其中打开。

use std::collections::HashMap;
use std::rc::Rc;

use fluxdown_ui_command_palette::{PaletteConfig, PaletteItem, PaletteItemKind, UsageStats};
use fluxdown_ui_components::FluxIcon;
use fluxdown_ui_downloads::PageCommand;
use fluxdown_ui_downloads::actions as dl;
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_settings::search_index;
use fluxdown_ui_shell::RouteId;
use gpui::{Action, App, SharedString, Window};
use gpui_component::{Icon, WindowExt as _};

use crate::{
    actions::{About, CheckUpdate, OpenLogsFolder, OpenSettings, OpenWebsite, Quit},
    activity::{ActivityEntry, DOWNLOADS_ROUTE},
    app::Desktop,
    windows::{WindowKey, WindowRegistry},
};

/// 设备本地偏好：面板条目使用记录（见 [`UsageStats`]）。
const USAGE_PREF_KEY: &str = "desktop.command_palette.usage";
/// 同义词文案用 `|` 分隔多个别名。
const ALIAS_SEPARATOR: char = '|';

/// ⌘K / Ctrl+K：当前宿主窗口已开面板则关闭，否则打开。已有其他对话框时不叠开。
///
/// 键盘触发时正处于焦点窗口自己的 update 栈内，必须 defer 后再操作窗口。
pub fn toggle(cx: &mut App) {
    cx.defer(|cx| {
        let focused = WindowRegistry::focused_window(cx).filter(|handle| {
            matches!(
                WindowRegistry::key_of(cx, handle.window_id()),
                Some(WindowKey::Main | WindowKey::Settings)
            )
        });
        let host = match focused {
            Some(handle) => handle,
            None => {
                crate::windows::main::reveal(cx);
                let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
                    return;
                };
                handle
            }
        };
        if let Err(error) = host.update(cx, |_, window, cx| {
            if fluxdown_ui_command_palette::is_open(window, cx) {
                fluxdown_ui_command_palette::close(window, cx);
                return;
            }
            if window.has_active_dialog(cx) {
                return;
            }
            let translator = Desktop::global(cx).translator.read(cx).clone();
            let config = build_config(&translator, cx);
            fluxdown_ui_command_palette::open(window, cx, &translator, config);
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
}

fn build_config(translator: &Translator, cx: &mut App) -> PaletteConfig {
    let mut english = translator.clone();
    english.set_locale("en");
    let store = Desktop::global(cx).settings_store.clone();
    let labels = Labels {
        current: translator,
        english: &english,
        hints: crate::shortcuts::hints(&store, translator, cx),
    };
    let mut items = commands(&labels, cx);
    items.extend(crate::plugin_contributions::palette_items(cx));
    items.extend(settings_items(&labels, cx));

    let usage = UsageStats::from_value(Desktop::preferences(cx).get(USAGE_PREF_KEY));
    PaletteConfig {
        items,
        usage,
        on_usage: Rc::new(|usage, cx| Desktop::set_pref(cx, USAGE_PREF_KEY, usage.to_value())),
    }
}

/// 当前语言与英文两套文案：英文标题与英文同义词作为别名，中文界面也能用英文搜索。
struct Labels<'a> {
    current: &'a Translator,
    english: &'a Translator,
    /// 应用内快捷键提示（目录 id → 展示文本），来自用户当前绑定；未绑定的动作不在表内。
    hints: HashMap<&'static str, String>,
}

/// 面板命令 id → 对应的应用内快捷键目录 id。
fn command_shortcut_id(command_id: &str) -> Option<&'static str> {
    Some(match command_id {
        "cmd.new_download" => "new_download",
        "cmd.open_torrent_file" => "open_torrent_file",
        "cmd.pause_all" => "pause_all",
        "cmd.resume_all" => "resume_all",
        "cmd.select_all" => "select_all",
        "cmd.search_tasks" => "focus_search",
        "cmd.cycle_density" => "cycle_density",
        "cmd.cycle_group_by" => "cycle_group",
        "cmd.cycle_sort" => "cycle_sort",
        "cmd.toggle_detail_panel" => "toggle_detail",
        "cmd.open_settings" => "open_settings",
        "cmd.quit" => "quit",
        _ => return None,
    })
}

impl Labels<'_> {
    fn title(&self, key: &str) -> SharedString {
        SharedString::from(self.current.text(key).to_owned())
    }

    /// 标题键的英文文本（与当前标题相同时省略）。
    fn english_alias(&self, title_key: &str) -> Option<SharedString> {
        let english = self.english.text(title_key);
        (english != self.current.text(title_key)).then(|| SharedString::from(english.to_owned()))
    }

    /// 同义词键在当前语言与英文下的全部别名（`|` 分隔，去重）。
    fn synonyms(&self, key: &str) -> Vec<SharedString> {
        let mut synonyms: Vec<SharedString> = Vec::new();
        for translator in [self.current, self.english] {
            for text in translator.text(key).split(ALIAS_SEPARATOR).map(str::trim) {
                if !text.is_empty() && !synonyms.iter().any(|known| known.as_ref() == text) {
                    synonyms.push(SharedString::from(text.to_owned()));
                }
            }
        }
        synonyms
    }

    fn command(
        &self,
        id: &'static str,
        title_key: &str,
        icon: FluxIcon,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> PaletteItem {
        let item = PaletteItem::new(PaletteItemKind::Command, id, self.title(title_key), run)
            .icon(Icon::new(icon))
            .aliases(self.english_alias(title_key));
        match command_shortcut_id(id).and_then(|shortcut| self.hints.get(shortcut)) {
            Some(hint) => item.shortcut(hint.clone()),
            None => item,
        }
    }
}

/// 全局命令：应用级动作经窗口派发（冒泡到 app 的全局处理器），下载动作派发到主窗口下载页。
fn commands(labels: &Labels, cx: &App) -> Vec<PaletteItem> {
    let mut items = vec![
        labels.command(
            "cmd.new_download",
            "menuNewDownload",
            FluxIcon::Plus,
            |w, cx| {
                dispatch(w, cx, &dl::NewDownload);
            },
        ),
        labels.command(
            "cmd.open_torrent_file",
            "openTorrentFile",
            FluxIcon::Magnet,
            |_, cx| run_in_downloads(cx, PageCommand::OpenTorrentFile),
        ),
        labels.command(
            "cmd.seed_local",
            "seedLocalTitle",
            FluxIcon::HardDrive,
            |_, cx| run_in_downloads(cx, PageCommand::SeedLocal),
        ),
        labels
            .command("cmd.pause_all", "pauseAll", FluxIcon::Pause, |_, cx| {
                run_in_downloads(cx, PageCommand::PauseAll);
            })
            .aliases(labels.synonyms("commandPalettePauseAllAliases")),
        labels
            .command("cmd.resume_all", "resumeAll", FluxIcon::Play, |_, cx| {
                run_in_downloads(cx, PageCommand::ResumeAll);
            })
            .aliases(labels.synonyms("commandPaletteResumeAllAliases")),
        labels.command(
            "cmd.clear_finished",
            "menuClearFinished",
            FluxIcon::Trash2,
            |_, cx| run_in_downloads(cx, PageCommand::ClearFinished),
        ),
        labels.command(
            "cmd.select_all",
            "selectAll",
            FluxIcon::CheckboxCheck,
            |_, cx| {
                run_in_downloads(cx, PageCommand::SelectAll);
            },
        ),
        labels.command(
            "cmd.search_tasks",
            "searchTasksPlaceholder",
            FluxIcon::Search,
            |_, cx| run_in_downloads(cx, PageCommand::FocusSearch),
        ),
        labels.command(
            "cmd.cycle_density",
            "menuCycleDensity",
            FluxIcon::Rows3,
            |_, cx| run_in_downloads(cx, PageCommand::CycleDensity),
        ),
        labels.command(
            "cmd.cycle_group_by",
            "menuCycleGroupBy",
            FluxIcon::Group,
            |_, cx| run_in_downloads(cx, PageCommand::CycleGroupBy),
        ),
        labels.command(
            "cmd.cycle_sort",
            "menuCycleSort",
            FluxIcon::ArrowUpDown,
            |_, cx| run_in_downloads(cx, PageCommand::CycleSort),
        ),
        labels.command(
            "cmd.toggle_detail_panel",
            "menuDetailPanel",
            FluxIcon::PanelBottom,
            |_, cx| run_in_downloads(cx, PageCommand::ToggleDetailPanel),
        ),
        labels.command(
            "cmd.queue_manager",
            "manageQueueAction",
            FluxIcon::Layers,
            |w, cx| dispatch(w, cx, &dl::OpenQueueManager),
        ),
    ];

    for entry in ActivityEntry::ALL {
        let label_key = entry.label_key();
        match entry {
            ActivityEntry::Downloads | ActivityEntry::Rss | ActivityEntry::Webhooks => {
                let Some(route) = entry.route() else {
                    continue;
                };
                let visible = entry.toggle().is_none_or(|toggle| {
                    Desktop::pref(cx, toggle.pref_key)
                        .and_then(|value| value.as_bool())
                        .unwrap_or(true)
                });
                if !visible {
                    continue;
                }
                let id: &'static str = match entry {
                    ActivityEntry::Downloads => "cmd.go_downloads",
                    ActivityEntry::Rss => "cmd.go_rss",
                    _ => "cmd.go_webhooks",
                };
                let icon = match entry {
                    ActivityEntry::Downloads => FluxIcon::Download,
                    ActivityEntry::Rss => FluxIcon::Rss,
                    _ => FluxIcon::Webhook,
                };
                let page = labels.current.text(label_key);
                let english_page = labels.english.text(label_key);
                let title = labels
                    .current
                    .text_with("commandPaletteGoTo", &[("page", page)]);
                let english = labels
                    .english
                    .text_with("commandPaletteGoTo", &[("page", english_page)]);
                items.push(
                    PaletteItem::new(PaletteItemKind::Command, id, title.clone(), move |_, cx| {
                        navigate(cx, route.clone());
                    })
                    .icon(Icon::new(icon))
                    .aliases((english != title).then_some(english))
                    .keywords([page.to_owned()]),
                );
            }
            ActivityEntry::Account => items.push(labels.command(
                "cmd.open_account",
                label_key,
                FluxIcon::User,
                |_, cx| crate::activity::open_account(cx),
            )),
            ActivityEntry::Settings => items.push(labels.command(
                "cmd.open_settings",
                label_key,
                FluxIcon::Settings,
                |w, cx| {
                    dispatch(w, cx, &OpenSettings);
                },
            )),
        }
    }

    items.extend([
        labels.command(
            "cmd.toggle_theme",
            "activityThemeToggle",
            FluxIcon::Palette,
            crate::activity::toggle_theme,
        ),
        labels.command(
            "cmd.customize_layout",
            "chromeLayoutCustomize",
            FluxIcon::GripVertical,
            |_, cx| customize_layout(cx),
        ),
        labels.command(
            "cmd.check_update",
            "menuCheckForUpdates",
            FluxIcon::RotateCw,
            |w, cx| dispatch(w, cx, &CheckUpdate),
        ),
        labels.command(
            "cmd.open_logs_folder",
            "menuOpenLogsFolder",
            FluxIcon::FolderOpen,
            |w, cx| dispatch(w, cx, &OpenLogsFolder),
        ),
        labels.command("cmd.website", "menuWebsite", FluxIcon::Globe, |w, cx| {
            dispatch(w, cx, &OpenWebsite);
        }),
        labels.command("cmd.about", "menuAbout", FluxIcon::Info, |w, cx| {
            dispatch(w, cx, &About);
        }),
    ]);
    let quit = labels
        .command("cmd.quit", "menuQuit", FluxIcon::Power, |w, cx| {
            dispatch(w, cx, &Quit);
        })
        .aliases(labels.synonyms("commandPaletteQuitAliases"));
    items.push(quit);
    items
}

/// 设置窗口的全部分类、子 Tab 与设置行；执行即打开设置窗口并定位。
fn settings_items(labels: &Labels, cx: &mut App) -> Vec<PaletteItem> {
    let desktop = Desktop::global(cx);
    let translator = desktop.translator.clone();
    let store = desktop.settings_store.clone();
    let referral_visible = desktop.referral_visibility.read(cx).visible();
    let entries = search_index(
        &translator,
        &store,
        &crate::activity::toggles(),
        referral_visible,
        cx,
    );
    let root = labels.title(fluxdown_ui_i18n::keys::SETTINGS);
    entries
        .into_iter()
        .map(|entry| {
            let detail = if entry.breadcrumb.is_empty() {
                root.clone()
            } else {
                SharedString::from(format!("{root} › {}", entry.breadcrumb))
            };
            let icon = if entry.target.row.is_some() {
                FluxIcon::SlidersHorizontal
            } else {
                FluxIcon::Settings
            };
            let target = entry.target;
            PaletteItem::new(
                PaletteItemKind::Setting,
                entry.id,
                entry.title,
                move |_, cx| {
                    crate::windows::settings::reveal(cx, target.clone());
                },
            )
            .icon(Icon::new(icon))
            .detail(detail)
            .aliases(entry.aliases)
            .keywords(entry.keywords)
        })
        .collect()
}

/// 应用级动作：从面板所在窗口派发，冒泡到 app 注册的全局处理器（或窗口内同名处理器）。
fn dispatch(window: &mut Window, cx: &mut App, action: &dyn Action) {
    window.dispatch_action(action.boxed_clone(), cx);
}

/// 下载页命令：主窗口到前台并切到下载路由，再直接调用下载页。
///
/// 不能把动作派发到下载页焦点：「切换分组 / 详情面板」等字母键动作在处理器里有
/// 「输入框聚焦时让给输入框」的守卫，从面板合成派发会被守卫吞掉；直接调用也不依赖
/// 下载页已在上一帧渲染进派发树。
fn run_in_downloads(cx: &mut App, command: PageCommand) {
    cx.defer(move |cx| {
        navigate_now(cx, DOWNLOADS_ROUTE);
        let Some(handle) = WindowRegistry::handle(cx, &WindowKey::Main) else {
            return;
        };
        let Some(downloads) = Desktop::global(cx)
            .main_downloads
            .as_ref()
            .and_then(gpui::WeakEntity::upgrade)
        else {
            return;
        };
        if let Err(error) = handle.update(cx, |_, window, cx| {
            downloads.update(cx, |view, cx| view.run_page_command(command, window, cx));
        }) {
            log::debug!("view or window released before lifecycle update: {error:#}");
        }
    });
}

/// 主窗口到前台并切到指定路由。
fn navigate(cx: &mut App, route: RouteId) {
    cx.defer(move |cx| navigate_now(cx, route));
}

pub(crate) fn navigate_now(cx: &mut App, route: RouteId) {
    crate::windows::main::reveal(cx);
    if let Some(shell) = Desktop::global(cx)
        .main_shell
        .as_ref()
        .and_then(gpui::WeakEntity::upgrade)
    {
        shell.update(cx, |shell, cx| shell.navigate(route, cx));
    }
}

/// 主窗口到前台并进入 chrome 布局编辑模式。
fn customize_layout(cx: &mut App) {
    cx.defer(|cx| {
        crate::windows::main::reveal(cx);
        if let Some(shell) = Desktop::global(cx)
            .main_shell
            .as_ref()
            .and_then(gpui::WeakEntity::upgrade)
        {
            shell.update(cx, |shell, cx| shell.set_editing(true, cx));
        }
    });
}
