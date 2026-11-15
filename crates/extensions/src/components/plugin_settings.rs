//! 单个插件的设置表单：按 widget 类型分发输入控件，提交前做
//! required / number / min-max / select 前置校验，全部通过才发起
//! `daemon.plugin.updateSettings`（`pattern` 由 daemon 侧 JS RegExp 校验）。

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use fluxdown_protocol::{PluginDto, RpcErrorData, SettingFieldDto};
use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, field_error, form, form_field, input_with_action, option_group,
    option_row,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, AppContext as _, ClipboardItem, Context, Entity, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::Button,
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};

use crate::{ExtensionsPort, controller::update_plugin_settings, error_text, ui};

/// 前置校验失败原因；文案映射留给 UI 层。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldError {
    Required,
    Number,
    Min(String),
    Max(String),
    Select,
    /// daemon 校验失败并指明了字段（如 `pattern` 不匹配）。
    Server(String),
}

/// 单条设置项前置校验：required → number/min/max → select 成员。
pub fn validate_field(field: &SettingFieldDto, raw: &str) -> Option<FieldError> {
    let value = raw.trim();
    if field.required && value.is_empty() {
        return Some(FieldError::Required);
    }
    if value.is_empty() {
        return None;
    }
    if field.setting_type == "number" {
        let Ok(number) = value.parse::<f64>() else {
            return Some(FieldError::Number);
        };
        if let Some(min) = field.min
            && number < min
        {
            return Some(FieldError::Min(trim_number(min)));
        }
        if let Some(max) = field.max
            && number > max
        {
            return Some(FieldError::Max(trim_number(max)));
        }
    }
    if field.widget == "select"
        && !field.options.is_empty()
        && !field.options.iter().any(|option| option.value == value)
    {
        return Some(FieldError::Select);
    }
    None
}

/// 实际发给引擎的值：引擎对数字按原串解析、对 select 要求成员、对 pattern 做正则匹配，
/// 因此数字去掉首尾空白，可选且留空的这三类字段不上报（引擎会拒绝空串）。
fn submit_value(field: &SettingFieldDto, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if field.setting_type == "number" {
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    let rejects_empty = field.pattern.is_some()
        || (field.widget == "select" && !field.options.iter().any(|o| o.value.is_empty()));
    if trimmed.is_empty() && !field.required && rejects_empty {
        return None;
    }
    Some(raw.to_owned())
}

/// 去掉整数值的多余小数位（3.0 → "3"，3.5 → "3.5"）。
pub fn trim_number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

/// 字段初始值：已保存值优先，否则 manifest 默认值（toggle 默认 `false`）。
fn initial_value(field: &SettingFieldDto, saved: &HashMap<String, String>) -> String {
    if let Some(value) = saved.get(&field.key) {
        return value.clone();
    }
    match field.default.as_deref() {
        Some(default) if !default.is_empty() => default.to_owned(),
        _ if field.widget == "toggle" => "false".to_owned(),
        _ => String::new(),
    }
}

fn range_hint(field: &SettingFieldDto) -> Option<String> {
    match (field.min, field.max) {
        (Some(min), Some(max)) => Some(format!("{} – {}", trim_number(min), trim_number(max))),
        (Some(min), None) => Some(format!("≥ {}", trim_number(min))),
        (None, Some(max)) => Some(format!("≤ {}", trim_number(max))),
        (None, None) => None,
    }
}

enum FieldControl {
    Text(Entity<InputState>),
    Textarea(Entity<TextareaState>),
    Toggle(bool),
    Select(String),
    Folder(String),
}

pub struct PluginSettingsForm {
    translator: Entity<Translator>,
    port: Arc<dyn ExtensionsPort>,
    identity: String,
    fields: Vec<SettingFieldDto>,
    display_fields: BTreeMap<usize, SettingFieldDto>,
    locale: String,
    controls: Vec<FieldControl>,
    errors: HashMap<String, FieldError>,
    server_error: Option<SharedString>,
    saving: bool,
}

fn localized_fields(fields: &[SettingFieldDto], locale: &str) -> BTreeMap<usize, SettingFieldDto> {
    fields
        .iter()
        .enumerate()
        .filter_map(|(index, field)| match field.localized(locale) {
            Cow::Borrowed(_) => None,
            Cow::Owned(field) => Some((index, field)),
        })
        .collect()
}

impl PluginSettingsForm {
    pub fn new(
        translator: Entity<Translator>,
        port: Arc<dyn ExtensionsPort>,
        plugin: &PluginDto,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let locale = translator.read(cx).locale().to_owned();
        let display_fields = localized_fields(&plugin.settings, &locale);
        cx.observe(&translator, |this, _, cx| {
            let locale = this.translator.read(cx).locale();
            if locale != this.locale {
                this.display_fields = localized_fields(&this.fields, locale);
                this.locale = locale.to_owned();
            }
            cx.notify();
        })
        .detach();
        let controls = plugin
            .settings
            .iter()
            .map(|field| {
                let value = initial_value(field, &plugin.settings_values);
                match field.widget.as_str() {
                    "toggle" => FieldControl::Toggle(value == "true"),
                    "select" => FieldControl::Select(value),
                    "folder" => FieldControl::Folder(value),
                    "textarea" => FieldControl::Textarea(
                        cx.new(|cx| TextareaState::new(window, cx).default_value(value)),
                    ),
                    widget => {
                        let placeholder = if field.setting_type == "number" {
                            range_hint(field).unwrap_or_default()
                        } else {
                            field.default.clone().unwrap_or_default()
                        };
                        FieldControl::Text(cx.new(|cx| {
                            InputState::new(window, cx)
                                .masked(widget == "password")
                                .default_value(value)
                                .placeholder(placeholder)
                        }))
                    }
                }
            })
            .collect();
        Self {
            translator,
            port,
            identity: plugin.identity.clone(),
            fields: plugin.settings.clone(),
            display_fields,
            locale,
            controls,
            errors: HashMap::new(),
            server_error: None,
            saving: false,
        }
    }

    pub fn is_saving(&self) -> bool {
        self.saving
    }

    fn value_of(&self, index: usize, cx: &Context<Self>) -> String {
        match &self.controls[index] {
            FieldControl::Text(input) => input.read(cx).value().to_string(),
            FieldControl::Textarea(input) => input.read(cx).value().to_string(),
            FieldControl::Toggle(checked) => if *checked { "true" } else { "false" }.to_owned(),
            FieldControl::Select(value) | FieldControl::Folder(value) => value.clone(),
        }
    }

    /// 校验并提交；校验失败只标红字段，成功后关闭所在对话框。
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let mut entries = HashMap::with_capacity(self.fields.len());
        let mut errors = HashMap::new();
        for (index, field) in self.fields.iter().enumerate() {
            let value = self.value_of(index, cx);
            if let Some(error) = validate_field(field, &value) {
                errors.insert(field.key.clone(), error);
            }
            if let Some(value) = submit_value(field, &value) {
                entries.insert(field.key.clone(), value);
            }
        }
        self.server_error = None;
        self.errors = errors;
        if !self.errors.is_empty() {
            cx.notify();
            return;
        }
        self.saving = true;
        cx.notify();
        let future = update_plugin_settings(&self.port, &self.identity, entries);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => this.apply_server_error(&error, cx),
                }
                cx.notify();
            }) else {
                // 对话框或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn apply_server_error(&mut self, error: &RpcErrorData, cx: &Context<Self>) {
        let translator = self.translator.read(cx);
        let message = error_text(translator, error);
        self.server_error = Some(SharedString::from(
            translator.text_with("pluginSettingsSaveFailed", &[("message", &message)]),
        ));
        if let Some(field) = error
            .field
            .as_deref()
            .filter(|field| self.fields.iter().any(|known| known.key == *field))
        {
            self.errors
                .insert(field.to_owned(), FieldError::Server(message));
        }
    }

    fn error_text(&self, key: &str, cx: &Context<Self>) -> Option<SharedString> {
        let translator = self.translator.read(cx);
        self.errors.get(key).map(|error| match error {
            FieldError::Required => {
                SharedString::from(translator.text("pluginErrRequired").to_owned())
            }
            FieldError::Number => SharedString::from(translator.text("pluginErrNumber").to_owned()),
            FieldError::Min(min) => {
                SharedString::from(translator.text_with("pluginErrMin", &[("min", min)]))
            }
            FieldError::Max(max) => {
                SharedString::from(translator.text_with("pluginErrMax", &[("max", max)]))
            }
            FieldError::Select => SharedString::from(translator.text("pluginErrSelect").to_owned()),
            FieldError::Server(message) => SharedString::from(message.clone()),
        })
    }

    fn render_control(
        &self,
        index: usize,
        field: &SettingFieldDto,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let disabled = self.saving;
        let control_height = active_theme(cx).density().control;
        match &self.controls[index] {
            FieldControl::Text(input) => Input::new(input)
                .control(cx)
                .w_full()
                .disabled(disabled)
                .when(field.widget == "password", Input::mask_toggle)
                .into_any_element(),
            FieldControl::Textarea(input) => Textarea::new(input)
                .w_full()
                .h(control_height * 3.)
                .text_size(active_theme(cx).tokens().typography.sm.size)
                .disabled(disabled)
                .into_any_element(),
            FieldControl::Toggle(checked) => Switch::new(("plugin-setting-toggle", index))
                .checked(*checked)
                .disabled(disabled)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.controls[index] = FieldControl::Toggle(*checked);
                    this.clear_error(index);
                    cx.notify();
                }))
                .into_any_element(),
            FieldControl::Select(value) => {
                let label = field
                    .options
                    .iter()
                    .find(|option| option.value == *value)
                    .map(|option| option.label.clone())
                    .unwrap_or_else(|| {
                        if value.is_empty() {
                            self.translator
                                .read(cx)
                                .text("pluginSelectPlaceholder")
                                .to_owned()
                        } else {
                            value.clone()
                        }
                    });
                let options = field
                    .options
                    .iter()
                    .map(|option| (option.value.clone(), option.label.clone()))
                    .collect::<Vec<_>>();
                let current = value.clone();
                let form = cx.entity();
                Button::new(("plugin-setting-select", index))
                    .outline()
                    .label(label)
                    .dropdown_caret(true)
                    .control(cx)
                    .w_full()
                    .disabled(disabled)
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                        options
                            .iter()
                            .fold(menu, |menu, (option_value, option_label)| {
                                let checked = *option_value == current;
                                let form = form.clone();
                                let option_value = option_value.clone();
                                menu.item(
                                    PopupMenuItem::new(option_label.clone())
                                        .checked(checked)
                                        .on_click(move |_, _, cx| {
                                            form.update(cx, |form, cx| {
                                                form.controls[index] =
                                                    FieldControl::Select(option_value.clone());
                                                form.clear_error(index);
                                                cx.notify();
                                            });
                                        }),
                                )
                            })
                            .scrollable(true)
                            .max_h(control_height * 8.)
                    })
                    .into_any_element()
            }
            FieldControl::Folder(path) => {
                let translator = self.translator.read(cx);
                let placeholder = translator.text("pluginFolderPickPlaceholder").to_owned();
                let browse = translator.text("browse").to_owned();
                let display = if path.is_empty() {
                    placeholder
                } else {
                    path.clone()
                };
                input_with_action(
                    ui::path_box(display, path.is_empty(), active_theme(cx)),
                    Button::new(("plugin-setting-folder", index))
                        .outline()
                        .icon(FluxIcon::FolderOpen)
                        .label(browse)
                        .control(cx)
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pick_folder(index, window, cx);
                        })),
                    cx,
                )
                .into_any_element()
            }
        }
    }

    fn clear_error(&mut self, index: usize) {
        if let Some(field) = self.fields.get(index) {
            self.errors.remove(&field.key);
        }
    }

    fn pick_folder(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.first()
            {
                let text = path.display().to_string();

                let Ok(()) = this.update(cx, |this, cx| {
                    this.controls[index] = FieldControl::Folder(text);
                    this.clear_error(index);
                    cx.notify();
                }) else {
                    // 扩展视图已释放，结束回调，不再更新状态。
                    return;
                };
            }
        })
        .detach();
    }

    fn copy_helper_script(&self, script: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(script));
        let message = self
            .translator
            .read(cx)
            .text("pluginHelperScriptCopied")
            .to_owned();
        window.push_notification(Notification::success(message), cx);
    }
}

impl Render for PluginSettingsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let copy_label = self
            .translator
            .read(cx)
            .text("pluginCopyHelperScript")
            .to_owned();
        // 连续的开关字段合并进同一个 option_group；其余字段逐个 form_field。
        let mut body = form(cx).when_some(self.server_error.clone(), |this, error| {
            this.child(
                field_error(error, cx)
                    .w_full()
                    .px(tokens.spacing.md)
                    .py(tokens.spacing.sm)
                    .rounded(tokens.radius.md)
                    .bg(tokens.colors.destructive.opacity(0.1)),
            )
        });
        let mut toggles: Vec<AnyElement> = Vec::new();
        for index in 0..self.fields.len() {
            let field = self
                .display_fields
                .get(&index)
                .unwrap_or(&self.fields[index]);
            let title = SharedString::from(if field.title.is_empty() {
                field.key.clone()
            } else {
                field.title.clone()
            });
            let description = (!field.description.is_empty())
                .then(|| SharedString::from(field.description.clone()));
            let error = self.error_text(&field.key, cx);
            let helper = field
                .helper_script
                .clone()
                .filter(|script| !script.is_empty())
                .map(|script| {
                    let label = field
                        .helper_label
                        .clone()
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| copy_label.clone());
                    h_flex().child(
                        Button::new(("plugin-setting-helper", index))
                            .outline()
                            .icon(FluxIcon::Copy)
                            .label(label)
                            .control(cx)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.copy_helper_script(script.clone(), window, cx);
                            })),
                    )
                });
            let control = self.render_control(index, field, cx);
            if matches!(self.controls[index], FieldControl::Toggle(_)) {
                toggles.push(option_row(title, description, control, cx).into_any_element());
                if helper.is_none() && error.is_none() {
                    continue;
                }
                body = body.child(
                    v_flex()
                        .w_full()
                        .gap(tokens.spacing.xs + tokens.spacing.xxs)
                        .child(option_group(std::mem::take(&mut toggles), cx))
                        .children(helper)
                        .when_some(error, |this, error| this.child(field_error(error, cx))),
                );
                continue;
            }
            if !toggles.is_empty() {
                body = body.child(option_group(std::mem::take(&mut toggles), cx));
            }
            body = body.child(
                v_flex()
                    .w_full()
                    .gap(tokens.spacing.xs + tokens.spacing.xxs)
                    .child(form_field(
                        title,
                        v_flex()
                            .w_full()
                            .gap(tokens.spacing.xs + tokens.spacing.xxs)
                            .child(control)
                            .children(helper),
                        description,
                        cx,
                    ))
                    .when_some(error, |this, error| this.child(field_error(error, cx))),
            );
        }
        if !toggles.is_empty() {
            body = body.child(option_group(toggles, cx));
        }
        // 内容区随内容伸缩，超出上限时纵向滚动；内边距给输入框聚焦环留出空间。
        // 上限必须放在外层 flex 容器上：Scrollable 内部滚动区是 size_full，只有作为
        // 被 max_h 约束的 flex 子项才能拿到确定高度，直接在滚动元素上设 max_h
        // 会让滚动区撑到内容高度、只被裁剪而永不滚动。min_h_0 让窗口较矮时继续收缩。
        v_flex().w_full().min_h_0().max_h(px(460.)).child(
            div()
                .w_full()
                .overflow_y_scrollbar()
                .child(div().p(tokens.spacing.xxs).child(body)),
        )
    }
}

/// 设置对话框内容：「插件设置」（插件视图）与「基础设置」（manifest 设置表单）两个 tab；
/// 只有一个可用内容时不显示 tab 栏。
pub struct PluginSettingsTabs {
    translator: Entity<Translator>,
    view: Option<gpui::AnyView>,
    form: Option<Entity<PluginSettingsForm>>,
    /// 0 = 插件设置视图，1 = 基础设置；仅在两者都存在时可切换。
    selected: usize,
}

impl PluginSettingsTabs {
    pub fn new(
        translator: Entity<Translator>,
        view: Option<gpui::AnyView>,
        form: Option<Entity<PluginSettingsForm>>,
    ) -> Self {
        Self {
            translator,
            view,
            form,
            selected: 0,
        }
    }

    /// 当前显示的是否为基础设置表单。
    fn showing_form(&self) -> bool {
        self.form.is_some() && (self.view.is_none() || self.selected == 1)
    }

    /// 当前 tab 是基础设置时返回其表单（保存按钮只对它生效）。
    pub fn active_form(&self) -> Option<Entity<PluginSettingsForm>> {
        if self.showing_form() {
            self.form.clone()
        } else {
            None
        }
    }

    pub fn is_saving(&self, cx: &gpui::App) -> bool {
        self.form
            .as_ref()
            .is_some_and(|form| form.read(cx).is_saving())
    }
}

impl Render for PluginSettingsTabs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = active_theme(cx);
        let translator = self.translator.read(cx);
        let both = self.view.is_some() && self.form.is_some();
        let labels = [
            SharedString::from(translator.text("pluginSettingsViewTab").to_owned()),
            SharedString::from(translator.text("pluginSettingsBasicTab").to_owned()),
        ];
        let tabs = cx.entity();
        let body = if self.showing_form() {
            self.form.clone().map(|form| form.into_any_element())
        } else {
            self.view.clone().map(|view| {
                div()
                    .w_full()
                    .h(theme.text_extent(420.))
                    .overflow_hidden()
                    .child(view)
                    .into_any_element()
            })
        };
        v_flex()
            .w_full()
            .min_h_0()
            .gap(theme.tokens().spacing.md)
            .when(both, |this| {
                this.child(fluxdown_ui_components::segmented_tabs(
                    "plugin-settings-tabs",
                    labels,
                    self.selected,
                    move |index, _, cx| {
                        tabs.update(cx, |this, cx| {
                            this.selected = index;
                            cx.notify();
                        });
                    },
                    cx,
                ))
            })
            .children(body)
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{SettingFieldDto, SettingOptionDto};
    use std::collections::BTreeMap;

    use super::{FieldError, submit_value, trim_number, validate_field};

    fn field(setting_type: &str, widget: &str) -> SettingFieldDto {
        SettingFieldDto {
            key: "k".to_owned(),
            title: "K".to_owned(),
            description: String::new(),
            setting_type: setting_type.to_owned(),
            widget: widget.to_owned(),
            options: Vec::new(),
            default: None,
            required: false,
            min: None,
            max: None,
            pattern: None,
            helper_script: None,
            helper_label: None,
            localizations: BTreeMap::new(),
        }
    }

    #[test]
    fn required_rejects_blank_only() {
        let mut required = field("string", "text");
        required.required = true;
        assert_eq!(validate_field(&required, "  "), Some(FieldError::Required));
        assert_eq!(validate_field(&required, "x"), None);
        assert_eq!(validate_field(&field("string", "text"), ""), None);
    }

    #[test]
    fn number_range_is_enforced() {
        let mut number = field("number", "number");
        number.min = Some(1.0);
        number.max = Some(10.5);
        assert_eq!(validate_field(&number, "abc"), Some(FieldError::Number));
        assert_eq!(
            validate_field(&number, "0"),
            Some(FieldError::Min("1".to_owned()))
        );
        assert_eq!(
            validate_field(&number, "11"),
            Some(FieldError::Max("10.5".to_owned()))
        );
        assert_eq!(validate_field(&number, " 5 "), None);
    }

    #[test]
    fn select_requires_known_option() {
        let mut select = field("string", "select");
        select.options.push(SettingOptionDto {
            value: "a".to_owned(),
            label: "A".to_owned(),
        });
        assert_eq!(validate_field(&select, "b"), Some(FieldError::Select));
        assert_eq!(validate_field(&select, "a"), None);
    }

    #[test]
    fn trim_number_drops_integer_fraction() {
        assert_eq!(trim_number(3.0), "3");
        assert_eq!(trim_number(3.5), "3.5");
        assert_eq!(trim_number(-2.0), "-2");
    }

    #[test]
    fn submit_value_matches_what_the_engine_accepts() {
        let number = field("number", "number");
        assert_eq!(submit_value(&number, " 5 "), Some("5".to_owned()));
        // 引擎对空串数字直接拒绝：可选且留空的数字字段不上报。
        assert_eq!(submit_value(&number, "  "), None);
        let mut select = field("string", "select");
        select.options.push(SettingOptionDto {
            value: "a".to_owned(),
            label: "A".to_owned(),
        });
        assert_eq!(submit_value(&select, ""), None);
        assert_eq!(submit_value(&select, "a"), Some("a".to_owned()));
        let mut patterned = field("string", "text");
        patterned.pattern = Some("^x+$".to_owned());
        assert_eq!(submit_value(&patterned, ""), None);
        // 普通文本保留原值（含首尾空白）。
        assert_eq!(
            submit_value(&field("string", "text"), " a "),
            Some(" a ".to_owned())
        );
        assert_eq!(
            submit_value(&field("string", "text"), ""),
            Some(String::new())
        );
    }
}
