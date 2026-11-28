//! 待确认写工具的人话预览:确认卡片据此显示「会动哪些任务 / 设置从什么改成什么 / 访问哪些地址」,
//! 并标出删除、重新下载这类不可撤销的动作(`destructive`)。
//!
//! 只用 agent 事件投影里的本机数据(任务显示名)与快速 RPC(`daemon.config.get`、`daemon.plugin.list`),
//! 不做联网 / 慢操作;参数解析失败、工具未知时返回 `preview: None`(执行时同样会以参数错误收场)。
//! 地址一律经 [`display_url`] 脱敏(去账号 / 令牌 / 查询串),文本值再过一遍日志脱敏规则。
//! `action` 取值固定,见 `AssistantToolPreviewDto::action`。

use std::collections::HashSet;

use fluxdown_protocol::{
    ASSISTANT_PREVIEW_MAX_ITEMS, AssistantSettingChangeDto, AssistantToolPreviewDto, PluginDto,
    method,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::admin::{ComponentArgs, PluginArgs, QueueAction, QueueArgs, RepairArgs, component_name};
use super::output::{MAX_OUTPUT_BYTES, display_path, display_url, finalize_text, truncate_chars};
use super::read_tasks::file_priority_label;
use super::settings;
use super::task_ops::{self, SeedLimits};
use super::tasks::{CreateArgs, EditArgs, ManageAction, ManageArgs, ToolRpc, rpc_json};
use super::{ToolCall, ToolContext, ToolPreview};

/// 预览里单个任务名 / 设置值的字符上限。
const MAX_TEXT_CHARS: usize = 120;

/// 预览所需的本机数据:除 RPC 外只有任务显示名。
pub(super) trait PreviewEnv: ToolRpc {
    /// 任务当前的显示名(文件名;尚未解析出文件名时用脱敏后的地址);不在任务表里返回 `None`。
    fn task_name(&self, task_id: &str) -> Option<String>;
}

impl PreviewEnv for ToolContext {
    fn task_name(&self, task_id: &str) -> Option<String> {
        self.inspect(|snapshot| {
            snapshot.daemon.task(task_id).map(|task| {
                if task.file_name.is_empty() {
                    display_url(&task.url)
                } else {
                    task.file_name.clone()
                }
            })
        })
    }
}

/// 为一次待确认的调用生成预览。
pub(super) async fn build(env: &impl PreviewEnv, call: &ToolCall) -> ToolPreview {
    let args = &call.arguments;
    let built = match call.name.as_str() {
        "fd_create_tasks" => create(args),
        "fd_manage_tasks" => manage(env, args),
        "fd_update_settings" => update_settings(env, args).await,
        "fd_manage_queues" => queues(env, args),
        "fd_install_component" => component(args),
        "fd_set_plugin_enabled" => plugin(env, args).await,
        "fd_repair" => repair(args),
        "fd_measure_routes" => measure(env, args),
        _ => None,
    };
    built.unwrap_or_else(ToolPreview::none)
}

fn parse<T: for<'de> Deserialize<'de>>(args: &Value) -> Option<T> {
    serde_json::from_value(args.clone()).ok()
}

fn text(raw: &str) -> String {
    truncate_chars(&finalize_text(raw, MAX_OUTPUT_BYTES), MAX_TEXT_CHARS)
}

/// 设置值的展示文本:字符串原样(脱敏 + 截断),其余取紧凑 JSON。
fn value_text(value: &Value) -> String {
    match value {
        Value::String(raw) => text(raw),
        other => text(&other.to_string()),
    }
}

/// 列表截到 [`ASSISTANT_PREVIEW_MAX_ITEMS`] 条,返回(保留的条目,被截掉的条数)。
fn capped(mut items: Vec<String>) -> (Vec<String>, u32) {
    let more = items.len().saturating_sub(ASSISTANT_PREVIEW_MAX_ITEMS);
    items.truncate(ASSISTANT_PREVIEW_MAX_ITEMS);
    (items, u32::try_from(more).unwrap_or(u32::MAX))
}

fn task_labels(env: &impl PreviewEnv, ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter()
        .filter(|id| seen.insert(id.as_str()))
        .map(|id| text(&env.task_name(id).unwrap_or_else(|| id.clone())))
        .collect()
}

fn ready(preview: AssistantToolPreviewDto, destructive: bool) -> Option<ToolPreview> {
    Some(ToolPreview {
        preview: Some(preview),
        destructive,
    })
}

fn change(key: &str, from: Option<String>, to: String) -> AssistantSettingChangeDto {
    AssistantSettingChangeDto {
        key: key.to_owned(),
        from,
        to,
    }
}

// ───────────────────────── fd_create_tasks ─────────────────────────

fn create(args: &Value) -> Option<ToolPreview> {
    let args: CreateArgs = parse(args)?;
    let urls: Vec<String> = args
        .items
        .iter()
        .map(|item| display_url(&item.url))
        .collect();
    let (urls, more_tasks) = capped(urls);
    let mut details = Vec::new();
    if let Some(dir) = args
        .options
        .save_dir
        .as_deref()
        .filter(|d| !d.trim().is_empty())
    {
        details.push(format!("saveDir: {}", text(&display_path(dir.trim()))));
    }
    if let Some(queue) = args
        .options
        .queue
        .as_deref()
        .filter(|q| !q.trim().is_empty())
    {
        details.push(format!("queue: {}", text(queue.trim())));
    }
    if let Some(category) = args
        .options
        .category
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        details.push(format!("category: {}", text(category.trim())));
    }
    if args.options.start_paused == Some(true) {
        details.push("startPaused".to_owned());
    }
    ready(
        AssistantToolPreviewDto {
            action: "create".to_owned(),
            urls,
            more_tasks,
            details,
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

// ───────────────────────── fd_manage_tasks ─────────────────────────

fn manage(env: &impl PreviewEnv, args: &Value) -> Option<ToolPreview> {
    let args: ManageArgs = parse(args)?;
    let (tasks, more_tasks) = capped(task_labels(env, &args.task_ids));
    let mut preview = AssistantToolPreviewDto {
        action: args.action.as_str().to_owned(),
        tasks,
        more_tasks,
        ..AssistantToolPreviewDto::default()
    };
    // 删除任务、重新下载(连已下载的文件一起丢掉)不可撤销。
    let destructive = matches!(args.action, ManageAction::Delete | ManageAction::Restart);
    match args.action {
        ManageAction::Delete => preview.delete_files = args.delete_files.unwrap_or(false),
        ManageAction::MoveToQueue => {
            if let Some(queue) = args.queue.as_deref() {
                preview.details.push(text(queue.trim()));
            }
        }
        ManageAction::Edit => {
            if let Some(edit) = &args.edit {
                preview.settings = edit_changes(edit);
            }
        }
        ManageAction::ChangeUrl => {
            if let Some(url) = args.url.as_deref() {
                preview.urls.push(display_url(url));
            }
        }
        ManageAction::SetSeedLimits => {
            let limits = args.seed_limits.as_ref()?;
            preview.settings = seed_changes(limits)?;
        }
        ManageAction::SetFilePriorities => {
            let items = args.file_priorities.as_deref()?;
            let planned = task_ops::plan_priorities(items).ok()?;
            let mut lines: Vec<String> = planned
                .iter()
                .map(|entry| format!("#{} → {}", entry.index, file_priority_label(entry.priority)))
                .collect();
            let hidden = lines.len().saturating_sub(ASSISTANT_PREVIEW_MAX_ITEMS);
            lines.truncate(ASSISTANT_PREVIEW_MAX_ITEMS);
            if hidden > 0 {
                lines.push(format!("… +{hidden}"));
            }
            preview.details = lines;
        }
        ManageAction::Pause
        | ManageAction::Resume
        | ManageAction::Restart
        | ManageAction::Recheck
        | ManageAction::Reannounce => {}
    }
    ready(preview, destructive)
}

fn edit_changes(edit: &EditArgs) -> Vec<AssistantSettingChangeDto> {
    let mut out = Vec::new();
    if let Some(proxy) = &edit.proxy {
        out.push(change("proxy", None, text(&display_url(proxy))));
    }
    if let Some(connections) = edit.connections {
        out.push(change("connections", None, connections.to_string()));
    }
    if let Some(kbps) = edit.speed_limit_kbps {
        out.push(change("speedLimitKBps", None, kbps.to_string()));
    }
    if let Some(agent) = &edit.user_agent {
        out.push(change("userAgent", None, text(agent)));
    }
    if let Some(headers) = &edit.headers {
        // 只列请求头名,值(可能含令牌)不进预览。
        let names: Vec<&str> = headers.keys().map(String::as_str).collect();
        out.push(change("headers", None, text(&names.join(", "))));
    }
    if let Some(dir) = &edit.save_dir {
        out.push(change("saveDir", None, text(&display_path(dir.trim()))));
    }
    if let Some(name) = &edit.file_name {
        out.push(change("fileName", None, text(name.trim())));
    }
    if let Some(category) = &edit.category {
        out.push(change("category", None, text(category.trim())));
    }
    if let Some(tags) = &edit.tags {
        out.push(change("tags", None, text(&tags.join(", "))));
    }
    out
}

fn seed_changes(args: &task_ops::SeedLimitsArgs) -> Option<Vec<AssistantSettingChangeDto>> {
    let plan = task_ops::plan_seed_limits(args).ok()?;
    let mut out = Vec::new();
    if let Some(milli) = plan.ratio_milli {
        out.push(change("ratioLimit", None, SeedLimits::ratio_label(milli)));
    }
    if let Some(milli) = plan.post_ratio_milli {
        out.push(change(
            "postRatioLimit",
            None,
            SeedLimits::ratio_label(milli),
        ));
    }
    if let Some(minutes) = plan.time_minutes {
        out.push(change("seedTime", None, SeedLimits::minutes_label(minutes)));
    }
    if let Some(minutes) = plan.inactive_minutes {
        out.push(change(
            "inactiveTime",
            None,
            SeedLimits::minutes_label(minutes),
        ));
    }
    if let Some(bps) = plan.upload_bps {
        out.push(change("uploadLimit", None, SeedLimits::upload_label(bps)));
    }
    Some(out)
}

// ───────────────────────── fd_update_settings ─────────────────────────

async fn update_settings(env: &impl PreviewEnv, args: &Value) -> Option<ToolPreview> {
    // 与执行同一套校验:拒绝 / 非法的键拿不到预览(也就不会在卡片上露出被拒绝的值)。
    let changes = settings::preview_changes(env, args.clone()).await.ok()?;
    let mut rows: Vec<AssistantSettingChangeDto> = changes
        .iter()
        .map(|(key, before, after)| {
            let (before, after) = settings::display_change(key, before.clone(), after.clone());
            change(
                key,
                (!before.is_null()).then(|| value_text(&before)),
                value_text(&after),
            )
        })
        .collect();
    let hidden = rows.len().saturating_sub(ASSISTANT_PREVIEW_MAX_ITEMS);
    rows.truncate(ASSISTANT_PREVIEW_MAX_ITEMS);
    let details = if hidden > 0 {
        vec![format!("… +{hidden}")]
    } else {
        Vec::new()
    };
    ready(
        AssistantToolPreviewDto {
            action: "updateSettings".to_owned(),
            settings: rows,
            details,
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

// ───────────────────────── fd_manage_queues ─────────────────────────

fn queues(env: &impl PreviewEnv, args: &Value) -> Option<ToolPreview> {
    let args: QueueArgs = parse(args)?;
    let action = match args.action {
        QueueAction::Start => "queueStart",
        QueueAction::Stop => "queueStop",
        QueueAction::Reorder => "queueReorder",
    };
    let (tasks, more_tasks) = match &args.task_ids {
        Some(ids) => capped(task_labels(env, ids)),
        None => (Vec::new(), 0),
    };
    ready(
        AssistantToolPreviewDto {
            action: action.to_owned(),
            tasks,
            more_tasks,
            details: vec![text(args.queue.trim())],
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

// ───────────────────────── fd_install_component ─────────────────────────

fn component(args: &Value) -> Option<ToolPreview> {
    let args: ComponentArgs = parse(args)?;
    let mut details = vec![component_name(args.component).to_owned()];
    if let Some(version) = args
        .version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        details.push(text(version));
    }
    ready(
        AssistantToolPreviewDto {
            action: "installComponent".to_owned(),
            details,
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

// ───────────────────────── fd_set_plugin_enabled ─────────────────────────

async fn plugin(env: &impl PreviewEnv, args: &Value) -> Option<ToolPreview> {
    let args: PluginArgs = parse(args)?;
    let identity = args.identity.trim();
    let onoff = |enabled: bool| String::from(if enabled { "on" } else { "off" });
    let found = rpc_json::<Vec<PluginDto>>(env, method::DAEMON_PLUGIN_LIST, json!({}))
        .await
        .ok()
        .and_then(|plugins| {
            plugins
                .into_iter()
                .find(|plugin| plugin.identity == identity)
        });
    let mut details = Vec::new();
    if let Some(plugin) = found.as_ref().filter(|plugin| !plugin.name.is_empty()) {
        details.push(text(&plugin.name));
    }
    ready(
        AssistantToolPreviewDto {
            action: "setPluginEnabled".to_owned(),
            settings: vec![change(
                &text(identity),
                found.as_ref().map(|plugin| onoff(plugin.enabled)),
                onoff(args.enabled),
            )],
            details,
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

// ───────────────────────── fd_repair ─────────────────────────

fn repair(args: &Value) -> Option<ToolPreview> {
    let args: RepairArgs = parse(args)?;
    let mut details = vec![text(&args.action)];
    if let Some(target) = args
        .target
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        details.push(text(&display_path(target)));
    }
    ready(
        AssistantToolPreviewDto {
            action: "repair".to_owned(),
            details,
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

// ───────────────────────── fd_measure_routes ─────────────────────────

/// 只取预览要显示的字段;完整校验在执行时。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeasureView {
    task_id: Option<String>,
    url: Option<String>,
    routes: Option<Vec<String>>,
}

fn measure(env: &impl PreviewEnv, args: &Value) -> Option<ToolPreview> {
    let args: MeasureView = parse(args)?;
    let tasks = match args.task_id.as_deref() {
        Some(id) => vec![text(&env.task_name(id).unwrap_or_else(|| id.to_owned()))],
        None => Vec::new(),
    };
    let urls: Vec<String> = args.url.iter().map(|url| display_url(url)).collect();
    let details = args
        .routes
        .iter()
        .flatten()
        .take(ASSISTANT_PREVIEW_MAX_ITEMS)
        .map(|route| text(&display_url(route)))
        .collect();
    ready(
        AssistantToolPreviewDto {
            action: "measureRoutes".to_owned(),
            tasks,
            urls,
            details,
            ..AssistantToolPreviewDto::default()
        },
        false,
    )
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{AgentPreferencesDto, DaemonConfigSnapshot};

    use super::super::ToolError;
    use super::super::tasks::test_support::MockRpc;
    use super::*;

    struct Env {
        rpc: MockRpc,
        names: Vec<(&'static str, &'static str)>,
    }

    impl ToolRpc for Env {
        async fn call(&self, method: &str, params: Value) -> Result<Value, ToolError> {
            self.rpc.call(method, params).await
        }

        async fn daemon_config(&self) -> Result<DaemonConfigSnapshot, ToolError> {
            self.rpc.daemon_config().await
        }

        async fn preferences(&self) -> Result<AgentPreferencesDto, ToolError> {
            self.rpc.preferences().await
        }
    }

    impl PreviewEnv for Env {
        fn task_name(&self, task_id: &str) -> Option<String> {
            self.names
                .iter()
                .find(|(id, _)| *id == task_id)
                .map(|(_, name)| (*name).to_owned())
        }
    }

    fn env() -> Env {
        let mut rpc = MockRpc::new(|name, _| match name {
            method::DAEMON_PLUGIN_LIST => Ok(json!([
                { "identity": "demo.plugin", "name": "Demo", "version": "1", "enabled": true, "devMode": false, "settings": [], "settingsValues": {}, "disabledReason": "None" },
            ])),
            _ => Ok(json!({})),
        });
        rpc.config.revision = 1;
        rpc.config
            .values
            .insert("max_concurrent_tasks".to_owned(), "3".to_owned());
        Env {
            rpc,
            names: vec![("t1", "movie.mkv"), ("t2", "album.zip")],
        }
    }

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            name: name.to_owned(),
            version: 1,
            arguments,
            max_output_bytes: super::super::output_budget(None),
        }
    }

    async fn preview_of(name: &str, arguments: Value) -> ToolPreview {
        build(&env(), &call(name, arguments)).await
    }

    fn dto(preview: &ToolPreview) -> &AssistantToolPreviewDto {
        preview.preview.as_ref().expect("a preview")
    }

    #[tokio::test]
    async fn delete_lists_task_names_and_is_destructive() {
        let out = preview_of(
            "fd_manage_tasks",
            json!({ "taskIds": ["t1", "t2", "t1", "ghost"], "action": "delete", "deleteFiles": true }),
        )
        .await;
        assert!(out.destructive);
        let dto = dto(&out);
        assert_eq!(dto.action, "delete");
        assert_eq!(dto.tasks, ["movie.mkv", "album.zip", "ghost"]);
        assert!(dto.delete_files);
        assert_eq!(dto.more_tasks, 0);
    }

    #[tokio::test]
    async fn long_task_lists_are_capped_with_a_remainder_count() {
        let ids: Vec<String> = (0..25).map(|i| format!("id{i}")).collect();
        let out = preview_of(
            "fd_manage_tasks",
            json!({ "taskIds": ids, "action": "pause" }),
        )
        .await;
        assert!(!out.destructive);
        let dto = dto(&out);
        assert_eq!(dto.tasks.len(), ASSISTANT_PREVIEW_MAX_ITEMS);
        assert_eq!(dto.more_tasks, 5);
    }

    #[tokio::test]
    async fn destructive_follows_the_action() {
        for (action, destructive) in [
            ("pause", false),
            ("resume", false),
            ("delete", true),
            ("restart", true),
            ("recheck", false),
            ("reannounce", false),
        ] {
            let out = preview_of(
                "fd_manage_tasks",
                json!({ "taskIds": ["t1"], "action": action }),
            )
            .await;
            assert_eq!(out.destructive, destructive, "{action}");
            assert_eq!(dto(&out).action, action);
            assert!(!dto(&out).delete_files);
        }
    }

    #[tokio::test]
    async fn move_change_url_and_edit_show_their_targets() {
        let moved = preview_of(
            "fd_manage_tasks",
            json!({ "taskIds": ["t1"], "action": "moveToQueue", "queue": "Night" }),
        )
        .await;
        assert_eq!(dto(&moved).details, ["Night"]);

        let url = preview_of(
            "fd_manage_tasks",
            json!({ "taskIds": ["t1"], "action": "changeUrl", "url": "https://u:p@cdn.example/f.zip?token=secret" }),
        )
        .await;
        assert_eq!(dto(&url).action, "changeUrl");
        let shown = dto(&url).urls.join(" ");
        assert!(shown.contains("cdn.example/f.zip"), "{shown}");
        for leaked in ["u:p", "secret"] {
            assert!(!shown.contains(leaked), "{leaked} leaked: {shown}");
        }

        let edit = preview_of(
            "fd_manage_tasks",
            json!({
                "taskIds": ["t1"], "action": "edit",
                "edit": { "connections": 8, "headers": { "X-Token": "hunter2" } },
            }),
        )
        .await;
        let keys: Vec<&str> = dto(&edit).settings.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["connections", "headers"]);
        let rendered = format!("{:?}", dto(&edit).settings);
        assert!(
            rendered.contains("X-Token") && !rendered.contains("hunter2"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn seed_limits_and_file_priorities_are_readable() {
        let seed = preview_of(
            "fd_manage_tasks",
            json!({
                "taskIds": ["t1"], "action": "setSeedLimits",
                "seedLimits": { "ratioLimit": 2.0, "seedTimeMinutes": -1, "uploadLimitKBps": 512 },
            }),
        )
        .await;
        let pairs: Vec<(&str, &str)> = dto(&seed)
            .settings
            .iter()
            .map(|s| (s.key.as_str(), s.to.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("ratioLimit", "2.00"),
                ("seedTime", "unlimited"),
                ("uploadLimit", "512 KiB/s")
            ]
        );

        let files = preview_of(
            "fd_manage_tasks",
            json!({
                "taskIds": ["t1"], "action": "setFilePriorities",
                "filePriorities": [{ "index": 0, "priority": "skip" }, { "index": 3, "priority": "maximum" }],
            }),
        )
        .await;
        assert_eq!(dto(&files).details, ["#0 → skip", "#3 → maximum"]);
    }

    #[tokio::test]
    async fn invalid_arguments_and_unknown_tools_have_no_preview() {
        for (name, args) in [
            ("fd_manage_tasks", json!({ "taskIds": ["t1"] })),
            (
                "fd_manage_tasks",
                json!({ "taskIds": ["t1"], "action": "explode" }),
            ),
            (
                "fd_manage_tasks",
                json!({ "taskIds": ["t1"], "action": "setSeedLimits" }),
            ),
            ("fd_update_settings", json!({ "values": {} })),
            ("fd_get_app_info", json!({})),
            ("fd_nope", json!({})),
        ] {
            let out = preview_of(name, args.clone()).await;
            assert!(out.preview.is_none(), "{name} {args}");
            assert!(!out.destructive);
        }
    }

    #[tokio::test]
    async fn create_lists_sanitized_urls_and_options() {
        let items: Vec<Value> = (0..22)
            .map(|i| json!({ "url": format!("https://user:pw@h.example/f{i}.zip?sig=abc") }))
            .collect();
        let out = preview_of(
            "fd_create_tasks",
            json!({ "items": items, "options": { "saveDir": "/data/dl", "queue": "Night", "startPaused": true } }),
        )
        .await;
        assert!(!out.destructive);
        let dto = dto(&out);
        assert_eq!(dto.action, "create");
        assert_eq!(dto.urls.len(), ASSISTANT_PREVIEW_MAX_ITEMS);
        assert_eq!(dto.more_tasks, 2);
        let shown = dto.urls.join(" ");
        for leaked in ["user:pw", "sig=abc"] {
            assert!(!shown.contains(leaked), "{leaked} leaked: {shown}");
        }
        assert_eq!(
            dto.details,
            ["saveDir: /data/dl", "queue: Night", "startPaused"]
        );
    }

    #[tokio::test]
    async fn settings_preview_shows_old_and_new_values() {
        let out = preview_of(
            "fd_update_settings",
            json!({ "values": { "max_concurrent_tasks": 5 } }),
        )
        .await;
        assert!(!out.destructive);
        let dto = dto(&out);
        assert_eq!(dto.action, "updateSettings");
        assert_eq!(dto.settings.len(), 1);
        assert_eq!(dto.settings[0].key, "max_concurrent_tasks");
        assert_eq!(dto.settings[0].from.as_deref(), Some("3"));
        assert_eq!(dto.settings[0].to, "5");
    }

    #[tokio::test]
    async fn tracker_preview_shows_the_real_diff_and_never_a_restored_removal() {
        let mut env = env();
        env.rpc.config.values.insert(
            "bt_custom_trackers".to_owned(),
            "https://t.example/SECRETPASSKEY/announce\nudp://old.example:6969/announce".to_owned(),
        );
        // 模型只看得到展示形态:保留第一条、删掉第二条、新增一条。
        let out = build(
            &env,
            &call(
                "fd_update_settings",
                json!({ "values": {
                    "bt_custom_trackers": "https://t.example\nudp://new.example:80/announce"
                } }),
            ),
        )
        .await;
        let card = dto(&out);
        assert_eq!(card.settings.len(), 1);
        // 被还原的第一条既不算移除也不算新增;移除的是第二条,新增的是第三条。
        assert_eq!(
            card.settings[0].from.as_deref(),
            Some("udp://old.example:6969")
        );
        assert_eq!(card.settings[0].to, "udp://new.example:80");
        let text = format!("{:?}", card.settings);
        assert!(!text.contains("SECRETPASSKEY"), "{text}");

        // 只新增:没有「改前」。
        let added = build(
            &env,
            &call(
                "fd_update_settings",
                json!({ "values": {
                    "bt_custom_trackers":
                        "https://t.example\nudp://old.example:6969\nudp://new.example:80/announce"
                } }),
            ),
        )
        .await;
        let added_card = dto(&added);
        assert_eq!(added_card.settings[0].from, None);
        assert_eq!(added_card.settings[0].to, "udp://new.example:80");
    }

    #[tokio::test]
    async fn refused_settings_get_no_preview() {
        let out = preview_of(
            "fd_update_settings",
            json!({ "values": { "proxy_password": "hunter2" } }),
        )
        .await;
        assert!(out.preview.is_none());
    }

    #[tokio::test]
    async fn queue_component_plugin_repair_and_measure_previews() {
        let start = preview_of(
            "fd_manage_queues",
            json!({ "action": "start", "queue": "Night" }),
        )
        .await;
        assert_eq!(dto(&start).action, "queueStart");
        assert_eq!(dto(&start).details, ["Night"]);

        let reorder = preview_of(
            "fd_manage_queues",
            json!({ "action": "reorder", "queue": "Night", "taskIds": ["t2", "t1"] }),
        )
        .await;
        assert_eq!(dto(&reorder).action, "queueReorder");
        assert_eq!(dto(&reorder).tasks, ["album.zip", "movie.mkv"]);

        let stop = preview_of(
            "fd_manage_queues",
            json!({ "action": "stop", "queue": "Night" }),
        )
        .await;
        assert_eq!(dto(&stop).action, "queueStop");

        let comp = preview_of(
            "fd_install_component",
            json!({ "component": "ytdlp", "version": "2025.01.01" }),
        )
        .await;
        assert_eq!(dto(&comp).action, "installComponent");
        assert_eq!(dto(&comp).details, ["ytdlp", "2025.01.01"]);

        let plugin = preview_of(
            "fd_set_plugin_enabled",
            json!({ "identity": "demo.plugin", "enabled": false }),
        )
        .await;
        let change = &dto(&plugin).settings[0];
        assert_eq!(dto(&plugin).action, "setPluginEnabled");
        assert_eq!(
            (
                change.key.as_str(),
                change.from.as_deref(),
                change.to.as_str()
            ),
            ("demo.plugin", Some("on"), "off")
        );
        assert_eq!(dto(&plugin).details, ["Demo"]);

        let fix = preview_of(
            "fd_repair",
            json!({ "action": "fix_dir_access", "target": "/data/dl" }),
        )
        .await;
        assert_eq!(dto(&fix).action, "repair");
        assert_eq!(dto(&fix).details, ["fix_dir_access", "/data/dl"]);

        let measure = preview_of(
            "fd_measure_routes",
            json!({ "url": "https://u:p@cdn.example/big.bin?t=1", "routes": ["direct", "socks5://127.0.0.1:1080"] }),
        )
        .await;
        assert_eq!(dto(&measure).action, "measureRoutes");
        assert!(!dto(&measure).urls.join(" ").contains("u:p"));
        assert_eq!(dto(&measure).details.len(), 2);
    }
}
