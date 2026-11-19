use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, FluxIcon, IconControlExt as _, field_hint,
};
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, App, ClickEvent, Context, Div, ParentElement, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, Sizable as _, Size,
    button::{Button, ButtonVariants as _, DropdownButton},
    h_flex,
    input::Input,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};

use super::ManifestView;
use crate::{
    components::selection_bar::{export_task_info_text, save_task_info},
    model::manifest::ManifestSort,
};

impl ManifestView {
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        let chips =
            h_flex().w_full().flex_wrap().gap(spacing.xs).children(
                self.selection.extensions().iter().enumerate().map(
                    |(index, (extension, count))| {
                        let active = self.selection.extension_active(extension);
                        let extension = extension.clone();
                        let label = if extension.is_empty() {
                            self.t("manifestNoExtension", cx)
                        } else {
                            SharedString::from(extension.clone())
                        };
                        Button::new(("manifest-extension", index))
                            .outline()
                            .control(cx)
                            .label(SharedString::from(format!("{label} {count}")))
                            .when(active, |button| button.primary())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.selection.toggle_extension(&extension);
                                cx.notify();
                            }))
                    },
                ),
            );
        let actions = h_flex()
            .w_full()
            .flex_wrap()
            .gap(spacing.xs)
            .child(
                Button::new("manifest-select-all")
                    .ghost()
                    .control(cx)
                    .label(self.t("manifestSelectAll", cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.selection.select_visible(false);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("manifest-invert")
                    .ghost()
                    .control(cx)
                    .label(self.t("manifestInvertSelection", cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.selection.select_visible(true);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("manifest-clear")
                    .ghost()
                    .control(cx)
                    .label(self.t("manifestClearSelection", cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.selection.clear();
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("manifest-sort")
                    .ghost()
                    .control(cx)
                    .label(self.t(
                        match self.selection.sort() {
                            ManifestSort::Name => "manifestSortByName",
                            ManifestSort::Size => "manifestSortBySizeDesc",
                        },
                        cx,
                    ))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.selection.toggle_sort();
                        cx.notify();
                    })),
            );
        v_flex()
            .w_full()
            .gap(spacing.sm)
            .child(Input::new(&self.search).control(cx).w_full())
            .child(chips)
            .child(actions)
    }

    fn crumb(&self, node: usize, label: SharedString, cx: &mut Context<Self>) -> Button {
        Button::new(("manifest-crumb", node))
            .ghost()
            .control(cx)
            .label(label)
            .disabled(node == self.selection.cwd())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selection.navigate(node);
                cx.notify();
            }))
    }
    pub(super) fn render_breadcrumb(&self, cx: &mut Context<Self>) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let mut row = h_flex().w_full().flex_wrap().gap(tokens.spacing.xs);
        if self.selection.searching() {
            return row.child(field_hint(
                self.text_with(
                    "manifestSearchResultCount",
                    &[("count", &self.selection.visible_count().to_string())],
                    cx,
                ),
                cx,
            ));
        }
        if self.selection.cwd() != 0 {
            row = row.child(
                Button::new("manifest-up")
                    .ghost()
                    .control_icon(cx)
                    .icon(FluxIcon::ArrowUp)
                    .tooltip(self.t("manifestBreadcrumbUpTooltip", cx))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.selection.up();
                        cx.notify();
                    })),
            );
        }
        let crumbs = self.selection.breadcrumbs();
        let collapsed = crumbs.len() > 5;
        for (index, (node, label)) in crumbs.iter().enumerate() {
            if collapsed && index > 1 && index < crumbs.len() - 2 {
                if index != 2 {
                    continue;
                }
                let hidden: Vec<_> = crumbs[2..crumbs.len() - 2]
                    .iter()
                    .map(|(node, label)| (*node, SharedString::from((*label).to_owned())))
                    .collect();
                let this = cx.weak_entity();
                row = row.child(
                    Button::new("manifest-hidden-crumbs")
                        .ghost()
                        .control_icon(cx)
                        .icon(FluxIcon::Ellipsis)
                        .tooltip(self.t("manifestBreadcrumbMoreTooltip", cx))
                        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                            hidden.iter().fold(menu, |menu, (node, label)| {
                                let this = this.clone();
                                let node = *node;
                                menu.item(PopupMenuItem::new(label.clone()).on_click(
                                    move |_, _, cx| {
                                        let Ok(()) = this.update(cx, |this, cx| {
                                            this.selection.navigate(node);
                                            cx.notify();
                                        }) else {
                                            // The containing form has been released.
                                            return;
                                        };
                                    },
                                ))
                            })
                        }),
                );
                continue;
            }
            if index > 0 {
                row = row.child(field_hint("/", cx));
            }
            let label = if *node == 0 {
                self.t("categoryAll", cx)
            } else {
                SharedString::from((*label).to_owned())
            };
            row = row.child(self.crumb(*node, label, cx));
        }
        row
    }

    fn queue_label(&self, id: &str, cx: &App) -> SharedString {
        match id {
            fluxdown_protocol::MAIN_QUEUE_ID => self.t("mainQueue", cx),
            fluxdown_protocol::LATER_QUEUE_ID => self.t("laterQueue", cx),
            _ => self.queues.iter().find(|queue| queue.id == id).map_or_else(
                || SharedString::from(id.to_owned()),
                |queue| SharedString::from(queue.name.clone()),
            ),
        }
    }
    fn queue_menu(
        &self,
        paused: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.weak_entity();
        let default_queue = if paused && !self.base.start_paused {
            fluxdown_protocol::LATER_QUEUE_ID
        } else {
            &self.base.queue_id
        };
        let queues: Vec<_> = self
            .queues
            .iter()
            .map(|queue| {
                let name = self.queue_label(&queue.id, cx);
                let label = self.text_with(
                    if paused {
                        "manifestLaterToQueue"
                    } else {
                        "manifestStartToQueue"
                    },
                    &[("name", name.as_ref())],
                    cx,
                );
                (queue.id.clone(), label, queue.id == default_queue)
            })
            .collect();
        move |menu, _, _| {
            queues.iter().fold(menu, |menu, (queue, label, checked)| {
                let this = this.clone();
                let queue = queue.clone();
                menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(*checked)
                        .on_click(move |_, window, cx| {
                            let queue = queue.clone();
                            let Ok(()) = this.update(cx, |this, cx| {
                                this.submit(paused, Some(queue), window, cx)
                            }) else {
                                // The containing form has been released.
                                return;
                            };
                        }),
                )
            })
        }
    }
    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let chrome = theme.extended().colors.chrome;
        let stroke = theme.extended().stroke.thin;
        let border = theme.extended().colors.hairline;
        let split_size = Size::Size(theme.density().control);
        let count = self.selection.selection_stat().count;
        let enabled = count > 0 && !self.picking && !self.submitting;
        let actions = h_flex()
            .w_full()
            .flex_wrap()
            .justify_end()
            .gap(tokens.spacing.sm)
            .child(
                Button::new("manifest-export-info")
                    .outline()
                    .control(cx)
                    .label(self.t("exportTaskInfoTxt", cx))
                    .disabled(self.submitting)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        // The resolver returns item IDs, not direct download URLs.
                        let rows = this.selection.items().iter().map(|item| {
                            (item.name.as_str(), "", u64::try_from(item.size).unwrap_or_default())
                        });
                        let failed = this.t("taskInfoExportFailed", cx).to_string();
                        save_task_info(export_task_info_text(rows), failed, window, cx);
                    })),
            )
            .child(
                Button::new("manifest-back")
                    .outline()
                    .control(cx)
                    .label(self.t("back", cx))
                    .disabled(self.submitting)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        if !this.submitting {
                            (this.on_back)(window, cx);
                        }
                    })),
            )
            .child(
                DropdownButton::new("manifest-later")
                    .with_size(split_size)
                    .disabled(!enabled)
                    .map(|button| {
                        if self.base.start_paused {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    })
                    .button(
                        Button::new("manifest-later-main")
                            .control(cx)
                            .label(self.t("downloadLater", cx))
                            .busy(self.submitting && self.submitting_paused)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit(true, None, window, cx)
                            })),
                    )
                    .dropdown_menu_with_anchor(Anchor::TopRight, self.queue_menu(true, cx)),
            )
            .child(
                DropdownButton::new("manifest-start")
                    .with_size(split_size)
                    .disabled(!enabled)
                    .map(|button| {
                        if self.base.start_paused {
                            button.outline()
                        } else {
                            button.primary()
                        }
                    })
                    .button(
                        Button::new("manifest-start-main")
                            .control(cx)
                            .label(self.text_with(
                                "manifestStartDownloadWithCount",
                                &[("count", &count.to_string())],
                                cx,
                            ))
                            .busy(self.submitting && !self.submitting_paused)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit(false, None, window, cx)
                            })),
                    )
                    .dropdown_menu_with_anchor(Anchor::TopRight, self.queue_menu(false, cx)),
            );
        v_flex()
            .w_full()
            .flex_none()
            .px(tokens.spacing.lg)
            .py(tokens.spacing.md)
            .gap(tokens.spacing.sm)
            .bg(chrome)
            .border_t(stroke)
            .border_color(border)
            .child(field_hint(
                self.summary(self.selection.selection_stat(), true, cx),
                cx,
            ))
            .child(actions)
    }
}
