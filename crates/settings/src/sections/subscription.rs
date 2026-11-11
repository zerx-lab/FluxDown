//! 多行列表编辑（Tracker / 服务器 / 订阅源）与订阅状态行。

use std::collections::HashSet;

use chrono::{Local, TimeZone as _};
use fluxdown_protocol::method;
use fluxdown_ui_components::{ButtonVariant, loading_button};
use fluxdown_ui_theme::{CONTROL_HEIGHT, active_theme};
use gpui::{
    App, AppContext as _, Entity, IntoElement as _, ParentElement, SharedString, Styled,
    Subscription, Window, px,
};
use gpui_component::{
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    v_flex,
};
use serde_json::json;

use super::SectionContext;
use crate::store::SettingsStore;
use crate::ui::{Control, SettingsRow, body_text, meta_text};

/// 列表型配置键的存储格式；编辑区一律每行一个条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListFormat {
    /// 按行存储（Tracker、订阅地址；保留 `#` 注释等自由文本）。
    Lines,
    /// 逗号分隔的 `host:port`（`ed2k_server_list` / `ed2k_server_sub_cache`）。
    /// 读取时同时容忍换行/空白分隔的旧值。
    Comma,
}

impl ListFormat {
    /// 存储值中的非空条目。
    fn entries(self, stored: &str) -> impl Iterator<Item = &str> {
        let is_separator: fn(char) -> bool = match self {
            Self::Lines => |c| c == '\n' || c == '\r',
            Self::Comma => |c| c == ',' || c.is_whitespace(),
        };
        stored
            .split(is_separator)
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
    }

    /// 存储值中的条目数。
    pub(crate) fn count(self, stored: &str) -> usize {
        self.entries(stored).count()
    }

    /// 存储值 → 编辑区文本（每行一个）。
    fn to_editor(self, stored: &str) -> String {
        match self {
            Self::Lines => stored.to_owned(),
            Self::Comma => self.entries(stored).collect::<Vec<_>>().join("\n"),
        }
    }

    /// 编辑区文本 → 存储值；与 daemon 的文本规范化（首尾去空白）结果一致，
    /// 使回显值可直接与上次写入值比较。
    fn to_stored(self, text: &str) -> String {
        match self {
            Self::Lines => text.trim().to_owned(),
            Self::Comma => {
                let mut seen = HashSet::new();
                self.entries(text)
                    .filter(|entry| seen.insert(entry.to_ascii_lowercase()))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        }
    }
}

struct TextareaSlot {
    state: Entity<TextareaState>,
    /// 最近一次与存储同步的值（存储格式）。
    last_synced: String,
    _subscription: Subscription,
}

/// 多行文本编辑一个 daemon 列表键；编辑区每行一个条目，改动或失焦即按 `format` 写回。
pub(crate) fn list_item(
    ctx: &SectionContext,
    title_key: &str,
    desc_key: &str,
    placeholder_key: &str,
    key: &'static str,
    format: ListFormat,
) -> SettingsRow {
    let field = textarea_field(ctx.store(), ctx.t(placeholder_key), key, format);
    ctx.item(title_key, Some(desc_key), field).vertical()
}

fn textarea_field(
    store: Entity<SettingsStore>,
    placeholder: SharedString,
    key: &'static str,
    format: ListFormat,
) -> Control {
    Control::custom(
        move |disabled, row_key: &SharedString, window: &mut Window, cx: &mut App| {
            let current = store.read(cx).daemon_str(key);
            let slot = window.use_keyed_state(
                SharedString::from(format!("{row_key}-{key}-textarea")),
                cx,
                {
                    let store = store.clone();
                    let current = current.clone();
                    let placeholder = placeholder.clone();
                    move |window, cx| {
                        let state = cx.new(|cx| {
                            TextareaState::new(window, cx)
                                .default_value(format.to_editor(&current))
                                .placeholder(placeholder)
                        });
                        let _subscription = cx.subscribe(
                            &state,
                            move |slot: &mut TextareaSlot, state, event: &InputEvent, cx| {
                                if matches!(event, InputEvent::Change | InputEvent::Blur) {
                                    // 按存储格式比较：编辑中的尾随换行、重复项等
                                    // 不改变存储值，既不写回也不触发回显覆盖光标。
                                    let stored = format.to_stored(&state.read(cx).value());
                                    if stored != slot.last_synced {
                                        slot.last_synced = stored.clone();
                                        store.update(cx, |store, cx| {
                                            store.set_daemon(key, stored, cx)
                                        });
                                    }
                                }
                            },
                        );
                        TextareaSlot {
                            state,
                            last_synced: current,
                            _subscription,
                        }
                    }
                },
            );
            slot.update(cx, |slot, cx| {
                if slot.last_synced != current {
                    let text = SharedString::from(format.to_editor(&current));
                    slot.last_synced = current;
                    slot.state
                        .update(cx, |state, cx| state.set_value(text, window, cx));
                }
            });
            let state = slot.read(cx).state.clone();
            Textarea::new(&state)
                // 文本区高度随行高增长：基准 120 + 6 行的行高增量（默认增量为 0）。
                .h(px(120.) + (active_theme(cx).density().control - CONTROL_HEIGHT) * 6.)
                .w_full()
                .disabled(disabled)
                .into_any_element()
        },
    )
}

#[derive(Clone, Copy)]
pub(crate) enum SubscriptionKind {
    BtTrackers,
    Ed2kServers,
    /// Kad 引导节点（nodes.dat）订阅。
    Ed2kNodes,
}

/// 订阅条数从哪里读。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CountSource {
    /// 数订阅缓存里的条目（按宿主写缓存时的拼接符）。
    Cache {
        key: &'static str,
        format: ListFormat,
    },
    /// 宿主直接给出的整数键（缓存是二进制，无法按分隔符计数）。
    Integer(&'static str),
}

impl CountSource {
    fn count(self, store: &SettingsStore) -> u64 {
        match self {
            Self::Cache { key, format } => {
                u64::try_from(format.count(&store.daemon_str(key))).unwrap_or(u64::MAX)
            }
            Self::Integer(key) => non_negative(store.daemon_i64(key)),
        }
    }
}

/// 整数条数：负数 / 哨兵按 0。
fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl SubscriptionKind {
    fn count_source(self) -> CountSource {
        match self {
            Self::BtTrackers => CountSource::Cache {
                key: "bt_tracker_sub_cache",
                format: ListFormat::Lines,
            },
            Self::Ed2kServers => CountSource::Cache {
                key: "ed2k_server_sub_cache",
                format: ListFormat::Comma,
            },
            Self::Ed2kNodes => CountSource::Integer("ed2k_nodes_dat_count"),
        }
    }
    fn updated_at_key(self) -> &'static str {
        match self {
            Self::BtTrackers => "bt_tracker_sub_updated_at",
            Self::Ed2kServers => "ed2k_server_sub_updated_at",
            Self::Ed2kNodes => "ed2k_nodes_dat_updated_at",
        }
    }
    fn method(self) -> &'static str {
        match self {
            Self::BtTrackers => method::DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH,
            Self::Ed2kServers => method::DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH,
            Self::Ed2kNodes => method::DAEMON_ED2K_NODES_SUBSCRIPTION_REFRESH,
        }
    }
    fn action(self) -> &'static str {
        match self {
            Self::BtTrackers => "btTrackerRefresh",
            Self::Ed2kServers => "ed2kServerRefresh",
            Self::Ed2kNodes => "ed2kNodesRefresh",
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            Self::BtTrackers => "btTrackerSub",
            Self::Ed2kServers => "ed2kServerSub",
            Self::Ed2kNodes => "ed2kNodesSub",
        }
    }
}

/// 订阅状态：已订阅条数、更新时间、立即更新。
pub(crate) fn status_item(
    ctx: &SectionContext,
    kind: SubscriptionKind,
    _cx: &mut App,
) -> SettingsRow {
    let store = ctx.store();
    let translator = ctx.translator.clone();
    let prefix = kind.prefix();
    let update_now = ctx.t(&format!("{prefix}UpdateNow"));
    let updating = ctx.t(&format!("{prefix}Updating"));
    let failed = ctx.t(&format!("{prefix}UpdateFailed"));
    let never = ctx.t(&format!("{prefix}NeverUpdated"));
    SettingsRow::custom(move |_, _, _, cx: &mut App| {
        let tokens = active_theme(cx).tokens();
        let snapshot = store.read(cx);
        let count = kind.count_source().count(snapshot);
        let updated_at = snapshot.daemon_i64(kind.updated_at_key());
        let busy = snapshot.is_busy(kind.action());
        let last_failed = snapshot
            .transient(kind.action())
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let status = translator.text_with(&format!("{prefix}Status"), &[("n", &count.to_string())]);
        let time = if updated_at > 0 {
            translator.text_with(
                &format!("{prefix}UpdatedAt"),
                &[("time", &format_unix(updated_at))],
            )
        } else {
            never.to_string()
        };
        let click_store = store.clone();
        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(tokens.spacing.md)
            .child(
                v_flex()
                    .gap(tokens.spacing.xxs)
                    .child(body_text(cx).child(SharedString::from(status)))
                    .child(
                        meta_text(cx)
                            .text_color(if last_failed {
                                tokens.colors.destructive
                            } else {
                                tokens.colors.muted_foreground
                            })
                            .child(if last_failed {
                                failed.clone()
                            } else {
                                SharedString::from(time)
                            }),
                    ),
            )
            .child(
                loading_button(
                    kind.action(),
                    if busy {
                        updating.clone()
                    } else {
                        update_now.clone()
                    },
                    ButtonVariant::Secondary,
                    busy,
                    cx,
                )
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    click_store.update(cx, |store, cx| {
                        store.call_with(
                            kind.action(),
                            kind.method(),
                            json!({}),
                            cx,
                            move |store, result, cx| {
                                let ok = result
                                    .as_ref()
                                    .ok()
                                    .and_then(|value| value.get("success"))
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false);
                                store.set_transient(kind.action(), json!(!ok), cx);
                            },
                        );
                    });
                }),
            )
            .into_any_element()
    })
}

/// Unix 秒 → 本地时间 `YYYY-MM-DD HH:MM`。
pub(crate) fn format_unix(secs: i64) -> String {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{CountSource, ListFormat, SubscriptionKind, non_negative};

    #[test]
    fn ed2k_nodes_count_reads_integer_key_others_count_cache() {
        assert_eq!(
            SubscriptionKind::Ed2kNodes.count_source(),
            CountSource::Integer("ed2k_nodes_dat_count")
        );
        assert!(matches!(
            SubscriptionKind::Ed2kServers.count_source(),
            CountSource::Cache {
                format: ListFormat::Comma,
                ..
            }
        ));
        assert!(matches!(
            SubscriptionKind::BtTrackers.count_source(),
            CountSource::Cache {
                format: ListFormat::Lines,
                ..
            }
        ));
        assert_eq!(SubscriptionKind::Ed2kNodes.prefix(), "ed2kNodesSub");
        assert_eq!(SubscriptionKind::Ed2kNodes.action(), "ed2kNodesRefresh");
    }

    #[test]
    fn ed2k_nodes_integer_count_clamps_negative_to_zero() {
        assert_eq!(non_negative(-1), 0);
        assert_eq!(non_negative(0), 0);
        assert_eq!(non_negative(1_234), 1_234);
    }

    #[test]
    fn comma_list_is_edited_one_per_line() {
        let stored = "176.123.5.89:4725,45.82.80.155:5687, 85.121.5.137:4232";
        assert_eq!(
            ListFormat::Comma.to_editor(stored),
            "176.123.5.89:4725\n45.82.80.155:5687\n85.121.5.137:4232"
        );
    }

    #[test]
    fn comma_list_reads_legacy_line_separated_value() {
        let legacy = "1.2.3.4:4661\r\n5.6.7.8:80\n";
        assert_eq!(
            ListFormat::Comma.to_editor(legacy),
            "1.2.3.4:4661\n5.6.7.8:80"
        );
        assert_eq!(ListFormat::Comma.count(legacy), 2);
    }

    #[test]
    fn comma_list_saves_trimmed_deduped_csv() {
        let text = " 1.2.3.4:4661 \n\nExample.org:4242\n1.2.3.4:4661\nexample.org:4242\n";
        assert_eq!(
            ListFormat::Comma.to_stored(text),
            "1.2.3.4:4661,Example.org:4242"
        );
    }

    #[test]
    fn trailing_newline_does_not_change_stored_value() {
        for format in [ListFormat::Lines, ListFormat::Comma] {
            assert_eq!(format.to_stored("a:1\n"), format.to_stored("a:1"));
        }
    }

    #[test]
    fn cache_counts_follow_storage_separator() {
        assert_eq!(ListFormat::Comma.count("1.1.1.1:1,2.2.2.2:2,3.3.3.3:3"), 3);
        assert_eq!(ListFormat::Lines.count("udp://a:1\n\nudp://b:2\n"), 2);
        assert_eq!(ListFormat::Comma.count(""), 0);
    }

    #[test]
    fn lines_format_keeps_inner_text_verbatim() {
        let text = "# comment\nhttps://a.example/list.txt\n";
        assert_eq!(
            ListFormat::Lines.to_stored(text),
            "# comment\nhttps://a.example/list.txt"
        );
        assert_eq!(ListFormat::Lines.to_editor(text), text);
    }
}
