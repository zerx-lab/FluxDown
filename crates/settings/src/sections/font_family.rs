use fluxdown_ui_theme::{FONT_FAMILY_KEY, active_theme, available_font_families};
use gpui::{
    App, AppContext as _, Entity, IntoElement as _, SharedString, Styled as _, Subscription,
};
use gpui_component::{
    Sizable as _, Size,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
};

use super::SectionContext;
use crate::ui::{Control, INPUT_WIDTH};

#[derive(Clone)]
struct FontOption {
    value: SharedString,
    label: SharedString,
}

impl SelectItem for FontOption {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &SharedString {
        &self.value
    }
}

type FontSelect = SelectState<SearchableVec<FontOption>>;
struct FontSlot {
    select: Entity<FontSelect>,
    label: SharedString,
    last_synced: Option<SharedString>,
    _subscription: Subscription,
}

fn options(default_label: SharedString, cx: &App) -> SearchableVec<FontOption> {
    let mut items = vec![FontOption {
        value: "".into(),
        label: default_label,
    }];
    items.extend(
        available_font_families(cx)
            .into_iter()
            .map(|name| FontOption {
                value: name.clone().into(),
                label: name.into(),
            }),
    );
    SearchableVec::new(items)
}

pub(super) fn field(ctx: &SectionContext) -> Control {
    let store = ctx.store();
    let default_label = ctx.t("fontFamilyDefault");
    Control::field(INPUT_WIDTH, move |disabled, key, width, window, cx| {
        let slot = window.use_keyed_state(SharedString::from(format!("{key}-font-family")), cx, {
            let store = store.clone();
            let label = default_label.clone();
            move |window, cx| {
                let select = cx.new(|cx| {
                    FontSelect::new(options(label.clone(), cx), None, window, cx).searchable(true)
                });
                let subscription = cx.subscribe(
                    &select,
                    move |_: &mut FontSlot,
                          _,
                          event: &SelectEvent<SearchableVec<FontOption>>,
                          cx| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            store.update(cx, |store, cx| {
                                store.set_pref_str(FONT_FAMILY_KEY, value.to_string(), cx)
                            });
                        }
                    },
                );
                FontSlot {
                    select,
                    label,
                    last_synced: None,
                    _subscription: subscription,
                }
            }
        });
        let selected: SharedString = store.read(cx).pref_str(FONT_FAMILY_KEY, "").into();
        slot.update(cx, |slot, cx| {
            if slot.label != default_label {
                slot.label = default_label.clone();
                slot.last_synced = None;
                slot.select.update(cx, |select, cx| {
                    select.set_items(options(default_label.clone(), cx), window, cx)
                });
            }
            if slot.last_synced.as_ref() != Some(&selected) {
                slot.last_synced = Some(selected.clone());
                slot.select.update(cx, |select, cx| {
                    select.set_selected_value(&selected, window, cx)
                });
            }
        });
        let select = slot.read(cx).select.clone();
        // A missing saved family stays visible as the preference, but the runtime uses the theme.
        let placeholder = if selected.is_empty() {
            default_label.clone()
        } else {
            selected
        };
        // Large uses 1rem text for the trigger, search input and list; pixel sizes
        // also scale their text, so keep the control height separate.
        Select::new(&select)
            .placeholder(placeholder)
            .w(width.unwrap_or_else(|| active_theme(cx).text_extent(INPUT_WIDTH)))
            .with_size(Size::Large)
            .h(active_theme(cx).density().control)
            .py_0()
            .disabled(disabled)
            .into_any_element()
    })
}
