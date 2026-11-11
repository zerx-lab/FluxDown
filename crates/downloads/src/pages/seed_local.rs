//! 「本机做种」对话框：把 daemon 主机上已有的文件 / 文件夹（可选现成 `.torrent`）登记为已完成
//! BT 任务并开始做种（`daemon.task.seedLocal`）。
//!
//! 入口与「打开种子文件」并列（文件菜单 / 命令面板 / 键盘动作）。路径指
//! daemon 所在主机：本机 agent 时可用系统选择器，输入框也可手填。该调用无现成种子时要为数据
//! 做哈希（分钟级），属于不自动重试的建任务提交：对话框在等待期间显示忙态并禁用提交，成功后
//! 关闭并提示，失败留在对话框内显示错误。

use std::sync::Arc;

use fluxdown_protocol::{DaemonSeedLocalParams, SeedLocalResultDto};
use fluxdown_ui_components::{
    BusyExt as _, ControlExt as _, FluxIcon, dialog_scroll_body, dialog_title, field_error,
    field_hint, form, form_field, input_with_action, option_group, option_row,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Subscription, Window,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    switch::Switch,
    v_flex,
};

use crate::{
    controller::{DownloadsCommand, DownloadsPort, DownloadsResult},
    model::format_bytes,
    strings::{DownloadStrings, error_text},
};

/// `daemon.task.seedLocal` 的结果归类：新建成功、info hash 已被现有任务持有（未新建），
/// 或只生成了种子文件（`createOnly`，未建任务）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SeedOutcome {
    Started,
    /// 携带现有任务名（可能为空）。
    Duplicate(String),
    /// 携带写出的 `.torrent` 路径。
    Saved(String),
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
            Self::Saved(path) => {
                Notification::success(translator.text_with("btCreateSaved", &[("path", path)]))
            }
        }
    }
}

/// 解析成功结果；无法解析（旧 daemon 只返回 `{taskId}` 亦可解析）视为已新建。
/// `create_only` = 请求的输出路径（只生成种子时）。只有结果带了 `torrentPath`，或确实没建任务
/// （`taskId` 为空）才算「已保存」；不认识 `createOnly` 的旧 daemon 会照常建任务并返回非空
/// `taskId`，此时按已新建报告，不谎称写出了种子文件。
fn seed_outcome(result: &DownloadsResult, create_only: Option<&str>) -> SeedOutcome {
    let dto = match result {
        DownloadsResult::Value(value) => {
            serde_json::from_value::<SeedLocalResultDto>(value.clone()).ok()
        }
        _ => None,
    };
    if let Some(requested) = create_only {
        match &dto {
            Some(dto) if !dto.torrent_path.is_empty() => {
                return SeedOutcome::Saved(dto.torrent_path.clone());
            }
            Some(dto) if !dto.task_id.is_empty() => {}
            _ => return SeedOutcome::Saved(requested.to_owned()),
        }
    }
    match dto {
        Some(dto) if dto.duplicate => SeedOutcome::Duplicate(dto.existing_name),
        _ => SeedOutcome::Started,
    }
}

/// 对话框宽度（默认字号下）。
const DIALOG_WIDTH: f32 = 520.;
/// 对话框正文的设计高度上限（超出滚动）。
const BODY_MAX_HEIGHT: f32 = 460.;
/// Tracker / Web 种子文本框自动增长的行数范围。
const TRACKER_ROWS: (usize, usize) = (3, 6);

/// 生成种子的格式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TorrentVersion {
    #[default]
    V1,
    V2,
    Hybrid,
}

impl TorrentVersion {
    const ALL: [Self; 3] = [Self::V1, Self::V2, Self::Hybrid];

    /// 线上取值；v1 是 daemon 缺省，发空串（不上线）。
    fn wire(self) -> &'static str {
        match self {
            Self::V1 => "",
            Self::V2 => "v2",
            Self::Hybrid => "hybrid",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::V1 => "btCreateVersionV1",
            Self::V2 => "btCreateVersionV2",
            Self::Hybrid => "btCreateVersionHybrid",
        }
    }
}

/// 分片大小选项：`0` = 自动，其后 16 KiB … 16 MiB 的 2 的幂。
fn piece_length_options() -> impl Iterator<Item = u32> {
    std::iter::once(0).chain((14..=24).map(|shift| 1u32 << shift))
}

fn piece_length_label(length: u32, auto: &SharedString) -> SharedString {
    if length == 0 {
        auto.clone()
    } else {
        SharedString::from(format_bytes(u64::from(length)))
    }
}

/// 按行拆分、trim 并丢弃空行。
fn non_blank_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// tracker 文本 → 参数：每行一个地址，空行分隔层级（BEP 12，同 qBittorrent；上线为空串）；
/// 首尾与连续空行只算一个分隔。
fn tracker_tier_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim) {
        if !line.is_empty() {
            out.push(line.to_owned());
        } else if out.last().is_some_and(|last| !last.is_empty()) {
            out.push(String::new());
        }
    }
    if out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// 表单原始值（提交时从各输入框读取）。
#[derive(Debug, Default)]
struct SeedLocalValues<'a> {
    path: &'a str,
    torrent_path: &'a str,
    trackers: &'a str,
    unlimited: bool,
    version: TorrentVersion,
    piece_length: u32,
    private: bool,
    web_seeds: &'a str,
    comment: &'a str,
    source: &'a str,
    torrent_output: &'a str,
    start_seeding: bool,
    super_seeding: bool,
    skip_check: bool,
}

/// 表单校验错误。
#[derive(Debug, PartialEq, Eq)]
enum SeedFormError {
    PathRequired,
    /// 不立即做种（只生成种子）却没给输出路径。
    OutputRequired,
}

/// 表单 → 提交参数。路径 trim 后为空 → `PathRequired`。现成种子路径 trim 后为空视为未提供：
/// 提供时 tracker 与全部生成选项一律忽略（种子自带），`skip_check` 只在此模式上线；
/// 否则为现场生成模式，`skip_check` 不上线，不立即做种时必须给输出路径且不开超级做种。
fn build_seed_params(values: &SeedLocalValues<'_>) -> Result<DaemonSeedLocalParams, SeedFormError> {
    let path = values.path.trim();
    if path.is_empty() {
        return Err(SeedFormError::PathRequired);
    }
    let torrent_path = values.torrent_path.trim();
    if !torrent_path.is_empty() {
        return Ok(DaemonSeedLocalParams {
            path: path.to_owned(),
            torrent_path: Some(torrent_path.to_owned()),
            unlimited: values.unlimited,
            super_seeding: values.super_seeding,
            skip_check: values.skip_check,
            ..Default::default()
        });
    }
    let torrent_output = values.torrent_output.trim();
    let create_only = !values.start_seeding;
    if create_only && torrent_output.is_empty() {
        return Err(SeedFormError::OutputRequired);
    }
    Ok(DaemonSeedLocalParams {
        path: path.to_owned(),
        torrent_path: None,
        trackers: tracker_tier_lines(values.trackers),
        unlimited: values.unlimited,
        version: values.version.wire().to_owned(),
        private: values.private,
        piece_length: values.piece_length,
        web_seeds: non_blank_lines(values.web_seeds),
        comment: values.comment.trim().to_owned(),
        source: values.source.trim().to_owned(),
        super_seeding: values.super_seeding && !create_only,
        torrent_output: torrent_output.to_owned(),
        create_only,
        ..Default::default()
    })
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
    web_seeds: Entity<TextareaState>,
    comment: Entity<InputState>,
    source: Entity<InputState>,
    torrent_output: Entity<InputState>,
    unlimited: bool,
    /// 「种子选项」是否展开（默认折叠）。
    options_open: bool,
    version: TorrentVersion,
    piece_length: u32,
    private: bool,
    start_seeding: bool,
    super_seeding: bool,
    skip_check: bool,
    /// 提交在途（哈希可能持续数分钟）。
    busy: bool,
    /// 系统选择器打开中。
    picking: bool,
    error: Option<SharedString>,
    /// 输入变化即清错误并重绘（现成种子是否填写决定 tracker 框 / 生成选项是否显示）。
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
        let output_placeholder = SharedString::from(
            translator
                .read(cx)
                .text("btCreateTorrentOutputPlaceholder")
                .to_owned(),
        );
        let path = cx.new(|cx| {
            InputState::new(window, cx).placeholder(strings.seed_local_path_placeholder.clone())
        });
        let torrent = cx.new(|cx| InputState::new(window, cx));
        let trackers =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(TRACKER_ROWS.0, TRACKER_ROWS.1));
        let web_seeds =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(TRACKER_ROWS.0, TRACKER_ROWS.1));
        let comment = cx.new(|cx| InputState::new(window, cx));
        let source = cx.new(|cx| InputState::new(window, cx));
        let torrent_output =
            cx.new(|cx| InputState::new(window, cx).placeholder(output_placeholder));
        let mut watch = Vec::with_capacity(7);
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
        for input in [&trackers, &web_seeds] {
            watch.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
                }
            }));
        }
        for input in [&comment, &source, &torrent_output] {
            watch.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
                }
            }));
        }
        Self {
            translator,
            port,
            strings,
            path,
            torrent,
            trackers,
            web_seeds,
            comment,
            source,
            torrent_output,
            unlimited: false,
            options_open: false,
            version: TorrentVersion::default(),
            piece_length: 0,
            private: false,
            start_seeding: true,
            super_seeding: false,
            skip_check: false,
            busy: false,
            picking: false,
            error: None,
            _watch: watch,
        }
    }

    fn t(&self, key: &str, cx: &App) -> SharedString {
        SharedString::from(self.translator.read(cx).text(key).to_owned())
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
        let path = self.path.read(cx).value();
        let torrent_path = self.torrent.read(cx).value();
        let trackers = self.trackers.read(cx).value();
        let web_seeds = self.web_seeds.read(cx).value();
        let comment = self.comment.read(cx).value();
        let source = self.source.read(cx).value();
        let torrent_output = self.torrent_output.read(cx).value();
        let built = build_seed_params(&SeedLocalValues {
            path: &path,
            torrent_path: &torrent_path,
            trackers: &trackers,
            unlimited: self.unlimited,
            version: self.version,
            piece_length: self.piece_length,
            private: self.private,
            web_seeds: &web_seeds,
            comment: &comment,
            source: &source,
            torrent_output: &torrent_output,
            start_seeding: self.start_seeding,
            super_seeding: self.super_seeding,
            skip_check: self.skip_check,
        });
        let params = match built {
            Ok(params) => params,
            Err(SeedFormError::PathRequired) => {
                self.error = Some(self.strings.seed_local_err_path_required.clone());
                cx.notify();
                return;
            }
            Err(SeedFormError::OutputRequired) => {
                self.error = Some(self.t("btCreateOutputRequired", cx));
                self.options_open = true;
                cx.notify();
                return;
            }
        };
        let create_only = params.create_only.then(|| params.torrent_output.clone());
        self.busy = true;
        self.error = None;
        cx.notify();
        let future = self
            .port
            .execute(DownloadsCommand::SeedLocal(Box::new(params)));
        let translator = self.translator.clone();
        let strings = self.strings.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;
            let outcome = result
                .as_ref()
                .ok()
                .map(|result| seed_outcome(result, create_only.as_deref()));
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

    /// 「种子选项」正文（只在现场生成模式出现）。
    fn render_create_options(&self, busy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();

        let current_version = self.version;
        let version_options: Vec<(TorrentVersion, SharedString)> = TorrentVersion::ALL
            .iter()
            .map(|version| (*version, self.t(version.label_key(), cx)))
            .collect();
        let this = cx.weak_entity();
        let version_picker = Button::new("seed-local-version")
            .outline()
            .control(cx)
            .label(self.t(current_version.label_key(), cx))
            .disabled(busy)
            .dropdown_caret(true)
            .w_full()
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                version_options.iter().fold(menu, |menu, (value, label)| {
                    let value = *value;
                    let this = this.clone();
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(value == current_version)
                            .on_click(move |_, _, cx| {
                                let Ok(()) = this.update(cx, |this, cx| {
                                    this.version = value;
                                    cx.notify();
                                }) else {
                                    // 对话框已释放，结束这次选择。
                                    return;
                                };
                            }),
                    )
                })
            });

        let current_piece = self.piece_length;
        let auto = self.t("btCreatePieceLengthAuto", cx);
        let piece_options: Vec<(u32, SharedString)> = piece_length_options()
            .map(|length| (length, piece_length_label(length, &auto)))
            .collect();
        let this = cx.weak_entity();
        let piece_picker = Button::new("seed-local-piece-length")
            .outline()
            .control(cx)
            .label(piece_length_label(current_piece, &auto))
            .disabled(busy)
            .dropdown_caret(true)
            .w_full()
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                piece_options.iter().fold(menu, |menu, (value, label)| {
                    let value = *value;
                    let this = this.clone();
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(value == current_piece)
                            .on_click(move |_, _, cx| {
                                let Ok(()) = this.update(cx, |this, cx| {
                                    this.piece_length = value;
                                    cx.notify();
                                }) else {
                                    // 对话框已释放，结束这次选择。
                                    return;
                                };
                            }),
                    )
                })
            });

        let switches = option_group(
            [
                option_row(
                    self.t("btCreatePrivate", cx),
                    Some(self.t("btCreatePrivateDesc", cx)),
                    Switch::new("seed-local-private")
                        .checked(self.private)
                        .disabled(busy)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.private = *checked;
                            cx.notify();
                        })),
                    cx,
                )
                .into_any_element(),
                option_row(
                    self.t("btCreateStartSeeding", cx),
                    None,
                    Switch::new("seed-local-start-seeding")
                        .checked(self.start_seeding)
                        .disabled(busy)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.start_seeding = *checked;
                            this.error = None;
                            cx.notify();
                        })),
                    cx,
                )
                .into_any_element(),
            ],
            cx,
        );

        v_flex()
            .w_full()
            .gap(tokens.spacing.md)
            .child(form_field(
                self.t("btCreateVersion", cx),
                version_picker,
                None,
                cx,
            ))
            .child(form_field(
                self.t("btCreatePieceLength", cx),
                piece_picker,
                None,
                cx,
            ))
            .child(form_field(
                self.t("btCreateWebSeeds", cx),
                Textarea::new(&self.web_seeds)
                    .disabled(busy)
                    .w_full()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height),
                Some(self.t("btCreateWebSeedsHint", cx)),
                cx,
            ))
            .child(form_field(
                self.t("btCreateComment", cx),
                Input::new(&self.comment)
                    .control(cx)
                    .disabled(busy)
                    .w_full(),
                None,
                cx,
            ))
            .child(form_field(
                self.t("btCreateSource", cx),
                Input::new(&self.source).control(cx).disabled(busy).w_full(),
                Some(self.t("btCreateSourceHint", cx)),
                cx,
            ))
            .child(form_field(
                self.t("btCreateTorrentOutput", cx),
                Input::new(&self.torrent_output)
                    .control(cx)
                    .disabled(busy)
                    .w_full(),
                None,
                cx,
            ))
            .child(switches)
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

        // 超级做种：只生成种子（不立即做种）时无意义。
        let super_seeding_enabled = has_torrent || self.start_seeding;
        let mut rows = vec![
            option_row(
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
            .into_any_element(),
            option_row(
                self.t("btSuperSeeding", cx),
                Some(self.t("btSuperSeedingDesc", cx)),
                Switch::new("seed-local-super-seeding")
                    .checked(self.super_seeding && super_seeding_enabled)
                    .disabled(busy || !super_seeding_enabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.super_seeding = *checked;
                        cx.notify();
                    })),
                cx,
            )
            .into_any_element(),
        ];
        if has_torrent {
            rows.push(
                option_row(
                    self.t("btSeedSkipCheck", cx),
                    Some(self.t("btSeedSkipCheckDesc", cx)),
                    Switch::new("seed-local-skip-check")
                        .checked(self.skip_check)
                        .disabled(busy)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.skip_check = *checked;
                            cx.notify();
                        })),
                    cx,
                )
                .into_any_element(),
            );
        }
        let switches = option_group(rows, cx);

        let options_open = self.options_open;
        let options_section = (!has_torrent).then(|| {
            v_flex()
                .w_full()
                .gap(tokens.spacing.md)
                .child(
                    h_flex().child(
                        Button::new("seed-local-options-toggle")
                            .ghost()
                            .icon(if options_open {
                                FluxIcon::ChevronDown
                            } else {
                                FluxIcon::ChevronRight
                            })
                            .label(self.t("btCreateOptions", cx))
                            .control(cx)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.options_open = !this.options_open;
                                cx.notify();
                            })),
                    ),
                )
                .when(options_open, |this| {
                    this.child(self.render_create_options(busy, cx))
                })
        });

        let body = v_flex()
            .w_full()
            .gap(tokens.spacing.lg)
            .child(field_hint(strings.seed_local_desc.clone(), cx))
            .child(
                form(cx)
                    .child(path_field)
                    .child(torrent_field)
                    .when(!has_torrent, |this| this.child(trackers_field))
                    .child(switches)
                    .children(options_section),
            );

        v_flex()
            .w_full()
            .min_h_0()
            .gap(tokens.spacing.lg)
            .child(dialog_scroll_body(
                "seed-local-body",
                Some(px(BODY_MAX_HEIGHT)),
                body,
                cx,
            ))
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
                            .label(self.t("cancel", cx))
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
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    path_input.update(cx, |input, cx| input.focus(window, cx));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        DownloadsResult, SeedFormError, SeedLocalValues, SeedOutcome, TorrentVersion,
        build_seed_params, non_blank_lines, piece_length_options, seed_outcome,
    };

    fn value(value: serde_json::Value) -> DownloadsResult {
        DownloadsResult::Value(value)
    }

    #[test]
    fn seed_outcome_distinguishes_duplicate_from_started() {
        assert_eq!(
            seed_outcome(&value(json!({ "taskId": "t1" })), None),
            SeedOutcome::Started
        );
        assert_eq!(
            seed_outcome(&DownloadsResult::Unit, None),
            SeedOutcome::Started
        );
        assert_eq!(
            seed_outcome(
                &value(
                    json!({ "taskId": "t1", "duplicate": true, "existingName": "电影 2024.mkv" })
                ),
                None
            ),
            SeedOutcome::Duplicate("电影 2024.mkv".to_owned())
        );
        assert_eq!(
            seed_outcome(&value(json!({ "taskId": "t1", "duplicate": true })), None),
            SeedOutcome::Duplicate(String::new())
        );
        // 做种同时写出种子：仍是「已开始」提示。
        assert_eq!(
            seed_outcome(
                &value(json!({ "taskId": "t1", "torrentPath": "/out/a.torrent" })),
                None
            ),
            SeedOutcome::Started
        );
    }

    #[test]
    fn create_only_outcome_reports_saved_path() {
        assert_eq!(
            seed_outcome(
                &value(json!({ "taskId": "", "torrentPath": r"C:\种子\电影 2024.torrent" })),
                Some(r"C:\种子\requested.torrent")
            ),
            SeedOutcome::Saved(r"C:\种子\电影 2024.torrent".to_owned())
        );
        // 没建任务（taskId 为空）但未回 torrentPath / 回 Unit：落回请求的输出路径。
        assert_eq!(
            seed_outcome(&value(json!({ "taskId": "" })), Some("/out/a.torrent")),
            SeedOutcome::Saved("/out/a.torrent".to_owned())
        );
        assert_eq!(
            seed_outcome(&DownloadsResult::Unit, Some("/out/a.torrent")),
            SeedOutcome::Saved("/out/a.torrent".to_owned())
        );
        // 不认识 createOnly 的旧 daemon：照常建了任务（非空 taskId、无 torrentPath），
        // 不能报「已保存」。
        assert_eq!(
            seed_outcome(&value(json!({ "taskId": "t1" })), Some("/out/a.torrent")),
            SeedOutcome::Started
        );
        assert_eq!(
            seed_outcome(
                &value(json!({ "taskId": "t0", "duplicate": true, "existingName": "电影" })),
                Some("/out/a.torrent")
            ),
            SeedOutcome::Duplicate("电影".to_owned())
        );
        // 回了 torrentPath 即以结果路径为准（无论 taskId）。
        assert_eq!(
            seed_outcome(
                &value(json!({ "taskId": "t2", "torrentPath": "/out/b.torrent" })),
                Some("/out/a.torrent")
            ),
            SeedOutcome::Saved("/out/b.torrent".to_owned())
        );
    }

    #[test]
    fn empty_path_is_rejected() {
        assert_eq!(
            build_seed_params(&SeedLocalValues {
                start_seeding: true,
                ..Default::default()
            }),
            Err(SeedFormError::PathRequired)
        );
        assert_eq!(
            build_seed_params(&SeedLocalValues {
                path: "  \t ",
                torrent_path: "/a.torrent",
                trackers: "udp://t",
                unlimited: true,
                start_seeding: true,
                ..Default::default()
            }),
            Err(SeedFormError::PathRequired)
        );
    }

    #[test]
    fn generate_mode_builds_full_params() {
        let params = build_seed_params(&SeedLocalValues {
            path: "  /data/Movies 2024  ",
            torrent_path: "   ",
            trackers: "\n  udp://tracker.one:6969/announce \r\nudp://tracker.three/announce\n\n   \nhttps://tracker.two/announce\n\n",
            unlimited: true,
            version: TorrentVersion::Hybrid,
            piece_length: 1 << 20,
            private: true,
            web_seeds: "\n  https://例子.测试/电影 2024/  \n\n http://mirror.example/a\n",
            comment: "  评论 🎬 ",
            source: " PT-站 ",
            torrent_output: " /out/a.torrent ",
            start_seeding: true,
            super_seeding: true,
            skip_check: true,
        })
        .expect("valid");
        assert_eq!(params.path, "/data/Movies 2024");
        assert_eq!(params.torrent_path, None);
        // 空行分隔层级：连续空行与首尾空行只算一个分隔。
        assert_eq!(
            params.trackers,
            vec![
                "udp://tracker.one:6969/announce".to_owned(),
                "udp://tracker.three/announce".to_owned(),
                String::new(),
                "https://tracker.two/announce".to_owned(),
            ]
        );
        assert!(params.unlimited);
        assert_eq!(params.version, "hybrid");
        assert_eq!(params.piece_length, 1 << 20);
        assert!(params.private);
        assert_eq!(
            params.web_seeds,
            vec![
                "https://例子.测试/电影 2024/".to_owned(),
                "http://mirror.example/a".to_owned(),
            ]
        );
        assert_eq!(params.comment, "评论 🎬");
        assert_eq!(params.source, "PT-站");
        assert_eq!(params.torrent_output, "/out/a.torrent");
        assert!(params.super_seeding);
        assert!(!params.create_only);
        // skip_check 只属于现成种子模式。
        assert!(!params.skip_check);
    }

    #[test]
    fn existing_torrent_ignores_create_options() {
        let params = build_seed_params(&SeedLocalValues {
            path: r"D:\共享 数据",
            torrent_path: r" \\nas\share\file.torrent ",
            trackers: "udp://tracker.one:6969/announce",
            version: TorrentVersion::V2,
            piece_length: 1 << 16,
            private: true,
            web_seeds: "https://a/b",
            comment: "c",
            source: "s",
            torrent_output: "",
            start_seeding: false,
            super_seeding: true,
            skip_check: true,
            ..Default::default()
        })
        .expect("valid even with start seeding off");
        assert_eq!(params.path, r"D:\共享 数据");
        assert_eq!(
            params.torrent_path.as_deref(),
            Some(r"\\nas\share\file.torrent")
        );
        assert!(params.trackers.is_empty());
        assert_eq!(params.version, "");
        assert_eq!(params.piece_length, 0);
        assert!(!params.private);
        assert!(params.web_seeds.is_empty());
        assert!(params.comment.is_empty() && params.source.is_empty());
        assert!(params.torrent_output.is_empty());
        assert!(!params.create_only);
        assert!(params.super_seeding);
        assert!(params.skip_check);
    }

    #[test]
    fn create_only_requires_output_and_drops_super_seeding() {
        let mut values = SeedLocalValues {
            path: "/data/file.iso",
            torrent_output: "   ",
            start_seeding: false,
            super_seeding: true,
            ..Default::default()
        };
        assert_eq!(
            build_seed_params(&values),
            Err(SeedFormError::OutputRequired)
        );
        values.torrent_output = "/out/file.torrent";
        let params = build_seed_params(&values).expect("output given");
        assert!(params.create_only);
        assert!(!params.super_seeding);
        assert_eq!(params.torrent_output, "/out/file.torrent");
    }

    #[test]
    fn version_and_piece_length_mapping() {
        assert_eq!(TorrentVersion::V1.wire(), "");
        assert_eq!(TorrentVersion::V2.wire(), "v2");
        assert_eq!(TorrentVersion::Hybrid.wire(), "hybrid");
        let lengths: Vec<u32> = piece_length_options().collect();
        assert_eq!(lengths.first(), Some(&0));
        assert_eq!(lengths.get(1), Some(&16_384));
        assert_eq!(lengths.last(), Some(&(16 * 1024 * 1024)));
        assert_eq!(lengths.len(), 12);
        assert!(lengths[1..].iter().all(|length| length.is_power_of_two()));
    }

    #[test]
    fn blank_lines_are_dropped() {
        assert!(non_blank_lines(" \n\r\n\t").is_empty());
        assert_eq!(
            non_blank_lines("a\r\n b "),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }
}
