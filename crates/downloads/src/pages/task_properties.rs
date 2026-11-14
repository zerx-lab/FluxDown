//! Unified task property editor. Secret values are never prefilled.

use std::{collections::HashMap, rc::Rc};

use crate::{
    controller::{DownloadsCommand, DownloadsResult},
    pages::task_edit::TaskEditTarget,
    strings::error_text,
};
use fluxdown_protocol::{EditTaskParams, TaskEditSnapshot};
use fluxdown_ui_components::{
    ControlExt as _, dialog_footer, dialog_title, field_hint, form, form_field,
};
use gpui::{
    Anchor, App, AppContext as _, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::Button,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    scroll::ScrollableElement as _,
    v_flex,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    SaveDir,
    FileName,
    Proxy,
    Limit,
    Segments,
    UserAgent,
    Headers,
    Cookies,
    AuthRef,
    Category,
    Tags,
    Scheduled,
}

struct PropertiesForm {
    target: Rc<TaskEditTarget>,
    snapshot: Option<TaskEditSnapshot>,
    fields: Vec<(Field, Entity<InputState>)>,
    cookies_touched: bool,
    _watches: Vec<Subscription>,
    loading: bool,
    saving: bool,
    error: Option<SharedString>,
    loading_text: SharedString,
    saving_text: SharedString,
    retry_text: SharedString,
    save_text: SharedString,
    warning: SharedString,
    unsupported: SharedString,
    url_label: SharedString,
    cookies_configured_label: SharedString,
    scheduled_hint: SharedString,
    copy_text: SharedString,
}

impl PropertiesForm {
    fn new(target: Rc<TaskEditTarget>, cx: &mut Context<Self>) -> Self {
        let translator = target.translator.read(cx);
        Self {
            target,
            snapshot: None,
            fields: Vec::new(),
            cookies_touched: false,
            _watches: Vec::new(),
            loading: true,
            error: None,
            saving: false,
            loading_text: translator.text("loading").into(),
            saving_text: translator.text("saving").into(),
            retry_text: translator.text("retry").into(),
            save_text: translator.text("taskEditSave").into(),
            warning: translator.text("taskEditConnectionWarning").into(),
            unsupported: translator.text("taskEditUnsupported").into(),
            url_label: translator.text("taskEditUrl").into(),
            cookies_configured_label: translator.text("taskEditCookiesConfigured").into(),
            scheduled_hint: translator.text("taskEditScheduledHint").into(),
            copy_text: translator.text("copy").into(),
        }
    }

    fn load(&mut self, snapshot: TaskEditSnapshot, window: &mut Window, cx: &mut Context<Self>) {
        let headers = match serde_json::to_string_pretty(&snapshot.headers) {
            Ok(headers) => headers,
            Err(error) => {
                eprintln!("cannot format task property headers: {error:#}");
                self.loading = false;
                self.error = Some(
                    self.target
                        .translator
                        .read(cx)
                        .text("taskEditHeadersInvalid")
                        .into(),
                );
                cx.notify();
                return;
            }
        };
        let values = [
            (Field::SaveDir, snapshot.save_dir.clone()),
            (Field::FileName, snapshot.file_name.clone()),
            (Field::Proxy, snapshot.proxy_url.clone()),
            (
                Field::Limit,
                (snapshot
                    .properties
                    .download_limit_bps
                    .max(0)
                    .saturating_add(1023)
                    / 1024)
                    .to_string(),
            ),
            (Field::Segments, snapshot.segments.to_string()),
            (Field::UserAgent, snapshot.properties.user_agent.clone()),
            (Field::Headers, headers),
            (Field::Cookies, String::new()),
            (Field::AuthRef, snapshot.properties.auth_ref.clone()),
            (Field::Category, snapshot.properties.category.clone()),
            (Field::Tags, snapshot.properties.tags.join(", ")),
            (
                Field::Scheduled,
                snapshot.properties.scheduled_at.to_string(),
            ),
        ];
        self.fields = values
            .into_iter()
            .map(|(field, value)| {
                (
                    field,
                    cx.new(|cx| {
                        InputState::new(window, cx)
                            .masked(field == Field::Cookies)
                            .default_value(value)
                    }),
                )
            })
            .collect();
        if let Some((_, cookies)) = self
            .fields
            .iter()
            .find(|(field, _)| *field == Field::Cookies)
        {
            self._watches
                .push(cx.subscribe(cookies, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.cookies_touched = true;
                        cx.notify();
                    }
                }));
        }
        if let Some((_, category)) = self
            .fields
            .iter()
            .find(|(field, _)| *field == Field::Category)
        {
            self._watches
                .push(cx.observe(category, |_, _, cx| cx.notify()));
        }
        self.snapshot = Some(snapshot);
        self.loading = false;
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        let mut params = EditTaskParams {
            task_id: snapshot.task_id.clone(),
            expected_revision: snapshot.revision.clone(),
            ..Default::default()
        };
        let values = self
            .fields
            .iter()
            .map(|(field, input)| (*field, input.read(cx).value()))
            .collect::<Vec<_>>();
        let value = |field| {
            values
                .iter()
                .find(|(kind, _)| *kind == field)
                .map_or("", |(_, value)| value.as_ref())
        };
        let capabilities = &snapshot.capabilities;
        let save_dir = value(Field::SaveDir);
        if capabilities.storage && save_dir != snapshot.save_dir {
            params.save_dir = Some(save_dir.to_owned());
        }
        let file_name = value(Field::FileName);
        if capabilities.rename && file_name != snapshot.file_name {
            params.file_name = Some(file_name.to_owned());
        }
        let proxy = value(Field::Proxy);
        if capabilities.proxy && proxy != snapshot.proxy_url {
            params.proxy_url = Some(proxy.to_owned());
        }
        {
            let raw_limit = value(Field::Limit);
            if raw_limit
                != (snapshot
                    .properties
                    .download_limit_bps
                    .max(0)
                    .saturating_add(1023)
                    / 1024)
                    .to_string()
            {
                let limit =
                    raw_limit.trim().parse::<i64>().ok().and_then(|value| {
                        (value >= 0).then_some(value.checked_mul(1024)).flatten()
                    });
                let Some(limit) = limit else {
                    self.error = Some(
                        self.target
                            .translator
                            .read(cx)
                            .text("taskEditInvalidNumber")
                            .into(),
                    );
                    cx.notify();
                    return;
                };
                params.download_limit_bps = Some(limit);
            }
        }
        if capabilities.request {
            let ua = value(Field::UserAgent);
            if ua != snapshot.properties.user_agent {
                params.user_agent = Some(ua.to_owned());
            }
            let headers_text = value(Field::Headers);
            let headers = match serde_json::from_str::<HashMap<String, String>>(headers_text) {
                Ok(headers) => headers,
                Err(_) => {
                    self.error = Some(
                        self.target
                            .translator
                            .read(cx)
                            .text("taskEditHeadersInvalid")
                            .into(),
                    );
                    cx.notify();
                    return;
                }
            };
            if headers != snapshot.headers {
                params.headers = Some(headers);
            }
            let cookies = value(Field::Cookies);
            if self.cookies_touched {
                params.cookies = Some(cookies.to_owned());
            }
            let auth = value(Field::AuthRef);
            if auth != snapshot.properties.auth_ref {
                params.auth_ref = Some(auth.to_owned());
            }
        }
        let segments = value(Field::Segments).trim().parse::<i32>().ok();
        if capabilities.segments
            && segments
                .is_none_or(|value| !(0..=fluxdown_protocol::MAX_TASK_SEGMENTS).contains(&value))
        {
            self.error = Some(
                self.target
                    .translator
                    .read(cx)
                    .text("taskEditInvalidNumber")
                    .into(),
            );
            cx.notify();
            return;
        }
        if capabilities.segments
            && let Some(segments) = segments.filter(|value| *value != snapshot.segments)
        {
            params.segments = Some(segments);
        }
        let category = value(Field::Category);
        if category != snapshot.properties.category {
            params.category = Some(category.to_owned());
        }
        let tags = value(Field::Tags)
            .split([',', '\n'])
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if tags != snapshot.properties.tags {
            params.tags = Some(tags);
        }
        if capabilities.schedule {
            let scheduled = value(Field::Scheduled)
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|value| *value >= 0);
            if scheduled.is_none() {
                self.error = Some(
                    self.target
                        .translator
                        .read(cx)
                        .text("taskEditInvalidNumber")
                        .into(),
                );
                cx.notify();
                return;
            }
            if let Some(scheduled) =
                scheduled.filter(|value| *value != snapshot.properties.scheduled_at)
            {
                params.scheduled_at = Some(scheduled);
            }
        }
        if [
            params.save_dir.is_some(),
            params.file_name.is_some(),
            params.proxy_url.is_some(),
            params.download_limit_bps.is_some(),
            params.segments.is_some(),
            params.user_agent.is_some(),
            params.headers.is_some(),
            params.cookies.is_some(),
            params.auth_ref.is_some(),
            params.category.is_some(),
            params.tags.is_some(),
            params.scheduled_at.is_some(),
        ]
        .iter()
        .all(|changed| !changed)
        {
            return;
        }
        if self.saving {
            return;
        }
        self.saving = true;
        self.error = None;
        cx.notify();
        let port = self.target.port.clone();
        let entity = cx.entity();
        let handle = window.window_handle();
        let save = port.execute(DownloadsCommand::EditTask(Box::new(params)));
        cx.spawn(async move |_, cx| {
            let result = save.await;
            if let Err(error) = handle.update(cx, |_, window, cx| {
                entity.update(cx, |form, cx| {
                    form.saving = false;
                    match result {
                        Ok(DownloadsResult::Unit) => {
                            form.error = None;
                            window.push_notification(
                                Notification::success(
                                    form.target.translator.read(cx).text("taskEditSaved"),
                                ),
                                cx,
                            );
                            window.close_dialog(cx);
                        }
                        Ok(_) => {
                            let message = form
                                .target
                                .translator
                                .read(cx)
                                .text("taskEditUnexpectedResult");
                            form.error = Some(message.into());
                            window.push_notification(Notification::error(message), cx);
                        }
                        Err(error) => {
                            let message = form.target.translator.read(cx).text_with(
                                "taskEditSaveFailed",
                                &[(
                                    "error",
                                    &error_text(form.target.translator.read(cx), &error),
                                )],
                            );
                            form.error = Some(message.clone().into());
                            window.push_notification(Notification::error(message), cx);
                        }
                    }
                    cx.notify();
                });
            }) {
                eprintln!("window closed before task property save result: {error:#}");
            }
        })
        .detach();
    }

    fn label(&self, field: Field, cx: &Context<Self>) -> SharedString {
        let key = match field {
            Field::SaveDir => "saveDir",
            Field::FileName => "colFileName",
            Field::Proxy => "taskProxy",
            Field::Limit => "taskEditDownloadLimit",
            Field::Segments => "configuredThreads",
            Field::UserAgent => "taskEditUserAgent",
            Field::Headers => "taskEditHeaders",
            Field::Cookies => "taskEditCookies",
            Field::AuthRef => "taskEditAuthRef",
            Field::Category => "taskEditCategory",
            Field::Tags => "taskEditTags",
            Field::Scheduled => "taskEditScheduledAt",
        };
        self.target.translator.read(cx).text(key).into()
    }
}

impl Render for PropertiesForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.loading {
            return v_flex()
                .p_4()
                .child(self.loading_text.clone())
                .into_any_element();
        }
        let Some(snapshot) = self.snapshot.as_ref() else {
            let target = self.target.clone();
            let form = cx.entity();
            return v_flex()
                .p_4()
                .gap_2()
                .child(
                    self.error
                        .clone()
                        .unwrap_or_else(|| self.loading_text.clone()),
                )
                .child(
                    Button::new("task-properties-retry")
                        .control(cx)
                        .label(self.retry_text.clone())
                        .on_click(cx.listener(move |_, _, window, cx| {
                            request_snapshot(target.clone(), form.clone(), window, cx)
                        })),
                )
                .into_any_element();
        };
        let mut body = form(cx).child(form_field(
            self.url_label.clone(),
            h_url(&snapshot.url, &self.copy_text, cx),
            None,
            cx,
        ));
        let save_dir_input = self
            .fields
            .iter()
            .find(|(field, _)| *field == Field::SaveDir)
            .map(|(_, input)| input.clone());
        for (field, input) in &self.fields {
            let supported = match field {
                Field::SaveDir => snapshot.capabilities.storage,
                Field::FileName => snapshot.capabilities.rename,
                Field::Proxy => snapshot.capabilities.proxy,
                Field::Segments => snapshot.capabilities.segments,
                Field::Scheduled => snapshot.capabilities.schedule,
                Field::Limit => true,
                Field::UserAgent | Field::Headers | Field::Cookies | Field::AuthRef => {
                    snapshot.capabilities.request
                }
                Field::Category | Field::Tags => true,
            };
            if *field == Field::Category {
                let current = input.read(cx).value().to_string();
                body = body.child(form_field(
                    self.label(*field, cx),
                    category_picker(
                        input.clone(),
                        save_dir_input.clone(),
                        current,
                        self.target.categories.clone(),
                        self.target
                            .translator
                            .read(cx)
                            .text("taskEditAutoCategory")
                            .into(),
                        cx,
                    ),
                    None,
                    cx,
                ));
            } else {
                body = body.child(form_field(
                    self.label(*field, cx),
                    Input::new(input)
                        .control(cx)
                        .disabled(!supported || self.saving)
                        .w_full(),
                    (!supported).then_some(self.unsupported.clone()),
                    cx,
                ));
            }
            if *field == Field::Cookies && snapshot.cookies_configured {
                if !self.cookies_touched || !input.read(cx).value().is_empty() {
                    body = body.child(field_hint(self.cookies_configured_label.clone(), cx));
                }
                if snapshot.capabilities.request {
                    let cookies = input.clone();
                    body = body.child(
                        Button::new("task-edit-clear-cookies")
                            .control(cx)
                            .label(
                                self.target
                                    .translator
                                    .read(cx)
                                    .text("taskEditClearCookies")
                                    .to_owned(),
                            )
                            .disabled(
                                self.saving
                                    || (self.cookies_touched && input.read(cx).value().is_empty()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cookies.update(cx, |input, cx| {
                                    input.set_value(String::new(), window, cx)
                                });
                                this.cookies_touched = true;
                                cx.notify();
                            })),
                    );
                }
            }
            if *field == Field::Scheduled {
                body = body.child(field_hint(self.scheduled_hint.clone(), cx));
            }
        }
        body = body
            .child(field_hint(self.warning.clone(), cx))
            .when_some(self.error.clone(), |body, error| body.child(error));
        if self.saving {
            body = body.child(field_hint(self.saving_text.clone(), cx));
        }
        v_flex()
            .w_full()
            .min_h_0()
            .max_h(px(620.))
            .child(
                div()
                    .id("task-properties-scroll")
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element()
    }
}

fn h_url(url: &str, copy: &str, cx: &Context<PropertiesForm>) -> impl IntoElement {
    use gpui_component::h_flex;
    let value = url.to_owned();
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .text_ellipsis_middle()
                .child(url.to_owned()),
        )
        .child(
            Button::new("task-edit-copy-url")
                .control(cx)
                .label(copy.to_owned())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(value.clone()))
                }),
        )
}

fn category_picker(
    input: Entity<InputState>,
    save_dir_input: Option<Entity<InputState>>,
    current: String,
    categories: Vec<fluxdown_protocol::CustomCategoryDto>,
    auto_label: SharedString,
    cx: &Context<PropertiesForm>,
) -> impl IntoElement {
    let label = if current.is_empty() {
        auto_label.clone()
    } else {
        categories
            .iter()
            .find(|category| category.id == current)
            .map_or_else(
                || current.clone().into(),
                |category| category.name.clone().into(),
            )
    };
    Button::new("task-edit-category")
        .outline()
        .control(cx)
        .w_full()
        .dropdown_caret(true)
        .label(label)
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
            let auto_input = input.clone();
            let mut menu = menu
                .min_w(px(280.))
                .max_w(px(480.))
                .max_h(px(320.))
                .scrollable(true)
                .item(
                    PopupMenuItem::new(auto_label.clone())
                        .checked(current.is_empty())
                        .on_click(move |_, window, cx| {
                            auto_input
                                .update(cx, |input, cx| input.set_value(String::new(), window, cx));
                        }),
                );
            for category in &categories {
                let id = category.id.clone();
                let configured_dir = category.save_dir.trim().to_owned();
                let item_input = input.clone();
                let save_dir_input = save_dir_input.clone();
                let checked = current == id;
                menu = menu.item(
                    PopupMenuItem::new(category.name.clone())
                        .checked(checked)
                        .on_click(move |_, window, cx| {
                            item_input
                                .update(cx, |input, cx| input.set_value(id.clone(), window, cx));
                            if !configured_dir.is_empty()
                                && let Some(save_dir_input) = &save_dir_input
                            {
                                save_dir_input.update(cx, |input, cx| {
                                    input.set_value(configured_dir.clone(), window, cx)
                                });
                            }
                        }),
                );
            }
            menu
        })
}

pub(crate) fn open_task_properties(target: TaskEditTarget, window: &mut Window, cx: &mut App) {
    let target = Rc::new(target);
    let form = cx.new(|cx| PropertiesForm::new(target.clone(), cx));
    let form_content = form.clone();
    let form_submit = form.clone();
    let title = target.strings.task_edit_title.clone();
    let save_label = form.read(cx).save_text.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let form_content = form_content.clone();
        let form_submit = form_submit.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(px(680.))
            .content(move |content, _, _| content.child(form_content.clone()))
            .footer(dialog_footer(
                None,
                save_label.clone(),
                fluxdown_ui_components::DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                form_submit.update(cx, |form, cx| form.save(window, cx));
                false
            })
    });
    request_snapshot(target, form, window, cx);
}

fn request_snapshot(
    target: Rc<TaskEditTarget>,
    form: Entity<PropertiesForm>,
    window: &mut Window,
    cx: &mut App,
) {
    let port = target.port.clone();
    let task_id = target.key.task_id().to_owned();
    let handle = window.window_handle();
    let load = port.execute(DownloadsCommand::TaskEditInfo { task_id });
    cx.spawn(async move |cx| {
        let result = load.await;
        if let Err(error) = handle.update(cx, |_, window, cx| {
            form.update(cx, |form, cx| match result {
                Ok(DownloadsResult::TaskEditInfo(snapshot)) => form.load(*snapshot, window, cx),
                Ok(_) => {
                    form.loading = false;
                    form.error = Some(
                        form.target
                            .translator
                            .read(cx)
                            .text("taskEditUnexpectedResult")
                            .into(),
                    );
                    cx.notify();
                }
                Err(error) => {
                    form.loading = false;
                    form.error = Some(error_text(form.target.translator.read(cx), &error).into());
                    cx.notify();
                }
            });
        }) {
            eprintln!("window closed before task property snapshot result: {error:#}");
        }
    })
    .detach();
}
