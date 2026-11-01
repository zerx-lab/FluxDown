//! 「新建下载」表单：字段、校验与提交；纯规则见 [`crate::model::new_download`]。
//!
//! 浏览器扩展 / NMH 的外部捕获也在本表单确认：捕获条目以链接行呈现（可与手填链接混合），
//! 提交时经 `agent.capture.resolve` 确认，Cookie / 请求头 / 请求体等上下文由 agent 合并；
//! 未确认的捕获在窗口关闭时一并忽略。

mod category;
mod hints;
mod preview;
mod probe;
mod url_rule;

use hints::HintAutofill;
use probe::LinkProbe;

use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use crate::{
    components::threads_risk::threads_risk_callout,
    controller::{
        DownloadsCommand, DownloadsPort, DownloadsResult, LAST_DOWNLOAD_TARGET_PREF,
        LAST_SAVE_DIR_PREF, REMEMBER_LAST_SAVE_DIR_PREF, SAVE_DIR_HISTORY_ENABLED_PREF,
        SAVE_DIR_HISTORY_PREF,
    },
    model::{
        devices::{DeviceEntry, DeviceKind, DispatchTarget, other_devices, resolve_target},
        dispatch::{RemoteDirCheck, check_remote_save_dir},
        new_download::{
            BuiltRequests, DEFAULT_HASH_ALGORITHM, DraftOptions, HASH_ALGORITHMS, MAX_THREADS,
            ProxyChoice, THREAD_PRESETS, ThreadChoice, UA_PRESET_CUSTOM, UA_PRESET_DEFAULT,
            UrlEntry, append_entries, build_requests, capture_entry, checksum_spec,
            compose_request, custom_segments, detect_ua_preset, manual_proxy_url, parse_entries,
            parse_input, ua_preset_keys, ua_preset_value,
        },
        save_dir_category::{CategoryMode, NewDownloadCategory, category_options},
        save_dir_history::{read_save_dir_history, record_save_dir, save_dir_choices},
    },
    strings::NewDownloadStrings,
    submission::{
        CapturedTask, NewDownloadSubmission, RemoteItem, RemoteSubmission, TorrentFileOptions,
    },
};
use fluxdown_protocol::{
    AgentSnapshot, CloudDevice, LinkDeviceInfo, PendingCaptureDto, QueueDto, SiteAuthCredentialDto,
};
use fluxdown_ui_components::{
    ControlExt as _, FluxIcon, IconControlExt as _, field_error, field_hint, field_label, form,
    form_field, form_gap, form_row, input_with_action, option_group, option_row,
};
use fluxdown_ui_i18n::Translator;
use fluxdown_ui_theme::active_theme;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, Div, Entity, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Window, canvas, deferred, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Icon, Sizable as _, Size, ThemeStyled as _, WindowExt as _,
    button::{Button, ButtonVariants as _, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};

/// 线程数选「自定义」时下拉收窄到的定宽（只需容纳「自定义」），右侧输入框吃满剩余宽度。
const CUSTOM_THREADS_DROPDOWN_WIDTH: f32 = 104.;
/// 预设下拉（UA、校验算法）定宽，右侧输入框吃满剩余宽度。
const PRESET_DROPDOWN_WIDTH: f32 = 140.;
/// 请求头名称列宽。
const HEADER_NAME_WIDTH: f32 = 168.;
/// 链接停止变化多久后才查询站点凭据（逐键输入时不逐字符发 RPC）。
const SITE_AUTH_LOOKUP_DEBOUNCE: Duration = Duration::from_millis(250);
/// 保存目录历史下拉的最大高度（超出滚动）。
const SAVE_DIR_MENU_MAX_HEIGHT: f32 = 320.;
/// 链接输入框随内容自增高的上限（行，含软换行）；默认一行，超出后框内滚动。与 Web 同值。
const URLS_MAX_ROWS: usize = 10;

/// 队列下拉候选（显示名由表单按内置队列本地化）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDownloadQueue {
    pub id: String,
    pub name: String,
}

/// 表单打开时的环境快照：默认值与下拉候选，由宿主从 daemon 快照 / 偏好投影。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewDownloadContext {
    /// 保存目录初值（沿用上次 / 全局默认）。
    pub save_dir: String,
    /// 保存目录历史（最新在前）；`None` = 偏好关闭，不列出也不记录。
    pub save_dir_history: Option<Vec<String>>,
    /// 「开始下载」默认落入的队列。
    pub queue_id: String,
    /// 线程数初值；0 = 自动。
    pub segments: i32,
    /// 队列清单（快照顺序）。
    pub queues: Vec<NewDownloadQueue>,
    /// 全局手动代理 URL；空 = 未配置（对应下拉项置灰）。
    pub manual_proxy_url: String,
    /// 预填链接（拖放）。
    pub initial_urls: Vec<String>,
    /// 分类目录路由（偏好 `custom_categories`）；`None` = 没有分类配置目录。
    pub category_route: Option<fluxdown_protocol::CategoryRouteDto>,
    /// 保存目录分类下拉的候选（同一偏好投影：可见、非 `all`、配置了目录的分类，按 `position`）。
    pub category_options: Vec<NewDownloadCategory>,
    /// 偏好 `custom_categories` 的原始列表（快捷添加网址规则时在其上追加再整体写回）。
    pub custom_categories: Vec<fluxdown_protocol::CustomCategoryDto>,
    /// 「下载到」候选：账号其他设备 + 已配对设备（本机始终可选，不在其中）；空 = 不显示选择器。
    pub targets: Vec<DeviceEntry>,
    /// 「下载到」初值：设备本地偏好里上次的目标（目标已不存在则为本机）。
    pub target: DispatchTarget,
}

/// 从 agent 快照投影「新建下载」环境（无主窗口时的外部捕获确认、菜单入口共用规则）。
#[must_use]
pub fn new_download_context_from_snapshot(snapshot: &AgentSnapshot) -> NewDownloadContext {
    build_new_download_context(
        &snapshot.daemon.config.values,
        &snapshot.daemon.runtime_stats.save_dir,
        &snapshot.preferences.values,
        &snapshot.daemon.queues,
        None,
        new_download_targets(
            &snapshot.cloud_devices,
            &snapshot.linked_devices,
            snapshot.cloud_connection.state == fluxdown_protocol::CloudConnectionState::Connected,
            true,
        ),
    )
}

/// 「下载到」候选设备（云账号其他设备 + 已配对设备；名册里的本机、重复 id 已剔除，同名
/// 设备已追加短码）。
#[must_use]
pub fn new_download_targets(
    cloud: &[CloudDevice],
    linked: &[LinkDeviceInfo],
    cloud_current: bool,
    service_current: bool,
) -> Vec<DeviceEntry> {
    other_devices(cloud, linked, cloud_current, service_current)
}

/// 偏好 `remember_last_save_dir` 开启且有记录时沿用上次目录，否则用全局
/// 默认（`default_save_dir`，空则 daemon 运行时目录）；队列优先 `selected_queue`，其次配置
/// `default_queue_id`，最后主队列；线程数优先队列 `default_segments`，其次全局配置。
pub(crate) fn build_new_download_context(
    config: &BTreeMap<String, String>,
    runtime_save_dir: &str,
    preferences: &BTreeMap<String, serde_json::Value>,
    queues: &[QueueDto],
    selected_queue: Option<&str>,
    targets: Vec<DeviceEntry>,
) -> NewDownloadContext {
    let config_str = |key: &str| config.get(key).map_or("", |value| value.trim());
    let remember = preferences
        .get(REMEMBER_LAST_SAVE_DIR_PREF)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let last_save_dir = preferences
        .get(LAST_SAVE_DIR_PREF)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let save_dir = if remember && !last_save_dir.is_empty() {
        last_save_dir
    } else {
        match config_str("default_save_dir") {
            "" => runtime_save_dir,
            configured => configured,
        }
    };
    let queue_id = selected_queue.unwrap_or_else(|| match config_str("default_queue_id") {
        "" => fluxdown_protocol::MAIN_QUEUE_ID,
        configured => configured,
    });
    let queue_segments = queues
        .iter()
        .find(|queue| queue.queue_id == queue_id)
        .map_or(0, |queue| queue.default_segments);
    let segments = if queue_segments > 0 {
        queue_segments
    } else {
        config_str("default_segments").parse::<i32>().unwrap_or(0)
    };
    let save_dir_history = preferences
        .get(SAVE_DIR_HISTORY_ENABLED_PREF)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
        .then(|| read_save_dir_history(preferences.get(SAVE_DIR_HISTORY_PREF)));
    let target = preferences
        .get(LAST_DOWNLOAD_TARGET_PREF)
        .and_then(serde_json::Value::as_str)
        .and_then(DispatchTarget::from_pref)
        // `link:<fp>` 已挂接到云条目时解析为该云条目；目标已不存在则回退本机。
        .and_then(|target| resolve_target(target, &targets))
        .unwrap_or_default();
    let categories = fluxdown_protocol::CustomCategoryDto::from_preference(
        preferences.get(fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY),
    );
    NewDownloadContext {
        save_dir: save_dir.to_owned(),
        save_dir_history,
        queue_id: queue_id.to_owned(),
        segments,
        queues: queues
            .iter()
            .map(|queue| NewDownloadQueue {
                id: queue.queue_id.clone(),
                name: queue.name.clone(),
            })
            .collect(),
        manual_proxy_url: manual_proxy_url(config),
        category_route: fluxdown_protocol::CategoryRouteDto::from_categories(&categories),
        category_options: category_options(&categories),
        custom_categories: categories,
        initial_urls: Vec::new(),
        targets,
        target,
    }
}

/// 「自定义线程数」输入框的初值：默认分段数大于 0 时预填，否则留空。
fn custom_threads_text(segments: i32) -> String {
    if segments > 0 {
        segments.to_string()
    } else {
        String::new()
    }
}

/// 新建下载对话框的提交回调。
pub type NewDownloadSubmit = Rc<dyn Fn(NewDownloadSubmission, &mut Window, &mut App)>;

struct HeaderRow {
    id: usize,
    key: Entity<InputState>,
    value: Entity<InputState>,
}

/// HTTP 认证框的站点凭据自动回填状态。
#[derive(Default)]
struct AuthAutofill {
    /// 两框当前值来自站点凭据自动回填（链接换站点时可被更新 / 清空）。
    filled: bool,
    /// 用户手动编辑过认证框：此后本表单不再自动覆盖。
    dirty: bool,
    /// 当前回填目标链接（单条 http(s) 且不沿用浏览器认证时才有）。
    target: Option<String>,
}

/// 独立窗口承载的「新建下载」表单。
///
/// 提交或取消都会关闭自身所在窗口（「打开种子文件」只提交种子：表单里还有待处理链接时
/// 窗口保留，链接原样留在表单中）；任务创建失败的提示由宿主展示。视图释放时仍未确认的
/// 外部捕获由宿主经 [`Self::take_captures`] 取走并忽略。
// 清单预解析与返回保留本表单实体，取消不会触发建任务。
pub struct NewDownloadView {
    strings: NewDownloadStrings,
    translator: Entity<Translator>,
    preview_gate: crate::model::preview::PreviewGate,
    preview_request: Option<(fluxdown_protocol::CreateTaskRequest, Option<String>)>,
    manifest: Option<Entity<crate::pages::manifest::ManifestView>>,
    context: NewDownloadContext,
    port: Arc<dyn DownloadsPort>,
    on_submit: NewDownloadSubmit,
    urls: Entity<TextareaState>,
    entries: Vec<UrlEntry>,
    /// 尚未确认 / 忽略的外部捕获；按 URL 与链接行对应。
    captures: Vec<PendingCaptureDto>,
    save_dir: Entity<InputState>,
    threads: ThreadChoice,
    custom_threads: Entity<InputState>,
    rename: Entity<InputState>,
    advanced_open: bool,
    http_user: Entity<InputState>,
    http_password: Entity<InputState>,
    save_site_auth: bool,
    auth_autofill: AuthAutofill,
    hint_fill: HintAutofill,
    /// 链接预探测（#402）：文件名 / 大小提示，并把探测名预填进「文件名」输入框供编辑。
    probe: LinkProbe,
    proxy_choice: ProxyChoice,
    custom_proxy: Entity<InputState>,
    ignore_tls_errors: bool,
    ua_preset: &'static str,
    user_agent: Entity<InputState>,
    cookie: Entity<TextareaState>,
    hash_algorithm: &'static str,
    checksum: Entity<InputState>,
    headers: Vec<HeaderRow>,
    header_seq: usize,
    picking: bool,
    /// 「下载到」当前目标；非本机时必然存在于 `context.targets`（设备消失即回退本机）。
    target: DispatchTarget,
    /// 远端保存目录（与本机 `save_dir` 各自保留，来回切换目标不互相覆盖）；
    /// 空 = 使用目标设备默认目录。
    remote_save_dir: Entity<InputState>,
    /// 保存目录历史下拉是否展开（按下输入框展开，点外部 / 选中 / 打开目录选择器收起）。
    save_dir_menu_open: bool,
    /// 保存目录组合框上一帧的窗口坐标，用于判定外部点击是否落在输入框上。
    save_dir_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// 保存目录的分类模式（默认自动）；仅 `context.category_route` 存在时有意义。
    category_mode: CategoryMode,
    /// 自动模式上一次写入目录框的值：目录框仍等于它说明用户没手动改过，可继续跟随。
    category_auto_dir: Option<String>,
    /// 自动模式按当前输入预测出的分类目录（触发器显示命中分类）；变化时同步更新，渲染只读。
    category_predicted: Option<String>,
}

impl NewDownloadView {
    /// 创建表单并聚焦链接输入框。`port` 用于站点凭据匹配。
    pub fn new(
        translator: Entity<Translator>,
        context: NewDownloadContext,
        port: Arc<dyn DownloadsPort>,
        on_submit: NewDownloadSubmit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strings = NewDownloadStrings::from_translator(translator.read(cx));
        let urls = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, URLS_MAX_ROWS)
                .default_value(context.initial_urls.join("\n"))
                .placeholder(strings.url_placeholder.clone())
        });
        let save_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(context.save_dir.clone())
                .placeholder(strings.save_dir_placeholder.clone())
        });
        let custom_threads = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(custom_threads_text(context.segments))
                .placeholder(strings.threads_custom_hint.clone())
                .validate(|text, _| text.chars().all(|c| c.is_ascii_digit()))
        });
        let http_password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(strings.http_auth_password.clone())
        });
        let cookie = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder(strings.cookie_placeholder.clone())
        });
        let rename = cx
            .new(|cx| InputState::new(window, cx).placeholder(strings.rename_placeholder.clone()));
        urls.update(cx, |input, cx| input.focus(window, cx));
        let target = if context
            .targets
            .iter()
            .any(|entry| entry.target() == context.target)
        {
            context.target.clone()
        } else {
            DispatchTarget::Local
        };
        let remote_placeholder = context
            .targets
            .iter()
            .find(|entry| entry.target() == target)
            .map_or_else(
                || strings.remote_dir_placeholder(None),
                |entry| strings.remote_dir_placeholder(entry.default_save_dir.as_deref()),
            );
        let remote_save_dir =
            cx.new(|cx| InputState::new(window, cx).placeholder(remote_placeholder));

        let mut this = Self {
            threads: ThreadChoice::from_segments(context.segments),
            rename,
            http_user: Self::input(strings.http_auth_user.clone(), window, cx),
            custom_proxy: Self::input(strings.proxy_placeholder.clone(), window, cx),
            user_agent: Self::input(strings.user_agent_desc.clone(), window, cx),
            checksum: Self::input(strings.checksum_placeholder.clone(), window, cx),
            probe: LinkProbe::new(strings.rename_placeholder.clone()),
            strings,
            translator: translator.clone(),
            preview_gate: crate::model::preview::PreviewGate::default(),
            preview_request: None,
            manifest: None,
            context,
            port,
            on_submit,
            urls,
            entries: Vec::new(),
            captures: Vec::new(),
            save_dir,
            custom_threads,
            advanced_open: false,
            http_password,
            save_site_auth: false,
            auth_autofill: AuthAutofill::default(),
            hint_fill: HintAutofill::default(),
            proxy_choice: ProxyChoice::FollowGlobal,
            ignore_tls_errors: false,
            ua_preset: UA_PRESET_DEFAULT,
            cookie,
            hash_algorithm: DEFAULT_HASH_ALGORITHM,
            headers: Vec::new(),
            header_seq: 0,
            picking: false,
            target,
            remote_save_dir,
            save_dir_menu_open: false,
            save_dir_bounds: Rc::default(),
            category_mode: CategoryMode::Auto,
            category_auto_dir: None,
            category_predicted: None,
        };
        this.refresh_entries(window, cx);
        this.subscribe_inputs(&translator, window, cx);
        this
    }

    fn input(
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
    }

    /// 文案随语言切换（含输入框占位：它们只在创建时取一次文案）；链接文本每次变化重新解析
    /// （驱动计数 / 批量态 / 按钮可用性），⌘/Ctrl+Enter 提交；单行输入框 Enter 提交；UA 手动
    /// 编辑时反推预设。
    fn subscribe_inputs(
        &self,
        translator: &Entity<Translator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.observe_in(translator, window, |this, translator, window, cx| {
            this.strings = NewDownloadStrings::from_translator(translator.read(cx));
            this.refresh_placeholders(window, cx);
            cx.notify();
        })
        .detach();
        cx.subscribe_in(
            &self.urls,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.refresh_entries(window, cx),
                InputEvent::PressEnter {
                    secondary: true, ..
                } => this.submit(false, None, window, cx),
                _ => {}
            },
        )
        .detach();
        for input in [
            &self.save_dir,
            &self.remote_save_dir,
            &self.custom_threads,
            &self.rename,
        ] {
            cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { shift: false, .. } => {
                        this.submit(false, None, window, cx);
                    }
                    _ => {}
                },
            )
            .detach();
        }
        // 重命名框是自动分类的预测输入之一；探测预填走 `set_value`（不发 Change），由探测同步处理。
        cx.subscribe_in(
            &self.rename,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::Change = event {
                    this.sync_category(window, cx);
                }
            },
        )
        .detach();
        cx.subscribe(&self.user_agent, |this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let detected = detect_ua_preset(&input.read(cx).value());
                if detected != this.ua_preset {
                    this.ua_preset = detected;
                    cx.notify();
                }
            }
        })
        .detach();
        // `set_value`（自动回填）不发 Change：这里只会收到用户手动编辑。
        for input in [&self.http_user, &self.http_password] {
            cx.subscribe(input, |this, _, event: &InputEvent, _| {
                if let InputEvent::Change = event {
                    this.auth_autofill.dirty = true;
                    this.auth_autofill.filled = false;
                }
            })
            .detach();
        }
    }

    /// 按当前语言重设全部输入框占位（打开表单时语言可能尚未从偏好恢复）。
    fn refresh_placeholders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 占位随探测结果：探测名只作提示，语言切换时不能把它重置回默认文案。
        let rename_placeholder = self.rename_placeholder();
        self.probe.set_placeholder(rename_placeholder.clone());
        let strings = &self.strings;
        let inputs = [
            (&self.save_dir, &strings.save_dir_placeholder),
            (&self.custom_threads, &strings.threads_custom_hint),
            (&self.rename, &rename_placeholder),
            (&self.http_user, &strings.http_auth_user),
            (&self.http_password, &strings.http_auth_password),
            (&self.custom_proxy, &strings.proxy_placeholder),
            (&self.user_agent, &strings.user_agent_desc),
            (&self.checksum, &strings.checksum_placeholder),
        ]
        .into_iter()
        .chain(self.headers.iter().flat_map(|row| {
            [
                (&row.key, &strings.header_name),
                (&row.value, &strings.header_value),
            ]
        }));
        for (input, placeholder) in inputs {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder.clone(), window, cx)
            });
        }
        for (textarea, placeholder) in [
            (&self.urls, &strings.url_placeholder),
            (&self.cookie, &strings.cookie_placeholder),
        ] {
            textarea.update(cx, |textarea, cx| {
                textarea.set_placeholder(placeholder.clone(), window, cx)
            });
        }
        self.refresh_remote_placeholder(window, cx);
    }

    fn refresh_entries(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parsed = parse_input(&self.urls.read(cx).value(), false);
        self.entries = parsed.entries;
        self.sync_site_auth(window, cx);
        self.sync_hints(parsed.hints, window, cx);
        self.sync_probe(window, cx);
        cx.notify();
    }

    /// 追加外部捕获：未见过的事务按链接行（含 `out=` 文件名）追加到链接框末尾，不改动
    /// 已有文本；表单原本为空时沿用捕获方指定的保存目录。
    pub fn add_captures(
        &mut self,
        captures: Vec<PendingCaptureDto>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fresh = captures
            .into_iter()
            .filter(|capture| {
                !self
                    .captures
                    .iter()
                    .any(|known| known.transaction_id == capture.transaction_id)
            })
            .collect::<Vec<_>>();
        if fresh.is_empty() {
            return;
        }
        let current = self.urls.read(cx).value().to_string();
        if current.trim().is_empty()
            && let Some(save_dir) = fresh
                .iter()
                .map(|capture| capture.save_dir.trim())
                .find(|save_dir| !save_dir.is_empty())
        {
            let save_dir = save_dir.to_owned();
            // 捕获方带来的分类目录视为自动写入值：之后仍可跟随文件名变化。
            if self
                .context
                .category_route
                .as_ref()
                .is_some_and(|route| route.is_category_dir(&save_dir))
            {
                self.category_auto_dir = Some(save_dir.clone());
            }
            self.save_dir
                .update(cx, |input, cx| input.set_value(save_dir, window, cx));
        }
        let text = append_entries(
            &current,
            fresh
                .iter()
                .map(|capture| capture_entry(&capture.url, &capture.file_name)),
        );
        self.urls
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.captures.extend(fresh);
        self.refresh_entries(window, cx);
    }

    /// 宿主在窗口已打开时追加链接（如拖入链接文件）：原文逐字保留，已存在的 URL 跳过。
    pub fn append_urls(&mut self, urls: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.urls.read(cx).value().to_string();
        let text = append_entries(
            &current,
            urls.into_iter().map(|url| UrlEntry {
                url,
                ..UrlEntry::default()
            }),
        );
        if text == current.trim_end() {
            return;
        }
        self.urls
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.refresh_entries(window, cx);
    }

    /// 只保留 agent 仍在等待确认的捕获（其余已在别处处理 / agent 重启丢失）；对应链接行
    /// 保留为普通链接。
    pub fn retain_captures(
        &mut self,
        pending: &[PendingCaptureDto],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.captures.len();
        self.captures.retain(|capture| {
            pending
                .iter()
                .any(|pending| pending.transaction_id == capture.transaction_id)
        });
        if self.captures.len() != before {
            self.refresh_entries(window, cx);
        }
    }

    fn capture_for(&self, url: &str) -> Option<&PendingCaptureDto> {
        self.captures.iter().find(|capture| capture.url == url)
    }

    /// 单条链接且是带 `Authorization` 的浏览器捕获：沿用浏览器认证，不回填站点凭据。
    fn uses_browser_auth(&self) -> bool {
        matches!(self.entries.as_slice(), [entry]
            if self.capture_for(&entry.url).is_some_and(PendingCaptureDto::has_authorization))
    }

    /// 站点凭据回填目标：单条 http(s) 链接，且不沿用浏览器认证。
    fn site_auth_target(&self) -> Option<String> {
        let [entry] = self.entries.as_slice() else {
            return None;
        };
        let lower = entry.url.to_ascii_lowercase();
        let http = lower.starts_with("http://") || lower.starts_with("https://");
        (http && !self.uses_browser_auth()).then(|| entry.url.clone())
    }

    /// 链接变化后按站点键查已保存凭据并回填 / 更新 / 清空认证两框（用户手动编辑过则不动）。
    /// 查询去抖；结果回来时目标已变则丢弃。
    fn sync_site_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auth_autofill.dirty {
            return;
        }
        let target = self.site_auth_target();
        if target == self.auth_autofill.target {
            return;
        }
        self.auth_autofill.target.clone_from(&target);
        let Some(url) = target else {
            self.clear_autofilled_auth(window, cx);
            return;
        };
        let port = self.port.clone();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(SITE_AUTH_LOOKUP_DEBOUNCE)
                .await;
            let current = this
                .read_with(cx, |this, _| {
                    this.auth_autofill.target.as_deref() == Some(url.as_str())
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let credential = match port
                .execute(DownloadsCommand::SiteAuthMatch { url: url.clone() })
                .await
            {
                Ok(DownloadsResult::Value(value)) => {
                    serde_json::from_value::<Option<SiteAuthCredentialDto>>(value)
                        .ok()
                        .flatten()
                }
                _ => None,
            };

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                if this.auth_autofill.dirty
                    || this.auth_autofill.target.as_deref() != Some(url.as_str())
                {
                    return;
                }
                match credential {
                    Some(credential) => {
                        this.http_user
                            .update(cx, |input, cx| input.set_value(credential.user, window, cx));
                        this.http_password
                            .update(cx, |input, cx| input.set_value(credential.pass, window, cx));
                        this.auth_autofill.filled = true;
                        cx.notify();
                    }
                    None => this.clear_autofilled_auth(window, cx),
                }
            }) else {
                // 表单或窗口已释放，不再回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn clear_autofilled_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.auth_autofill.filled {
            return;
        }
        for input in [&self.http_user, &self.http_password] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.auth_autofill.filled = false;
        cx.notify();
    }

    /// 取走仍未确认的外部捕获（宿主在视图释放时忽略它们，agent 不留悬挂事务）。
    pub fn take_captures(&mut self) -> Vec<PendingCaptureDto> {
        std::mem::take(&mut self.captures)
    }

    fn is_batch(&self) -> bool {
        self.entries.len() > 1
    }

    fn all_magnet(&self) -> bool {
        !self.entries.is_empty()
            && self.entries.iter().all(|entry| {
                entry
                    .url
                    .get(..7)
                    .is_some_and(|s| s.eq_ignore_ascii_case("magnet:"))
            })
    }

    /// 当前远端目标；本机为 `None`。
    fn target_entry(&self) -> Option<&DeviceEntry> {
        match &self.target {
            DispatchTarget::Local => None,
            target => self
                .context
                .targets
                .iter()
                .find(|entry| entry.target() == *target),
        }
    }

    fn is_remote(&self) -> bool {
        self.target_entry().is_some()
    }

    /// 远端保存目录输入按目标路径风格的校验。
    fn remote_dir_check(&self, entry: &DeviceEntry, cx: &App) -> RemoteDirCheck {
        check_remote_save_dir(&self.remote_save_dir.read(cx).value(), entry.path_style)
    }

    fn can_submit(&self, cx: &App) -> bool {
        if self.picking
            || self.preview_gate.is_active()
            || self.manifest.is_some()
            || self.entries.is_empty()
        {
            return false;
        }
        match self.target_entry() {
            Some(entry) => self.remote_dir_check(entry, cx) != RemoteDirCheck::Invalid,
            None => !self.save_dir.read(cx).value().trim().is_empty(),
        }
    }

    fn target_label(&self, entry: &DeviceEntry) -> SharedString {
        let status = match entry.online {
            Some(true) => &self.strings.device_online,
            Some(false) => &self.strings.device_offline,
            None => &self.strings.device_presence_unknown,
        };
        if entry.is_standalone_link() {
            SharedString::from(format!(
                "{} · {} · {status}",
                entry.label, self.strings.device_lan_tag
            ))
        } else {
            SharedString::from(format!("{} · {status}", entry.label))
        }
    }

    /// 切换「下载到」目标：远端保存目录占位随目标默认目录刷新。
    fn set_target(&mut self, target: DispatchTarget, window: &mut Window, cx: &mut Context<Self>) {
        if target != DispatchTarget::Local
            && !self
                .context
                .targets
                .iter()
                .any(|entry| entry.target() == target)
        {
            return;
        }
        self.target = target;
        self.refresh_remote_placeholder(window, cx);
        cx.notify();
    }

    fn refresh_remote_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.strings.remote_dir_placeholder(
            self.target_entry()
                .and_then(|entry| entry.default_save_dir.as_deref()),
        );
        self.remote_save_dir.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
    }

    /// 宿主推送最新的设备名册（在线状态 / 默认目录 / 增减设备）。当前目标已消失则回退本机。
    pub fn set_targets(
        &mut self,
        targets: Vec<DeviceEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context.targets == targets {
            return;
        }
        self.context.targets = targets;
        if self.target != DispatchTarget::Local && self.target_entry().is_none() {
            self.target = DispatchTarget::Local;
        }
        self.refresh_remote_placeholder(window, cx);
        cx.notify();
    }

    /// 宿主拿到 daemon 数据后重投影的环境（冷启动由浏览器扩展唤起时，首个快照尚不带 daemon
    /// 配置 / 队列 / 运行时目录，表单按空环境打开）：队列、手动代理等候选整体替换；保存目录与
    /// 线程数只替换仍停在旧初值的（用户已改、捕获已指定的保持不动）。「下载到」名册与目标
    /// 由 [`Self::set_targets`] 维护，这里不动。
    pub fn refresh_context(
        &mut self,
        context: NewDownloadContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NewDownloadContext {
            save_dir,
            save_dir_history,
            queue_id,
            segments,
            queues,
            category_route,
            category_options,
            custom_categories,
            manual_proxy_url,
            ..
        } = context;
        if save_dir != self.context.save_dir
            && *self.save_dir.read(cx).value() == *self.context.save_dir
        {
            let value = save_dir.clone();
            self.save_dir
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let previous_segments = self.context.segments;
        if segments != previous_segments
            && self.threads == ThreadChoice::from_segments(previous_segments)
            && *self.custom_threads.read(cx).value() == *custom_threads_text(previous_segments)
        {
            self.threads = ThreadChoice::from_segments(segments);
            self.custom_threads.update(cx, |input, cx| {
                input.set_value(custom_threads_text(segments), window, cx)
            });
        }
        if manual_proxy_url.is_empty() && self.proxy_choice == ProxyChoice::GlobalManual {
            self.proxy_choice = ProxyChoice::FollowGlobal;
        }
        self.context.save_dir = save_dir;
        self.context.save_dir_history = save_dir_history;
        self.context.queue_id = queue_id;
        self.context.segments = segments;
        self.context.queues = queues;
        self.context.category_route = category_route;
        self.context.category_options = category_options;
        self.context.custom_categories = custom_categories;
        if let CategoryMode::Category(id) = &self.category_mode
            && !self
                .context
                .category_options
                .iter()
                .any(|option| option.id == *id)
        {
            self.category_mode = CategoryMode::Auto;
        }
        self.context.manual_proxy_url = manual_proxy_url;
        self.sync_category(window, cx);
        cx.notify();
    }

    /// 把一次本机提交的保存目录记入历史（偏好关闭时不记）；尽力而为，失败只做诊断记录。
    fn record_save_dir_history(&mut self, dir: &str, cx: &mut Context<Self>) {
        let Some(history) = self.context.save_dir_history.as_mut() else {
            return;
        };
        let Some(next) = record_save_dir(history, dir) else {
            return;
        };
        let value = serde_json::Value::from(next.clone());
        *history = next;
        self.write_local_preference(SAVE_DIR_HISTORY_PREF, value, cx);
    }

    /// 设备本地偏好写入：尽力而为，失败只做诊断记录。
    fn write_local_preference(
        &self,
        key: &'static str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let write = self
            .port
            .execute(DownloadsCommand::SetLocalPreference { key, value });
        // 表单随后可能关窗：写入在后台执行器上跑完，不随视图释放而取消。
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = write.await {
                    eprintln!(
                        "local preference write failed: {:?} ({:?})",
                        error.code, error.reason
                    );
                }
            })
            .detach();
    }

    fn segments(&self, cx: &App) -> i32 {
        match self.threads {
            ThreadChoice::Auto => 0,
            ThreadChoice::Preset(value) => value.min(MAX_THREADS),
            ThreadChoice::Custom => custom_segments(&self.custom_threads.read(cx).value()),
        }
    }

    fn header_map(&self, cx: &App) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for row in &self.headers {
            let key = row.key.read(cx).value().trim().to_owned();
            if key.is_empty() {
                continue;
            }
            map.insert(key, row.value.read(cx).value().trim().to_owned());
        }
        map
    }

    /// 草稿 → 请求选项。
    fn draft_options(&self, later: bool, queue_override: Option<String>, cx: &App) -> DraftOptions {
        let queue_id = queue_override.unwrap_or_else(|| {
            if later {
                fluxdown_protocol::LATER_QUEUE_ID.to_owned()
            } else {
                self.context.queue_id.clone()
            }
        });
        DraftOptions {
            save_dir: self.save_dir.read(cx).value().trim().to_owned(),
            segments: self.segments(cx),
            cookies: self.cookie.read(cx).value().trim().to_owned(),
            proxy_url: self.proxy_choice.wire(
                &self.context.manual_proxy_url,
                &self.custom_proxy.read(cx).value(),
            ),
            user_agent: self.user_agent.read(cx).value().trim().to_owned(),
            queue_id,
            checksum: checksum_spec(self.hash_algorithm, &self.checksum.read(cx).value()),
            ignore_tls_errors: self.ignore_tls_errors,
            headers: self.header_map(cx),
            start_paused: later,
            rename: self.rename_value(cx),
            http_user: self.http_user.read(cx).value().trim().to_owned(),
            http_password: self.http_password.read(cx).value().to_string(),
            save_site_auth: self.save_site_auth,
        }
    }

    fn submit(
        &mut self,
        later: bool,
        queue_override: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_submit(cx) {
            return;
        }
        // 远端目标没有队列 / 稍后下载：只有立即开始一条路径。
        if let Some(entry) = self.target_entry().cloned() {
            if !later && queue_override.is_none() {
                self.submit_remote(&entry, window, cx);
            }
            return;
        }
        let options = self.draft_options(later, queue_override, cx);
        let built = build_requests(&self.entries, &options);
        if let [item] = built.items.as_slice() {
            let request = built.request_for(item);
            if crate::model::preview::previewable(&request) {
                self.start_preview(request, window, cx);
                return;
            }
        }
        self.submit_requests(built, window, cx);
    }

    fn submit_requests(
        &mut self,
        built: BuiltRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let BuiltRequests { common, items } = built;
        let mut tasks = Vec::with_capacity(items.len());
        let mut captures = Vec::new();
        for item in items {
            match self
                .captures
                .iter()
                .position(|capture| capture.url == item.url)
            {
                Some(index) => captures.push(CapturedTask {
                    transaction_id: self.captures.remove(index).transaction_id,
                    request: compose_request(&common, item),
                }),
                None => tasks.push(item),
            }
        }
        self.record_save_dir_history(&common.save_dir, cx);
        let offered_save_dir = self.context.save_dir.clone();
        (self.on_submit)(
            NewDownloadSubmission::Tasks {
                common: Box::new(common),
                items: tasks,
                captures,
                category_route: self
                    .context
                    .category_route
                    .clone()
                    .filter(|_| self.category_mode == CategoryMode::Auto),
                offered_save_dir,
            },
            window,
            cx,
        );
        window.remove_window();
    }

    fn pick_torrent_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        self.picking = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(self.strings.select_torrent.clone()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths
                    .into_iter()
                    .filter(|path| has_extension(path, &["torrent"]))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking = false;
                cx.notify();
                if paths.is_empty() {
                    return;
                }
                let options = TorrentFileOptions {
                    save_dir: this.save_dir.read(cx).value().trim().to_owned(),
                    queue_id: this.context.queue_id.clone(),
                    start_paused: false,
                };
                (this.on_submit)(
                    NewDownloadSubmission::TorrentFiles { paths, options },
                    window,
                    cx,
                );
                // 只提交了种子：表单里尚未提交的链接留给用户继续处理，没有待处理链接才关窗。
                if this.entries.is_empty() {
                    window.remove_window();
                }
            }) else {
                // 表单或窗口已释放，不再回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn import_txt_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking {
            return;
        }
        self.picking = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(self.strings.import_txt.clone()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths
                    .into_iter()
                    .filter(|path| has_extension(path, &["txt", "text"]))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let cancelled = paths.is_empty();
            // 读文件在后台线程：单文件读取失败跳过，继续处理其他文件。
            let imported = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .iter()
                        .filter_map(|path| std::fs::read_to_string(path).ok())
                        .flat_map(|content| parse_entries(&content, true))
                        .collect::<Vec<_>>()
                })
                .await;

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking = false;
                cx.notify();
                if cancelled {
                    return;
                }
                if imported.is_empty() {
                    window.push_notification(
                        Notification::warning(this.strings.import_txt_none.clone()),
                        cx,
                    );
                    return;
                }
                let count = imported.len();
                let merged = append_entries(&this.urls.read(cx).value(), imported);
                this.urls
                    .update(cx, |input, cx| input.set_value(merged, window, cx));
                this.refresh_entries(window, cx);
                window.push_notification(
                    Notification::success(this.strings.format_import_found(count)),
                    cx,
                );
            }) else {
                // 表单或窗口已释放，不再回写异步结果。
                return;
            };
        })
        .detach();
    }

    /// 下发到其他设备：每条链接一次下发，目录空 = 目标设备默认目录；已随下发处理的外部
    /// 捕获从表单摘出（宿主忽略 agent 里对应的待确认事务）。
    fn submit_remote(&mut self, entry: &DeviceEntry, window: &mut Window, cx: &mut Context<Self>) {
        let save_dir = match self.remote_dir_check(entry, cx) {
            RemoteDirCheck::Invalid => return,
            RemoteDirCheck::UseDefault => None,
            RemoteDirCheck::Explicit(dir) => Some(dir),
        };
        let single = self.entries.len() == 1;
        let rename = self.rename_value(cx);
        let items = self
            .entries
            .iter()
            .map(|item| RemoteItem {
                url: item.url.clone(),
                file_name: if single && !rename.is_empty() {
                    Some(rename.clone())
                } else {
                    Some(item.file_name.clone()).filter(|name| !name.is_empty())
                },
            })
            .collect::<Vec<_>>();
        let mut captures = Vec::new();
        for item in &items {
            if let Some(index) = self
                .captures
                .iter()
                .position(|capture| capture.url == item.url)
            {
                captures.push(self.captures.remove(index).transaction_id);
            }
        }
        // 挂接直连且在线 → 局域网直连（一次性投递），否则云端 / 独立直连按条目本身。
        let route = entry.route();
        (self.on_submit)(
            NewDownloadSubmission::Remote(RemoteSubmission {
                target: route.clone(),
                device_label: entry.label.clone(),
                target_offline: matches!(route, DispatchTarget::Cloud(_))
                    && entry.online == Some(false),
                items,
                save_dir,
                captures,
            }),
            window,
            cx,
        );
        window.remove_window();
    }

    fn pick_save_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 系统目录选择器只能选本机目录；远端目录靠输入。
        if self.picking || self.is_remote() {
            return;
        }
        self.save_dir_menu_open = false;
        self.picking = true;
        cx.notify();
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(self.strings.save_dir_placeholder.clone()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.first().map(|path| path.display().to_string()),
                _ => None,
            };

            let Ok(()) = this.update_in(cx, |this, window, cx| {
                this.picking = false;
                if let Some(path) = picked {
                    this.save_dir
                        .update(cx, |input, cx| input.set_value(path, window, cx));
                }
                cx.notify();
            }) else {
                // 表单或窗口已释放，不再回写异步结果。
                return;
            };
        })
        .detach();
    }

    fn add_header_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.header_seq += 1;
        let key_hint = self.strings.header_name.clone();
        let value_hint = self.strings.header_value.clone();
        let key = cx.new(|cx| InputState::new(window, cx).placeholder(key_hint));
        let value = cx.new(|cx| InputState::new(window, cx).placeholder(value_hint));
        self.headers.push(HeaderRow {
            id: self.header_seq,
            key,
            value,
        });
        cx.notify();
    }

    fn set_threads(&mut self, choice: ThreadChoice, window: &mut Window, cx: &mut Context<Self>) {
        if choice == ThreadChoice::Custom && self.threads != ThreadChoice::Custom {
            // 进入自定义：若当前是数字预设则预填，便于快速编辑。
            let prefill = match self.threads {
                ThreadChoice::Preset(value) => value.to_string(),
                _ => String::new(),
            };
            self.custom_threads.update(cx, |input, cx| {
                input.set_value(prefill, window, cx);
                input.focus(window, cx);
            });
        }
        self.threads = choice;
        cx.notify();
    }

    fn set_ua_preset(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.ua_preset = key;
        if key != UA_PRESET_CUSTOM {
            let value = ua_preset_value(key);
            self.user_agent
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        cx.notify();
    }

    fn queue_label(&self, queue_id: &str) -> SharedString {
        self.context
            .queues
            .iter()
            .find(|queue| queue.id == queue_id)
            .map_or_else(
                || self.strings.queue_name(queue_id, queue_id),
                |queue| self.strings.queue_name(&queue.id, &queue.name),
            )
    }

    fn proxy_label(&self, choice: ProxyChoice) -> SharedString {
        match choice {
            ProxyChoice::FollowGlobal => self.strings.proxy_follow.clone(),
            ProxyChoice::Direct => self.strings.proxy_direct.clone(),
            ProxyChoice::System => self.strings.proxy_system.clone(),
            ProxyChoice::GlobalManual => self.strings.proxy_global_manual.clone(),
            ProxyChoice::Custom => self.strings.proxy_custom.clone(),
        }
    }

    fn ua_label(&self, key: &str) -> SharedString {
        match key {
            "chrome" => self.strings.ua_chrome.clone(),
            "firefox" => self.strings.ua_firefox.clone(),
            "edge" => self.strings.ua_edge.clone(),
            "safari" => self.strings.ua_safari.clone(),
            UA_PRESET_CUSTOM => self.strings.ua_custom.clone(),
            _ => self.strings.ua_inherit.clone(),
        }
    }

    fn threads_label(&self, choice: ThreadChoice) -> SharedString {
        match choice {
            ThreadChoice::Auto => self.strings.threads_auto.clone(),
            ThreadChoice::Preset(value) => SharedString::from(value.to_string()),
            ThreadChoice::Custom => self.strings.threads_custom.clone(),
        }
    }

    /// 单选下拉：当前项打勾，禁用项置灰；选择后经弱引用回写表单。
    /// `width = None` 时铺满所在字段宽度（与输入框同高同宽）。
    fn dropdown<T: Clone + PartialEq + 'static>(
        &self,
        id: &'static str,
        width: Option<Pixels>,
        current: T,
        options: Vec<(T, SharedString, bool)>,
        on_pick: impl Fn(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = options
            .iter()
            .find(|(value, _, _)| *value == current)
            .map(|(_, label, _)| label.clone())
            .unwrap_or_default();
        let this = cx.weak_entity();
        let on_pick = Rc::new(on_pick);
        Button::new(id)
            .outline()
            .control(cx)
            .label(label)
            .dropdown_caret(true)
            .map(|button| match width {
                Some(width) => button.flex_none().w(width),
                None => button.w_full(),
            })
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                options.iter().fold(menu, |menu, (value, label, disabled)| {
                    let checked = *value == current;
                    let this = this.clone();
                    let value = value.clone();
                    let on_pick = on_pick.clone();
                    menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(checked)
                            .disabled(*disabled)
                            .on_click(move |_, window, cx| {
                                let value = value.clone();

                                let Ok(()) = this.update(cx, |this, cx| {
                                    on_pick(this, value, window, cx);
                                }) else {
                                    // 视图已释放，结束这次回调而不再更新状态。
                                    return;
                                };
                            }),
                    )
                })
            })
            .into_any_element()
    }

    /// 队列选择菜单：选中即以该队列提交（`later` 决定是否暂停）。
    fn queue_menu(
        &self,
        later: bool,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.weak_entity();
        let queues = self
            .context
            .queues
            .iter()
            .map(|queue| {
                (
                    queue.id.clone(),
                    self.strings.queue_name(&queue.id, &queue.name),
                )
            })
            .collect::<Vec<_>>();
        move |menu, _, _| {
            queues.iter().fold(menu, |menu, (queue_id, label)| {
                let this = this.clone();
                let queue_id = queue_id.clone();
                menu.item(
                    PopupMenuItem::new(label.clone()).on_click(move |_, window, cx| {
                        let queue_id = queue_id.clone();

                        let Ok(()) = this.update(cx, |this, cx| {
                            this.submit(later, Some(queue_id), window, cx);
                        }) else {
                            // 视图已释放，结束这次回调而不再更新状态。
                            return;
                        };
                    }),
                )
            })
        }
    }

    /// 多行输入（链接 / Cookie）：13px 正文，与单行输入框同一视觉体系。
    ///
    /// 不要再加 `px`/`py`：多行 `Input` 已按尺寸给内部编辑区设置了内边距，外层再叠
    /// 一层会让文字离边框两倍远。
    ///
    /// `height` 为默认字号下的高度；至少容纳 `lines` 行正文加上下小间距，文字放大后随行高增长。
    fn textarea(state: &Entity<TextareaState>, height: Pixels, lines: f32, cx: &App) -> Textarea {
        let theme = active_theme(cx);
        let tokens = theme.tokens();
        let text_height = tokens.typography.sm.line_height * lines + tokens.spacing.xs * 2.;
        Textarea::new(state)
            .h(height.max(text_height))
            .w_full()
            .text_size(tokens.typography.sm.size)
            .line_height(tokens.typography.sm.line_height)
    }

    /// 开关行（放进 `option_group`）。
    fn toggle(
        &self,
        id: &'static str,
        title: SharedString,
        description: Option<SharedString>,
        checked: bool,
        on_change: fn(&mut Self, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        option_row(
            title,
            description,
            Switch::new(id).checked(checked).on_click(cx.listener(
                move |this, checked: &bool, _, cx| {
                    on_change(this, *checked);
                    cx.notify();
                },
            )),
            cx,
        )
        .into_any_element()
    }

    /// 窗口标题栏已显示「新建下载」，这里只保留一行说明，避免标题重复。
    fn render_header(&self, cx: &App) -> Div {
        let tokens = active_theme(cx).tokens();
        div()
            .flex_none()
            .px(tokens.spacing.lg)
            .pt(tokens.spacing.md)
            .pb(tokens.spacing.sm)
            .child(field_hint(self.strings.subtitle.clone(), cx))
    }

    /// 链接：标签行右侧显示已识别条数；多行输入下方是种子 / 文本导入入口。
    fn render_urls(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let spacing = theme.tokens().spacing;
        let count = self.entries.len();
        let has_text = !self.urls.read(cx).value().trim().is_empty();
        let url_box = {
            let tokens = theme.tokens();
            Textarea::new(&self.urls)
                .w_full()
                .text_size(tokens.typography.sm.size)
                .line_height(tokens.typography.sm.line_height)
        };
        v_flex()
            .w_full()
            .gap(spacing.xs + spacing.xxs)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(field_label(self.strings.url_label.clone(), cx))
                    .when(count > 0, |this| {
                        this.child(field_hint(self.strings.format_url_count(count), cx))
                    }),
            )
            .child(url_box)
            .when(has_text && count == 0, |this| {
                this.child(field_error(self.strings.no_valid_url.clone(), cx))
            })
            .children(self.render_probe_status(cx))
            .when(!self.captures.is_empty() && !self.is_remote(), |this| {
                this.child(field_hint(self.strings.capture_context_hint.clone(), cx))
            })
            .child(
                h_flex()
                    .gap(spacing.sm)
                    .child(
                        Button::new("new-download-open-torrent")
                            .ghost()
                            .icon(FluxIcon::FolderOpen)
                            .label(self.strings.open_torrent.clone())
                            .control(cx)
                            .disabled(self.picking || self.is_remote())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.pick_torrent_files(window, cx);
                            })),
                    )
                    .child(
                        Button::new("new-download-import-txt")
                            .ghost()
                            .icon(FluxIcon::FileText)
                            .label(self.strings.import_txt.clone())
                            .control(cx)
                            .disabled(self.picking)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.import_txt_files(window, cx);
                            })),
                    ),
            )
    }

    /// 「下载到」目标选择：本机 / 云设备 / 已配对设备；没有其他设备时不显示。
    fn render_target(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.context.targets.is_empty() {
            return None;
        }
        let mut options = vec![(
            DispatchTarget::Local,
            self.strings.this_device.clone(),
            false,
        )];
        options.extend(
            self.context
                .targets
                .iter()
                .map(|entry| (entry.target(), self.target_label(entry), false)),
        );
        let dropdown = self.dropdown(
            "new-download-target",
            None,
            self.target.clone(),
            options,
            |this, target, window, cx| this.set_target(target, window, cx),
            cx,
        );
        let hint = match self.target_entry() {
            None => self.strings.download_to_hint.clone(),
            Some(entry) => {
                let offline = match (entry.online, entry.kind) {
                    (Some(true), _) => None,
                    (Some(false), DeviceKind::Cloud) => Some(&self.strings.target_offline_hint),
                    (Some(false), DeviceKind::Paired) => {
                        Some(&self.strings.target_paired_offline_hint)
                    }
                    (None, _) => Some(&self.strings.device_presence_unknown),
                };
                match offline {
                    Some(offline) => SharedString::from(format!(
                        "{offline} {}",
                        self.strings.remote_options_hint
                    )),
                    None => self.strings.remote_options_hint.clone(),
                }
            }
        };
        Some(form_field(
            self.strings.download_to.clone(),
            dropdown,
            Some(hint),
            cx,
        ))
    }

    fn render_save_dir(&self, cx: &mut Context<Self>) -> Div {
        if let Some(entry) = self.target_entry() {
            let invalid = self.remote_dir_check(entry, cx) == RemoteDirCheck::Invalid;
            let field = form_field(
                self.strings.save_dir.clone(),
                input_with_action(
                    Input::new(&self.remote_save_dir).control(cx).w_full(),
                    Button::new("new-download-browse")
                        .outline()
                        .icon(FluxIcon::FolderOpen)
                        .label(self.strings.browse.clone())
                        .control(cx)
                        .disabled(true),
                    cx,
                ),
                (!invalid).then(|| self.strings.remote_dir_hint.clone()),
                cx,
            );
            return field.when(invalid, |this| {
                this.child(field_error(
                    self.strings.path_invalid_text(entry.path_style),
                    cx,
                ))
            });
        }
        form_field(
            self.strings.save_dir.clone(),
            input_with_action(
                self.render_save_dir_input(cx),
                Button::new("new-download-browse")
                    .outline()
                    .icon(FluxIcon::FolderOpen)
                    .label(self.strings.browse.clone())
                    .control(cx)
                    .disabled(self.picking)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.pick_save_dir(window, cx);
                    })),
                cx,
            ),
            None,
            cx,
        )
    }

    /// 保存目录历史（偏好关闭时为空）中除当前输入以外的目录。
    fn save_dir_choices(&self, cx: &App) -> Vec<SharedString> {
        let Some(history) = self.context.save_dir_history.as_deref() else {
            return Vec::new();
        };
        save_dir_choices(history, &self.save_dir.read(cx).value())
            .into_iter()
            .map(|dir| SharedString::from(dir.to_owned()))
            .collect()
    }

    /// 保存目录输入 + 历史下拉（迅雷式组合框）：输入框照常手输，按下输入框即在其正下方
    /// 展开与输入框等宽的最近目录列表；点外部或选中一项收起。没有可选目录时只是普通输入框。
    fn render_save_dir_combo(&self, cx: &mut Context<Self>) -> AnyElement {
        let choices = self.save_dir_choices(cx);
        let mut input = Input::new(&self.save_dir).control(cx).w_full();
        if choices.is_empty() {
            return input.into_any_element();
        }
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        input = input.suffix(
            Icon::new(FluxIcon::ChevronDown)
                .size(extended.icon.sm)
                .text_color(tokens.colors.muted_foreground),
        );
        let bounds = self.save_dir_bounds.clone();
        let open = self.save_dir_menu_open && !self.picking;
        div()
            .relative()
            .w_full()
            .child({
                let bounds = bounds.clone();
                // 记录组合框窗口坐标（无命中区域，不拦截鼠标）。
                canvas(move |rect, _, _| bounds.set(rect), |_, (), _, _| {})
                    .absolute()
                    .size_full()
            })
            // 捕获阶段：输入框自身会消费按下事件，冒泡阶段收不到。
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Left && !this.save_dir_menu_open {
                    this.save_dir_menu_open = true;
                    cx.notify();
                }
            }))
            .child(input)
            .when(open, |this| {
                let hover = extended.colors.row_hover;
                let rows: Vec<_> = choices
                    .into_iter()
                    .enumerate()
                    .map(|(index, dir)| {
                        let label = dir.clone();
                        div()
                            .id(("new-download-save-dir-choice", index))
                            .w_full()
                            .px(tokens.spacing.sm)
                            .py(tokens.spacing.xs)
                            .rounded(tokens.radius.md)
                            .cursor_pointer()
                            .truncate()
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                let value = dir.clone();
                                this.save_dir
                                    .update(cx, |input, cx| input.set_value(value, window, cx));
                                this.save_dir_menu_open = false;
                                cx.notify();
                            }))
                            .child(label)
                    })
                    .collect();
                this.child(
                    deferred(
                        v_flex()
                            .id("new-download-save-dir-choices")
                            .occlude()
                            .absolute()
                            .top_full()
                            .left_0()
                            .w_full()
                            .mt(tokens.spacing.xxs)
                            .p(tokens.spacing.xxs)
                            .max_h(px(SAVE_DIR_MENU_MAX_HEIGHT))
                            .overflow_y_scroll()
                            .popover_style(cx)
                            .text_size(tokens.typography.sm.size)
                            .line_height(tokens.typography.sm.line_height)
                            .children(rows)
                            .on_mouse_down_out(cx.listener(
                                move |this, event: &MouseDownEvent, _, cx| {
                                    // 按在输入框上由捕获处理保持展开，其余位置收起。
                                    if !bounds.get().contains(&event.position) {
                                        this.save_dir_menu_open = false;
                                        cx.notify();
                                    }
                                },
                            )),
                    )
                    .with_priority(1),
                )
            })
            .into_any_element()
    }

    /// 线程数：下拉铺满字段；选「自定义」时下拉收窄定宽，右侧数字输入吃满剩余宽度。
    fn render_threads(&self, cx: &mut Context<Self>) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        let mut options = vec![(ThreadChoice::Auto, self.strings.threads_auto.clone(), false)];
        options.extend(THREAD_PRESETS.iter().map(|value| {
            (
                ThreadChoice::Preset(*value),
                self.threads_label(ThreadChoice::Preset(*value)),
                false,
            )
        }));
        options.push((
            ThreadChoice::Custom,
            self.strings.threads_custom.clone(),
            false,
        ));
        let dropdown = self.dropdown(
            "new-download-threads",
            None,
            self.threads,
            options,
            |this, choice, window, cx| this.set_threads(choice, window, cx),
            cx,
        );
        let custom = self.threads == ThreadChoice::Custom;
        let dropdown_slot = if custom {
            div()
                .flex_none()
                .w(active_theme(cx).text_extent(CUSTOM_THREADS_DROPDOWN_WIDTH))
        } else {
            div().flex_1().min_w_0()
        };
        let control = h_flex()
            .w_full()
            .gap(spacing.sm)
            .child(dropdown_slot.child(dropdown))
            .when(custom, |this| {
                this.child(
                    Input::new(&self.custom_threads)
                        .control(cx)
                        .flex_1()
                        .min_w_0(),
                )
            });
        form_field(self.strings.threads.clone(), control, None, cx)
    }

    /// 文件名：独占一行（与保存目录同宽）；批量时隐藏。
    fn render_rename(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.is_batch() {
            return None;
        }
        Some(form_field(
            self.strings.rename.clone(),
            Input::new(&self.rename).control(cx).w_full(),
            None,
            cx,
        ))
    }

    /// 线程数独占一行；全磁力 / 下发时隐藏（分段数只对本机下载有意义）。线程数过高时下方展开风险提示。
    fn render_threads_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.all_magnet() || self.is_remote() {
            return None;
        }
        let risk = threads_risk_callout(
            "new-download-threads-risk",
            i64::from(self.segments(cx)),
            self.translator.read(cx),
        );
        Some(v_flex().w_full().child(self.render_threads(cx)).child(risk))
    }

    fn render_http_auth(&self, cx: &mut Context<Self>) -> Div {
        form_field(
            self.strings.http_auth.clone(),
            form_row(
                [
                    Input::new(&self.http_user)
                        .control(cx)
                        .w_full()
                        .into_any_element(),
                    Input::new(&self.http_password)
                        .control(cx)
                        .mask_toggle()
                        .w_full()
                        .into_any_element(),
                ],
                cx,
            ),
            Some(if self.uses_browser_auth() {
                self.strings.capture_auth_hint.clone()
            } else {
                self.strings.http_auth_desc.clone()
            }),
            cx,
        )
    }

    fn render_proxy(&self, cx: &mut Context<Self>) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        let manual = self.context.manual_proxy_url.clone();
        let options = ProxyChoice::ALL
            .into_iter()
            .map(|choice| {
                let (label, disabled) = match choice {
                    ProxyChoice::GlobalManual if manual.is_empty() => (
                        SharedString::from(format!(
                            "{}（{}）",
                            self.proxy_label(choice),
                            self.strings.proxy_not_configured
                        )),
                        true,
                    ),
                    ProxyChoice::GlobalManual => (
                        SharedString::from(format!("{} · {manual}", self.proxy_label(choice))),
                        false,
                    ),
                    _ => (self.proxy_label(choice), false),
                };
                (choice, label, disabled)
            })
            .collect();
        let dropdown = self.dropdown(
            "new-download-proxy",
            None,
            self.proxy_choice,
            options,
            |this, choice, _, cx| {
                this.proxy_choice = choice;
                cx.notify();
            },
            cx,
        );
        let control = v_flex()
            .w_full()
            .gap(spacing.sm)
            .child(dropdown)
            .when(self.proxy_choice == ProxyChoice::Custom, |this| {
                this.child(Input::new(&self.custom_proxy).control(cx).w_full())
            });
        form_field(
            self.strings.proxy.clone(),
            control,
            Some(self.strings.proxy_desc.clone()),
            cx,
        )
    }

    /// 预设下拉 + 同行输入框（UA、校验和）：下拉定宽，输入框吃满剩余宽度。
    fn preset_with_input(preset: AnyElement, input: &Entity<InputState>, cx: &App) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        h_flex().w_full().gap(spacing.sm).child(preset).child(
            div()
                .flex_1()
                .min_w_0()
                .child(Input::new(input).control(cx).w_full()),
        )
    }

    fn render_user_agent(&self, cx: &mut Context<Self>) -> Div {
        let options = ua_preset_keys()
            .map(|key| (key, self.ua_label(key), false))
            .collect();
        let preset = self.dropdown(
            "new-download-ua-preset",
            Some(active_theme(cx).text_extent(PRESET_DROPDOWN_WIDTH)),
            self.ua_preset,
            options,
            |this, key, window, cx| this.set_ua_preset(key, window, cx),
            cx,
        );
        form_field(
            self.strings.user_agent.clone(),
            Self::preset_with_input(preset, &self.user_agent, cx),
            Some(self.strings.user_agent_desc.clone()),
            cx,
        )
    }

    fn render_cookie(&self, cx: &mut Context<Self>) -> Div {
        form_field(
            self.strings.cookie.clone(),
            Self::textarea(&self.cookie, px(72.), 3., cx),
            Some(self.strings.cookie_desc.clone()),
            cx,
        )
    }

    fn render_checksum(&self, cx: &mut Context<Self>) -> Div {
        let options = HASH_ALGORITHMS
            .iter()
            .map(|algorithm| (*algorithm, SharedString::from(*algorithm), false))
            .collect();
        let preset = self.dropdown(
            "new-download-hash-algorithm",
            Some(active_theme(cx).text_extent(PRESET_DROPDOWN_WIDTH)),
            self.hash_algorithm,
            options,
            |this, algorithm, _, cx| {
                this.hash_algorithm = algorithm;
                cx.notify();
            },
            cx,
        );
        form_field(
            self.strings.checksum.clone(),
            Self::preset_with_input(preset, &self.checksum, cx),
            Some(self.strings.checksum_desc.clone()),
            cx,
        )
    }

    /// 自定义请求头：每行「名称 | 值 | 删除」，末尾「添加请求头」。
    fn render_headers(&self, cx: &mut Context<Self>) -> Div {
        let spacing = active_theme(cx).tokens().spacing;
        let mut rows = v_flex().w_full().gap(spacing.sm);
        for row in &self.headers {
            let row_id = row.id;
            rows = rows.child(
                h_flex()
                    .w_full()
                    .gap(spacing.sm)
                    .child(
                        div()
                            .flex_none()
                            .w(active_theme(cx).text_extent(HEADER_NAME_WIDTH))
                            .child(Input::new(&row.key).control(cx).w_full()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&row.value).control(cx).w_full()),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "new-download-header-remove-{row_id}"
                        )))
                        .ghost()
                        .icon(FluxIcon::X)
                        .control_icon(cx)
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.headers.retain(|row| row.id != row_id);
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        rows = rows.child(
            h_flex().child(
                Button::new("new-download-header-add")
                    .outline()
                    .icon(FluxIcon::Plus)
                    .label(self.strings.add_header.clone())
                    .control(cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.add_header_row(window, cx);
                    })),
            ),
        );
        form_field(
            self.strings.headers.clone(),
            rows,
            Some(self.strings.headers_desc.clone()),
            cx,
        )
    }

    /// 「高级」折叠区：标题行可点击展开；展开后字段同样走 `form_field`，开关进 `option_group`。
    fn render_advanced(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let extended = theme.extended().clone();
        let open = self.advanced_open;
        let disclosure = h_flex()
            .id("new-download-advanced-toggle")
            .w_full()
            .gap(tokens.spacing.xs)
            .items_center()
            .cursor_pointer()
            .child(
                Icon::new(if open {
                    FluxIcon::ChevronDown
                } else {
                    FluxIcon::ChevronRight
                })
                .size(extended.icon.md)
                .text_color(extended.colors.text_tertiary),
            )
            .child(
                div()
                    .text_size(tokens.typography.sm.size)
                    .line_height(tokens.typography.sm.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.colors.foreground)
                    .child(self.strings.advanced.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .h(extended.stroke.thin)
                    .ml(tokens.spacing.sm)
                    .bg(extended.colors.hairline),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.advanced_open = !this.advanced_open;
                cx.notify();
            }));
        let section = v_flex().w_full().gap(form_gap(cx)).child(disclosure);
        if !open {
            return section;
        }
        let show_auth = !self.is_batch() && !self.all_magnet();
        let mut options = Vec::with_capacity(2);
        if show_auth {
            options.push(self.toggle(
                "new-download-save-site-auth",
                self.strings.http_auth_save.clone(),
                None,
                self.save_site_auth,
                |this, checked| this.save_site_auth = checked,
                cx,
            ));
        }
        options.push(self.toggle(
            "new-download-ignore-tls",
            self.strings.ignore_tls.clone(),
            Some(self.strings.ignore_tls_desc.clone()),
            self.ignore_tls_errors,
            |this, checked| this.ignore_tls_errors = checked,
            cx,
        ));
        let mut fields = form(cx);
        if show_auth {
            fields = fields.child(self.render_http_auth(cx));
        }
        section.child(
            fields
                .child(self.render_proxy(cx))
                .child(self.render_user_agent(cx))
                .child(self.render_cookie(cx))
                .child(self.render_checksum(cx))
                .child(self.render_headers(cx))
                .child(option_group(options, cx)),
        )
    }

    fn render_form(&self, cx: &mut Context<Self>) -> Div {
        let tokens = active_theme(cx).tokens().clone();
        form(cx)
            .px(tokens.spacing.lg)
            .pt(tokens.spacing.xs)
            .pb(tokens.spacing.lg)
            .child(self.render_urls(cx))
            .children(self.render_target(cx))
            .child(self.render_save_dir(cx))
            .children(self.render_rename(cx))
            .children(self.render_threads_row(cx))
            .children((!self.is_remote()).then(|| self.render_advanced(cx)))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let theme = active_theme(cx);
        let tokens = theme.tokens().clone();
        let enabled = self.can_submit(cx);
        let later_queue = self.queue_label(fluxdown_protocol::LATER_QUEUE_ID);
        let start_queue = self.queue_label(&self.context.queue_id);
        // 分体按钮：两半统一控件高度（`Size::Size` 让右侧箭头半成为 `density.control` 见方，
        // 文字取 `text_base` = 13px），左半主按钮再经 `control` 统一内边距。
        let split_size = Size::Size(theme.density().control);
        h_flex()
            .w_full()
            .flex_none()
            .px(tokens.spacing.lg)
            .py(tokens.spacing.md)
            .justify_end()
            .gap(tokens.spacing.sm)
            .bg(theme.extended().colors.chrome)
            .border_t(theme.extended().stroke.thin)
            .border_color(theme.extended().colors.hairline)
            .child(
                Button::new("new-download-cancel")
                    .outline()
                    .label(self.strings.cancel.clone())
                    .control(cx)
                    .on_click(|_, window, _| window.remove_window()),
            )
            .children(if self.is_remote() {
                // 下发只有「立即开始」：没有队列，也没有稍后下载。
                vec![
                    Button::new("new-download-start-remote")
                        .primary()
                        .label(self.strings.format_start(self.entries.len()))
                        .control(cx)
                        .disabled(!enabled)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.submit(false, None, window, cx);
                        }))
                        .into_any_element(),
                ]
            } else {
                vec![
                    DropdownButton::new("new-download-later")
                        .outline()
                        .with_size(split_size)
                        .disabled(!enabled)
                        .button(
                            Button::new("new-download-later-main")
                                .label(self.strings.download_later.clone())
                                .control(cx)
                                .tooltip(self.strings.format_later_tooltip(&later_queue))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.submit(true, None, window, cx);
                                })),
                        )
                        .dropdown_menu_with_anchor(Anchor::TopRight, self.queue_menu(true, cx))
                        .into_any_element(),
                    DropdownButton::new("new-download-start")
                        .primary()
                        .with_size(split_size)
                        .disabled(!enabled)
                        .button(
                            Button::new("new-download-start-main")
                                .label(self.strings.format_start(self.entries.len()))
                                .control(cx)
                                .tooltip(self.strings.format_start_tooltip(&start_queue))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.submit(false, None, window, cx);
                                })),
                        )
                        .dropdown_menu_with_anchor(Anchor::TopRight, self.queue_menu(false, cx))
                        .into_any_element(),
                ]
            })
    }
}

impl Render for NewDownloadView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = active_theme(cx).tokens().clone();
        if let Some(manifest) = &self.manifest {
            return div().size_full().child(manifest.clone()).into_any_element();
        }
        if self.preview_gate.is_active() {
            return self.render_preview(cx).into_any_element();
        }
        v_flex()
            .size_full()
            // 表单区用 surface（白），与设置窗口内容区一致。
            .bg(tokens.colors.surface)
            .child(self.render_header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_form(cx).overflow_y_scrollbar()),
            )
            .child(self.render_footer(cx))
            .into_any_element()
    }
}

/// 扩展名匹配（忽略大小写），文件选择器不支持按类型过滤时在结果上兜底。
fn has_extension(path: &std::path::Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}
