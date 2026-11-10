//! 自动数量以 0 落库；自定义数量与会话内记忆独立，渲染只读取主机实际值。

use std::rc::Rc;

use fluxdown_protocol::{DaemonConfigKind, daemon_config_field};
use fluxdown_ui_theme::active_theme;
use gpui::{App, ParentElement as _, SharedString, Styled as _, Window};
use gpui_component::h_flex;

use super::SectionContext;
use crate::ui::{
    Control, DROPDOWN_MIN_WIDTH, Getter, NUMBER_WIDTH, Setter, dropdown_button, render_number,
};

fn custom_target(current: i64, remembered: Option<i64>, default: i64, max: i64) -> i64 {
    if current > 0 {
        current
    } else {
        remembered.filter(|value| *value > 0).unwrap_or(default)
    }
    .clamp(1, max)
}

pub(super) fn control(
    ctx: &SectionContext,
    key: &'static str,
    memory_key: &'static str,
    default: i64,
) -> Control {
    let store = ctx.store();
    let Some(DaemonConfigKind::Integer { max, .. }) =
        daemon_config_field(key).map(|field| field.kind)
    else {
        unreachable!("automatic quantity requires an integer configuration field: {key}");
    };
    let options = vec![
        (SharedString::from("auto"), ctx.t("auto")),
        (SharedString::from("custom"), ctx.t("customThreads")),
    ];
    Control::field(
        DROPDOWN_MIN_WIDTH,
        move |disabled, row_key, width, window: &mut Window, cx: &mut App| {
            let current = store.read(cx).daemon_i64(key);
            let mode_store = store.clone();
            let dropdown = dropdown_button(
                format!("{row_key}-mode"),
                &options,
                SharedString::from(if current == 0 { "auto" } else { "custom" }),
                disabled,
                width,
                Rc::new(move |mode: SharedString, cx: &mut App| {
                    if disabled {
                        return;
                    }
                    mode_store.update(cx, |store, cx| {
                        // 菜单展开期间主机可能更新配置，始终以点击时的实际值为准。
                        let current = store.daemon_i64(key);
                        let target = match mode.as_ref() {
                            "auto" if current != 0 => 0,
                            "custom" if current == 0 => custom_target(
                                current,
                                store
                                    .transient(memory_key)
                                    .and_then(serde_json::Value::as_i64),
                                default,
                                max,
                            ),
                            _ => return,
                        };
                        store.set_daemon_i64(key, target, cx);
                        // 断连等情况下写入被拒，不能改变会话记忆。
                        if store.daemon_i64(key) == target && current > 0 {
                            store.set_transient(memory_key, serde_json::json!(current), cx);
                        }
                    });
                }),
                cx,
            );
            let mut controls = h_flex()
                .gap(active_theme(cx).tokens().spacing.sm)
                .items_center()
                .child(dropdown);
            if current > 0 {
                let get_store = store.clone();
                let set_store = store.clone();
                let get: Getter<f64> =
                    Rc::new(move |cx: &App| get_store.read(cx).daemon_i64(key) as f64);
                let set: Setter<f64> = Rc::new(move |value: f64, cx: &mut App| {
                    if disabled {
                        return;
                    }
                    set_store.update(cx, |store, cx| {
                        // 隐藏前排队的输入事件不能将主机刚切回的自动模式改为自定义。
                        if store.daemon_i64(key) == 0 {
                            return;
                        }
                        let target = (value.round() as i64).clamp(1, max);
                        store.set_daemon_i64(key, target, cx);
                        if store.daemon_i64(key) == target {
                            store.set_transient(memory_key, serde_json::json!(target), cx);
                        }
                    });
                });
                controls = controls.child(render_number(
                    1.0,
                    max as f64,
                    1.0,
                    &get,
                    &set,
                    disabled,
                    &SharedString::from(format!("{row_key}-{key}-custom")),
                    Some(active_theme(cx).text_extent(NUMBER_WIDTH)),
                    window,
                    cx,
                ));
            }
            controls
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{DaemonConfigKind, custom_target, daemon_config_field};

    #[test]
    fn custom_values_take_precedence_over_session_memory() {
        assert_eq!(custom_target(64, Some(16), 16, 512), 64);
        assert_eq!(custom_target(6, Some(2), 2, 8), 6);
    }

    #[test]
    fn automatic_mode_restores_previous_custom_value() {
        assert_eq!(custom_target(0, Some(64), 16, 512), 64);
        assert_eq!(custom_target(0, Some(6), 2, 8), 6);
        assert_eq!(custom_target(0, None, 16, 512), 16);
        assert_eq!(custom_target(0, None, 2, 8), 2);
    }

    #[test]
    fn remembered_counts_cannot_restore_zero_or_exceed_catalog_limits() {
        for (key, default, expected_max) in [("default_segments", 16, 512), ("cdn_max_nodes", 2, 8)]
        {
            let Some(DaemonConfigKind::Integer { max, .. }) =
                daemon_config_field(key).map(|field| field.kind)
            else {
                panic!("expected integer quantity: {key}");
            };
            assert_eq!(max, expected_max);
            assert_eq!(custom_target(0, Some(0), default, max), default);
            assert_eq!(custom_target(0, Some(-1), default, max), default);
            assert_eq!(custom_target(0, Some(max + 1), default, max), max);
            assert_eq!(custom_target(0, Some(1), default, max), 1);
        }
    }
}
