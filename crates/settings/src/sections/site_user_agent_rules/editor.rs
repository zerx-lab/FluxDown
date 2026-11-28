//! Site rule form and User-Agent preset picker.

use std::rc::Rc;

use fluxdown_protocol::user_agent_presets;
use fluxdown_ui_components::{
    BusyHold, ButtonVariant, ControlExt as _, button, dialog_scroll_body, dialog_title,
    field_error, form, form_field, loading_button, option_row,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::Button,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};

use super::super::user_agent::UA_PRESETS;
use super::{Change, INVALID, Rule, save_change};
use crate::{store::SettingsStore, ui::dialog_footer};

struct Editor {
    store: Entity<SettingsStore>,
    translator: Translator,
    base: Rc<Vec<Rule>>,
    index: Option<usize>,
    domain: Entity<InputState>,
    user_agent: Entity<InputState>,
    include_subdomains: bool,
    saving: bool,
    hold: BusyHold,
    error: Option<&'static str>,
}

pub(super) fn open(
    store: Entity<SettingsStore>,
    translator: Translator,
    base: Rc<Vec<Rule>>,
    index: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(translator.text(if index.is_some() {
        "siteUserAgentEdit"
    } else {
        "siteUserAgentAdd"
    }));
    let view = cx.new(|cx| {
        let existing = index.and_then(|index| base.get(index));
        let include_subdomains = existing.is_some_and(|rule| rule.include_subdomains);
        let domain = cx.new(|cx| {
            let initial = existing.map_or_else(String::new, |rule| rule.domain.clone());
            InputState::new(window, cx).default_value(initial)
        });
        let user_agent = cx.new(|cx| {
            let initial = existing.map_or_else(String::new, |rule| rule.user_agent.clone());
            InputState::new(window, cx).default_value(initial)
        });
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&user_agent, |_: &mut Editor, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        Editor {
            store,
            translator,
            include_subdomains,
            base,
            index,
            domain,
            user_agent,
            saving: false,
            hold: BusyHold::default(),
            error: None,
        }
    });
    let focus = view.read(cx).domain.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(520.))
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    focus.update(cx, |input, cx| input.focus(window, cx));
}

impl Editor {
    fn t(&self, key: &str) -> SharedString {
        self.translator.text(key).into()
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !self.store.read(cx).daemon_connected() {
            return;
        }
        let domain = self.domain.read(cx).value();
        let domain = domain.trim();
        let ua = self.user_agent.read(cx).value();
        let ua = ua.trim_matches([' ', '\t']);
        if domain.is_empty() || ua.is_empty() || ua.contains(['\r', '\n']) {
            self.error = Some(INVALID);
            cx.notify();
            return;
        }
        let rule = Rule {
            domain: domain.to_owned(),
            user_agent: ua.to_owned(),
            include_subdomains: self.include_subdomains,
        };
        let change = match self.index {
            Some(index) => Change::Edit(index, rule),
            None => Change::Add(rule),
        };
        self.saving = true;
        self.error = None;
        self.hold.start(cx);
        cx.notify();
        save_change(
            self.store.clone(),
            self.base.clone(),
            change,
            window,
            cx,
            |this, result, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => window.close_dialog(cx),
                    Err(key) => {
                        this.error = Some(key);
                        cx.notify();
                    }
                }
            },
        );
    }

    fn presets(&self, cx: &App) -> impl IntoElement {
        let mut items: Vec<(SharedString, String)> = UA_PRESETS
            .iter()
            .filter_map(|(key, value)| {
                let label = match *key {
                    "chrome" => "userAgentPresetChrome",
                    "firefox" => "userAgentPresetFirefox",
                    "edge" => "userAgentPresetEdge",
                    "safari" => "userAgentPresetSafari",
                    _ => return None,
                };
                Some((self.t(label), (*value).to_owned()))
            })
            .collect();
        items.extend(
            user_agent_presets::from_preference(
                self.store.read(cx).pref(user_agent_presets::PREF_KEY),
            )
            .into_iter()
            .map(|preset| (preset.name.into(), preset.value)),
        );
        let current = self.user_agent.read(cx).value();
        let label = items
            .iter()
            .find(|(_, value)| value == current.trim_matches([' ', '\t']))
            .map_or_else(
                || self.t("userAgentPresetCustom"),
                |(label, _)| label.clone(),
            );
        let input = self.user_agent.clone();
        Button::new("site-ua-preset")
            .outline()
            .control(cx)
            .w_full()
            .min_w_0()
            .truncate()
            .label(label)
            .dropdown_caret(true)
            .disabled(self.saving)
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, window, cx| {
                let theme = active_theme(cx);
                let height = (theme.density().nav_row * 8.).min(window.viewport_size().height / 2.);
                let menu = menu
                    .max_w(theme.text_extent(360.))
                    .max_h(height)
                    .scrollable(true);
                items.iter().fold(menu, |menu, (label, value)| {
                    let input = input.clone();
                    let selected = value == current.trim_matches([' ', '\t']);
                    let value = value.clone();
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(selected)
                            .on_click(move |_, window, cx| {
                                input.update(cx, |input, cx| {
                                    input.set_value(value.clone(), window, cx);
                                });
                            }),
                    )
                })
            })
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let spacing = active_theme(cx).tokens().spacing;
        let subdomains = Switch::new("site-ua-subdomains")
            .checked(self.include_subdomains)
            .disabled(self.saving)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.include_subdomains = *checked;
                cx.notify();
            }));
        let body = form(cx)
            .child(form_field(
                self.t("siteUserAgentDomain"),
                Input::new(&self.domain)
                    .control(cx)
                    .disabled(self.saving)
                    .w_full(),
                None,
                cx,
            ))
            .child(option_row(
                self.t("siteUserAgentSubdomains"),
                None,
                subdomains,
                cx,
            ))
            .child(form_field(
                self.t("userAgent"),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(spacing.xs)
                    .child(self.presets(cx))
                    .child(
                        Input::new(&self.user_agent)
                            .control(cx)
                            .disabled(self.saving)
                            .w_full(),
                    ),
                None,
                cx,
            ));
        let loading = self.saving || self.hold.is_held();
        v_flex()
            .w_full()
            .min_h_0()
            .gap(spacing.md)
            .child(dialog_scroll_body("site-ua-editor-body", None, body, cx))
            .when_some(self.error, |this, key| {
                this.child(field_error(self.t(key), cx))
            })
            .child(dialog_footer(
                None,
                [
                    button(
                        "site-ua-cancel",
                        self.t("cancel"),
                        ButtonVariant::Secondary,
                        cx,
                    )
                    .disabled(self.saving)
                    .on_click(|_, window, cx| window.close_dialog(cx))
                    .into_any_element(),
                    loading_button(
                        "site-ua-save",
                        self.t("proxyRuleSave"),
                        ButtonVariant::Primary,
                        loading,
                        cx,
                    )
                    .disabled(loading || !self.store.read(cx).daemon_connected())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)))
                    .into_any_element(),
                ],
                cx,
            ))
    }
}
