//! 全局 User-Agent：预设下拉 + 自定义输入。

use std::rc::Rc;

use fluxdown_ui_components::ControlExt as _;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, Entity, IntoElement as _, ParentElement, SharedString, Styled,
    Subscription, Window, div,
};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    v_flex,
};

use super::SectionContext;
use crate::ui::{
    DROPDOWN_MIN_WIDTH, INPUT_WIDTH, SettingsRow, body_text, dropdown_button, meta_text,
};

pub(crate) const UA_KEY: &str = "global_user_agent";

/// 预设 UA（key → UA 字符串），与下载表单及 Web 端保持同步。
pub(crate) const UA_PRESETS: &[(&str, &str)] = &[
    (
        "chrome",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
    ),
    (
        "firefox",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) Gecko/20100101 Firefox/147.0",
    ),
    (
        "edge",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.3800.70",
    ),
    (
        "safari",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15",
    ),
];

/// UA 字符串 → 预设键；空 = `default`，未命中 = `custom`。
#[must_use]
pub(crate) fn detect_preset(ua: &str) -> &'static str {
    if ua.is_empty() {
        return "default";
    }
    UA_PRESETS
        .iter()
        .find(|(_, value)| *value == ua)
        .map_or("custom", |(key, _)| key)
}

struct CustomSlot {
    input: Entity<InputState>,
    last_synced: SharedString,
    _subscription: Subscription,
}

pub(crate) fn row(ctx: &SectionContext) -> SettingsRow {
    let store = ctx.store();
    let options: Vec<(SharedString, SharedString)> = vec![
        (
            SharedString::from("default"),
            ctx.t("userAgentPresetDefault"),
        ),
        (SharedString::from("chrome"), ctx.t("userAgentPresetChrome")),
        (
            SharedString::from("firefox"),
            ctx.t("userAgentPresetFirefox"),
        ),
        (SharedString::from("edge"), ctx.t("userAgentPresetEdge")),
        (SharedString::from("safari"), ctx.t("userAgentPresetSafari")),
        (SharedString::from("custom"), ctx.t("userAgentPresetCustom")),
    ];
    let placeholder = ctx.t("userAgentPlaceholder");
    let title = ctx.t("userAgent");
    let description = ctx.t("userAgentDesc");
    let render_title = title.clone();
    let render_description = description.clone();
    let mut row = SettingsRow::custom_field(
        Some(DROPDOWN_MIN_WIDTH),
        move |disabled, key, width, window: &mut Window, cx: &mut App| {
            let tokens = active_theme(cx).tokens();
            let current = store.read(cx).daemon_str(UA_KEY);
            let preset = detect_preset(&current);
            let custom_active = store
                .read(cx)
                .transient("ua_custom_mode")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                || preset == "custom";

            let selected_key: SharedString = if custom_active {
                SharedString::from("custom")
            } else {
                SharedString::from(preset)
            };
            let select_store = store.clone();
            let dropdown = dropdown_button(
                format!("{key}-preset-dropdown"),
                &options,
                selected_key,
                disabled,
                None,
                Rc::new(move |value: SharedString, cx: &mut App| {
                    select_store.update(cx, |store, cx| match value.as_ref() {
                        "default" => {
                            store.set_transient("ua_custom_mode", serde_json::json!(false), cx);
                            store.set_daemon(UA_KEY, "", cx);
                        }
                        "custom" => {
                            store.set_transient("ua_custom_mode", serde_json::json!(true), cx);
                        }
                        preset_key => {
                            store.set_transient("ua_custom_mode", serde_json::json!(false), cx);
                            if let Some((_, ua)) = UA_PRESETS.iter().find(|(k, _)| *k == preset_key)
                            {
                                store.set_daemon(UA_KEY, *ua, cx);
                            }
                        }
                    });
                }),
                cx,
            );

            // 以卡片实际可用宽度换行，而非按窗口单双列猜测控件空间。
            let header = div()
                .flex()
                .flex_wrap()
                .items_center()
                .w_full()
                .gap_x(tokens.spacing.lg)
                .gap_y(tokens.spacing.sm)
                .child(
                    v_flex()
                        .flex_grow(1.)
                        .flex_basis(active_theme(cx).text_extent(INPUT_WIDTH))
                        .min_w_0()
                        .gap(tokens.spacing.xxs)
                        .child(body_text(cx).child(render_title.clone()))
                        .child(meta_text(cx).child(render_description.clone())),
                )
                .child(
                    div()
                        .w(width
                            .unwrap_or_else(|| active_theme(cx).text_extent(DROPDOWN_MIN_WIDTH)))
                        .max_w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .child(dropdown),
                );
            let mut column = v_flex()
                .w_full()
                .min_w_0()
                .gap(tokens.spacing.sm)
                .child(header);
            if custom_active {
                let slot =
                    window.use_keyed_state(SharedString::from(format!("{key}-custom")), cx, {
                        let store = store.clone();
                        let current = SharedString::from(current.clone());
                        let placeholder = placeholder.clone();
                        move |window, cx| {
                            let input = cx.new(|cx| {
                                InputState::new(window, cx)
                                    .default_value(current.clone())
                                    .placeholder(placeholder)
                            });
                            let _subscription = cx.subscribe(
                                &input,
                                move |slot: &mut CustomSlot, input, event: &InputEvent, cx| {
                                    if matches!(event, InputEvent::Change | InputEvent::Blur) {
                                        let value = input.read(cx).value();
                                        if value != slot.last_synced {
                                            slot.last_synced = value.clone();
                                            store.update(cx, |store, cx| {
                                                store.set_daemon(UA_KEY, value.to_string(), cx)
                                            });
                                        }
                                    }
                                },
                            );
                            CustomSlot {
                                input,
                                last_synced: current,
                                _subscription,
                            }
                        }
                    });
                let current = SharedString::from(current);
                slot.update(cx, |slot, cx| {
                    // daemon 对 Text 键 trim 归一化；按 trim 比较，避免逐字输入的尾随空格被回写抹掉。
                    if slot.last_synced.trim() != current.trim() {
                        slot.last_synced = current.clone();
                        slot.input
                            .update(cx, |input, cx| input.set_value(current.clone(), window, cx));
                    }
                });
                let input = slot.read(cx).input.clone();
                column = column.child(Input::new(&input).control(cx).w_full().disabled(disabled));
            }
            column.into_any_element()
        },
    );
    // 自渲染不丢失设置搜索所需的标题与说明。
    row.title = title;
    row.description(description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_presets() {
        assert_eq!(detect_preset(""), "default");
        assert_eq!(detect_preset(UA_PRESETS[0].1), "chrome");
        assert_eq!(detect_preset("Custom/1.0"), "custom");
    }
}
