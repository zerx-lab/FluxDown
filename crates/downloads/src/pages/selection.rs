//! 引擎发起的交互选择（HLS 画质 / BT 文件 / 插件变体）弹出窗口内容。
//!
//! 每个 [`fluxdown_protocol::SelectionRequestDto`] 对应一个独立 `Floating` 窗口
//! （见 `crate::windows::selection`，app 侧）；窗口的开启/关闭由 app 监听会话事件
//! 完成，本视图只负责收集用户选择并调用 `daemon.selection.resolve`。
//! `FileExists` 不走本视图，由 [`crate::FileConflictView`] 聚合承载。
//! BT 文件选择带「保存目录」：预填任务当前目录，用户改了才在答复里带回，
//! 引擎在真正下载前搬目录并重开任务。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::{
    BtFileDto, CustomCategoryDto, HlsQualityOptionDto, PluginCandidateDto, ResolveVariantOptionDto,
    SelectionKind, SelectionOutcome, SelectionRequestDto, SelectionResolutionDto,
};
use fluxdown_ui_components::toast;
use fluxdown_ui_components::{
    BusyExt as _, CheckState, ControlExt as _, FluxIcon, check_mark, check_row, field_hint,
    form_field, input_with_action, tabular_numbers, tooltip_view,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    Disableable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};

use super::download_name::DownloadName;
use crate::{
    controller::{DownloadsCommand, DownloadsPort},
    model::{CategoryIndex, format_bytes},
};

mod plugin_choice;

const BT_CATEGORY_KEYS: [(&str, &str); 7] = [
    ("video", "categoryVideo"),
    ("audio", "categoryAudio"),
    ("document", "categoryDocument"),
    ("image", "categoryImage"),
    ("program", "categoryProgram"),
    ("archive", "categoryArchive"),
    ("other", "categoryOther"),
];
const BT_MIN_SIZES: [(i64, u8, usize); 4] = [
    (0, 0, 0),
    (10 << 20, 10, 2),
    (100 << 20, 100, 2),
    (1 << 30, 1, 3),
];

struct BtFilter {
    input: Entity<InputState>,
    expanded: bool,
    query: String,
    min_size: i64,
    categories: HashSet<usize>,
    counts: Vec<usize>,
    rules: Vec<CustomCategoryDto>,
    file_categories: Vec<usize>,
}

impl BtFilter {
    fn active(&self) -> bool {
        self.expanded
            && (!self.query.is_empty() || self.min_size > 0 || !self.categories.is_empty())
    }

    fn matches(&self, position: usize, file: &BtFile) -> bool {
        bt_file_matches(
            &self.query,
            self.min_size,
            &self.categories,
            self.file_categories[position],
            file,
        )
    }
}

/// `query` 已是修剪后的小写串（每次输入变化只算一次）；匹配完整相对路径，目录名与扩展名均可搜。
fn bt_file_matches(
    query: &str,
    min_size: i64,
    categories: &HashSet<usize>,
    category: usize,
    file: &BtFile,
) -> bool {
    (min_size == 0 || file.size >= min_size)
        && (categories.is_empty() || categories.contains(&category))
        && (query.is_empty() || file.path_lower.contains(query))
}

fn bt_basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn bt_category(path: &str, index: &CategoryIndex, other: usize) -> usize {
    index
        .rule_index_for_file_name(bt_basename(path))
        .unwrap_or(other)
}

pub struct SelectionView {
    strings: SelectionStrings,
    request: SelectionRequestDto,
    task_name: SharedString,
    port: Arc<dyn DownloadsPort>,
    state: SelectionState,
    /// BT 保存目录输入；其它选择种类没有。
    save_dir_input: Option<Entity<InputState>>,
    /// BT 名称输入，预填任务当前名称；其它选择种类没有。
    name_input: Option<Entity<InputState>>,
    bt_filter: Option<BtFilter>,
    download_name: DownloadName,
    /// 系统目录选择器打开中，避免重入。
    picking_save_dir: bool,
    submitting: bool,
}

enum SelectionState {
    Hls {
        options: Vec<HlsQualityOptionDto>,
        selected: i32,
    },
    Bt {
        files: Vec<BtFile>,
        selected: HashSet<i32>,
        order: BtFileOrder,
        /// 请求预填的当前保存目录。空串 = 旧 daemon 未带目录。
        save_dir: String,
    },
    Variant {
        options: Vec<ResolveVariantOptionDto>,
        selected: i32,
    },
    Plugin {
        site: String,
        url: String,
        candidates: Vec<PluginCandidateDto>,
        /// 引擎给出的默认候选（徽标所在项）；与初始预选一致，之后不随用户选择变化。
        default_identity: String,
        selected: String,
        remember: bool,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BtFileSort {
    #[default]
    Original,
    Type,
    Size,
}

struct BtFile {
    index: i32,
    size: i64,
    path: SharedString,
    /// 筛选搜索用的小写路径，构造时算一次，渲染期不再逐文件分配。
    path_lower: String,
    leaf: SharedString,
    depth: f32,
}

impl From<&BtFileDto> for BtFile {
    fn from(file: &BtFileDto) -> Self {
        let path = SharedString::from(file.path.as_str());
        let depth = file.path.matches('/').count().min(4) as f32;
        let leaf = match file.path.rsplit_once('/') {
            Some((_, leaf)) => SharedString::from(leaf),
            None => path.clone(),
        };
        Self {
            index: file.index,
            size: file.size,
            path_lower: file.path.to_lowercase(),
            path,
            leaf,
            depth,
        }
    }
}

impl BtFile {
    fn label(&self, sort: BtFileSort) -> (f32, &SharedString) {
        if sort == BtFileSort::Original {
            (self.depth, &self.leaf)
        } else {
            (0.0, &self.path)
        }
    }
}

#[derive(Default)]
struct BtFileOrder {
    sort: BtFileSort,
    // Positions in the original payload, not torrent file IDs.
    positions: Vec<usize>,
}

impl BtFileOrder {
    fn set_sort(&mut self, sort: BtFileSort, files: &[BtFile]) {
        self.sort = sort;
        self.positions.clear();
        if sort == BtFileSort::Original {
            return;
        }
        self.positions.extend(0..files.len());
        self.positions.sort_unstable_by(|&a, &b| {
            let left = &files[a];
            let right = &files[b];
            let comparison = match sort {
                BtFileSort::Original => std::cmp::Ordering::Equal,
                BtFileSort::Type => {
                    extension_bytes(left.leaf.as_str()).cmp(extension_bytes(right.leaf.as_str()))
                }
                BtFileSort::Size => right.size.cmp(&left.size),
            };
            comparison.then_with(|| a.cmp(&b))
        });
    }

    /// 按显示顺序给出 `(原始位置, 文件)`；筛选的分类表按原始位置索引。
    fn iter<'a>(&'a self, files: &'a [BtFile]) -> impl Iterator<Item = (usize, &'a BtFile)> {
        (0..files.len()).map(|i| {
            let position = self.positions.get(i).copied().unwrap_or(i);
            (position, &files[position])
        })
    }
}

fn extension_bytes(name: &str) -> impl Iterator<Item = u8> + '_ {
    let extension = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension,
        _ => "",
    };
    extension.bytes().map(|byte| byte.to_ascii_lowercase())
}

/// 单请求选择窗口（HLS 画质 / BT 文件 / 插件变体）能承载的请求及其默认选择。
/// `FileExists` 由聚合的「文件已存在」窗口承载（见 [`crate::FileConflictView`]），没有计划。
pub struct SelectionPlan {
    state: SelectionState,
    categories: Vec<CustomCategoryDto>,
}

impl SelectionPlan {
    #[must_use]
    pub fn for_request(
        request: &SelectionRequestDto,
        categories: Vec<CustomCategoryDto>,
    ) -> Option<Self> {
        let state = match &request.kind {
            SelectionKind::Hls { options } => {
                let selected = match request.default_choice {
                    SelectionOutcome::Hls { index } => index,
                    _ => options.first().map_or(0, |option| option.index),
                };
                SelectionState::Hls {
                    options: options.clone(),
                    selected,
                }
            }
            SelectionKind::Bt { files, save_dir } => {
                let selected = match &request.default_choice {
                    SelectionOutcome::Bt { indices, .. } if !indices.is_empty() => {
                        indices.iter().copied().collect()
                    }
                    _ => files.iter().map(|file| file.index).collect(),
                };
                SelectionState::Bt {
                    files: files.iter().map(BtFile::from).collect(),
                    selected,
                    order: BtFileOrder::default(),
                    save_dir: save_dir.clone(),
                }
            }
            SelectionKind::Variant { options } => {
                let selected = match request.default_choice {
                    SelectionOutcome::Variant { index } => index,
                    _ => options.first().map_or(0, |option| option.index),
                };
                SelectionState::Variant {
                    options: options.clone(),
                    selected,
                }
            }
            SelectionKind::Plugin {
                site,
                url,
                candidates,
            } => {
                let selected =
                    plugin_choice::initial_selection(candidates, &request.default_choice);
                SelectionState::Plugin {
                    site: site.clone(),
                    url: url.clone(),
                    default_identity: selected.clone(),
                    selected,
                    remember: false,
                    candidates: candidates.clone(),
                }
            }
            SelectionKind::FileExists { .. } => return None,
        };
        Some(Self { state, categories })
    }
}

impl SelectionView {
    /// 按计划初始化默认选择，并启动每秒刷新一次的倒计时。
    /// BT 选择额外建保存目录输入框（预填请求里的当前目录；旧 daemon 为空，只显示占位）。
    pub fn new(
        translator: Entity<Translator>,
        request: SelectionRequestDto,
        plan: SelectionPlan,
        task_name: String,
        port: Arc<dyn DownloadsPort>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strings = SelectionStrings::from_translator(translator.read(cx));
        let state = plan.state;
        let mut categories = plan.categories;
        let save_dir_input = match &state {
            SelectionState::Bt { save_dir, .. } => {
                let initial = save_dir.clone();
                let placeholder = strings.save_dir_placeholder.clone();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(initial)
                        .placeholder(placeholder)
                });
                // 确认按钮是否可点取决于目录是否为空，输入变化必须重绘。
                cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
                .detach();
                Some(input)
            }
            _ => None,
        };
        let task_name = SharedString::from(task_name);
        let download_name = DownloadName::with_fixed_name(task_name.clone());
        let name_input = matches!(state, SelectionState::Bt { .. }).then(|| {
            let initial = task_name.clone();
            cx.new(|cx| InputState::new(window, cx).default_value(initial))
        });
        let bt_filter = match &state {
            SelectionState::Bt { files, .. } if files.len() > 1 => {
                let placeholder = strings.bt_filter_search.clone();
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
                cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Some(filter) = &mut this.bt_filter
                    {
                        filter.query = input.read(cx).value().trim().to_lowercase();
                        cx.notify();
                    }
                })
                .detach();
                categories.sort_by_key(|category| category.position);
                let other = categories
                    .iter()
                    .position(|category| category.builtin_type.as_deref() == Some("other"))
                    .unwrap_or(categories.len());
                let mut counts = vec![0; categories.len().max(other + 1)];
                let index = CategoryIndex::from_dtos(categories);
                let file_categories = files
                    .iter()
                    .map(|file| {
                        let category = bt_category(&file.path, &index, other);
                        counts[category] += 1;
                        category
                    })
                    .collect();
                Some(BtFilter {
                    input,
                    expanded: false,
                    query: String::new(),
                    min_size: 0,
                    categories: HashSet::new(),
                    counts,
                    rules: index.into_dtos(),
                    file_categories,
                })
            }
            _ => None,
        };
        cx.observe_in(&translator, window, |this, translator, window, cx| {
            this.strings = SelectionStrings::from_translator(translator.read(cx));
            if let Some(input) = &this.save_dir_input {
                let placeholder = this.strings.save_dir_placeholder.clone();
                input.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx)
                });
            }
            if let Some(filter) = &this.bt_filter {
                let placeholder = this.strings.bt_filter_search.clone();
                filter.input.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx)
                });
            }
            cx.notify();
        })
        .detach();
        // 倒计时走字；无截止（用户关闭了自动选择默认项）时没有可更新的内容，不每秒重绘文件列表。
        if request.has_deadline() {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        Self {
            strings,
            request,
            task_name,
            port,
            state,
            save_dir_input,
            name_input,
            bt_filter,
            download_name,
            picking_save_dir: false,
            submitting: false,
        }
    }

    /// HLS 无取消入口，与 daemon 超时后按默认值强制解析的语义一致。
    fn cancellable(&self) -> bool {
        !matches!(self.state, SelectionState::Hls { .. })
    }

    /// 距截止的整秒数；无截止（用户关闭了自动选择默认项）为 `None`，页脚不显示倒计时。
    fn remaining_seconds(&self) -> Option<i64> {
        self.request
            .has_deadline()
            .then(|| ((self.request.deadline_unix_ms - now_unix_ms()) / 1000).max(0))
    }

    /// 引擎把空 BT 选择回退为全部文件，所以全不选时不允许确认（与 web 一致）。
    /// 保存目录是否可空见 [`bt_save_dir_confirmable`]。
    fn can_confirm(&self, cx: &App) -> bool {
        match &self.state {
            SelectionState::Bt {
                selected, save_dir, ..
            } => {
                !selected.is_empty()
                    && bt_save_dir_confirmable(save_dir, &self.save_dir_input_value(cx))
            }
            SelectionState::Plugin { selected, .. } => !selected.is_empty(),
            _ => true,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        let outcome = match &self.state {
            SelectionState::Hls { selected, .. } => SelectionOutcome::Hls { index: *selected },
            SelectionState::Bt {
                selected, save_dir, ..
            } => SelectionOutcome::Bt {
                indices: selected.iter().copied().collect(),
                save_dir: bt_save_dir_outcome(save_dir, &self.save_dir_input_value(cx)),
                custom_name: self.name_override(cx),
            },
            SelectionState::Variant { selected, .. } => {
                SelectionOutcome::Variant { index: *selected }
            }
            SelectionState::Plugin {
                selected, remember, ..
            } => match plugin_choice::confirm_outcome(selected, *remember) {
                Some(outcome) => outcome,
                None => return,
            },
        };
        self.resolve(outcome, window, cx);
    }

    /// 放弃当前选择；BT 删除任务，窗口在请求撤回后关闭。
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = cancel_command(&self.state, &self.request);
        self.submit(command, window, cx);
    }

    fn resolve(&mut self, outcome: SelectionOutcome, window: &mut Window, cx: &mut Context<Self>) {
        let command = DownloadsCommand::ResolveSelection(SelectionResolutionDto {
            request_id: self.request.request_id.clone(),
            outcome,
        });
        self.submit(command, window, cx);
    }

    fn submit(&mut self, command: DownloadsCommand, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitting {
            return;
        }
        self.submitting = true;
        cx.notify();
        let future = self.port.execute(command);
        cx.spawn_in(window, async move |this, cx| {
            let result = future.await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.submitting = false;
                if result.is_err() {
                    window.push_notification(toast::error(this.strings.action_failed.clone()), cx);
                }
                cx.notify();
            }) else {
                // 视图或窗口已释放，停止回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn name_override(&self, cx: &App) -> Option<String> {
        let input = self.name_input.as_ref()?;
        self.download_name.override_name(input.read(cx))
    }

    fn save_dir_input_value(&self, cx: &App) -> SharedString {
        self.save_dir_input
            .as_ref()
            .map(|input| input.read(cx).value())
            .unwrap_or_default()
    }

    /// 系统目录选择器只能选本机目录。打开期间禁止重入；选中后写入输入框。
    fn pick_save_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking_save_dir || self.submitting || self.save_dir_input.is_none() {
            return;
        }
        self.picking_save_dir = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.strings.save_dir_placeholder.clone()),
        });
        cx.spawn_in(window, async move |this, cx| {
            // 取消不是失败；选择器本身出错或通道断开才提示，不把错误吞掉。
            let (picked, failed) = match receiver.await {
                Ok(Ok(Some(paths))) => {
                    (paths.first().map(|path| path.display().to_string()), false)
                }
                Ok(Ok(None)) => (None, false),
                Ok(Err(_)) => (None, true),
                Err(_) => (None, true),
            };
            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking_save_dir = false;
                if let Some(path) = picked
                    && let Some(input) = &this.save_dir_input
                {
                    input.update(cx, |input, cx| input.set_value(path, window, cx));
                }
                if failed {
                    window.push_notification(
                        toast::error(this.strings.file_picker_error.clone()),
                        cx,
                    );
                }
                // set_value 不发 Change，确认按钮是否可点要靠这次重绘。
                cx.notify();
            }) else {
                // 视图或窗口已释放，不再回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 名称：单文件种子是文件名，多文件种子是外层目录名。
    fn render_name(&self, cx: &mut Context<Self>) -> Option<Div> {
        let input = self.name_input.as_ref()?;
        let this = cx.weak_entity();
        let auto = check_row(
            "bt-auto-name",
            self.download_name.is_auto(),
            self.strings.auto_name.clone(),
            move |checked, window, cx| {
                let Ok(()) = this.update(cx, |this, cx| {
                    if let Some(input) = &this.name_input {
                        this.download_name.set_auto(checked, input, window, cx);
                    }
                    cx.notify();
                }) else {
                    return;
                };
            },
            cx,
        )
        .flex_none();
        Some(
            form_field(
                self.strings.name.clone(),
                h_flex()
                    .w_full()
                    .gap(active_theme(cx).tokens().spacing.sm)
                    .child(
                        Input::new(input)
                            .control(cx)
                            .flex_1()
                            .min_w_0()
                            .disabled(self.download_name.is_auto()),
                    )
                    .child(auto),
                None,
                cx,
            )
            .flex_none(),
        )
    }

    /// 保存目录在全选工具栏之上：标签 + 输入框 + 浏览。长路径留在输入框里滚动，不撑开窗口。
    fn render_save_dir(&self, cx: &mut Context<Self>) -> Option<Div> {
        let input = self.save_dir_input.as_ref()?;
        Some(
            form_field(
                self.strings.save_dir.clone(),
                input_with_action(
                    Input::new(input).control(cx).w_full(),
                    Button::new("bt-save-dir-browse")
                        .outline()
                        .icon(FluxIcon::FolderOpen)
                        .label(self.strings.browse.clone())
                        .control(cx)
                        .disabled(self.picking_save_dir || self.submitting)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.pick_save_dir(window, cx);
                        })),
                    cx,
                ),
                None,
                cx,
            )
            .flex_none(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let title_style = theme.extended().title;
        let (title, description) = match &self.state {
            SelectionState::Hls { .. } => (
                self.strings.hls_title.clone(),
                self.strings.hls_desc.clone(),
            ),
            SelectionState::Bt { files, .. } => (
                self.strings.bt_title.clone(),
                if files.len() == 1 {
                    self.strings.bt_desc_single.clone()
                } else {
                    SharedString::from(
                        self.strings
                            .bt_desc
                            .replace("{count}", &files.len().to_string()),
                    )
                },
            ),
            SelectionState::Variant { .. } => (
                self.strings.variant_title.clone(),
                self.strings.variant_desc.clone(),
            ),
            SelectionState::Plugin { .. } => {
                (self.strings.plugin_title.clone(), SharedString::default())
            }
        };
        v_flex()
            .gap(tokens.spacing.xs)
            .p(tokens.spacing.md)
            .child(
                div()
                    .text_size(title_style.size)
                    .line_height(title_style.line_height)
                    .font_weight(title_style.weight)
                    .child(title),
            )
            .child(
                div()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .text_color(tokens.colors.muted_foreground)
                    .child(if matches!(self.state, SelectionState::Plugin { .. }) {
                        // 站点名加粗；任务名已在窗口标题里，不重复。
                        self.plugin_description(cx)
                    } else if self.task_name.is_empty() {
                        description.into_any_element()
                    } else {
                        SharedString::from(format!("{description} · {}", self.task_name))
                            .into_any_element()
                    }),
            )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        // HLS / 画质选择窗口固定尺寸：文字放大后选项可能超出窗口，列表内滚动。
        match &self.state {
            SelectionState::Hls { options, selected } => div()
                .id("selection-hls-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(self.render_hls(options, *selected, cx))
                .into_any_element(),
            SelectionState::Bt {
                files,
                selected,
                order,
                ..
            } => self
                .render_bt(files, selected, order, cx)
                .into_any_element(),
            SelectionState::Variant { options, selected } => div()
                .id("selection-variant-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(self.render_variant(options, *selected, cx))
                .into_any_element(),
            SelectionState::Plugin { .. } => self.render_plugin(cx),
        }
    }

    fn render_hls(
        &self,
        options: &[HlsQualityOptionDto],
        selected: i32,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let hover = theme.extended().colors.row_hover;
        v_flex()
            .gap(tokens.spacing.xxs)
            .px(tokens.spacing.md)
            .children(options.iter().map(|option| {
                let checked = option.index == selected;
                let index = option.index;
                let label = SharedString::from(format!(
                    "{}×{} · {} kbps",
                    option.width,
                    option.height,
                    option.bandwidth / 1000
                ));
                h_flex()
                    .id(("hls-option", index as usize))
                    .cursor_pointer()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .rounded(tokens.radius.md)
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_features(tabular_numbers())
                    .map(|row| {
                        if checked {
                            row.bg(tokens.colors.accent)
                        } else {
                            row.hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Hls { selected, .. } = &mut this.state {
                            *selected = index;
                        }
                        cx.notify();
                    }))
                    .child(check_mark(check_state(checked), cx))
                    .child(div().flex_1().child(label))
            }))
    }

    fn render_variant(
        &self,
        options: &[ResolveVariantOptionDto],
        selected: i32,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let hover = theme.extended().colors.row_hover;
        v_flex()
            .gap(tokens.spacing.xxs)
            .px(tokens.spacing.md)
            .children(options.iter().map(|option| {
                let checked = option.index == selected;
                let index = option.index;
                let label = SharedString::from(option.label.clone());
                let meta = SharedString::from(format!(
                    "{} · {}×{}",
                    option.container, option.width, option.height
                ));
                h_flex()
                    .id(("variant-option", index as usize))
                    .cursor_pointer()
                    .items_center()
                    .gap(tokens.spacing.sm)
                    .px(tokens.spacing.sm)
                    .py(tokens.spacing.xs)
                    .rounded(tokens.radius.md)
                    .map(|row| {
                        if checked {
                            row.bg(tokens.colors.accent)
                        } else {
                            row.hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Variant { selected, .. } = &mut this.state {
                            *selected = index;
                        }
                        cx.notify();
                    }))
                    .child(check_mark(check_state(checked), cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(tokens.spacing.xxs)
                            .child(
                                div()
                                    .text_size(tokens.typography.sm.size)
                                    .line_height(tokens.typography.sm.line_height)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_size(tokens.typography.xs.size)
                                    .line_height(tokens.typography.xs.line_height)
                                    .font_features(tabular_numbers())
                                    .text_color(tokens.colors.muted_foreground)
                                    .child(meta),
                            ),
                    )
            }))
    }

    fn set_bt_visible(&mut self, checked: bool) {
        let SelectionState::Bt {
            files, selected, ..
        } = &mut self.state
        else {
            return;
        };
        let filter = self.bt_filter.as_ref().filter(|filter| filter.active());
        if !checked && filter.is_none() {
            selected.clear();
            return;
        }
        for (position, file) in files.iter().enumerate() {
            if filter.is_none_or(|filter| filter.matches(position, file)) {
                if checked {
                    selected.insert(file.index);
                } else {
                    selected.remove(&file.index);
                }
            }
        }
    }

    fn render_bt_filters(&self, filter: &BtFilter, cx: &mut Context<Self>) -> Div {
        let spacing = active_theme(cx).tokens().spacing.sm;
        let categories = h_flex().flex_wrap().gap(spacing).children(
            filter
                .counts
                .iter()
                .enumerate()
                .filter(|(_, count)| **count > 0)
                .map(|(category, count)| {
                    let rule = filter.rules.get(category);
                    let kind = rule.map_or(Some("other"), |rule| rule.builtin_type.as_deref());
                    let label = BT_CATEGORY_KEYS
                        .iter()
                        .position(|(id, _)| Some(*id) == kind)
                        .map_or_else(
                            || rule.map_or("", |rule| rule.name.as_str()),
                            |index| self.strings.bt_categories[index].as_ref(),
                        );
                    let button = Button::new(("bt-category", category));
                    let button = if filter.categories.contains(&category) {
                        button.primary()
                    } else {
                        button.outline()
                    };
                    button
                        .control(cx)
                        .label(format!("{label} {count}"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(filter) = &mut this.bt_filter
                                && !filter.categories.remove(&category)
                            {
                                filter.categories.insert(category);
                            }
                            cx.notify();
                        }))
                }),
        );
        let sizes = h_flex()
            .items_center()
            .flex_wrap()
            .gap(spacing)
            .child(self.strings.bt_filter_min_size.clone())
            .children(
                BT_MIN_SIZES
                    .iter()
                    .enumerate()
                    .map(|(index, &(size, value, power))| {
                        let button = Button::new(("bt-min-size", index));
                        let button = if filter.min_size == size {
                            button.primary()
                        } else {
                            button.outline()
                        };
                        button
                            .control(cx)
                            .label(if size == 0 {
                                self.strings.bt_filter_any_size.clone()
                            } else {
                                let units = fluxdown_ui_components::size_units::labels();
                                let unit = units.get(power).copied().unwrap_or("B");
                                SharedString::from(format!("{value} {unit}"))
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(filter) = &mut this.bt_filter {
                                    filter.min_size = size;
                                }
                                cx.notify();
                            }))
                    }),
            );
        v_flex()
            .gap(spacing)
            .child(Input::new(&filter.input).control(cx).w_full())
            .child(categories)
            .child(sizes)
    }

    fn render_bt_sort(&self, order: &BtFileOrder, cx: &mut Context<Self>) -> Div {
        let spacing = active_theme(cx).tokens().spacing.xs;
        let options = [
            (
                BtFileSort::Original,
                "bt-sort-original",
                &self.strings.sort_original,
            ),
            (BtFileSort::Type, "bt-sort-type", &self.strings.sort_type),
            (BtFileSort::Size, "bt-sort-size", &self.strings.sort_size),
        ];
        h_flex()
            .flex_wrap()
            .gap(spacing)
            .children(options.into_iter().map(|(sort, id, label)| {
                let button = Button::new(id);
                let button = if order.sort == sort {
                    button.primary()
                } else {
                    button.outline()
                };
                button
                    .control(cx)
                    .label(label.clone())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let SelectionState::Bt { files, order, .. } = &mut this.state
                            && order.sort != sort
                        {
                            order.set_sort(sort, files);
                            cx.notify();
                        }
                    }))
            }))
    }

    fn render_bt(
        &self,
        files: &[BtFile],
        selected: &HashSet<i32>,
        order: &BtFileOrder,
        cx: &mut Context<Self>,
    ) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        let filter = self.bt_filter.as_ref().filter(|filter| filter.active());
        let mut visible = order
            .iter(files)
            .filter(|(position, file)| filter.is_none_or(|filter| filter.matches(*position, file)))
            .peekable();
        let no_matches = filter.is_some() && visible.peek().is_none();
        let selected_size: i64 = files
            .iter()
            .filter(|file| selected.contains(&file.index))
            .map(|file| file.size)
            .sum();
        let toolbar = h_flex()
            .items_center()
            .flex_wrap()
            .gap(tokens.spacing.sm)
            .child(
                Button::new("bt-select-all")
                    .outline()
                    .control(cx)
                    .disabled(no_matches)
                    .label(if filter.is_some() {
                        self.strings.bt_select_visible.clone()
                    } else {
                        self.strings.select_all.clone()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.set_bt_visible(true);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("bt-deselect-all")
                    .outline()
                    .control(cx)
                    .disabled(no_matches)
                    .label(if filter.is_some() {
                        self.strings.bt_clear_visible.clone()
                    } else {
                        self.strings.deselect_all.clone()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.set_bt_visible(false);
                        cx.notify();
                    })),
            )
            .children(self.bt_filter.as_ref().map(|filter| {
                Button::new("bt-toggle-filters")
                    .outline()
                    .control(cx)
                    .label(if filter.expanded {
                        self.strings.bt_filter_hide.clone()
                    } else {
                        self.strings.bt_filter_show.clone()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(filter) = &mut this.bt_filter {
                            filter.expanded = !filter.expanded;
                        }
                        cx.notify();
                    }))
            }))
            .child(
                div()
                    .flex_1()
                    .text_right()
                    .text_size(tokens.typography.xs.size)
                    .line_height(tokens.typography.xs.line_height)
                    .font_features(tabular_numbers())
                    .text_color(tokens.colors.muted_foreground)
                    .child(SharedString::from(format!(
                        "{} · {}",
                        self.strings
                            .selected_count
                            .replace("{n}", &selected.len().to_string()),
                        format_bytes(selected_size.max(0) as u64)
                    ))),
            );
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .children(visible.map(|(_, file)| {
                let checked = selected.contains(&file.index);
                let index = file.index;
                let (depth, label) = file.label(order.sort);
                let full_path = file.path.clone();
                let this = cx.weak_entity();
                check_row(
                    ("bt-file", index as usize),
                    checked,
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(tokens.spacing.sm)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_middle()
                                .child(label.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(tokens.typography.xs.size)
                                .font_features(tabular_numbers())
                                .text_color(tokens.colors.muted_foreground)
                                .child(SharedString::from(format_bytes(file.size.max(0) as u64))),
                        ),
                    move |check, _, cx| {
                        let Ok(()) = this.update(cx, |this, cx| {
                            if let SelectionState::Bt { selected, .. } = &mut this.state {
                                if check {
                                    selected.insert(index);
                                } else {
                                    selected.remove(&index);
                                }
                            }
                            cx.notify();
                        }) else {
                            // 视图已释放，结束回调而不再更新状态。
                            return;
                        };
                    },
                    cx,
                )
                .pl(tokens.spacing.xs + tokens.spacing.lg * depth)
                .pr(tokens.spacing.sm)
                .when(checked, |row| row.bg(tokens.colors.accent))
                .tooltip(move |window, cx| tooltip_view(full_path.clone(), window, cx))
            }))
            .overflow_y_scrollbar();
        // 保存目录在全选工具栏之上，不进文件列表的滚动区。
        v_flex()
            .size_full()
            .gap(tokens.spacing.sm)
            .px(tokens.spacing.md)
            .pb(tokens.spacing.md)
            .children(self.render_name(cx))
            .children(self.render_save_dir(cx))
            .child(toolbar)
            .when(files.len() > 1, |body| {
                body.child(self.render_bt_sort(order, cx))
            })
            .children(
                self.bt_filter
                    .as_ref()
                    .filter(|filter| filter.expanded)
                    .map(|filter| self.render_bt_filters(filter, cx)),
            )
            .when(no_matches, |view| {
                view.child(
                    div()
                        .text_size(tokens.typography.sm.size)
                        .line_height(tokens.typography.sm.line_height)
                        .text_color(tokens.colors.muted_foreground)
                        .child(self.strings.bt_filter_no_matches.clone()),
                )
            })
            .child(div().flex_1().min_h_0().child(list))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let countdown = self.remaining_seconds().map(|remaining| {
            SharedString::from(
                self.strings
                    .auto_default_in
                    .replace("{seconds}", &remaining.to_string()),
            )
        });
        let confirm_label = if let SelectionState::Bt {
            files, selected, ..
        } = &self.state
        {
            let size: i64 = files
                .iter()
                .filter(|file| selected.contains(&file.index))
                .map(|file| file.size)
                .sum();
            SharedString::from(
                self.strings
                    .bt_confirm
                    .replace("{count}", &selected.len().to_string())
                    .replace("{size}", &format_bytes(size.max(0) as u64)),
            )
        } else if matches!(self.state, SelectionState::Plugin { .. }) {
            self.strings.plugin_confirm.clone()
        } else {
            self.strings.confirm.clone()
        };
        h_flex()
            .justify_between()
            .items_center()
            .px(tokens.spacing.md)
            .py(tokens.spacing.sm)
            .bg(extended.colors.chrome)
            .border_t(extended.stroke.thin)
            .border_color(extended.colors.hairline)
            .child(countdown.map_or_else(div, |countdown| {
                field_hint(countdown, cx).font_features(tabular_numbers())
            }))
            .child(
                h_flex()
                    .gap(tokens.spacing.sm)
                    .when(self.cancellable(), |row| {
                        row.child(
                            Button::new("selection-cancel")
                                .outline()
                                .control(cx)
                                .label(self.strings.cancel.clone())
                                .disabled(self.submitting)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.cancel(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("selection-confirm")
                            .primary()
                            .control(cx)
                            .label(confirm_label)
                            .busy(self.submitting)
                            .disabled(self.submitting || !self.can_confirm(cx))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx);
                            })),
                    ),
            )
    }
}

impl Render for SelectionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        v_flex()
            .size_full()
            .bg(tokens.colors.surface)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .child(self.render_footer(cx))
    }
}

fn check_state(checked: bool) -> CheckState {
    if checked {
        CheckState::Checked
    } else {
        CheckState::Unchecked
    }
}

#[derive(Clone)]
struct SelectionStrings {
    hls_title: SharedString,
    hls_desc: SharedString,
    bt_title: SharedString,
    bt_desc: SharedString,
    bt_desc_single: SharedString,
    bt_confirm: SharedString,
    variant_title: SharedString,
    variant_desc: SharedString,
    plugin_title: SharedString,
    plugin_desc: String,
    plugin_default_badge: SharedString,
    plugin_remember: String,
    plugin_remember_hint: SharedString,
    plugin_confirm: SharedString,
    select_all: SharedString,
    deselect_all: SharedString,
    bt_filter_show: SharedString,
    bt_filter_hide: SharedString,
    bt_filter_search: SharedString,
    bt_filter_min_size: SharedString,
    bt_filter_any_size: SharedString,
    bt_select_visible: SharedString,
    bt_clear_visible: SharedString,
    bt_filter_no_matches: SharedString,
    bt_categories: [SharedString; BT_CATEGORY_KEYS.len()],
    sort_original: SharedString,
    sort_type: SharedString,
    sort_size: SharedString,
    selected_count: SharedString,
    name: SharedString,
    auto_name: SharedString,
    save_dir: SharedString,
    save_dir_placeholder: SharedString,
    browse: SharedString,
    file_picker_error: SharedString,
    confirm: SharedString,
    cancel: SharedString,
    auto_default_in: SharedString,
    action_failed: SharedString,
}

impl SelectionStrings {
    fn from_translator(translator: &Translator) -> Self {
        Self {
            hls_title: shared(translator.text("hlsQualityTitle")),
            hls_desc: shared(translator.text("hlsQualityDesc")),
            bt_title: shared(translator.text("btFileSelectTitle")),
            bt_desc: shared(translator.text("btFileSelectDesc")),
            bt_desc_single: shared(translator.text("btFileSelectDescSingle")),
            bt_confirm: shared(translator.text("btFileSelectConfirm")),
            variant_title: shared(translator.text("resolveVariantTitle")),
            variant_desc: shared(translator.text("resolveVariantDesc")),
            plugin_title: shared(translator.text("pluginChoiceTitle")),
            plugin_desc: translator.text("pluginChoiceDesc").to_owned(),
            plugin_default_badge: shared(translator.text("pluginChoiceDefaultBadge")),
            plugin_remember: translator.text("pluginChoiceRemember").to_owned(),
            plugin_remember_hint: shared(translator.text("pluginChoiceRememberHint")),
            plugin_confirm: shared(translator.text("pluginChoiceConfirm")),
            select_all: shared(translator.text("btFileSelectAll")),
            deselect_all: shared(translator.text("deselectAll")),
            bt_filter_show: shared(translator.text("btFilterShow")),
            bt_filter_hide: shared(translator.text("btFilterHide")),
            bt_filter_search: shared(translator.text("webSelectionFilterPlaceholder")),
            bt_filter_min_size: shared(translator.text("btFilterMinSize")),
            bt_filter_any_size: shared(translator.text("btFilterAnySize")),
            bt_select_visible: shared(translator.text("btFilterSelectVisible")),
            bt_clear_visible: shared(translator.text("btFilterClearVisible")),
            bt_filter_no_matches: shared(translator.text("btFilterNoMatches")),
            bt_categories: BT_CATEGORY_KEYS.map(|(_, key)| shared(translator.text(key))),
            sort_original: shared(translator.text("btFileSortOriginal")),
            sort_type: shared(translator.text("btFileSortType")),
            sort_size: shared(translator.text("btFileSortSize")),
            selected_count: shared(translator.text("selectedCount")),
            name: shared(translator.text("btFileColumnName")),
            auto_name: shared(translator.text("auto")),
            save_dir: shared(translator.text("saveDir")),
            save_dir_placeholder: shared(translator.text("selectSaveDir")),
            browse: shared(translator.text("browse")),
            file_picker_error: shared(translator.text("filePickerErrorNative")),
            confirm: shared(translator.text("confirm")),
            cancel: shared(translator.text("cancel")),
            auto_default_in: shared(translator.text("selectionAutoDefaultIn")),
            action_failed: shared(translator.text("localServiceActionFailed")),
        }
    }
}

fn shared(value: &str) -> SharedString {
    SharedString::from(value.to_owned())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// 「取消」按钮发出的命令。BT 的取消即放弃这个任务：连同已暂存的数据一起删除，不留暂停项；
/// 删除会撤回本次选择请求，窗口随之关闭。其它选择种类照旧把取消答复给 daemon。
fn cancel_command(state: &SelectionState, request: &SelectionRequestDto) -> DownloadsCommand {
    match state {
        SelectionState::Bt { .. } => DownloadsCommand::Delete {
            task_id: request.task_id.clone(),
            delete_files: true,
        },
        _ => DownloadsCommand::ResolveSelection(SelectionResolutionDto {
            request_id: request.request_id.clone(),
            outcome: SelectionOutcome::Cancelled,
        }),
    }
}

/// 保存目录输入是否允许确认。
///
/// 请求带了当前目录时，去空白后必须非空（清空不能确认，避免空路径开下）。
/// 请求没带目录（旧 daemon，`initial` 去空白后为空）时，空输入也允许确认，
/// 答复里 `save_dir = None`，引擎沿用任务当前目录。
fn bt_save_dir_confirmable(initial: &str, input: &str) -> bool {
    initial.trim().is_empty() || !input.trim().is_empty()
}

/// 把保存目录输入收成 [`SelectionOutcome::Bt`] 的 `save_dir`。
///
/// `Some` 仅当去空白后非空，且与预填目录（同样只去首尾空白，不改大小写、不换分隔符）
/// 不同：引擎据此在真正下载前搬到新目录并重开任务。未改、只改了首尾空白、或空输入
/// 都是 `None`（沿用当前目录）。
#[must_use]
fn bt_save_dir_outcome(initial: &str, input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed == initial.trim() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use fluxdown_protocol::{
        ApplicationErrorCode, BtFileDto, CustomCategoryDto, RpcErrorData, SELECTION_NO_DEADLINE,
        SelectionKind, SelectionOutcome, SelectionRequestDto,
    };
    use fluxdown_ui_i18n::I18nCatalog;
    use gpui::{AppContext as _, TestApp};

    use super::{
        BT_CATEGORY_KEYS, BT_MIN_SIZES, BtFile, BtFileOrder, BtFileSort, CategoryIndex,
        SelectionPlan, SelectionState, SelectionView, bt_category, bt_file_matches,
        bt_save_dir_confirmable, bt_save_dir_outcome, cancel_command,
    };
    use crate::controller::{DownloadsCommand, DownloadsPort, DownloadsResult, PortFuture};

    fn request(kind: SelectionKind) -> SelectionRequestDto {
        SelectionRequestDto {
            request_id: "req-1".to_owned(),
            task_id: "task-1".to_owned(),
            kind,
            default_choice: SelectionOutcome::Cancelled,
            deadline_unix_ms: SELECTION_NO_DEADLINE,
        }
    }

    #[test]
    fn bt_cancel_deletes_the_task_with_its_data() {
        let state = SelectionState::Bt {
            files: Vec::new(),
            selected: HashSet::new(),
            order: BtFileOrder::default(),
            save_dir: String::new(),
        };
        let request = request(SelectionKind::Bt {
            files: Vec::new(),
            save_dir: String::new(),
        });
        assert!(matches!(
            cancel_command(&state, &request),
            DownloadsCommand::Delete { task_id, delete_files: true } if task_id == "task-1"
        ));
    }

    #[test]
    fn other_cancels_still_answer_the_selection() {
        let state = SelectionState::Variant {
            options: Vec::new(),
            selected: 0,
        };
        let request = request(SelectionKind::Variant {
            options: Vec::new(),
        });
        assert!(matches!(
            cancel_command(&state, &request),
            DownloadsCommand::ResolveSelection(resolution)
                if resolution.request_id == "req-1"
                    && resolution.outcome == SelectionOutcome::Cancelled
        ));
    }

    #[test]
    fn bt_file_sort_keeps_torrent_ids_and_original_tie_order() {
        let files = [
            (9, "dir.mp4/README", 5),
            (3, "one.MKV", 10),
            (7, "two.mkv", 20),
            (0, ".txt", 20),
            (4, "notes.txt", 1),
        ]
        .map(|(index, path, size)| {
            BtFile::from(&BtFileDto {
                index,
                path: path.to_owned(),
                size,
            })
        });
        let mut order = BtFileOrder::default();
        let ids = |order: &BtFileOrder| {
            order
                .iter(&files)
                .map(|(_, file)| file.index)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&order), [9, 3, 7, 0, 4]);
        order.set_sort(BtFileSort::Type, &files);
        assert_eq!(ids(&order), [9, 0, 3, 7, 4]);
        order.set_sort(BtFileSort::Size, &files);
        assert_eq!(ids(&order), [7, 0, 3, 9, 4]);
        order.set_sort(BtFileSort::Original, &files);
        assert_eq!(ids(&order), [9, 3, 7, 0, 4]);
    }

    #[test]
    fn sorted_bt_rows_keep_the_full_path_for_equal_size_files() {
        let prefix = "collection/season/bonus/features/a-very-long-common-directory";
        let alpha = format!("{prefix}/a-very-long-common-file-name-alpha.mkv");
        let beta = format!("{prefix}/a-very-long-common-file-name-beta.mkv");
        let files = [
            (14, alpha.as_str(), 10),
            (2, "notes.txt", 20),
            (39, beta.as_str(), 10),
        ]
        .map(|(index, path, size)| {
            BtFile::from(&BtFileDto {
                index,
                path: path.to_owned(),
                size,
            })
        });
        let mut order = BtFileOrder::default();
        fn rows<'a>(order: &'a BtFileOrder, files: &'a [BtFile]) -> Vec<(i32, (f32, &'a str))> {
            order
                .iter(files)
                .map(|(_, file)| {
                    let (depth, label) = file.label(order.sort);
                    (file.index, (depth, label.as_str()))
                })
                .collect()
        }
        assert_eq!(
            rows(&order, &files),
            [
                (14, (4.0, "a-very-long-common-file-name-alpha.mkv")),
                (2, (0.0, "notes.txt")),
                (39, (4.0, "a-very-long-common-file-name-beta.mkv")),
            ]
        );
        order.set_sort(BtFileSort::Type, &files);
        assert_eq!(
            rows(&order, &files),
            [
                (14, (0.0, alpha.as_str())),
                (39, (0.0, beta.as_str())),
                (2, (0.0, "notes.txt")),
            ]
        );
        order.set_sort(BtFileSort::Size, &files);
        assert_eq!(
            rows(&order, &files),
            [
                (2, (0.0, "notes.txt")),
                (14, (0.0, alpha.as_str())),
                (39, (0.0, beta.as_str())),
            ]
        );
    }

    #[test]
    fn automatic_bt_name_is_displayed_without_losing_a_manual_draft() {
        struct Offline;

        impl DownloadsPort for Offline {
            fn execute(&self, _: DownloadsCommand) -> PortFuture<DownloadsResult> {
                Box::pin(async { Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)) })
            }
        }

        let mut app = TestApp::new();
        let translator = app.update(|cx| {
            gpui_component::init(cx);
            fluxdown_ui_theme::init(cx);
            let catalog = I18nCatalog::load_embedded().expect("catalog");
            cx.new(|_| catalog.translator("en"))
        });
        let mut win = app.open_window(|window, cx| {
            let request = request(SelectionKind::Bt {
                files: vec![BtFileDto {
                    index: 0,
                    path: "movie.mkv".to_owned(),
                    size: 10,
                }],
                save_dir: String::new(),
            });
            let plan = SelectionPlan::for_request(&request, Vec::new()).expect("BT selection plan");
            SelectionView::new(
                translator,
                request,
                plan,
                "movie.mkv".to_owned(),
                Arc::new(Offline),
                window,
                cx,
            )
        });
        win.update(|view, window, cx| {
            let input = view.name_input.as_ref().expect("BT name input");
            assert_eq!(view.name_override(cx), None);
            view.download_name.set_auto(false, input, window, cx);
            for unchanged in ["  movie.mkv ", "   "] {
                input.update(cx, |input, cx| input.set_value(unchanged, window, cx));
                assert_eq!(view.name_override(cx), None);
                view.download_name.set_auto(true, input, window, cx);
                assert_eq!(&*input.read(cx).value(), "movie.mkv");
                assert_eq!(view.name_override(cx), None);
                view.download_name.set_auto(false, input, window, cx);
                assert_eq!(&*input.read(cx).value(), unchanged);
                assert_eq!(view.name_override(cx), None);
            }
            input.update(cx, |input, cx| input.set_value("  My Movie  ", window, cx));
            assert_eq!(view.name_override(cx).as_deref(), Some("My Movie"));
            view.download_name.set_auto(true, input, window, cx);
            assert_eq!(&*input.read(cx).value(), "movie.mkv");
            assert_eq!(view.name_override(cx), None);
            view.download_name.set_auto(false, input, window, cx);
            assert_eq!(&*input.read(cx).value(), "  My Movie  ");
            assert_eq!(view.name_override(cx).as_deref(), Some("My Movie"));

            for original in ["movie.mkv", "  movie.mkv  "] {
                input.update(cx, |input, cx| input.set_value(original, window, cx));
                assert_eq!(view.name_override(cx), None);
            }
        });
    }

    #[test]
    fn unchanged_dir_keeps_current() {
        assert_eq!(bt_save_dir_outcome("/data", "/data"), None);
        assert_eq!(bt_save_dir_outcome(r"D:\Movies", r"D:\Movies"), None);
        assert_eq!(
            bt_save_dir_outcome(r"\\nas\share\下载", r"\\nas\share\下载"),
            None
        );
    }

    #[test]
    fn changed_dir_is_sent_trimmed() {
        assert_eq!(
            bt_save_dir_outcome("/data", "/new"),
            Some("/new".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome("/data", "  /new  "),
            Some("/new".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome("/storage/emulated/0/Download", "/sdcard/Movies"),
            Some("/sdcard/Movies".to_owned())
        );
    }

    #[test]
    fn surrounding_whitespace_is_not_a_change() {
        assert_eq!(bt_save_dir_outcome("/data", "  /data  "), None);
        assert_eq!(bt_save_dir_outcome("  /data  ", "/data"), None);
        assert_eq!(
            bt_save_dir_outcome(
                "/var/mobile/Containers/Data/Application/uuid/Documents",
                "  /var/mobile/Containers/Data/Application/uuid/Documents  "
            ),
            None
        );
    }

    #[test]
    fn empty_input_keeps_current() {
        assert_eq!(bt_save_dir_outcome("/data", ""), None);
        assert_eq!(bt_save_dir_outcome("/data", "   "), None);
        assert_eq!(bt_save_dir_outcome("", ""), None);
        assert_eq!(bt_save_dir_outcome("", "   "), None);
    }

    #[test]
    fn empty_initial_with_typed_dir_is_sent() {
        assert_eq!(
            bt_save_dir_outcome("", "  /picked  "),
            Some("/picked".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome("", "  C:\\Users\\zero\\Downloads  "),
            Some("C:\\Users\\zero\\Downloads".to_owned())
        );
    }

    #[test]
    fn path_text_is_sent_as_typed_beyond_trimming() {
        // 客户端只去首尾空白、不做路径规范化：内部空格与末尾分隔符原样带回，
        // 是否同一目录由引擎（`normalize_save_dir`）判定。
        assert_eq!(
            bt_save_dir_outcome("/data", "/data/My  Movies"),
            Some("/data/My  Movies".to_owned())
        );
        assert_eq!(
            bt_save_dir_outcome(r"D:\Movies", "D:\\Movies\\"),
            Some("D:\\Movies\\".to_owned())
        );
    }

    #[test]
    fn confirmable_requires_dir_unless_old_daemon() {
        assert!(!bt_save_dir_confirmable("/data", ""));
        assert!(!bt_save_dir_confirmable("/data", "   "));
        assert!(bt_save_dir_confirmable("/data", "/data"));
        assert!(bt_save_dir_confirmable("/data", "/new"));
        // 旧 daemon 没带目录：空输入仍可确认（答复 None）。
        assert!(bt_save_dir_confirmable("", ""));
        assert!(bt_save_dir_confirmable("", "   "));
        assert!(bt_save_dir_confirmable("  ", ""));
        assert!(bt_save_dir_confirmable("", r"D:\Movies"));
    }

    fn bt_file(path: &str, size: i64) -> BtFile {
        BtFile::from(&BtFileDto {
            index: 0,
            path: path.to_owned(),
            size,
        })
    }

    #[test]
    fn bt_category_uses_ordered_filename_rules_and_falls_back_to_other() {
        let mut rules = CustomCategoryDto::builtin_defaults();
        let mut subtitles = rules[1].clone();
        subtitles.id = "subtitles".into();
        subtitles.builtin_type = None;
        subtitles.extensions = vec![".SRT".into()];
        rules[1].extensions = vec!["avi".into()];
        rules.insert(1, subtitles.clone());
        let mut episodes = subtitles.clone();
        episodes.id = "episodes".into();
        episodes.match_mode = "regex".into();
        episodes.regex_pattern = r"^S\d+E\d+".into();
        rules.insert(0, episodes);
        let mut site = subtitles;
        site.id = "site".into();
        site.match_mode = "url".into();
        site.url_pattern = "*".into();
        rules.insert(0, site);
        let other = rules.len() - 1;

        let index = CategoryIndex::from_dtos(rules);
        assert_eq!(bt_category(r"Season\s01e02.AVI", &index, other), 1);
        assert_eq!(bt_category("Season/Track.srt", &index, other), 3);
        assert_eq!(bt_category("Season/Video.AVI", &index, other), 4);
        assert_eq!(bt_category("Season/Video.mkv", &index, other), other);
        assert_eq!(bt_category("Season.srt/.srt", &index, other), other);
    }

    #[test]
    fn bt_filter_matches_extension_and_directory_names() {
        let none = HashSet::new();
        let file = bt_file("Trailer/episode.MKV", 1);
        assert!(bt_file_matches("mkv", 0, &none, 0, &file));
        assert!(bt_file_matches("trailer", 0, &none, 0, &file));
        assert!(!bt_file_matches("srt", 0, &none, 0, &file));
        assert!(bt_file_matches("", 0, &none, 0, &file));
    }

    #[test]
    fn bt_filter_min_size_uses_1024_boundaries() {
        let none = HashSet::new();
        let min = BT_MIN_SIZES[1].0;
        assert_eq!(min, 10 * 1024 * 1024);
        assert!(bt_file_matches("", min, &none, 0, &bt_file("a", min)));
        assert!(!bt_file_matches("", min, &none, 0, &bt_file("a", min - 1)));
        assert_eq!(BT_MIN_SIZES[3].0, 1024 * 1024 * 1024);
    }

    #[test]
    fn bt_filter_combines_category_size_and_query() {
        let video = BT_CATEGORY_KEYS
            .iter()
            .position(|(id, _)| *id == "video")
            .unwrap();
        let audio = BT_CATEGORY_KEYS
            .iter()
            .position(|(id, _)| *id == "audio")
            .unwrap();
        let picked: HashSet<usize> = [video].into_iter().collect();
        let file = bt_file("Season/Trailer.mkv", 20 << 20);
        assert!(bt_file_matches("trailer", 10 << 20, &picked, video, &file));
        assert!(!bt_file_matches("trailer", 10 << 20, &picked, audio, &file));
        assert!(!bt_file_matches("other", 10 << 20, &picked, video, &file));
        assert!(!bt_file_matches("trailer", 30 << 20, &picked, video, &file));
    }

    /// 排序只定顺序，筛选定可见与批量范围：分类按原始位置取，批量勾选不碰隐藏项。
    #[test]
    fn sorted_bt_rows_filter_and_bulk_select_by_original_position() {
        struct Offline;

        impl DownloadsPort for Offline {
            fn execute(&self, _: DownloadsCommand) -> PortFuture<DownloadsResult> {
                Box::pin(async { Err(RpcErrorData::new(ApplicationErrorCode::Internal, false)) })
            }
        }

        let mut app = TestApp::new();
        let translator = app.update(|cx| {
            gpui_component::init(cx);
            fluxdown_ui_theme::init(cx);
            let catalog = I18nCatalog::load_embedded().expect("catalog");
            cx.new(|_| catalog.translator("en"))
        });
        let files = [
            (5, "small.mkv", 10),
            (6, "notes.txt", 50),
            (7, "large.mkv", 30),
            (8, "medium.mp4", 20),
        ]
        .map(|(index, path, size)| BtFileDto {
            index,
            path: path.to_owned(),
            size,
        });
        let mut win = app.open_window(|window, cx| {
            let request = request(SelectionKind::Bt {
                files: files.to_vec(),
                save_dir: String::new(),
            });
            let plan = SelectionPlan::for_request(&request, CustomCategoryDto::builtin_defaults())
                .expect("BT selection plan");
            SelectionView::new(
                translator,
                request,
                plan,
                "pack".to_owned(),
                Arc::new(Offline),
                window,
                cx,
            )
        });
        let visible = |view: &SelectionView| {
            let SelectionState::Bt { files, order, .. } = &view.state else {
                panic!("BT state");
            };
            let filter = view.bt_filter.as_ref().filter(|filter| filter.active());
            order
                .iter(files)
                .filter(|(position, file)| filter.is_none_or(|f| f.matches(*position, file)))
                .map(|(_, file)| file.index)
                .collect::<Vec<_>>()
        };
        win.update(|view, _, _| {
            let SelectionState::Bt { files, order, .. } = &mut view.state else {
                panic!("BT state");
            };
            order.set_sort(BtFileSort::Size, files);
            let filter = view.bt_filter.as_mut().expect("multi-file filter");
            let video = filter
                .rules
                .iter()
                .position(|rule| rule.builtin_type.as_deref() == Some("video"))
                .expect("video category");
            filter.expanded = true;
            filter.categories.insert(video);
        });
        win.update(|view, _, _| {
            assert_eq!(visible(view), [7, 8, 5]);
            view.set_bt_visible(false);
            let SelectionState::Bt { selected, .. } = &view.state else {
                panic!("BT state");
            };
            assert_eq!(*selected, HashSet::from([6]));
            view.set_bt_visible(true);
            let SelectionState::Bt { selected, .. } = &view.state else {
                panic!("BT state");
            };
            assert_eq!(*selected, HashSet::from([5, 6, 7, 8]));
        });
    }
}
