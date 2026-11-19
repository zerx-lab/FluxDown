use std::borrow::Cow;

use gpui::{App, Entity, SharedString, Window};
use gpui_component::input::InputState;

#[derive(Default)]
pub(super) struct DownloadName {
    detected: SharedString,
    mode: NameMode,
}

#[derive(Default)]
enum NameMode {
    #[default]
    Auto,
    // Auto displays the detected name but retains the exact text to restore in manual mode.
    AutoWithDraft(SharedString),
    Manual,
}

impl DownloadName {
    pub(super) fn new(detected: SharedString) -> Self {
        Self {
            detected,
            mode: NameMode::Auto,
        }
    }

    pub(super) fn is_auto(&self) -> bool {
        !matches!(self.mode, NameMode::Manual)
    }

    pub(super) fn detected(&self) -> &SharedString {
        &self.detected
    }

    pub(super) fn set_auto(
        &mut self,
        auto: bool,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if auto == self.is_auto() {
            return;
        }
        if auto {
            let state = input.read(cx);
            if state.text() == self.detected.as_str() {
                self.mode = NameMode::Auto;
            } else {
                let text: Cow<'_, str> = state.text().into();
                self.mode = NameMode::AutoWithDraft(SharedString::from(text.as_ref()));
                set_text(input, self.detected.clone(), window, cx);
            }
        } else if let NameMode::AutoWithDraft(draft) =
            std::mem::replace(&mut self.mode, NameMode::Manual)
            && input.read(cx).text() != draft.as_str()
        {
            set_text(input, draft, window, cx);
        }
    }

    pub(super) fn set_detected(
        &mut self,
        detected: &str,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if detected == self.detected.as_str() {
            return;
        }
        let follow = self.is_auto() || input.read(cx).text() == self.detected.as_str();
        self.detected = SharedString::from(detected);
        if follow && input.read(cx).text() != detected {
            set_text(input, self.detected.clone(), window, cx);
        }
    }

    pub(super) fn override_name(&self, input: &InputState) -> Option<String> {
        if self.is_auto() || input.text() == self.detected.as_str() {
            return None;
        }
        let value: Cow<'_, str> = input.text().into();
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed == self.detected.trim() {
            None
        } else if trimmed.len() == value.len() {
            Some(value.into_owned())
        } else {
            Some(trimmed.to_owned())
        }
    }
}

fn set_text(input: &Entity<InputState>, value: SharedString, window: &mut Window, cx: &mut App) {
    input.update(cx, |input, cx| input.set_value(value, window, cx));
}
