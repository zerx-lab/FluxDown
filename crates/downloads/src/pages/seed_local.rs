//! 「本机做种」对话框：把 daemon 主机上已有的文件 / 文件夹（可选现成 `.torrent`）登记为已完成
//! BT 任务并开始做种（`daemon.task.seedLocal`）。
//!
//! 入口与「打开种子文件」并列（文件菜单 / 命令面板 / 键盘动作）。路径指
//! daemon 所在主机：本机 agent 时可用系统选择器，输入框也可手填。该调用无现成种子时要为数据
//! 做哈希（分钟级），属于不自动重试的建任务提交：对话框在等待期间显示忙态并禁用提交，成功后
//! 关闭并提示，失败留在对话框内显示错误。

use std::sync::Arc;

use fluxdown_protocol::SeedLocalResultDto;
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, FluxIcon, dialog_title, field_error, field_hint, form,
    form_field, input_with_action, option_group, option_row,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Subscription, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    notification::Notification,
    switch::Switch,
    v_flex,
};

use crate::{
    controller::{DownloadsCommand, DownloadsPort, DownloadsResult},
    strings::{DownloadStrings, error_text},
};

/// `daemon.task.seedLocal` 的结果归类：新建成功，或 info hash 已被现有任务持有（未新建）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SeedOutcome {
    Started,
    /// 携带现有任务名（可能为空）。
    Duplicate(String),
}

impl SeedOutcome {
    fn notification(&self, strings: &DownloadStrings, translator: &Translator) -> Notification {
        match self {
            Self::Started => Notification::success(strings.seed_local_started.clone()),
            Self::Duplicate(name) if name.is_empty() => {
                Notification::warning(translator.text("duplicateTorrentToastUnnamed").to_owned())
            }
            Self::Duplicate(name) => Notification::warning(
                translator.text_with("duplicateTorrentToast", &[("name", name)]),
            ),
        }
    }
}

/// 解析成功结果；无法解析（旧 daemon 只返回 `{taskId}` 亦可解析）视为已新建。
fn seed_outcome(result: &DownloadsResult) -> SeedOutcome {
    let DownloadsResult::Value(value) = result else {
        return SeedOutcome::Started;
    };
    match serde_json::from_value::<SeedLocalResultDto>(value.clone()) {
        Ok(dto) if dto.duplicate => SeedOutcome::Duplicate(dto.existing_name),
        _ => SeedOutcome::Started,
    }
}

/// 对话框宽度（默认字号下）。
const DIALOG_WIDTH: f32 = 520.;
/// Tracker 文本框自动增长的行数范围。
const TRACKER_ROWS: (usize, usize) = (3, 6);

/// 表单 → 提交参数。
#[derive(Debug, PartialEq, Eq)]
struct SeedLocalRequest {
    path: String,
    torrent_path: Option<String>,
    trackers: Vec<String>,
    unlimited: bool,
}

impl SeedLocalRequest {
    /// 路径 trim 后为空 → `None`（调用方显示「请选择要做种的文件或文件夹」）。
    /// 现成种子路径 trim 后为空视为未提供；提供时 tracker 一律忽略（种子自带 announce），
    /// 否则按行拆分、trim 并丢弃空行。
    fn from_form(path: &str, torrent_path: &str, trackers: &str, unlimited: bool) -> Option<Self> {
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        let torrent_path = Some(torrent_path.trim())
            .filter(|torrent| !torrent.is_empty())
            .map(str::to_owned);
        let trackers = if torrent_path.is_some() {
            Vec::new()
        } else {
            trackers
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        };
        Some(Self {
            path: path.to_owned(),
            torrent_path,
            trackers,
            unlimited,
        })
    }

    fn into_command(self) -> DownloadsCommand {
        DownloadsCommand::SeedLocal {
            path: self.path,
            torrent_path: self.torrent_path,
            trackers: self.trackers,
            unlimited: self.unlimited,
        }
    }
}

/// 选择器的目标字段与形态。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    File,
    Folder,
    Torrent,
}

struct SeedLocalForm {
    translator: Entity<Translator>,
    port: Arc<dyn DownloadsPort>,
    strings: DownloadStrings,
    path: Entity<InputState>,
    torrent: Entity<InputState>,
    trackers: Entity<TextareaState>,
    unlimited: bool,
    /// 提交在途（哈希可能持续数分钟）。
    busy: bool,
    /// 系统选择器打开中。
    picking: bool,
    error: Option<SharedString>,
    /// 输入变化即清错误并重绘（现成种子是否填写决定 tracker 框是否显示）。
    _watch: Vec<Subscription>,
}

impl SeedLocalForm {
    fn new(
        port: Arc<dyn DownloadsPort>,
        strings: DownloadStrings,
        translator: Entity<Translator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path = cx.new(|cx| {
            InputState::new(window, cx).placeholder(strings.seed_local_path_placeholder.clone())
        });
        let torrent = cx.new(|cx| InputState::new(window, cx));
        let trackers =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(TRACKER_ROWS.0, TRACKER_ROWS.1));
        let mut watch = Vec::with_capacity(3);
        for input in [&path, &torrent] {
            watch.push(cx.subscribe_in(
                input,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.error = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    _ => {}
                },
            ));
        }
        watch.push(cx.subscribe(&trackers, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                cx.notify();
            }
        }));
        Self {
            translator,
            port,
            strings,
            path,
            torrent,
            trackers,
            unlimited: false,
            busy: false,
            picking: false,
            error: None,
            _watch: watch,
        }
    }

    fn pick(&mut self, target: Pick, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking || self.busy {
            return;
        }
        self.picking = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: target != Pick::Folder,
            directories: target == Pick::Folder,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking = false;
                if let Some(picked) = picked {
                    let input = if target == Pick::Torrent {
                        &this.torrent
                    } else {
                        &this.path
                    };
                    let text = picked.display().to_string();
                    input.update(cx, |input, cx| input.set_value(text, window, cx));
                }
                cx.notify();
            }) else {
                // 对话框或窗口已释放，不再回写选择结果。
                return;
            };
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(request) = SeedLocalRequest::from_form(
            &self.path.read(cx).value(),
            &self.torrent.read(cx).value(),
            &self.trackers.read(cx).value(),
            self.unlimited,
        ) else {
            self.error = Some(self.strings.seed_local_err_path_required.clone());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let future = self.port.execute(request.into_command());
        let translator = self.translator.clone();
        let strings = self.strings.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let outcome = result.as_ref().ok().map(seed_outcome);
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match (result, outcome.clone()) {
                    (Ok(_), Some(outcome)) => {
                        window.close_dialog(cx);
                        window.push_notification(
                            outcome.notification(&this.strings, this.translator.read(cx)),
                            cx,
                        );
                    }
                    (Err(error), _) => {
                        this.error = Some(SharedString::from(error_text(
                            this.translator.read(cx),
                            &error,
                        )));
                    }
                    (Ok(_), None) => {}
                }
                cx.notify();
            }) else {
                // 等待期间对话框已被关掉：任务照常登记（或判定为重复），提示仍推送到窗口。
                if let Some(outcome) = outcome
                    && let Err(error) = cx.update(|window, cx| {
                        let notification = outcome.notification(&strings, translator.read(cx));
                        window.push_notification(notification, cx);
                    })
                {
                    eprintln!("window closed before the seed-local result notice: {error:#}");
                }
                return;
            };
        })
        .detach();
    }
}

impl Render for SeedLocalForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        let busy = self.busy;
        let locked = busy || self.picking;
        let has_torrent = !self.torrent.read(cx).value().trim().is_empty();
        let strings = self.strings.clone();

        let pick_button = |id: &'static str,
                           icon: FluxIcon,
                           label: SharedString,
                           target: Pick,
                           cx: &mut Context<Self>| {
            Button::new(id)
                .ghost()
                .icon(icon)
                .label(label)
                .control(cx)
                .disabled(locked)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.pick(target, window, cx);
                }))
        };

        let path_field = form_field(
            strings.seed_local_path.clone(),
            v_flex()
                .w_full()
                .gap(tokens.spacing.sm)
                .child(Input::new(&self.path).control(cx).disabled(busy).w_full())
                .child(
                    h_flex()
                        .gap(tokens.spacing.sm)
                        .child(pick_button(
                            "seed-local-pick-file",
                            FluxIcon::FileText,
                            strings.seed_local_pick_file.clone(),
                            Pick::File,
                            cx,
                        ))
                        .child(pick_button(
                            "seed-local-pick-folder",
                            FluxIcon::FolderOpen,
                            strings.seed_local_pick_folder.clone(),
                            Pick::Folder,
                            cx,
                        )),
                ),
            None,
            cx,
        );
        let torrent_field = form_field(
            strings.seed_local_torrent.clone(),
            input_with_action(
                Input::new(&self.torrent)
                    .control(cx)
                    .disabled(busy)
                    .w_full(),
                pick_button(
                    "seed-local-pick-torrent",
                    FluxIcon::Magnet,
                    strings.seed_local_pick_torrent.clone(),
                    Pick::Torrent,
                    cx,
                ),
                cx,
            ),
            Some(strings.seed_local_torrent_hint.clone()),
            cx,
        );
        let trackers_field = form_field(
            strings.seed_local_trackers.clone(),
            Textarea::new(&self.trackers)
                .disabled(busy)
                .w_full()
                .text_size(tokens.typography.sm.size)
                .line_height(tokens.typography.sm.line_height),
            Some(strings.seed_local_trackers_hint.clone()),
            cx,
        );
        let unlimited = option_group(
            [option_row(
                strings.seed_local_unlimited.clone(),
                Some(strings.seed_local_unlimited_desc.clone()),
                Switch::new("seed-local-unlimited")
                    .checked(self.unlimited)
                    .disabled(busy)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.unlimited = *checked;
                        cx.notify();
                    })),
                cx,
            )
            .into_any_element()],
            cx,
        );

        v_flex()
            .w_full()
            .gap(tokens.spacing.lg)
            .child(field_hint(strings.seed_local_desc.clone(), cx))
            .child(
                form(cx)
                    .child(path_field)
                    .child(torrent_field)
                    .when(!has_torrent, |this| this.child(trackers_field))
                    .child(unlimited),
            )
            .when(busy, |this| {
                this.child(field_hint(strings.seed_local_working.clone(), cx))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(field_error(error, cx))
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap(tokens.spacing.sm)
                    .child(
                        Button::new("seed-local-cancel")
                            .outline()
                            .label(SharedString::from(
                                self.translator.read(cx).text("cancel").to_owned(),
                            ))
                            .control(cx)
                            .disabled(busy)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("seed-local-submit")
                            .primary()
                            .label(strings.seed_local_submit.clone())
                            .control(cx)
                            .busy(busy)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}

/// 打开「本机做种」对话框。
pub(crate) fn open_seed_local_dialog(
    port: Arc<dyn DownloadsPort>,
    strings: DownloadStrings,
    translator: Entity<Translator>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = strings.seed_local_title.clone();
    let view = cx.new(|cx| SeedLocalForm::new(port, strings, translator, window, cx));
    let path_input = view.read(cx).path.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = view.clone();
        dialog
            .title(dialog_title(title.clone(), cx))
            .w(active_theme(cx).text_extent(DIALOG_WIDTH))
            // 回车由路径输入框的 `PressEnter` 提交；默认确认会抢先关窗。
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    path_input.update(cx, |input, cx| input.focus(window, cx));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DownloadsResult, SeedLocalRequest, SeedOutcome, seed_outcome};

    #[test]
    fn seed_outcome_distinguishes_duplicate_from_started() {
        let value = |value| DownloadsResult::Value(value);
        assert_eq!(
            seed_outcome(&value(json!({ "taskId": "t1" }))),
            SeedOutcome::Started
        );
        assert_eq!(seed_outcome(&DownloadsResult::Unit), SeedOutcome::Started);
        assert_eq!(
            seed_outcome(&value(
                json!({ "taskId": "t1", "duplicate": true, "existingName": "电影 2024.mkv" })
            )),
            SeedOutcome::Duplicate("电影 2024.mkv".to_owned())
        );
        assert_eq!(
            seed_outcome(&value(json!({ "taskId": "t1", "duplicate": true }))),
            SeedOutcome::Duplicate(String::new())
        );
    }

    #[test]
    fn empty_path_is_rejected() {
        assert_eq!(SeedLocalRequest::from_form("", "", "", false), None);
        assert_eq!(
            SeedLocalRequest::from_form("  \t ", "/a.torrent", "udp://t", true),
            None
        );
    }

    #[test]
    fn path_is_trimmed_and_torrent_defaults_to_generated() {
        let request = SeedLocalRequest::from_form("  /data/Movies 2024  ", "   ", "", false)
            .expect("path present");
        assert_eq!(
            request,
            SeedLocalRequest {
                path: "/data/Movies 2024".to_owned(),
                torrent_path: None,
                trackers: Vec::new(),
                unlimited: false,
            }
        );
    }

    #[test]
    fn trackers_split_by_line_trim_and_skip_blanks() {
        let request = SeedLocalRequest::from_form(
            r"D:\共享 数据",
            "",
            "  udp://tracker.one:6969/announce \r\n\n   \nhttps://tracker.two/announce\n",
            true,
        )
        .expect("path present");
        assert_eq!(request.path, r"D:\共享 数据");
        assert_eq!(
            request.trackers,
            vec![
                "udp://tracker.one:6969/announce".to_owned(),
                "https://tracker.two/announce".to_owned(),
            ]
        );
        assert!(request.unlimited);
    }

    #[test]
    fn existing_torrent_ignores_trackers() {
        let request = SeedLocalRequest::from_form(
            "/data/file.iso",
            " /data/file.torrent ",
            "udp://tracker.one:6969/announce",
            false,
        )
        .expect("path present");
        assert_eq!(request.torrent_path.as_deref(), Some("/data/file.torrent"));
        assert!(request.trackers.is_empty());
    }
}
