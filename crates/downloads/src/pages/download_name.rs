use std::borrow::Cow;

use gpui::{App, Entity, SharedString, Window};
use gpui_component::input::InputState;

#[derive(Default)]
pub(super) struct DownloadName {
    detected: SharedString,
    mode: NameMode,
    fixed_name: bool,
}

#[derive(Default)]
enum NameMode {
    #[default]
    Auto,
    // Auto displays the detected name but retains the exact text to restore in manual mode.
    AutoWithDraft {
        text: SharedString,
        prefilled: bool,
    },
    Manual {
        prefilled: bool,
    },
}

impl DownloadName {
    pub(super) fn with_fixed_name(detected: SharedString) -> Self {
        Self {
            detected,
            mode: NameMode::Auto,
            fixed_name: true,
        }
    }

    pub(super) fn is_auto(&self) -> bool {
        !matches!(self.mode, NameMode::Manual { .. })
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
            let text = input.read(cx).text();
            let matches_detected = text == self.detected.as_str();
            let prefilled = matches!(self.mode, NameMode::Manual { prefilled: true });
            if matches_detected && (self.detected.is_empty() || prefilled) {
                self.mode = NameMode::Auto;
            } else {
                let text: Cow<'_, str> = text.into();
                self.mode = NameMode::AutoWithDraft {
                    text: SharedString::from(text.as_ref()),
                    prefilled: prefilled && text.trim() == self.detected.trim(),
                };
                if !matches_detected {
                    set_text(input, self.detected.clone(), window, cx);
                }
            }
        } else {
            let previous = std::mem::replace(
                &mut self.mode,
                NameMode::Manual {
                    prefilled: !self.detected.is_empty(),
                },
            );
            if let NameMode::AutoWithDraft { text, prefilled } = previous {
                self.mode = NameMode::Manual {
                    prefilled: prefilled && text.trim() == self.detected.trim(),
                };
                if input.read(cx).text() != text.as_str() {
                    set_text(input, text, window, cx);
                }
            }
        }
    }

    pub(super) fn set_detected(
        &mut self,
        detected: &str,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text = input.read(cx).text();
        let write = match &mut self.mode {
            NameMode::Manual { prefilled } => {
                // Matching a new detection does not make an existing draft a prefill.
                let untouched = *prefilled && text == self.detected.as_str();
                let write = text != detected
                    && (untouched
                        || (!detected.is_empty() && Cow::<'_, str>::from(text).trim().is_empty()));
                *prefilled = !detected.is_empty() && (write || untouched);
                write
            }
            _ => text != detected,
        };
        if detected != self.detected.as_str() {
            self.detected = SharedString::from(detected);
        }
        if write {
            set_text(input, self.detected.clone(), window, cx);
        }
    }

    pub(super) fn override_name(&self, input: &InputState) -> Option<String> {
        let NameMode::Manual { prefilled } = self.mode else {
            return None;
        };
        // For a fixed name, restoring the original text always means no rename.
        let omit_detected = self.fixed_name || prefilled;
        if omit_detected && input.text() == self.detected.as_str() {
            return None;
        }
        let value: Cow<'_, str> = input.text().into();
        let trimmed = value.trim();
        if trimmed.is_empty() || (omit_detected && trimmed == self.detected.trim()) {
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
