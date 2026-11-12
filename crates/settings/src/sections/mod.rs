//! 设置分区：每个模块产出一个 [`SettingPage`]。
//!
//! 页面闭包只捕获 `Entity<SettingsStore>` 与预先解析好的文案；
//! 所有键名、范围、枚举值以 `fluxdown_protocol` 目录为准。

pub(crate) mod about;
pub(crate) mod api;
mod app_icon;
pub(crate) mod appearance;
mod auto_quantity;
pub(crate) mod bt;
mod bt_guard;
mod bt_guard_add_dialog;
mod bt_guard_bans;
mod bt_guard_client_rule_dialog;
mod bt_guard_client_rules;
mod bt_guard_events;
pub(crate) mod categories;
pub(crate) mod category_dialog;
pub(crate) mod cloud_channel_dialog;
pub(crate) mod cloud_dialogs;
pub(crate) mod cloud_push;
pub(crate) mod doctor;
pub(crate) mod download;
pub(crate) mod ed2k;
mod ed2k_probe;
mod font_family;
pub(crate) mod general;
mod icon_pack;
pub(crate) mod notify;
pub(crate) mod proxy;
pub(crate) mod proxy_rule_dialog;
pub(crate) mod proxy_rules;
pub(crate) mod rate_limit;
mod retention;
pub(crate) mod shortcuts;
pub(crate) mod site_auth;
mod sound;
pub(crate) mod subscription;
pub(crate) mod user_agent;
pub(crate) mod webhook;
pub(crate) mod webhook_dialog;

use fluxdown_protocol::{DaemonConfigKind, daemon_config_field};
use fluxdown_ui_i18n::Translator;
use gpui::{AnyView, App, Entity, ParentElement as _, SharedString};
use gpui_component::Icon;

use crate::store::SettingsStore;
use crate::ui::{Control, SettingsPage, SettingsRow, SettingsSection, SettingsTab, meta_text};

/// 分区构建上下文：存储实体 + 当前语言的翻译快照。
pub(crate) struct SectionContext<'a> {
    pub store: &'a Entity<SettingsStore>,
    pub translator: &'a Translator,
}

impl SectionContext<'_> {
    /// 翻译文案；键缺失时回退为键名。
    pub(crate) fn t(&self, key: &str) -> SharedString {
        SharedString::from(self.translator.text(key).to_owned())
    }

    #[must_use]
    pub(crate) fn store(&self) -> Entity<SettingsStore> {
        self.store.clone()
    }

    // ───────────────────────── daemon 字段 ─────────────────────────

    pub(crate) fn daemon_switch(&self, key: &'static str) -> Control {
        let get = self.store();
        let set = self.store();
        Control::switch(
            move |cx: &App| get.read(cx).daemon_bool(key),
            move |value, cx: &mut App| {
                set.update(cx, |store, cx| store.set_daemon_bool(key, value, cx));
            },
        )
    }

    pub(crate) fn daemon_number(&self, key: &'static str) -> Control {
        self.daemon_number_with(key, 1.0)
    }

    pub(crate) fn daemon_number_with(&self, key: &'static str, step: f64) -> Control {
        let get = self.store();
        let set = self.store();
        let (min, max, float) = match daemon_config_field(key).map(|field| field.kind) {
            Some(DaemonConfigKind::Integer { min, max }) => (min as f64, max as f64, false),
            Some(DaemonConfigKind::Float { min }) => (min, f64::MAX, true),
            _ => (f64::MIN, f64::MAX, false),
        };
        Control::number(
            min,
            max,
            step,
            move |cx: &App| {
                if float {
                    get.read(cx).daemon_f64(key)
                } else {
                    get.read(cx).daemon_i64(key) as f64
                }
            },
            move |value, cx: &mut App| {
                set.update(cx, |store, cx| {
                    if float {
                        store.set_daemon_f64(key, value, cx);
                    } else {
                        store.set_daemon_i64(key, value.round() as i64, cx);
                    }
                });
            },
        )
    }

    pub(crate) fn daemon_input(&self, key: &'static str) -> Control {
        let get = self.store();
        let set = self.store();
        Control::input(
            move |cx: &App| SharedString::from(get.read(cx).daemon_str(key)),
            move |value: SharedString, cx: &mut App| {
                set.update(cx, |store, cx| store.set_daemon(key, value.to_string(), cx));
            },
        )
    }

    /// 枚举下拉：`options` 为 `(wire 值, 文案)`。
    pub(crate) fn daemon_dropdown(
        &self,
        key: &'static str,
        options: Vec<(SharedString, SharedString)>,
    ) -> Control {
        let get = self.store();
        let set = self.store();
        Control::dropdown(
            options,
            move |cx: &App| SharedString::from(get.read(cx).daemon_str(key)),
            move |value: SharedString, cx: &mut App| {
                set.update(cx, |store, cx| store.set_daemon(key, value.to_string(), cx));
            },
        )
    }

    /// 由协议目录的枚举值生成下拉，文案键为 `{prefix}{Value}`（首字母大写驼峰）。
    pub(crate) fn daemon_enum_dropdown(&self, key: &'static str, label_prefix: &str) -> Control {
        let values: &[&str] = match daemon_config_field(key).map(|field| field.kind) {
            Some(DaemonConfigKind::Enum(values)) => values,
            _ => &[],
        };
        let options = values
            .iter()
            .map(|value| {
                (
                    SharedString::from(*value),
                    self.t(&format!("{label_prefix}{}", camel(value))),
                )
            })
            .collect();
        self.daemon_dropdown(key, options)
    }

    // ───────────────────────── 偏好字段 ─────────────────────────

    pub(crate) fn pref_switch(&self, key: &'static str, default: bool) -> Control {
        let get = self.store();
        let set = self.store();
        Control::switch(
            move |cx: &App| get.read(cx).pref_bool(key, default),
            move |value, cx: &mut App| {
                set.update(cx, |store, cx| store.set_pref_bool(key, value, cx));
            },
        )
    }

    pub(crate) fn pref_input(&self, key: &'static str, default: &'static str) -> Control {
        let get = self.store();
        let set = self.store();
        Control::input(
            move |cx: &App| SharedString::from(get.read(cx).pref_str(key, default)),
            move |value: SharedString, cx: &mut App| {
                set.update(cx, |store, cx| {
                    store.set_pref_str(key, value.to_string(), cx)
                });
            },
        )
    }

    pub(crate) fn pref_dropdown(
        &self,
        key: &'static str,
        default: &'static str,
        options: Vec<(SharedString, SharedString)>,
    ) -> Control {
        let get = self.store();
        let set = self.store();
        Control::dropdown(
            options,
            move |cx: &App| SharedString::from(get.read(cx).pref_str(key, default)),
            move |value: SharedString, cx: &mut App| {
                set.update(cx, |store, cx| {
                    store.set_pref_str(key, value.to_string(), cx)
                });
            },
        )
    }

    // ───────────────────────── 条目 ─────────────────────────

    /// 标题 + 可选描述的标准行；描述键不存在时不显示描述。
    pub(crate) fn item(
        &self,
        title_key: &str,
        desc_key: Option<&str>,
        control: Control,
    ) -> SettingsRow {
        let mut row = SettingsRow::new(self.t(title_key), control);
        if let Some(desc_key) = desc_key
            && self.translator.text(desc_key) != desc_key
        {
            row = row.description(self.t(desc_key));
        }
        row
    }
}

/// `delete_files` → `DeleteFiles`；`or` → `Or`。
pub(crate) fn camel(value: &str) -> String {
    value
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// 由外部 capability 提供内容的整页（账户 / 扩展）；槽位为空时显示分类描述。
pub(crate) fn slot_page(
    ctx: &SectionContext,
    key: &'static str,
    title_key: &str,
    desc_key: &str,
    icon: impl Into<Icon>,
    view: Option<AnyView>,
) -> SettingsPage {
    let page = SettingsPage::new(key, ctx.t(title_key), ctx.t(desc_key), icon);
    match view {
        Some(view) => page.tab(SettingsTab::new("", SharedString::default()).view(view)),
        None => {
            let description = ctx.t(desc_key);
            page.sections([SettingsSection::new().row(SettingsRow::custom(
                move |_, _, _, cx: &mut App| meta_text(cx).child(description.clone()),
            ))])
        }
    }
}
