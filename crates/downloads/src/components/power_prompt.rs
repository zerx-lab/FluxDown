//! Two-input low-speed power plan; arming remains an explicit user action.

use std::rc::Rc;

use fluxdown_protocol::PowerLowSpeedCondition;
use fluxdown_ui_components::{
    ControlExt as _, DialogIntent, dialog_footer, dialog_title, field_hint, form, form_field,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{App, AppContext as _, ParentElement as _, Styled as _, Window};
use gpui_component::{
    WindowExt as _,
    input::{Input, InputState},
    notification::Notification,
};

pub(super) type LowSpeedConfirm = Rc<dyn Fn(PowerLowSpeedCondition, &mut App)>;

pub(super) fn parse_condition(kbps: &str, minutes: &str) -> Option<PowerLowSpeedCondition> {
    let kbps = kbps
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|n| (1..=1_048_576).contains(n))?;
    let minutes = minutes
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|n| (1..=1_440).contains(n))?;
    Some(PowerLowSpeedCondition {
        threshold_bps: kbps * 1024,
        duration_secs: minutes * 60,
    })
}

pub(super) fn open(
    window: &mut Window,
    cx: &mut App,
    translator: Translator,
    initial: PowerLowSpeedCondition,
    confirm: LowSpeedConfirm,
) {
    let kbps = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value((initial.threshold_bps / 1024).to_string())
            .validate(|text, _| text.chars().all(|c| c.is_ascii_digit()))
    });
    let minutes = cx.new(|cx| {
        InputState::new(window, cx)
            .default_value((initial.duration_secs / 60).to_string())
            .validate(|text, _| text.chars().all(|c| c.is_ascii_digit()))
    });
    let focus = kbps.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let content_kbps = kbps.clone();
        let content_minutes = minutes.clone();
        let ok_kbps = kbps.clone();
        let ok_minutes = minutes.clone();
        let content_t = translator.clone();
        let invalid = translator.text("powerLowSpeedInvalid").to_owned();
        let confirm = confirm.clone();
        dialog
            .title(dialog_title(
                translator.text("powerLowSpeedTitle").to_owned(),
                cx,
            ))
            .w(active_theme(cx).text_extent(520.))
            .content(move |content, _, cx| {
                content.child(
                    form(cx)
                        .child(form_field(
                            content_t.text("powerLowSpeedThreshold").to_owned(),
                            Input::new(&content_kbps)
                                .control(cx)
                                .suffix(field_hint(
                                    content_t.text("statusSpeedLimitKbs").to_owned(),
                                    cx,
                                ))
                                .w_full(),
                            None,
                            cx,
                        ))
                        .child(form_field(
                            content_t.text("powerLowSpeedDuration").to_owned(),
                            Input::new(&content_minutes)
                                .control(cx)
                                .suffix(field_hint(
                                    content_t.text("shutdownMinutesUnit").to_owned(),
                                    cx,
                                ))
                                .w_full(),
                            Some(content_t.text("powerLowSpeedWarning").to_owned().into()),
                            cx,
                        )),
                )
            })
            .footer(dialog_footer(
                Some(translator.text("cancel").to_owned().into()),
                translator.text("confirm").to_owned(),
                DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let Some(condition) = parse_condition(
                    ok_kbps.read(cx).value().as_ref(),
                    ok_minutes.read(cx).value().as_ref(),
                ) else {
                    window.push_notification(Notification::error(invalid.clone()), cx);
                    return false;
                };
                confirm(condition, cx);
                true
            })
    });
    focus.update(cx, |input, cx| input.focus(window, cx));
}

#[cfg(test)]
mod tests {
    use super::parse_condition;

    #[test]
    fn low_speed_inputs_reject_unsafe_values_and_convert_units() {
        let value = parse_condition(" 5 ", "5").expect("valid plan");
        assert_eq!(value.threshold_bps, 5120);
        assert_eq!(value.duration_secs, 300);
        for (speed, duration) in [
            ("", "5"),
            ("-1", "5"),
            ("0", "5"),
            ("1.5", "5"),
            ("5", "0"),
            ("1048577", "5"),
            ("5", "1441"),
            ("18446744073709551616", "5"),
            ("5", "bad"),
        ] {
            assert_eq!(parse_condition(speed, duration), None);
        }
        let maximum = parse_condition("1048576", "1440").expect("bounded maximum");
        assert_eq!(maximum.threshold_bps, 1024 * 1024 * 1024);
        assert_eq!(maximum.duration_secs, 86400);
    }
}
