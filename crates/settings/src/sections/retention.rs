//! 已完成记录保留天数：预设下拉（从不 / 1 / 7 / 30 天）+ 「自定义」数字输入（天）。
//!
//! 落库值是天数，0 = 从不。「自定义」是纯展示模式：值恰好等于某个预设时，
//! 仍用会话内标记保持在自定义态，避免输入 `3` → `30` 的中途下拉与输入框突然切换。

use std::rc::Rc;

use fluxdown_protocol::{DaemonConfigKind, daemon_config_field};
use fluxdown_ui_theme::active_theme;
use gpui::{App, ParentElement as _, SharedString, Styled as _, Window};
use gpui_component::h_flex;

use super::SectionContext;
use crate::ui::{
    Control, DROPDOWN_MIN_WIDTH, Getter, NUMBER_WIDTH, Setter, dropdown_button, meta_text,
    render_number,
};

const KEY: &str = "completed_record_retention_days";
/// 会话内「处于自定义模式」标记（transient，不落库）。
const CUSTOM_MODE_KEY: &str = "completed_record_retention_custom";
/// 预设天数（0 = 从不），与下拉项一一对应。
const PRESETS: [i64; 4] = [0, 1, 7, 30];
/// 切到「自定义」且没有可复用的自定义值时的起始天数（刻意不等于任何预设）。
const CUSTOM_START_DAYS: i64 = 14;
const CUSTOM_ID: &str = "custom";

/// 下拉当前项：预设天数的字符串，或 [`CUSTOM_ID`]。
fn mode_id(days: i64, custom_flag: bool) -> String {
    if (custom_flag && days > 0) || !PRESETS.contains(&days) {
        CUSTOM_ID.to_owned()
    } else {
        days.to_string()
    }
}

/// 切到「自定义」时写入的天数：当前值已是非预设值则沿用，否则用起始值。
fn custom_target(current: i64, max: i64) -> i64 {
    if current > 0 && !PRESETS.contains(&current) {
        current
    } else {
        CUSTOM_START_DAYS
    }
    .clamp(1, max)
}

pub(super) fn control(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let Some(DaemonConfigKind::Integer { max, .. }) =
        daemon_config_field(KEY).map(|field| field.kind)
    else {
        unreachable!("completed record retention requires an integer configuration field");
    };
    let options = vec![
        (SharedString::from("0"), ctx.t("retentionNever")),
        (SharedString::from("1"), ctx.t("retentionDay1")),
        (SharedString::from("7"), ctx.t("retentionDay7")),
        (SharedString::from("30"), ctx.t("retentionDay30")),
        (SharedString::from(CUSTOM_ID), ctx.t("retentionCustom")),
    ];
    let unit = ctx.t("retentionDaysUnit");
    Control::field(
        DROPDOWN_MIN_WIDTH,
        move |disabled, row_key, width, window: &mut Window, cx: &mut App| {
            let current = store.read(cx).daemon_i64(KEY);
            let custom_flag = store
                .read(cx)
                .transient(CUSTOM_MODE_KEY)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let mode = mode_id(current, custom_flag);
            let mode_store = store.clone();
            let dropdown = dropdown_button(
                format!("{row_key}-mode"),
                &options,
                SharedString::from(mode.clone()),
                disabled,
                width,
                Rc::new(move |choice: SharedString, cx: &mut App| {
                    if disabled {
                        return;
                    }
                    mode_store.update(cx, |store, cx| {
                        // 菜单展开期间主机可能更新配置，始终以点击时的实际值为准。
                        let current = store.daemon_i64(KEY);
                        if choice.as_ref() == CUSTOM_ID {
                            let target = custom_target(current, max);
                            store.set_daemon_i64(KEY, target, cx);
                            // 写入被拒（断连等）时不进入自定义态，显示须与实际值一致。
                            if store.daemon_i64(KEY) == target {
                                store.set_transient(CUSTOM_MODE_KEY, serde_json::json!(true), cx);
                            }
                        } else if let Ok(days) = choice.parse::<i64>() {
                            store.set_transient(CUSTOM_MODE_KEY, serde_json::json!(false), cx);
                            if days != current {
                                store.set_daemon_i64(KEY, days, cx);
                            }
                        }
                    });
                }),
                cx,
            );
            let mut controls = h_flex()
                .gap(active_theme(cx).tokens().spacing.sm)
                .items_center()
                .child(dropdown);
            if mode == CUSTOM_ID {
                let get_store = store.clone();
                let set_store = store.clone();
                let get: Getter<f64> =
                    Rc::new(move |cx: &App| get_store.read(cx).daemon_i64(KEY) as f64);
                let set: Setter<f64> = Rc::new(move |value: f64, cx: &mut App| {
                    if disabled {
                        return;
                    }
                    set_store.update(cx, |store, cx| {
                        // 自定义态下最少 1 天：0 只能经下拉选「从不」。
                        store.set_daemon_i64(KEY, (value.round() as i64).clamp(1, max), cx);
                    });
                });
                controls = controls
                    .child(render_number(
                        1.0,
                        max as f64,
                        1.0,
                        &get,
                        &set,
                        disabled,
                        &SharedString::from(format!("{row_key}-{KEY}-custom")),
                        Some(active_theme(cx).text_extent(NUMBER_WIDTH)),
                        window,
                        cx,
                    ))
                    .child(meta_text(cx).flex_shrink_0().child(unit.clone()));
            }
            controls
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{CUSTOM_START_DAYS, DaemonConfigKind, daemon_config_field};
    use super::{KEY, PRESETS, custom_target, mode_id};

    /// 与 Web 端 `retention.test.ts` 共用的用例。
    const FIXTURE: &str = include_str!("../../tests/fixtures/retention_cases.json");

    fn fixture() -> serde_json::Value {
        serde_json::from_str(FIXTURE).expect("fixture json")
    }

    fn max_days() -> i64 {
        match daemon_config_field(KEY).map(|field| field.kind) {
            Some(DaemonConfigKind::Integer { max, .. }) => max,
            other => panic!("unexpected retention field kind: {other:?}"),
        }
    }

    #[test]
    fn fixture_constants_match_implementation() {
        let fixture = fixture();
        let presets: Vec<i64> = fixture["presets"]
            .as_array()
            .expect("presets")
            .iter()
            .map(|v| v.as_i64().expect("preset"))
            .collect();
        assert_eq!(presets, PRESETS);
        assert_eq!(fixture["customStart"].as_i64(), Some(CUSTOM_START_DAYS));
        assert!(!PRESETS.contains(&CUSTOM_START_DAYS));
        assert_eq!(max_days(), 3650);
    }

    #[test]
    fn mode_matches_shared_cases() {
        for case in fixture()["mode"].as_array().expect("mode") {
            let days = case["days"].as_i64().expect("days");
            let flag = case["customFlag"].as_bool().expect("flag");
            assert_eq!(
                mode_id(days, flag),
                case["expect"].as_str().expect("expect"),
                "days={days} flag={flag}"
            );
        }
    }

    #[test]
    fn custom_target_matches_shared_cases() {
        for case in fixture()["customTarget"].as_array().expect("cases") {
            let current = case["current"].as_i64().expect("current");
            let max = case["max"].as_i64().expect("max");
            assert_eq!(
                custom_target(current, max),
                case["expect"].as_i64().expect("expect"),
                "current={current}"
            );
        }
    }
}
