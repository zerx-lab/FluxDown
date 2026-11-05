//! 由 agent 快照推导托盘展示（可见性、文案、关机倒计时），只在输入变化时推送给宿主。

use std::path::Path;
use std::sync::Arc;

use fluxdown_protocol::app_icon::AppIconChoice;
use fluxdown_protocol::{AgentSnapshot, PowerAction, PowerStatusDto};
use fluxdown_ui_i18n::{I18nCatalog, Translator};
use serde_json::Value;

use super::{TrayIconSource, TrayModel, TrayPort};
use crate::app_icon::IconStore;
use crate::event_hub::AgentEventHub;

const DEFAULT_TOOLTIP: &str = "FluxDown";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inputs {
    visible: bool,
    locale: Option<String>,
    power: PowerStatusDto,
    icon: AppIconChoice,
}

impl Inputs {
    fn read(snapshot: &AgentSnapshot) -> Self {
        Self {
            visible: snapshot.shell.tray_available && snapshot.shell.resident,
            locale: snapshot
                .preferences
                .values
                .get("general.locale")
                .and_then(Value::as_str)
                .filter(|locale| *locale != "system")
                .map(str::to_owned),
            power: snapshot.power,
            icon: crate::app_icon::choice_of(&snapshot.preferences),
        }
    }
}

pub(super) struct TrayPresenter {
    port: Option<Arc<dyn TrayPort>>,
    translator: Option<Translator>,
    last: Option<Inputs>,
    icons: IconStore,
    /// 当前选择对应的图标；选择变化时才重新读取 / 缩放图片。
    icon: Option<(AppIconChoice, TrayIconSource)>,
}

impl TrayPresenter {
    pub(super) fn new(port: Option<Arc<dyn TrayPort>>, agent_data_dir: impl AsRef<Path>) -> Self {
        let translator = port
            .as_ref()
            .and_then(|_| match I18nCatalog::load_embedded() {
                Ok(catalog) => {
                    Some(Arc::new(catalog).translator(&fluxdown_ui_i18n::system_locale()))
                }
                Err(error) => {
                    tracing::warn!(error = %error, "tray translations unavailable");
                    None
                }
            });
        Self {
            port,
            translator,
            last: None,
            icons: IconStore::new(agent_data_dir.as_ref()),
            icon: None,
        }
    }

    pub(super) fn update(&mut self, events: &AgentEventHub) {
        let Self {
            port,
            translator,
            last,
            icons,
            icon,
        } = self;
        let (Some(port), Some(translator)) = (port.as_ref(), translator.as_mut()) else {
            return;
        };
        let inputs = events.inspect(Inputs::read);
        if last.as_ref() == Some(&inputs) {
            return;
        }
        let locale = inputs
            .locale
            .clone()
            .unwrap_or_else(fluxdown_ui_i18n::system_locale);
        translator.set_locale(&locale);
        let mut model = build(translator, &inputs, COUNTDOWN_IN_MENU);
        model.icon = cached_icon(icon, icons, &inputs.icon);
        port.apply(&model);
        *last = Some(inputs);
    }
}

/// 选择没变就复用上次的图标；变化时才读图片、缩放（读不出来回落默认，见 `tray_icon_source`）。
fn cached_icon(
    cache: &mut Option<(AppIconChoice, TrayIconSource)>,
    store: &IconStore,
    choice: &AppIconChoice,
) -> TrayIconSource {
    if let Some((cached, source)) = cache
        && cached == choice
    {
        return source.clone();
    }
    let source = crate::app_icon::tray_icon_source(choice, store);
    *cache = Some((choice.clone(), source.clone()));
    source
}

/// macOS 不给状态栏项设 tooltip（#953：macOS 27 上 tooltip 收不到移出事件而常驻），
/// 倒计时改挂在「取消关机 / 睡眠」菜单项上。
const COUNTDOWN_IN_MENU: bool = cfg!(target_os = "macos");

fn build(translator: &Translator, inputs: &Inputs, countdown_in_menu: bool) -> TrayModel {
    let (countdown_key, cancel_key) = match inputs.power.action {
        PowerAction::Shutdown => ("shutdownCountdown", "trayCancelShutdown"),
        PowerAction::Sleep => ("sleepCountdown", "trayCancelSleep"),
    };
    let remaining = inputs.power.countdown_remaining_secs.map(format_remaining);
    let tooltip = remaining.as_ref().map_or_else(
        || DEFAULT_TOOLTIP.to_owned(),
        |time| translator.text_with(countdown_key, &[("time", time)]),
    );
    let cancel_shutdown = inputs.power.armed_delay_secs.map(|_| {
        let label = translator.text(cancel_key);
        match remaining.as_ref() {
            Some(time) if countdown_in_menu => format!("{label} ({time})"),
            _ => label.to_owned(),
        }
    });
    TrayModel {
        visible: inputs.visible,
        show_window: translator.text("trayShowWindow").to_owned(),
        settings: translator.text("settings").to_owned(),
        pause_all: translator.text("pauseAll").to_owned(),
        resume_all: translator.text("resumeAll").to_owned(),
        cancel_shutdown,
        restart: translator.text("trayRestart").to_owned(),
        quit: translator.text("trayExit").to_owned(),
        tooltip,
        // 图标由 `TrayPresenter` 按应用图标偏好填入（需要读图片文件）。
        icon: TrayIconSource::Default,
    }
}

/// `mm:ss`，与 GPUI `crates/app/src/power.rs::format_remaining` 一致。
fn format_remaining(total_secs: u64) -> String {
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translator(locale: &str) -> Translator {
        Arc::new(I18nCatalog::load_embedded().expect("embedded catalog")).translator(locale)
    }

    #[test]
    fn countdown_drives_tooltip_and_cancel_item() {
        let inputs = Inputs {
            visible: true,
            locale: Some("zh".to_owned()),
            power: PowerStatusDto {
                armed_delay_secs: Some(60),
                countdown_remaining_secs: Some(65),
                action: PowerAction::Shutdown,
            },
            icon: AppIconChoice::Default,
        };
        let model = build(&translator("zh"), &inputs, false);
        assert!(model.tooltip.contains("01:05"), "{}", model.tooltip);
        let cancel = model.cancel_shutdown.expect("cancel item while armed");
        assert!(!cancel.contains("01:05"), "{cancel}");

        let model = build(&translator("zh"), &inputs, true);
        let cancel = model.cancel_shutdown.expect("cancel item while armed");
        assert!(cancel.ends_with("(01:05)"), "{cancel}");

        let idle = Inputs {
            power: PowerStatusDto::default(),
            ..inputs
        };
        let model = build(&translator("zh"), &idle, true);
        assert_eq!(model.tooltip, DEFAULT_TOOLTIP);
        assert_eq!(model.cancel_shutdown, None);
    }

    /// 查表缺键时 `Translator` 回退为键名；托盘文案必须全部存在于 en / zh 基线。
    #[test]
    fn menu_labels_exist_in_both_baselines() {
        for (action, countdown_key, cancel_key) in [
            (
                PowerAction::Shutdown,
                "shutdownCountdown",
                "trayCancelShutdown",
            ),
            (PowerAction::Sleep, "sleepCountdown", "trayCancelSleep"),
        ] {
            let inputs = Inputs {
                visible: true,
                locale: None,
                power: PowerStatusDto {
                    armed_delay_secs: Some(0),
                    countdown_remaining_secs: Some(1),
                    action,
                },
                icon: AppIconChoice::Default,
            };
            for locale in ["en", "zh"] {
                let model = build(&translator(locale), &inputs, false);
                for (label, key) in [
                    (model.show_window.as_str(), "trayShowWindow"),
                    (model.pause_all.as_str(), "pauseAll"),
                    (model.resume_all.as_str(), "resumeAll"),
                    (model.restart.as_str(), "trayRestart"),
                    (model.quit.as_str(), "trayExit"),
                    (
                        model.cancel_shutdown.as_deref().unwrap_or_default(),
                        cancel_key,
                    ),
                ] {
                    assert_ne!(label, key, "{locale} is missing {key}");
                }
                assert!(!model.tooltip.contains(countdown_key), "{locale}");
            }
        }
    }

    #[test]
    fn tray_app_icon_is_compared_by_key_not_bytes() {
        let bolt = TrayIconSource::Image {
            key: "bolt".to_owned(),
            png: Arc::from(&b"one"[..]),
        };
        let same_key = TrayIconSource::Image {
            key: "bolt".to_owned(),
            png: Arc::from(&b"two"[..]),
        };
        let custom = TrayIconSource::Image {
            key: "custom:0a1b2c".to_owned(),
            png: Arc::from(&b"one"[..]),
        };

        assert_eq!(bolt, same_key);
        assert_ne!(bolt, custom);
        assert_ne!(bolt, TrayIconSource::Default);
        assert_eq!(TrayIconSource::Default, TrayIconSource::Default);
    }

    #[test]
    fn tray_app_icon_cache_reloads_only_when_the_choice_changes() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-tray-icon-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let store = IconStore::new(&dir);
        let mut cache = None;

        let first = cached_icon(&mut cache, &store, &AppIconChoice::Default);
        assert_eq!(first, TrayIconSource::Default);
        let cached_choice = cache.as_ref().map(|(choice, _)| choice.clone());
        assert_eq!(cached_choice, Some(AppIconChoice::Default));

        // 自定义图片不存在：回落默认，但缓存键记的是用户的选择，不会每次重读磁盘。
        let custom = AppIconChoice::Custom {
            revision: "0a1b2c3d4e5f".to_owned(),
        };
        assert_eq!(
            cached_icon(&mut cache, &store, &custom),
            TrayIconSource::Default
        );
        let cached_choice = cache.as_ref().map(|(choice, _)| choice.clone());
        assert_eq!(cached_choice, Some(custom));
    }
}
