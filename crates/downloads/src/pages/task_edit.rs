//! 任务编辑：「更换链接」「修改线程数」与「编辑代理」对话框及其提交流程。
//!
//! 入口：下载页右键 / 应用菜单，以及任务详情「常规」页（停靠面板与独立详情窗口共用）。
//! 失败与成功提示推送到打开对话框的窗口；代理变更的失败任务由 daemon 直接重试。

use std::{rc::Rc, sync::Arc};

use fluxdown_protocol::{MAX_TASK_SEGMENTS, RpcErrorData};
use fluxdown_ui_components::{
    ControlExt as _, DialogIntent, dialog_footer, dialog_title, field_error, field_hint, form,
    form_field,
};
use fluxdown_ui_i18n::Translator;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Window, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    WindowExt as _,
    input::{Input, InputEvent, InputState},
    notification::Notification,
};

use crate::{
    actions::{ChangeSelectedSegments, ChangeSelectedUrl, EditSelectedProxy},
    components::{
        task_table::{TaskMenuFacts, task_menu_facts_for, url_has_prefix},
        threads_risk::{parse_segments_input, threads_risk_callout},
    },
    controller::{DownloadsCommand, DownloadsPort},
    model::{RowKey, TaskState, TaskStore},
    pages::downloads::DownloadView,
    strings::{DownloadStrings, change_url_error_text, error_text, set_segments_error_text},
};

/// 线程数输入 → 线程数：必须是 `0..=MAX_TASK_SEGMENTS` 的整数（0 = 自动）；空串 / 非数字 /
/// 负数 / 超限 → `None`。
fn parse_task_segments(input: &str) -> Option<i32> {
    let value = i32::try_from(input.trim().parse::<i64>().ok()?).ok()?;
    (0..=MAX_TASK_SEGMENTS).contains(&value).then_some(value)
}

/// 「修改线程数」对话框内容：输入框 + 风险提示 + 说明 + 校验错误。
struct SegmentsForm {
    translator: Entity<Translator>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    label: SharedString,
    field_hint: SharedString,
    note: SharedString,
    invalid: SharedString,
    /// 输入变化即清错误并重绘（风险提示随输入联动）。
    _watch: Subscription,
}

impl SegmentsForm {
    fn new(
        strings: &DownloadStrings,
        translator: Entity<Translator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("0")
                .validate(|text, _| text.chars().all(|c| c.is_ascii_digit()))
        });
        let watch = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                cx.notify();
            }
        });
        Self {
            translator,
            input,
            error: None,
            label: strings.edit_threads_label.clone(),
            field_hint: strings.edit_threads_zero_hint.clone(),
            note: strings.edit_threads_hint.clone(),
            invalid: strings.edit_threads_invalid.clone(),
            _watch: watch,
        }
    }

    /// 解析当前输入；非法时在表单内显示错误并返回 `None`。
    fn parse(&mut self, cx: &mut Context<Self>) -> Option<i32> {
        let parsed = parse_task_segments(&self.input.read(cx).value());
        if parsed.is_none() {
            self.error = Some(self.invalid.clone());
            cx.notify();
        }
        parsed
    }
}

impl Render for SegmentsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let segments = parse_segments_input(&self.input.read(cx).value());
        form(cx)
            .child(form_field(
                self.label.clone(),
                Input::new(&self.input).control(cx).w_full(),
                Some(self.field_hint.clone()),
                cx,
            ))
            .child(threads_risk_callout(
                "task-segments-threads-risk",
                segments,
                self.translator.read(cx),
            ))
            .child(field_hint(self.note.clone(), cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(field_error(error, cx))
            })
    }
}

/// 新链接的协议类别（对齐引擎 `change_task_url`：http(s) / ftp，或待解封装的 `thunder://`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UrlKind {
    Http,
    Ftp,
    Thunder,
}

fn url_kind(url: &str) -> Option<UrlKind> {
    if url_has_prefix(url, &["http://", "https://"]) {
        Some(UrlKind::Http)
    } else if url_has_prefix(url, &["ftp://"]) {
        Some(UrlKind::Ftp)
    } else if url_has_prefix(url, &["thunder://"]) {
        Some(UrlKind::Thunder)
    } else {
        None
    }
}

/// 更换链接的客户端预校验失败原因（与引擎稳定错误码一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UrlError {
    /// `invalid-url`：空串。
    Invalid,
    /// `protocol-unsupported`：不是 http(s) / ftp / thunder。
    Unsupported,
    /// `protocol-mismatch`：HTTP ↔ FTP 跨协议换源。
    Mismatch,
}

impl UrlError {
    fn text_key(self) -> &'static str {
        match self {
            Self::Invalid => "webChangeUrlErrInvalidUrl",
            Self::Unsupported => "webChangeUrlErrProtocolUnsupported",
            Self::Mismatch => "webChangeUrlErrProtocolMismatch",
        }
    }
}

/// 校验新链接：trim 后非空；scheme 为 http(s) / ftp / thunder（忽略大小写）；非 thunder 时
/// 协议类别须与当前链接一致（thunder 要先解码才知道真实协议，交给引擎判定）。
/// 通过则返回 trim 后的链接。
fn validate_new_url(current: &str, input: &str) -> Result<String, UrlError> {
    let url = input.trim();
    if url.is_empty() {
        return Err(UrlError::Invalid);
    }
    match url_kind(url) {
        None => Err(UrlError::Unsupported),
        Some(UrlKind::Thunder) => Ok(url.to_owned()),
        Some(new) => match url_kind(current) {
            Some(old) if old == new => Ok(url.to_owned()),
            Some(UrlKind::Http | UrlKind::Ftp) => Err(UrlError::Mismatch),
            _ => Err(UrlError::Unsupported),
        },
    }
}

/// 「更换链接」预校验结果。
enum UrlCheck {
    /// 与当前链接相同：无需请求。
    Unchanged,
    Changed(String),
    /// 已在表单内显示错误。
    Invalid,
}

/// 「更换链接」对话框内容：URL 输入框 + 内联校验错误。
struct UrlForm {
    translator: Entity<Translator>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    label: SharedString,
    current_url: String,
    /// 输入变化即清错误。
    _watch: Subscription,
}

impl UrlForm {
    fn new(
        strings: &DownloadStrings,
        translator: Entity<Translator>,
        current_url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current_url.clone())
                .placeholder(strings.change_url_placeholder.clone())
        });
        let watch = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                cx.notify();
            }
        });
        Self {
            translator,
            input,
            error: None,
            label: strings.change_url_label.clone(),
            current_url,
            _watch: watch,
        }
    }

    fn check(&mut self, cx: &mut Context<Self>) -> UrlCheck {
        match validate_new_url(&self.current_url, &self.input.read(cx).value()) {
            Ok(url) if url == self.current_url => UrlCheck::Unchanged,
            Ok(url) => UrlCheck::Changed(url),
            Err(error) => {
                let text = self.translator.read(cx).text(error.text_key()).to_owned();
                self.error = Some(SharedString::from(text));
                cx.notify();
                UrlCheck::Invalid
            }
        }
    }
}

impl Render for UrlForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        form(cx)
            .child(form_field(
                self.label.clone(),
                Input::new(&self.input).control(cx).w_full(),
                None,
                cx,
            ))
            .when_some(self.error.clone(), |this, error| {
                this.child(field_error(error, cx))
            })
    }
}
/// 代理编辑表单：空值表示跟随全局与站点规则；输入其他支持的 URL 或直连 / 系统哨兵。
struct ProxyForm {
    input: Entity<InputState>,
    error: Option<SharedString>,
    label: SharedString,
    hint: SharedString,
    invalid: SharedString,
    _watch: Subscription,
}

impl ProxyForm {
    fn new(
        current: String,
        translator: &Entity<Translator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let watch = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                cx.notify();
            }
        });
        Self {
            input,
            error: None,
            label: SharedString::from(translator.read(cx).text("taskProxy")),
            hint: SharedString::from(translator.read(cx).text("taskEditProxyHint")),
            invalid: SharedString::from(translator.read(cx).text("taskEditProxyInvalid")),
            _watch: watch,
        }
    }

    fn check(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let value = self.input.read(cx).value().trim().to_owned();
        if valid_proxy_url(&value) {
            Some(value)
        } else {
            self.error = Some(self.invalid.clone());
            cx.notify();
            None
        }
    }
}

fn valid_proxy_url(value: &str) -> bool {
    if value.is_empty() || matches!(value, "direct://" | "system://") {
        return true;
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if !["http", "https", "socks4", "socks4a", "socks5", "socks5h"].contains(&scheme) {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    !host_port.is_empty()
        && !host_port.ends_with(':')
        && host_port.rsplit_once(':').is_some_and(|(host, port)| {
            !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port != 0)
        })
        && !rest.contains(['?', '#'])
        && rest
            .split_once('/')
            .is_none_or(|(_, path)| path.is_empty() || path == "/")
}

impl Render for ProxyForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        form(cx)
            .child(form_field(
                self.label.clone(),
                Input::new(&self.input).control(cx).w_full(),
                None,
                cx,
            ))
            .child(field_hint(self.hint.clone(), cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(field_error(error, cx))
            })
    }
}

/// 打开编辑对话框所需的上下文：下载页（右键 / 应用菜单）、停靠详情面板与独立详情窗口共用。
/// 结果（失败 / 成功提示）推送到打开对话框的窗口。
pub(crate) struct TaskEditTarget {
    pub(crate) key: RowKey,
    pub(crate) store: Rc<TaskStore>,
    pub(crate) port: Arc<dyn DownloadsPort>,
    pub(crate) strings: DownloadStrings,
    pub(crate) translator: Entity<Translator>,
}

impl TaskEditTarget {
    fn allows(&self, allow: fn(&TaskMenuFacts) -> bool) -> bool {
        task_menu_facts_for(&self.store, &self.key).is_some_and(|facts| allow(&facts))
    }
}

/// 「编辑代理」：预填当前任务代理，确认后下发 `SetProxy`。
pub(crate) fn open_edit_proxy_dialog(target: TaskEditTarget, window: &mut Window, cx: &mut App) {
    if !target.allows(TaskMenuFacts::can_edit_proxy) {
        return;
    }
    let Some(current) = target
        .store
        .get(&target.key)
        .map(|row| row.proxy_url.clone())
    else {
        return;
    };
    let title = target.strings.task_edit_proxy.clone();
    let ok_label = target.strings.confirm.clone();
    let cancel_label = target.strings.cancel.clone();
    let translator = target.translator.clone();
    let form_entity = cx.new(|cx| ProxyForm::new(current, &translator, window, cx));
    let dialog_form = form_entity.clone();
    let target = Rc::new(target);
    window.open_dialog(cx, move |dialog, _, cx| {
        let content_form = dialog_form.clone();
        let ok_form = dialog_form.clone();
        let target = Rc::clone(&target);
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(px(520.))
            .content(move |content, _, _| content.child(content_form.clone()))
            .footer(dialog_footer(
                Some(cancel_label.clone()),
                ok_label.clone(),
                DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let Some(proxy_url) = ok_form.update(cx, |form, cx| form.check(cx)) else {
                    return false;
                };
                if target
                    .store
                    .get(&target.key)
                    .is_some_and(|row| row.proxy_url == proxy_url && row.state != TaskState::Failed)
                {
                    return true;
                }
                let task_id = target.key.task_id().to_owned();
                submit_task_edit(
                    &target,
                    DownloadsCommand::SetProxy { task_id, proxy_url },
                    EditKind::SetProxy,
                    window,
                    cx,
                );
                true
            })
    });
    let input = form_entity.read(cx).input.clone();
    input.update(cx, |state, cx| state.focus(window, cx));
}

/// 「更换链接」：URL 输入框预填当前地址，确认后下发 `ChangeUrl`。任务不满足条件时无操作。
pub(crate) fn open_change_url_dialog(target: TaskEditTarget, window: &mut Window, cx: &mut App) {
    if !target.allows(TaskMenuFacts::can_change_url) {
        return;
    }
    let Some(current_url) = target.store.get(&target.key).map(|row| row.url.clone()) else {
        return;
    };
    let title = target.strings.change_url_title.clone();
    let ok_label = target.strings.confirm.clone();
    let cancel_label = target.strings.cancel.clone();
    let translator = target.translator.clone();
    let strings = target.strings.clone();
    let form_entity = cx.new(|cx| UrlForm::new(&strings, translator, current_url, window, cx));
    let dialog_form = form_entity.clone();
    let target = Rc::new(target);
    window.open_dialog(cx, move |dialog, _, cx| {
        let content_form = dialog_form.clone();
        let ok_form = dialog_form.clone();
        let target = Rc::clone(&target);
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(px(520.))
            .content(move |content, _, _| content.child(content_form.clone()))
            .footer(dialog_footer(
                Some(cancel_label.clone()),
                ok_label.clone(),
                DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let url = match ok_form.update(cx, |form, cx| form.check(cx)) {
                    UrlCheck::Invalid => return false,
                    // 与当前链接相同：无需请求，直接关闭。
                    UrlCheck::Unchanged => return true,
                    UrlCheck::Changed(url) => url,
                };
                let task_id = target.key.task_id().to_owned();
                submit_task_edit(
                    &target,
                    DownloadsCommand::ChangeUrl { task_id, url },
                    EditKind::ChangeUrl,
                    window,
                    cx,
                );
                true
            })
    });
    let input = form_entity.read(cx).input.clone();
    input.update(cx, |state, cx| state.focus(window, cx));
}

/// 「修改线程数」：确认后下发 `SetSegments`。任务不满足条件时无操作。
pub(crate) fn open_change_segments_dialog(
    target: TaskEditTarget,
    window: &mut Window,
    cx: &mut App,
) {
    if !target.allows(TaskMenuFacts::can_change_segments) {
        return;
    }
    let title = target.strings.edit_threads_title.clone();
    let ok_label = target.strings.confirm.clone();
    let cancel_label = target.strings.cancel.clone();
    let translator = target.translator.clone();
    let strings = target.strings.clone();
    let form_entity = cx.new(|cx| SegmentsForm::new(&strings, translator, window, cx));
    let dialog_form = form_entity.clone();
    let target = Rc::new(target);
    window.open_dialog(cx, move |dialog, _, cx| {
        let content_form = dialog_form.clone();
        let ok_form = dialog_form.clone();
        let target = Rc::clone(&target);
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(px(520.))
            .content(move |content, _, _| content.child(content_form.clone()))
            .footer(dialog_footer(
                Some(cancel_label.clone()),
                ok_label.clone(),
                DialogIntent::Confirm,
                cx,
            ))
            .on_ok(move |_, window, cx| {
                let Some(segments) = ok_form.update(cx, |form, cx| form.parse(cx)) else {
                    return false;
                };
                let task_id = target.key.task_id().to_owned();
                submit_task_edit(
                    &target,
                    DownloadsCommand::SetSegments { task_id, segments },
                    EditKind::SetSegments,
                    window,
                    cx,
                );
                true
            })
    });
    let input = form_entity.read(cx).input.clone();
    input.update(cx, |state, cx| state.focus(window, cx));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    ChangeUrl,
    SetSegments,
    SetProxy,
}

/// 编辑命令本身失败，或编辑成功后补发的 `Resume` 失败。
enum EditFailure {
    Edit(RpcErrorData),
    Resume(RpcErrorData),
}

/// 下发编辑命令；成功后失败态任务串行补发 `Resume`。失败与成功提示推送到当前窗口。
fn submit_task_edit(
    target: &TaskEditTarget,
    command: DownloadsCommand,
    kind: EditKind,
    window: &mut Window,
    cx: &mut App,
) {
    // 提交时刻的状态决定是否补发 Resume（对话框打开期间状态可能已变）。
    let was_failed = kind != EditKind::SetProxy
        && target
            .store
            .get(&target.key)
            .is_some_and(|row| row.state == TaskState::Failed);
    let task_id = target.key.task_id().to_owned();
    let port = Arc::clone(&target.port);
    let translator = target.translator.clone();
    let success_notice = match kind {
        EditKind::SetSegments => Some(target.strings.threads_changed.clone()),
        EditKind::SetProxy => Some(SharedString::from(
            target.translator.read(cx).text("taskEditProxyApplied"),
        )),
        EditKind::ChangeUrl => None,
    };
    let handle = window.window_handle();
    let edit = port.execute(command);
    cx.spawn(async move |cx| {
        let result = match edit.await {
            Ok(_) if was_failed => port
                .execute(DownloadsCommand::Resume { task_id })
                .await
                .map(|_| ())
                .map_err(EditFailure::Resume),
            Ok(_) => Ok(()),
            Err(error) => Err(EditFailure::Edit(error)),
        };
        let notification = match result {
            Ok(()) => success_notice.map(Notification::success),
            Err(failure) => {
                let text = translator.read_with(cx, |translator, _| match (&failure, kind) {
                    (EditFailure::Edit(error), EditKind::ChangeUrl) => {
                        change_url_error_text(translator, error)
                    }
                    (EditFailure::Edit(error), EditKind::SetSegments) => {
                        set_segments_error_text(translator, error)
                    }
                    (EditFailure::Edit(error), EditKind::SetProxy) => error_text(translator, error),
                    (EditFailure::Resume(error), _) => error_text(translator, error),
                });
                Some(Notification::error(text))
            }
        };
        if let Some(notification) = notification
            && let Err(error) = handle.update(cx, |_, window, cx| {
                window.push_notification(notification, cx);
            })
        {
            eprintln!("window closed before the task edit result notice: {error:#}");
        }
    })
    .detach();
}

impl DownloadView {
    /// 唯一选中的本地任务 → 编辑上下文（条件由对话框入口按同一事实源再判）。
    fn single_edit_target(&self, cx: &Context<Self>) -> Option<TaskEditTarget> {
        let selected = self.table_state.read(cx).delegate().selected_keys();
        let [key] = selected.as_slice() else {
            return None;
        };
        Some(TaskEditTarget {
            key: key.clone(),
            store: Rc::clone(self.controller.store()),
            port: Arc::clone(&self.port),
            strings: self.strings.clone(),
            translator: self.translator.clone(),
        })
    }

    /// 右键 / 应用菜单「更换链接」。
    pub(crate) fn on_change_selected_url(
        &mut self,
        _: &ChangeSelectedUrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.single_edit_target(cx) {
            open_change_url_dialog(target, window, cx);
        }
    }

    /// 右键 / 应用菜单「修改线程数」。
    pub(crate) fn on_change_selected_segments(
        &mut self,
        _: &ChangeSelectedSegments,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.single_edit_target(cx) {
            open_change_segments_dialog(target, window, cx);
        }
    }
    /// 右键 / 应用菜单「编辑代理」。
    pub(crate) fn on_edit_selected_proxy(
        &mut self,
        _: &EditSelectedProxy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.single_edit_target(cx) {
            open_edit_proxy_dialog(target, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UrlError, parse_task_segments, validate_new_url};

    #[test]
    fn segments_input_accepts_zero_to_max_and_rejects_the_rest() {
        assert_eq!(parse_task_segments("0"), Some(0));
        assert_eq!(parse_task_segments(" 16 "), Some(16));
        assert_eq!(parse_task_segments("512"), Some(512));
        // 空串不等同于「自动」：必须显式输入，避免误清已设值。
        for input in ["", "  ", "abc", "-1", "513", "99999999999", "1.5"] {
            assert_eq!(parse_task_segments(input), None, "{input:?}");
        }
    }

    #[test]
    fn new_url_must_be_non_empty_and_a_supported_scheme() {
        let current = "https://a.example/f.bin";
        assert_eq!(validate_new_url(current, ""), Err(UrlError::Invalid));
        assert_eq!(validate_new_url(current, "   "), Err(UrlError::Invalid));
        for url in [
            "sftp://a/f",
            "ed2k://|file|a|1|h|/",
            "magnet:?xt=urn:btih:a",
            "a.com/f",
        ] {
            assert_eq!(
                validate_new_url(current, url),
                Err(UrlError::Unsupported),
                "{url}"
            );
        }
        assert_eq!(
            validate_new_url(current, "  HTTP://b.example/f.bin  "),
            Ok("HTTP://b.example/f.bin".to_owned())
        );
    }

    #[test]
    fn new_url_protocol_class_must_match_current_unless_thunder() {
        assert_eq!(
            validate_new_url("http://a/f", "https://b/f"),
            Ok("https://b/f".to_owned())
        );
        assert_eq!(
            validate_new_url("ftp://a/f", "FTP://b/f"),
            Ok("FTP://b/f".to_owned())
        );
        assert_eq!(
            validate_new_url("http://a/f", "ftp://b/f"),
            Err(UrlError::Mismatch)
        );
        assert_eq!(
            validate_new_url("ftp://a/f", "https://b/f"),
            Err(UrlError::Mismatch)
        );
        // thunder 需解码后才知真实协议，交给引擎判定。
        assert!(validate_new_url("ftp://a/f", "thunder://QUFodHRwOi8v").is_ok());
        // 当前链接本身不可换源（如 BT）：不受支持。
        assert_eq!(
            validate_new_url("magnet:?xt=urn:btih:a", "http://b/f"),
            Err(UrlError::Unsupported)
        );
    }

    #[test]
    fn task_proxy_validation_matches_supported_wire_values() {
        for value in [
            "",
            "direct://",
            "system://",
            "http://localhost:8080",
            "https://proxy.example:443/",
            "socks4://127.0.0.1:1080",
            "socks4a://proxy:1080",
            "socks5://user:pass@proxy:1080",
            "socks5h://proxy:1080",
        ] {
            assert!(super::valid_proxy_url(value), "{value:?}");
        }
        for value in [
            "HTTP://proxy:80",
            "ftp://proxy:21",
            "http://proxy",
            "http://proxy:0",
            "http://:8080",
            "http://proxy:8080/path",
            "http://proxy:8080?secret=value",
        ] {
            assert!(!super::valid_proxy_url(value), "{value:?}");
        }
    }
}
