//! 管理类写工具:`fd_manage_queues`、`fd_install_component`、`fd_set_plugin_enabled`、`fd_repair`。
//!
//! RPC 对应:
//! - `fd_manage_queues`:`daemon.queue.list`(解析队列名)+ `daemon.queue.start` / `daemon.queue.stop` /
//!   `daemon.queue.reorder`(重排前用 `daemon.task.get` 核对任务确实排在该队列里);
//! - `fd_install_component`:`daemon.component.install`(安装 / 更新 ffmpeg、yt-dlp,会联网下载);
//! - `fd_set_plugin_enabled`:`daemon.plugin.list` + `daemon.plugin.setEnabled`;
//! - `fd_repair`:`agent.diagnostics.repair`,只开放体检报告里的修复动作(不含打开窗口 / 发测试通知)。

use std::collections::HashSet;

use fluxdown_protocol::{ComponentKind, MAIN_QUEUE_ID, PluginDto, method};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ToolError;
use super::tasks::{
    MAX_URL_CHARS, STATUS_PENDING, ToolRpc, check_max, check_no_control, fetch_task, invalid,
    resolve_queue, rpc_json,
};
use crate::diagnostics::{
    ACTION_ENABLE_AUTOSTART, ACTION_ENABLE_SERVICE, ACTION_FIX_COMPONENT, ACTION_FIX_COMSPEC,
    ACTION_FIX_DIR_ACCESS, ACTION_REFRESH_ED2K_SERVERS, ACTION_REFRESH_TRACKERS, ACTION_REGISTER,
    ACTION_REREGISTER, ACTION_USE_THIS_INSTALL,
};

const MAX_QUEUE_CHARS: usize = 128;
const MAX_REORDER_IDS: usize = 200;
const MAX_ID_CHARS: usize = 128;
const MAX_IDENTITY_CHARS: usize = 200;
const MAX_VERSION_CHARS: usize = 64;
const MAX_TARGET_CHARS: usize = 1024;

/// 体检修复里 AI 可以代用户执行的动作(不含只开窗口的 `open_settings` / `open_log_dir`、测试通知)。
pub(super) const REPAIR_ACTIONS: [&str; 10] = [
    ACTION_REREGISTER,
    ACTION_USE_THIS_INSTALL,
    ACTION_ENABLE_SERVICE,
    ACTION_REGISTER,
    ACTION_REFRESH_TRACKERS,
    ACTION_REFRESH_ED2K_SERVERS,
    ACTION_FIX_DIR_ACCESS,
    ACTION_FIX_COMPONENT,
    ACTION_FIX_COMSPEC,
    ACTION_ENABLE_AUTOSTART,
];

// ───────────────────────── fd_manage_queues ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum QueueAction {
    Start,
    Stop,
    Reorder,
}

impl QueueAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Reorder => "reorder",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct QueueArgs {
    pub(super) action: QueueAction,
    /// 队列名或 ID。
    pub(super) queue: String,
    /// `reorder`:排在该队列里的任务,按期望的新顺序。
    pub(super) task_ids: Option<Vec<String>>,
}

impl QueueArgs {
    /// 结构校验;返回去重后的任务 ID(非 `reorder` 为空)。
    fn check(&self) -> Result<Vec<String>, ToolError> {
        check_max("queue", &self.queue, MAX_QUEUE_CHARS)?;
        if self.queue.trim().is_empty() {
            return Err(invalid("queue must not be empty"));
        }
        match (self.action, &self.task_ids) {
            (QueueAction::Reorder, None) => {
                Err(invalid("taskIds is required with action 'reorder'"))
            }
            (QueueAction::Reorder, Some(ids)) => {
                if ids.is_empty() || ids.len() > MAX_REORDER_IDS {
                    return Err(invalid(format!(
                        "taskIds must contain 1..={MAX_REORDER_IDS} entries"
                    )));
                }
                let mut seen = HashSet::with_capacity(ids.len());
                let mut unique = Vec::with_capacity(ids.len());
                for id in ids {
                    let chars = id.chars().count();
                    if chars == 0 || chars > MAX_ID_CHARS {
                        return Err(invalid(format!(
                            "each task id must be 1..={MAX_ID_CHARS} characters"
                        )));
                    }
                    if seen.insert(id.as_str()) {
                        unique.push(id.clone());
                    }
                }
                Ok(unique)
            }
            (_, Some(_)) => Err(invalid("taskIds is only valid with action 'reorder'")),
            (_, None) => Ok(Vec::new()),
        }
    }
}

pub(super) async fn manage_queues(rpc: &impl ToolRpc, args: Value) -> Result<Value, ToolError> {
    let args: QueueArgs = super::parse_args(args)?;
    let task_ids = args.check()?;
    let queue_id = resolve_queue(rpc, &args.queue).await?;
    match args.action {
        QueueAction::Start => {
            rpc.call(method::DAEMON_QUEUE_START, json!({ "queueId": queue_id }))
                .await?;
        }
        QueueAction::Stop => {
            if queue_id == MAIN_QUEUE_ID {
                return Err(ToolError::Refused(
                    "the default queue always runs and cannot be stopped".to_owned(),
                ));
            }
            rpc.call(method::DAEMON_QUEUE_STOP, json!({ "queueId": queue_id }))
                .await?;
        }
        QueueAction::Reorder => {
            for id in &task_ids {
                let task = fetch_task(rpc, id).await?;
                let in_queue = task.queue_id == queue_id
                    || (task.queue_id.is_empty() && queue_id == MAIN_QUEUE_ID);
                if !in_queue || task.status != STATUS_PENDING {
                    return Err(invalid(format!(
                        "task '{id}' is not waiting in that queue; only queued (not started) tasks can be reordered"
                    )));
                }
            }
            rpc.call(
                method::DAEMON_QUEUE_REORDER,
                json!({ "queueId": queue_id, "taskIds": task_ids }),
            )
            .await?;
        }
    }
    Ok(json!({
        "ok": true,
        "action": args.action.as_str(),
        "queueId": queue_id,
        "tasks": task_ids.len(),
    }))
}

// ───────────────────────── fd_install_component ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ComponentArgs {
    pub(super) component: ComponentKind,
    pub(super) version: Option<String>,
}

pub(super) fn component_name(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Ffmpeg => "ffmpeg",
        ComponentKind::Ytdlp => "ytdlp",
    }
}

pub(super) async fn install_component(rpc: &impl ToolRpc, args: Value) -> Result<Value, ToolError> {
    let args: ComponentArgs = super::parse_args(args)?;
    let version = match args.version.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(version) => {
            check_max("version", version, MAX_VERSION_CHARS)?;
            if !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            {
                return Err(invalid(
                    "version may only contain letters, digits, '.', '-' and '_'",
                ));
            }
            Some(version.to_owned())
        }
    };
    let mut params = json!({ "component": component_name(args.component) });
    if let (Some(version), Some(object)) = (&version, params.as_object_mut()) {
        object.insert("version".to_owned(), json!(version));
    }
    rpc.call(method::DAEMON_COMPONENT_INSTALL, params).await?;
    Ok(json!({
        "ok": true,
        "component": component_name(args.component),
        "version": version,
    }))
}

// ───────────────────────── fd_set_plugin_enabled ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PluginArgs {
    pub(super) identity: String,
    pub(super) enabled: bool,
}

pub(super) async fn set_plugin_enabled(
    rpc: &impl ToolRpc,
    args: Value,
) -> Result<Value, ToolError> {
    let args: PluginArgs = super::parse_args(args)?;
    let identity = args.identity.trim();
    if identity.is_empty() {
        return Err(invalid("identity must not be empty"));
    }
    check_max("identity", identity, MAX_IDENTITY_CHARS)?;
    let plugins: Vec<PluginDto> = rpc_json(rpc, method::DAEMON_PLUGIN_LIST, json!({})).await?;
    let plugin = plugins
        .iter()
        .find(|plugin| plugin.identity == identity)
        .ok_or_else(|| ToolError::Rpc(format!("no installed plugin with identity '{identity}'")))?;
    if args.enabled && plugin.disabled_reason == "Revoked" {
        return Err(ToolError::Refused(
            "this plugin was revoked and cannot be enabled".to_owned(),
        ));
    }
    if plugin.enabled == args.enabled {
        return Ok(
            json!({ "ok": true, "changed": false, "identity": identity, "enabled": args.enabled }),
        );
    }
    rpc.call(
        method::DAEMON_PLUGIN_SET_ENABLED,
        json!({ "identity": identity, "enabled": args.enabled }),
    )
    .await?;
    Ok(json!({ "ok": true, "changed": true, "identity": identity, "enabled": args.enabled }))
}

// ───────────────────────── fd_repair ─────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RepairArgs {
    pub(super) action: String,
    #[serde(default)]
    pub(super) target: Option<String>,
}

pub(super) async fn repair(rpc: &impl ToolRpc, args: Value) -> Result<Value, ToolError> {
    let args: RepairArgs = super::parse_args(args)?;
    if !REPAIR_ACTIONS.contains(&args.action.as_str()) {
        return Err(invalid(format!(
            "action must be one of: {}",
            REPAIR_ACTIONS.join(", ")
        )));
    }
    let target = args.target.as_deref().map(str::trim).unwrap_or_default();
    check_max("target", target, MAX_TARGET_CHARS.min(MAX_URL_CHARS))?;
    check_no_control("target", target)?;
    let result = rpc
        .call(
            method::AGENT_DIAGNOSTICS_REPAIR,
            json!({ "action": args.action, "target": target }),
        )
        .await?;
    Ok(json!({ "action": args.action, "target": target, "result": result }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tasks::test_support::MockRpc;
    use super::*;

    fn queues() -> Value {
        json!([
            { "queueId": "main", "name": "Main", "speedLimitKbps": 0, "maxConcurrent": 0,
              "defaultSaveDir": "", "position": 0, "defaultSegments": 0, "defaultUserAgent": "" },
            { "queueId": "night", "name": "Night", "speedLimitKbps": 0, "maxConcurrent": 0,
              "defaultSaveDir": "", "position": 1, "defaultSegments": 0, "defaultUserAgent": "" },
        ])
    }

    fn rpc() -> MockRpc {
        MockRpc::new(|name, params| match name {
            method::DAEMON_QUEUE_LIST => Ok(queues()),
            method::DAEMON_TASK_GET => {
                let id = params["taskId"].as_str().unwrap_or_default();
                Ok(match id {
                    "waiting" | "waiting2" => {
                        json!({ "taskId": id, "queueId": "night", "status": 0 })
                    }
                    "running" => json!({ "taskId": id, "queueId": "night", "status": 1 }),
                    _ => json!({ "taskId": id, "queueId": "main", "status": 0 }),
                })
            }
            method::DAEMON_PLUGIN_LIST => Ok(json!([
                { "identity": "a@x", "name": "A", "version": "1", "enabled": false, "devMode": false, "settings": [], "settingsValues": {}, "disabledReason": "Manual" },
                { "identity": "r@x", "name": "R", "version": "1", "enabled": false, "devMode": false, "settings": [], "settingsValues": {}, "disabledReason": "Revoked" },
                { "identity": "on@x", "name": "On", "version": "1", "enabled": true, "devMode": false, "settings": [], "settingsValues": {}, "disabledReason": "None" },
            ])),
            _ => Ok(json!({ "ok": true })),
        })
    }

    #[tokio::test]
    async fn queue_start_resolves_names_and_stop_refuses_the_main_queue() {
        let mock = rpc();
        let started = manage_queues(&mock, json!({ "action": "start", "queue": "night" }))
            .await
            .expect("ok");
        assert_eq!(started["queueId"], "night");
        assert_eq!(
            mock.calls_of(method::DAEMON_QUEUE_START)[0]["queueId"],
            "night"
        );
        let refused = manage_queues(&mock, json!({ "action": "stop", "queue": "main" })).await;
        assert!(matches!(refused, Err(ToolError::Refused(_))), "{refused:?}");
        assert!(mock.calls_of(method::DAEMON_QUEUE_STOP).is_empty());
        let unknown = manage_queues(&mock, json!({ "action": "stop", "queue": "nope" })).await;
        assert!(matches!(unknown, Err(ToolError::InvalidArguments(_))));
    }

    #[tokio::test]
    async fn queue_reorder_validates_membership_and_dedups() {
        let mock = rpc();
        let ok = manage_queues(
            &mock,
            json!({ "action": "reorder", "queue": "night", "taskIds": ["waiting2", "waiting", "waiting2"] }),
        )
        .await
        .expect("ok");
        assert_eq!(ok["tasks"], 2);
        assert_eq!(
            mock.calls_of(method::DAEMON_QUEUE_REORDER)[0]["taskIds"],
            json!(["waiting2", "waiting"])
        );
        for ids in [json!(["running"]), json!(["elsewhere"])] {
            let bad = manage_queues(
                &mock,
                json!({ "action": "reorder", "queue": "night", "taskIds": ids }),
            )
            .await;
            assert!(matches!(bad, Err(ToolError::InvalidArguments(_))), "{ids}");
        }
        assert_eq!(mock.calls_of(method::DAEMON_QUEUE_REORDER).len(), 1);
        for args in [
            json!({ "action": "reorder", "queue": "night" }),
            json!({ "action": "start", "queue": "night", "taskIds": ["a"] }),
            json!({ "action": "reorder", "queue": "night", "taskIds": [] }),
            json!({ "action": "start", "queue": " " }),
        ] {
            let bad = manage_queues(&mock, args.clone()).await;
            assert!(matches!(bad, Err(ToolError::InvalidArguments(_))), "{args}");
        }
    }

    #[tokio::test]
    async fn component_install_validates_the_version() {
        let mock = rpc();
        let ok = install_component(
            &mock,
            json!({ "component": "ytdlp", "version": " 2025.01.15 " }),
        )
        .await
        .expect("ok");
        assert_eq!(ok["component"], "ytdlp");
        let calls = mock.calls_of(method::DAEMON_COMPONENT_INSTALL);
        assert_eq!(
            calls[0],
            json!({ "component": "ytdlp", "version": "2025.01.15" })
        );
        install_component(&mock, json!({ "component": "ffmpeg" }))
            .await
            .expect("latest");
        assert_eq!(
            mock.calls_of(method::DAEMON_COMPONENT_INSTALL)[1],
            json!({ "component": "ffmpeg" })
        );
        for bad in [
            json!({ "component": "ytdlp", "version": "1.0; rm -rf" }),
            json!({ "component": "curl" }),
            json!({ "component": "ytdlp", "extra": 1 }),
        ] {
            let result = install_component(&mock, bad.clone()).await;
            assert!(
                matches!(result, Err(ToolError::InvalidArguments(_))),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn plugin_toggle_checks_existence_state_and_revocation() {
        let mock = rpc();
        let on = set_plugin_enabled(&mock, json!({ "identity": "a@x", "enabled": true }))
            .await
            .expect("ok");
        assert_eq!(on["changed"], true);
        assert_eq!(
            mock.calls_of(method::DAEMON_PLUGIN_SET_ENABLED)[0],
            json!({ "identity": "a@x", "enabled": true })
        );
        let noop = set_plugin_enabled(&mock, json!({ "identity": "on@x", "enabled": true }))
            .await
            .expect("ok");
        assert_eq!(noop["changed"], false);
        assert_eq!(mock.calls_of(method::DAEMON_PLUGIN_SET_ENABLED).len(), 1);
        let revoked =
            set_plugin_enabled(&mock, json!({ "identity": "r@x", "enabled": true })).await;
        assert!(matches!(revoked, Err(ToolError::Refused(_))));
        let missing = set_plugin_enabled(&mock, json!({ "identity": "zz", "enabled": true })).await;
        assert!(matches!(missing, Err(ToolError::Rpc(_))));
    }

    #[tokio::test]
    async fn repair_only_allows_report_actions() {
        let mock = rpc();
        let ok = repair(
            &mock,
            json!({ "action": "fix_component", "target": " yt-dlp " }),
        )
        .await
        .expect("ok");
        assert_eq!(ok["target"], "yt-dlp");
        assert_eq!(
            mock.calls_of(method::AGENT_DIAGNOSTICS_REPAIR)[0],
            json!({ "action": "fix_component", "target": "yt-dlp" })
        );
        for action in [
            "open_settings",
            "open_log_dir",
            "test_notification",
            "rm -rf",
            "",
        ] {
            let bad = repair(&mock, json!({ "action": action })).await;
            assert!(
                matches!(bad, Err(ToolError::InvalidArguments(_))),
                "{action}"
            );
        }
        assert_eq!(mock.calls_of(method::AGENT_DIAGNOSTICS_REPAIR).len(), 1);
    }
}
