//! 速率限制控件：数字 + 单位下拉（KB/s / MB/s / GB/s），落库为字节/秒；0 = 不限。
//!
//! 单位按 1024 进制，与状态栏限速预设（`mb_to_bytes`）一致。用户选的单位存于
//! 会话级 transient；外部（状态栏、其他客户端）改值后若所选单位无法精确表示新值，
//! 自动回退到能精确表示的最大单位，避免把非零限速显示成 `0`。

use std::rc::Rc;

use fluxdown_ui_theme::active_theme;
use gpui::{App, IntoElement as _, ParentElement as _, SharedString, Styled as _, Window, div};
use gpui_component::h_flex;

use super::SectionContext;
use crate::ui::{
    Control, Getter, NUMBER_WIDTH, Setter, UNIT_DROPDOWN_WIDTH, dropdown_button, render_number,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RateUnit {
    Kb,
    Mb,
    Gb,
}

impl RateUnit {
    /// 从小到大。
    const ALL: [Self; 3] = [Self::Kb, Self::Mb, Self::Gb];

    const fn factor(self) -> i64 {
        match self {
            Self::Kb => 1 << 10,
            Self::Mb => 1 << 20,
            Self::Gb => 1 << 30,
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Kb => "kb",
            Self::Mb => "mb",
            Self::Gb => "gb",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Kb => "KB/s",
            Self::Mb => "MB/s",
            Self::Gb => "GB/s",
        }
    }

    const fn step(self) -> f64 {
        match self {
            Self::Kb => 64.0,
            Self::Mb | Self::Gb => 1.0,
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|unit| unit.id() == id)
    }
}

/// 显示精度：两位小数。
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 字节/秒 → 该单位下的显示值（保留两位小数）。
fn to_display(bytes: i64, unit: RateUnit) -> f64 {
    round2(bytes as f64 / unit.factor() as f64)
}

/// 显示值 → 字节/秒：先截到显示精度，保证结果在该单位下总能被 [`exact`] 还原；
/// 负数归零，溢出饱和。
fn to_bytes(value: f64, unit: RateUnit) -> i64 {
    (round2(value) * unit.factor() as f64).round().max(0.0) as i64
}

/// 该单位的两位小数显示能否无损还原为 `bytes`。
fn exact(bytes: i64, unit: RateUnit) -> bool {
    to_bytes(to_display(bytes, unit), unit) == bytes
}

/// 能精确表示 `bytes` 的最大单位；0（不限）默认 MB/s。
fn best_unit(bytes: i64) -> RateUnit {
    if bytes <= 0 {
        return RateUnit::Mb;
    }
    RateUnit::ALL
        .into_iter()
        .rev()
        .find(|unit| bytes >= unit.factor() && exact(bytes, *unit))
        .unwrap_or(RateUnit::Kb)
}

/// 用户选的单位能精确表示当前值时沿用，否则回退 [`best_unit`]。
fn effective_unit(bytes: i64, chosen: Option<RateUnit>) -> RateUnit {
    match chosen {
        Some(unit) if bytes <= 0 || exact(bytes, unit) => unit,
        _ => best_unit(bytes),
    }
}

/// 切换单位的新字节值：保留当前显示的数字，换算到新单位。
fn switch_target(bytes: i64, from: RateUnit, to: RateUnit) -> i64 {
    to_bytes(to_display(bytes, from), to)
}

/// `key`：字节/秒 daemon 键；`unit_key`：会话内记住所选单位的 transient 键。
pub(crate) fn control(ctx: &SectionContext, key: &'static str, unit_key: &'static str) -> Control {
    let store = ctx.store();
    let options: Vec<(SharedString, SharedString)> = RateUnit::ALL
        .into_iter()
        .map(|unit| {
            (
                SharedString::from(unit.id()),
                SharedString::from(unit.label()),
            )
        })
        .collect();
    Control::field(
        NUMBER_WIDTH,
        move |disabled, row_key, width, window: &mut Window, cx: &mut App| {
            let gap = active_theme(cx).tokens().spacing.sm;
            let bytes = store.read(cx).daemon_i64(key);
            let chosen = store
                .read(cx)
                .transient(unit_key)
                .and_then(serde_json::Value::as_str)
                .and_then(RateUnit::from_id);
            let unit = effective_unit(bytes, chosen);

            let get_store = store.clone();
            let set_store = store.clone();
            let get: Getter<f64> =
                Rc::new(move |cx: &App| to_display(get_store.read(cx).daemon_i64(key), unit));
            let set: Setter<f64> = Rc::new(move |value: f64, cx: &mut App| {
                set_store.update(cx, |store, cx| {
                    store.set_daemon_i64(key, to_bytes(value, unit), cx);
                    // 超出两位小数的输入落库时被截断；字节值可能没变（set_daemon 不通知），
                    // 强制重绘让输入框回显实际生效的值。
                    if (round2(value) - value).abs() >= f64::EPSILON {
                        cx.notify();
                    }
                });
            });
            // 数字槽按单位分键：步长与换算随单位切换重建。
            let number = render_number(
                0.0,
                (i64::MAX / unit.factor()) as f64,
                unit.step(),
                &get,
                &set,
                disabled,
                &SharedString::from(format!("{row_key}-{key}-{}", unit.id())),
                width,
                window,
                cx,
            );

            let unit_store = store.clone();
            let dropdown = dropdown_button(
                format!("{row_key}-unit"),
                &options,
                SharedString::from(unit.id()),
                disabled,
                None,
                Rc::new(move |value: SharedString, cx: &mut App| {
                    let Some(next) = RateUnit::from_id(&value) else {
                        return;
                    };
                    if next == unit {
                        return;
                    }
                    unit_store.update(cx, |store, cx| {
                        // 保留输入框里的数字，只换单位：「10」+ 选 MB/s = 10 MB/s。
                        let target = switch_target(store.daemon_i64(key), unit, next);
                        store.set_daemon_i64(key, target, cx);
                        // 写入被拒（断连等）时不切单位，避免显示与实际限速不符。
                        if store.daemon_i64(key) == target {
                            store.set_transient(unit_key, serde_json::json!(next.id()), cx);
                        }
                    });
                }),
                cx,
            );

            h_flex()
                .gap(gap)
                .items_center()
                .child(number)
                .child(
                    div()
                        .w(active_theme(cx).text_extent(UNIT_DROPDOWN_WIDTH))
                        .child(dropdown),
                )
                .into_any_element()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: i64 = 1 << 20;

    #[test]
    fn unlimited_keeps_chosen_unit_and_defaults_to_mb() {
        assert_eq!(effective_unit(0, None), RateUnit::Mb);
        assert_eq!(effective_unit(0, Some(RateUnit::Gb)), RateUnit::Gb);
        assert!(to_display(0, RateUnit::Gb).abs() < f64::EPSILON);
    }

    #[test]
    fn best_unit_picks_largest_exact_unit() {
        assert_eq!(best_unit(5 << 30), RateUnit::Gb);
        assert_eq!(best_unit(3 * MB / 2), RateUnit::Mb);
        assert_eq!(best_unit(512 * 1024), RateUnit::Kb);
        assert_eq!(best_unit(1000), RateUnit::Kb);
    }

    #[test]
    fn chosen_unit_that_would_hide_a_limit_falls_back() {
        // 512 KB/s 用 GB/s 两位小数会显示成 0（= 不限），必须回退。
        assert_eq!(effective_unit(512 * 1024, Some(RateUnit::Gb)), RateUnit::Kb);
        assert_eq!(effective_unit(3 * MB / 2, Some(RateUnit::Kb)), RateUnit::Kb);
    }

    #[test]
    fn typed_decimals_round_trip_through_bytes() {
        let bytes = to_bytes(0.3, RateUnit::Mb);
        assert!((to_display(bytes, RateUnit::Mb) - 0.3).abs() < f64::EPSILON);
        assert_eq!(to_bytes(1.5, RateUnit::Gb), 3 << 29);
    }

    #[test]
    fn typing_never_makes_the_unit_jump() {
        // 超精度输入与步进产生的浮点噪声都必须落在所选单位可精确表示的值上，
        // 否则下一帧单位会自己跳走、输入框被重建。
        let typed = [
            0.01,
            0.1,
            0.123,
            0.30000000000000004,
            1.005,
            2.3,
            999.99,
            123_456.789,
        ];
        for unit in RateUnit::ALL {
            for value in typed {
                let bytes = to_bytes(value, unit);
                assert_eq!(effective_unit(bytes, Some(unit)), unit, "{value} {unit:?}");
                assert!((to_display(bytes, unit) - round2(value)).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn switching_unit_keeps_the_shown_number() {
        assert_eq!(
            switch_target(10 * 1024, RateUnit::Kb, RateUnit::Mb),
            10 * MB
        );
        assert_eq!(
            switch_target(3 * MB / 2, RateUnit::Mb, RateUnit::Gb),
            3 << 29
        );
        assert_eq!(switch_target(0, RateUnit::Mb, RateUnit::Kb), 0);
        let bytes = switch_target(to_bytes(0.3, RateUnit::Mb), RateUnit::Mb, RateUnit::Kb);
        assert!((to_display(bytes, RateUnit::Kb) - 0.3).abs() < f64::EPSILON);
        assert_eq!(effective_unit(bytes, Some(RateUnit::Kb)), RateUnit::Kb);
    }

    #[test]
    fn negative_input_clamps_to_unlimited() {
        assert_eq!(to_bytes(-4.0, RateUnit::Kb), 0);
    }
}
