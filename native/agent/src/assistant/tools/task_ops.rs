//! `fd_manage_tasks` 版本 2 新增的动作:重新下载、换源、BT 校验 / 重新汇报 / 做种限制 / 文件优先级。
//!
//! 参数校验与逐任务执行都在这里,`tasks.rs` 的 `manage_tasks` 只负责解析任务、分发与汇总。
//! RPC 对应:
//! - `restart`:`daemon.task.restartFile`(`recreateIfPresent`)+ 必要时 `daemon.task.create`(与界面
//!   「重新下载」同一条路径:有产物则连文件一起删掉,再按原请求重建);
//! - `changeUrl`:`daemon.task.changeUrl`;
//! - `recheck` / `reannounce`:`daemon.task.recheck` / `daemon.task.reannounce`;
//! - `setSeedLimits`:`daemon.task.setSeedLimits`;
//! - `setFilePriorities`:`daemon.task.setFilePriorities`。

use fluxdown_protocol::{
    BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM, BT_FILE_PRIORITY_NORMAL,
    BT_FILE_PRIORITY_SKIP, BtFilePriorityDto, CreateTaskRequest, method,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ToolError;
use super::output::display_url;
use super::tasks::{
    MAX_URL_CHARS, ManageAction, ManageArgs, SEED_LIMIT_FOLLOW_GLOBAL, STATUS_COMPLETED,
    STATUS_DOWNLOADING, STATUS_PREPARING, TaskView, ToolRpc, check_max, check_no_control, encode,
    invalid, is_torrent_file_url, rpc_json, soft,
};

/// 做种限制:`-1` = 不限制。
const SEED_LIMIT_UNLIMITED: i64 = -1;
/// 分享率上限(倍)。
const MAX_SEED_RATIO: f64 = 1000.0;
/// 做种 / 不活跃时长上限(分钟)。
const MAX_SEED_MINUTES: i64 = 10_000_000;
/// 上传限速上限(KiB / 秒),换算成字节 / 秒后不超过 1e12。
const MAX_UPLOAD_KBPS: u64 = 976_000_000;
/// 一次最多改多少个文件的优先级。
pub(super) const MAX_PRIORITY_ENTRIES: usize = 500;

/// 逐任务结果,汇总进 `applied` / `skipped` / `failed`。
#[derive(Default)]
pub(super) struct Outcome {
    applied: Vec<Value>,
    skipped: Vec<Value>,
    failed: Vec<Value>,
}

impl Outcome {
    pub(super) fn merge(
        self,
        applied: &mut Vec<Value>,
        skipped: &mut Vec<Value>,
        failed: &mut Vec<Value>,
    ) {
        applied.extend(self.applied);
        skipped.extend(self.skipped);
        failed.extend(self.failed);
    }
}

/// 版本 2 动作的已校验参数。
#[derive(Default)]
pub(super) struct ExtPlan {
    pub(super) url: String,
    pub(super) seed: Option<SeedPlan>,
    pub(super) priorities: Vec<BtFilePriorityDto>,
}

// ───────────────────────── 参数 ─────────────────────────

/// `setSeedLimits` 的参数:只写给出的字段,其余保持任务当前值。
/// 分享率 / 时长:`-2` 跟随全局,`-1` 不限制,`>= 0` 自定义。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SeedLimitsArgs {
    pub(super) ratio_limit: Option<f64>,
    pub(super) post_ratio_limit: Option<f64>,
    pub(super) seed_time_minutes: Option<i64>,
    pub(super) inactive_time_minutes: Option<i64>,
    #[serde(rename = "uploadLimitKBps")]
    pub(super) upload_limit_kbps: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SeedPlan {
    pub(super) ratio_milli: Option<i64>,
    pub(super) post_ratio_milli: Option<i64>,
    pub(super) time_minutes: Option<i64>,
    pub(super) inactive_minutes: Option<i64>,
    pub(super) upload_bps: Option<i64>,
}

/// 一个任务的完整做种限制(线上单位)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SeedLimits {
    pub(super) ratio_milli: i64,
    pub(super) post_ratio_milli: i64,
    pub(super) time_minutes: i64,
    pub(super) inactive_minutes: i64,
    pub(super) upload_bps: i64,
}

impl SeedLimits {
    fn of(task: &TaskView) -> Self {
        Self {
            ratio_milli: task.seed_ratio_limit_milli,
            post_ratio_milli: task.seed_post_ratio_limit_milli,
            time_minutes: task.seed_time_limit_minutes,
            inactive_minutes: task.seed_inactive_time_limit_minutes,
            upload_bps: task.seed_upload_limit_bps,
        }
    }

    pub(super) fn with(self, plan: SeedPlan) -> Self {
        Self {
            ratio_milli: plan.ratio_milli.unwrap_or(self.ratio_milli),
            post_ratio_milli: plan.post_ratio_milli.unwrap_or(self.post_ratio_milli),
            time_minutes: plan.time_minutes.unwrap_or(self.time_minutes),
            inactive_minutes: plan.inactive_minutes.unwrap_or(self.inactive_minutes),
            upload_bps: plan.upload_bps.unwrap_or(self.upload_bps),
        }
    }

    /// 给模型 / 确认卡片看的取值:跟随全局 / 不限制 / 数值。
    pub(super) fn ratio_label(milli: i64) -> String {
        match milli {
            SEED_LIMIT_FOLLOW_GLOBAL => "global".to_owned(),
            SEED_LIMIT_UNLIMITED => "unlimited".to_owned(),
            other => format!("{:.2}", other as f64 / 1000.0),
        }
    }

    pub(super) fn minutes_label(minutes: i64) -> String {
        match minutes {
            SEED_LIMIT_FOLLOW_GLOBAL => "global".to_owned(),
            SEED_LIMIT_UNLIMITED => "unlimited".to_owned(),
            other => format!("{other} min"),
        }
    }

    pub(super) fn upload_label(bps: i64) -> String {
        if bps <= 0 {
            "unlimited".to_owned()
        } else {
            format!("{} KiB/s", bps / 1024)
        }
    }

    fn describe(self) -> Value {
        json!({
            "ratioLimit": Self::ratio_label(self.ratio_milli),
            "postRatioLimit": Self::ratio_label(self.post_ratio_milli),
            "seedTime": Self::minutes_label(self.time_minutes),
            "inactiveTime": Self::minutes_label(self.inactive_minutes),
            "uploadLimit": Self::upload_label(self.upload_bps),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum FilePriority {
    Skip,
    Normal,
    High,
    Maximum,
}

impl FilePriority {
    pub(super) fn wire(self) -> u8 {
        match self {
            Self::Skip => BT_FILE_PRIORITY_SKIP,
            Self::Normal => BT_FILE_PRIORITY_NORMAL,
            Self::High => BT_FILE_PRIORITY_HIGH,
            Self::Maximum => BT_FILE_PRIORITY_MAXIMUM,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FilePriorityArg {
    pub(super) index: u32,
    pub(super) priority: FilePriority,
}

// ───────────────────────── 校验 ─────────────────────────

/// 字段与动作 / 工具版本的搭配。
pub(super) fn check_fields(args: &ManageArgs, extended: bool) -> Result<(), ToolError> {
    let action = args.action;
    if !extended {
        if action.is_extended() {
            return Err(invalid(format!(
                "action '{}' requires fd_manage_tasks version 2",
                action.as_str()
            )));
        }
        if args.url.is_some() || args.seed_limits.is_some() || args.file_priorities.is_some() {
            return Err(invalid(
                "url, seedLimits and filePriorities require fd_manage_tasks version 2",
            ));
        }
    }
    if args.url.is_some() && action != ManageAction::ChangeUrl {
        return Err(invalid("url is only valid with action 'changeUrl'"));
    }
    if args.seed_limits.is_some() && action != ManageAction::SetSeedLimits {
        return Err(invalid(
            "seedLimits is only valid with action 'setSeedLimits'",
        ));
    }
    if args.file_priorities.is_some() && action != ManageAction::SetFilePriorities {
        return Err(invalid(
            "filePriorities is only valid with action 'setFilePriorities'",
        ));
    }
    Ok(())
}

pub(super) fn plan_new_url(url: &str) -> Result<String, ToolError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(invalid("url must not be empty"));
    }
    check_max("url", url, MAX_URL_CHARS)?;
    check_no_control("url", url)?;
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("url is not a valid URL"))?;
    if !matches!(parsed.scheme(), "http" | "https" | "ftp" | "ftps") || parsed.host_str().is_none()
    {
        return Err(invalid("url must be an http(s) or ftp(s) link"));
    }
    Ok(url.to_owned())
}

fn ratio_to_milli(field: &str, value: f64) -> Result<i64, ToolError> {
    let bad = || {
        invalid(format!(
            "{field} must be -2 (follow global), -1 (unlimited) or a ratio between 0 and {MAX_SEED_RATIO}"
        ))
    };
    if !value.is_finite() {
        return Err(bad());
    }
    if value < 0.0 {
        let whole = value.round();
        if (value - whole).abs() > 1e-9 {
            return Err(bad());
        }
        return match whole as i64 {
            SEED_LIMIT_FOLLOW_GLOBAL => Ok(SEED_LIMIT_FOLLOW_GLOBAL),
            SEED_LIMIT_UNLIMITED => Ok(SEED_LIMIT_UNLIMITED),
            _ => Err(bad()),
        };
    }
    if value > MAX_SEED_RATIO {
        return Err(bad());
    }
    Ok((value * 1000.0).round() as i64)
}

fn check_minutes(field: &str, value: i64) -> Result<i64, ToolError> {
    if value == SEED_LIMIT_FOLLOW_GLOBAL
        || value == SEED_LIMIT_UNLIMITED
        || (0..=MAX_SEED_MINUTES).contains(&value)
    {
        Ok(value)
    } else {
        Err(invalid(format!(
            "{field} must be -2 (follow global), -1 (unlimited) or 0..={MAX_SEED_MINUTES} minutes"
        )))
    }
}

pub(super) fn plan_seed_limits(args: &SeedLimitsArgs) -> Result<SeedPlan, ToolError> {
    let mut plan = SeedPlan::default();
    if let Some(value) = args.ratio_limit {
        plan.ratio_milli = Some(ratio_to_milli("seedLimits.ratioLimit", value)?);
    }
    if let Some(value) = args.post_ratio_limit {
        plan.post_ratio_milli = Some(ratio_to_milli("seedLimits.postRatioLimit", value)?);
    }
    if let Some(value) = args.seed_time_minutes {
        plan.time_minutes = Some(check_minutes("seedLimits.seedTimeMinutes", value)?);
    }
    if let Some(value) = args.inactive_time_minutes {
        plan.inactive_minutes = Some(check_minutes("seedLimits.inactiveTimeMinutes", value)?);
    }
    if let Some(kbps) = args.upload_limit_kbps {
        if kbps > MAX_UPLOAD_KBPS {
            return Err(invalid(format!(
                "seedLimits.uploadLimitKBps must be at most {MAX_UPLOAD_KBPS} (0 = unlimited)"
            )));
        }
        plan.upload_bps = Some(i64::try_from(kbps.saturating_mul(1024)).unwrap_or(i64::MAX));
    }
    if plan == SeedPlan::default() {
        return Err(invalid("seedLimits must contain at least one field"));
    }
    Ok(plan)
}

pub(super) fn plan_priorities(
    items: &[FilePriorityArg],
) -> Result<Vec<BtFilePriorityDto>, ToolError> {
    if items.is_empty() || items.len() > MAX_PRIORITY_ENTRIES {
        return Err(invalid(format!(
            "filePriorities must contain 1..={MAX_PRIORITY_ENTRIES} entries"
        )));
    }
    let mut planned: Vec<BtFilePriorityDto> = Vec::with_capacity(items.len());
    for item in items {
        match planned.iter().find(|entry| entry.index == item.index) {
            Some(existing) if existing.priority != item.priority.wire() => {
                return Err(invalid(format!(
                    "file index {} is listed twice with different priorities",
                    item.index
                )));
            }
            Some(_) => {}
            None => planned.push(BtFilePriorityDto {
                index: item.index,
                priority: item.priority.wire(),
            }),
        }
    }
    Ok(planned)
}

// ───────────────────────── 执行 ─────────────────────────

fn not_bt(task: &TaskView) -> Value {
    task.describe_with("not a BitTorrent task")
}

/// 重新下载:已完成 / 失败的任务。有产物则连文件一起删掉再按原请求重建,产物已丢失则就地重启。
pub(super) async fn restart(rpc: &impl ToolRpc, tasks: &[TaskView]) -> Result<Outcome, ToolError> {
    let mut out = Outcome::default();
    for task in tasks {
        if is_torrent_file_url(&task.url) {
            out.skipped.push(task.describe_with(
                "tasks added from a .torrent file cannot be redownloaded (the torrent file is not kept)",
            ));
            continue;
        }
        if !(task.status == STATUS_COMPLETED || task.is_failed()) {
            out.skipped.push(task.describe_with(
                "only completed or failed tasks can be redownloaded; pause / resume covers running ones",
            ));
            continue;
        }
        let recreate: bool = match rpc_json(
            rpc,
            method::DAEMON_TASK_RESTART_FILE,
            json!({ "taskId": task.task_id, "recreateIfPresent": true }),
        )
        .await
        {
            Ok(recreate) => recreate,
            Err(error) => {
                out.failed.push(task.describe_with(&soft(error)?));
                continue;
            }
        };
        if !recreate {
            out.applied.push(json!({
                "taskId": task.task_id, "name": task.file_name, "mode": "restarted",
            }));
            continue;
        }
        // 守护进程已删除任务及其文件:按原请求重建。
        let request = recreate_request(task);
        let params = json!({ "request": encode(method::DAEMON_TASK_CREATE, &request)? });
        match rpc.call(method::DAEMON_TASK_CREATE, params).await {
            Ok(created) => out.applied.push(json!({
                "taskId": task.task_id,
                "name": task.file_name,
                "mode": "recreated",
                "newTaskId": created.get("taskId").cloned().unwrap_or(Value::Null),
            })),
            Err(error) => out.failed.push(task.describe_with(&format!(
                "the task and its files were removed but it could not be recreated: {}",
                soft(error)?
            ))),
        }
    }
    Ok(out)
}

/// 与界面「重新下载」一致:取原始地址与任务自带的下载选项。
fn recreate_request(task: &TaskView) -> CreateTaskRequest {
    CreateTaskRequest {
        url: if task.origin_url.is_empty() {
            task.url.clone()
        } else {
            task.origin_url.clone()
        },
        file_name: task.file_name.clone(),
        save_dir: task.save_dir.clone(),
        segments: 0,
        cookies: String::new(),
        referrer: task.referrer.clone(),
        proxy_url: task.proxy_url.clone(),
        user_agent: String::new(),
        queue_id: task.queue_id.clone(),
        checksum: task.checksum.clone(),
        ignore_tls_errors: task.ignore_tls_errors,
        headers: None,
        torrent_b64: None,
        method: None,
        body: None,
        audio_url: None,
        start_paused: false,
        http_user: String::new(),
        http_password: String::new(),
        save_site_auth: false,
    }
}

/// 换源:只对暂停 / 排队 / 失败的非 BT 任务。
pub(super) async fn change_url(
    rpc: &impl ToolRpc,
    tasks: &[TaskView],
    url: &str,
) -> Result<Outcome, ToolError> {
    let mut out = Outcome::default();
    for task in tasks {
        if task.is_bt() {
            out.skipped
                .push(task.describe_with("BitTorrent tasks have no replaceable URL"));
        } else if task.status == STATUS_COMPLETED {
            out.skipped.push(task.describe_with("task is completed"));
        } else if matches!(task.status, STATUS_DOWNLOADING | STATUS_PREPARING) {
            out.skipped
                .push(task.describe_with("pause the task before changing its URL"));
        } else {
            let params = json!({ "taskId": task.task_id, "url": url });
            match rpc.call(method::DAEMON_TASK_CHANGE_URL, params).await {
                Ok(_) => out.applied.push(json!({
                    "taskId": task.task_id, "name": task.file_name, "url": display_url(url),
                })),
                Err(error) => out.failed.push(task.describe_with(&soft(error)?)),
            }
        }
    }
    Ok(out)
}

/// 重新校验本地数据(BT)。
pub(super) async fn recheck(rpc: &impl ToolRpc, tasks: &[TaskView]) -> Result<Outcome, ToolError> {
    let mut out = Outcome::default();
    for task in tasks {
        if !task.is_bt() {
            out.skipped.push(not_bt(task));
            continue;
        }
        let params = json!({ "taskId": task.task_id });
        match rpc.call(method::DAEMON_TASK_RECHECK, params).await {
            Ok(_) => out.applied.push(task.describe()),
            Err(error) => out.failed.push(task.describe_with(&soft(error)?)),
        }
    }
    Ok(out)
}

/// 立即向 tracker 重新汇报(BT,仅下载中 / 做种中)。
pub(super) async fn reannounce(
    rpc: &impl ToolRpc,
    tasks: &[TaskView],
) -> Result<Outcome, ToolError> {
    let mut out = Outcome::default();
    for task in tasks {
        let running = matches!(task.status, STATUS_DOWNLOADING | STATUS_PREPARING)
            || (task.status == STATUS_COMPLETED && task.seeding_status == 1);
        if !task.is_bt() {
            out.skipped.push(not_bt(task));
        } else if !running {
            out.skipped
                .push(task.describe_with("task is not downloading or seeding"));
        } else {
            let params = json!({ "taskId": task.task_id });
            match rpc.call(method::DAEMON_TASK_REANNOUNCE, params).await {
                Ok(_) => out.applied.push(task.describe()),
                Err(error) => out.failed.push(task.describe_with(&soft(error)?)),
            }
        }
    }
    Ok(out)
}

/// 任务级做种限制:合并任务当前值后整体写入。
pub(super) async fn set_seed_limits(
    rpc: &impl ToolRpc,
    tasks: &[TaskView],
    plan: SeedPlan,
) -> Result<Outcome, ToolError> {
    let mut out = Outcome::default();
    for task in tasks {
        if !task.is_bt() {
            out.skipped.push(not_bt(task));
            continue;
        }
        let limits = SeedLimits::of(task).with(plan);
        let params = json!({
            "taskId": task.task_id,
            "ratioLimitMilli": limits.ratio_milli,
            "postRatioLimitMilli": limits.post_ratio_milli,
            "seedTimeLimitMinutes": limits.time_minutes,
            "inactiveTimeLimitMinutes": limits.inactive_minutes,
            "uploadLimitBps": limits.upload_bps,
        });
        match rpc.call(method::DAEMON_TASK_SET_SEED_LIMITS, params).await {
            Ok(_) => out.applied.push(json!({
                "taskId": task.task_id, "name": task.file_name, "limits": limits.describe(),
            })),
            Err(error) => out.failed.push(task.describe_with(&soft(error)?)),
        }
    }
    Ok(out)
}

/// BT 文件优先级(文件序号取自 `fd_get_bt_status` 版本 2 的 `files`)。
pub(super) async fn set_file_priorities(
    rpc: &impl ToolRpc,
    tasks: &[TaskView],
    priorities: &[BtFilePriorityDto],
) -> Result<Outcome, ToolError> {
    let mut out = Outcome::default();
    for task in tasks {
        if !task.is_bt() {
            out.skipped.push(not_bt(task));
            continue;
        }
        let params = json!({
            "taskId": task.task_id,
            "priorities": encode(method::DAEMON_TASK_SET_FILE_PRIORITIES, &priorities)?,
        });
        match rpc
            .call(method::DAEMON_TASK_SET_FILE_PRIORITIES, params)
            .await
        {
            Ok(_) => out.applied.push(json!({
                "taskId": task.task_id, "name": task.file_name, "files": priorities.len(),
            })),
            Err(error) => out.failed.push(task.describe_with(&soft(error)?)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::method;
    use serde_json::{Value, json};

    use super::super::tasks::manage_tasks_versioned;
    use super::super::tasks::test_support::MockRpc;
    use super::*;

    fn task(id: &str, url: &str, status: i32) -> Value {
        json!({
            "taskId": id, "url": url, "fileName": format!("{id}.bin"), "status": status,
            "saveDir": "/dl", "queueId": "", "originUrl": "",
        })
    }

    fn rpc(tasks: Vec<Value>) -> MockRpc {
        MockRpc::new(move |name, params| match name {
            method::DAEMON_TASK_GET => {
                let id = params["taskId"].as_str().unwrap_or_default();
                tasks
                    .iter()
                    .find(|task| task["taskId"] == id)
                    .cloned()
                    .ok_or_else(|| ToolError::Rpc("daemon.task.get failed: not-found".to_owned()))
            }
            method::DAEMON_TASK_RESTART_FILE => Ok(json!(true)),
            method::DAEMON_TASK_CREATE => Ok(json!({ "taskId": "new-1" })),
            _ => Ok(json!({})),
        })
    }

    const MAGNET: &str = "magnet:?xt=urn:btih:abc";

    #[tokio::test]
    async fn v1_rejects_extended_actions_and_fields() {
        let mock = rpc(vec![task("t1", MAGNET, 3)]);
        for args in [
            json!({ "taskIds": ["t1"], "action": "restart" }),
            json!({ "taskIds": ["t1"], "action": "pause", "url": "https://a.example/x" }),
        ] {
            let result = manage_tasks_versioned(&mock, args, false).await;
            assert!(
                matches!(&result, Err(ToolError::InvalidArguments(m)) if m.contains("version 2")),
                "{result:?}"
            );
        }
        assert!(mock.calls.lock().map(|c| c.is_empty()).unwrap_or(false));
    }

    #[tokio::test]
    async fn field_action_mismatches_are_rejected() {
        let mock = rpc(vec![task("t1", MAGNET, 3)]);
        for args in [
            json!({ "taskIds": ["t1"], "action": "pause", "seedLimits": { "ratioLimit": 1.0 } }),
            json!({ "taskIds": ["t1"], "action": "setSeedLimits" }),
            json!({ "taskIds": ["t1"], "action": "changeUrl" }),
            json!({ "taskIds": ["t1", "t2"], "action": "changeUrl", "url": "https://a.example/x" }),
            json!({ "taskIds": ["t1"], "action": "setFilePriorities", "filePriorities": [] }),
            json!({ "taskIds": ["t1"], "action": "setSeedLimits", "seedLimits": {} }),
        ] {
            let result = manage_tasks_versioned(&mock, args.clone(), true).await;
            assert!(
                matches!(result, Err(ToolError::InvalidArguments(_))),
                "{args}"
            );
        }
        assert!(mock.calls.lock().map(|c| c.is_empty()).unwrap_or(false));
    }

    #[test]
    fn seed_limit_values_are_validated_and_converted() {
        let ok = |args: Value| {
            plan_seed_limits(&serde_json::from_value(args).expect("args")).expect("valid")
        };
        let plan = ok(json!({ "ratioLimit": 1.5, "seedTimeMinutes": 90, "uploadLimitKBps": 512 }));
        assert_eq!(plan.ratio_milli, Some(1500));
        assert_eq!(plan.time_minutes, Some(90));
        assert_eq!(plan.upload_bps, Some(512 * 1024));
        assert_eq!(ok(json!({ "ratioLimit": -2 })).ratio_milli, Some(-2));
        assert_eq!(
            ok(json!({ "inactiveTimeMinutes": -1 })).inactive_minutes,
            Some(-1)
        );
        for bad in [
            json!({ "ratioLimit": -3 }),
            json!({ "ratioLimit": -1.5 }),
            json!({ "ratioLimit": 100000 }),
            json!({ "postRatioLimit": -9 }),
            json!({ "seedTimeMinutes": -5 }),
            json!({ "uploadLimitKBps": 2_000_000_000_u64 }),
        ] {
            let args: SeedLimitsArgs = serde_json::from_value(bad.clone()).expect("shape");
            assert!(plan_seed_limits(&args).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn seed_limits_merge_with_current_values_and_skip_non_bt() {
        let mut bt = task("bt", MAGNET, 3);
        bt["seedRatioLimitMilli"] = json!(2000);
        bt["seedTimeLimitMinutes"] = json!(30);
        let mock = rpc(vec![bt, task("http", "https://a.example/x", 3)]);
        let result = manage_tasks_versioned(
            &mock,
            json!({
                "taskIds": ["bt", "http"], "action": "setSeedLimits",
                "seedLimits": { "seedTimeMinutes": -1 },
            }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(result["applied"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["skipped"][0]["taskId"], "http");
        let calls = mock.calls_of(method::DAEMON_TASK_SET_SEED_LIMITS);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["ratioLimitMilli"], 2000);
        assert_eq!(calls[0]["seedTimeLimitMinutes"], -1);
        assert_eq!(calls[0]["postRatioLimitMilli"], -2);
        assert_eq!(calls[0]["uploadLimitBps"], 0);
    }

    #[tokio::test]
    async fn change_url_validates_and_only_touches_replaceable_tasks() {
        let mock = rpc(vec![task("t1", "https://old.example/f", 2)]);
        let bad = manage_tasks_versioned(
            &mock,
            json!({ "taskIds": ["t1"], "action": "changeUrl", "url": "javascript:alert(1)" }),
            true,
        )
        .await;
        assert!(matches!(bad, Err(ToolError::InvalidArguments(_))));
        let ok = manage_tasks_versioned(
            &mock,
            json!({ "taskIds": ["t1"], "action": "changeUrl", "url": " https://new.example/f " }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(ok["applied"][0]["taskId"], "t1");
        assert_eq!(
            mock.calls_of(method::DAEMON_TASK_CHANGE_URL)[0]["url"],
            "https://new.example/f"
        );
        let running = rpc(vec![task("t2", "https://old.example/f", 1)]);
        let skipped = manage_tasks_versioned(
            &running,
            json!({ "taskIds": ["t2"], "action": "changeUrl", "url": "https://new.example/f" }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(skipped["skipped"].as_array().map(Vec::len), Some(1));
        assert!(running.calls_of(method::DAEMON_TASK_CHANGE_URL).is_empty());
    }

    #[tokio::test]
    async fn restart_recreates_from_the_stored_request() {
        let mut done = task("t1", "https://cdn.example/f", 3);
        done["originUrl"] = json!("https://origin.example/f");
        done["checksum"] = json!("sha256=ab");
        let mock = rpc(vec![
            done,
            task("run", "https://a.example/x", 1),
            task("seed", "torrent-file://local/x", 3),
        ]);
        let result = manage_tasks_versioned(
            &mock,
            json!({ "taskIds": ["t1", "run", "seed"], "action": "restart" }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(result["applied"][0]["mode"], "recreated");
        assert_eq!(result["applied"][0]["newTaskId"], "new-1");
        assert_eq!(result["skipped"].as_array().map(Vec::len), Some(2));
        let restart = mock.calls_of(method::DAEMON_TASK_RESTART_FILE);
        assert_eq!(restart.len(), 1);
        assert_eq!(restart[0]["recreateIfPresent"], true);
        let create = mock.calls_of(method::DAEMON_TASK_CREATE);
        assert_eq!(create[0]["request"]["url"], "https://origin.example/f");
        assert_eq!(create[0]["request"]["checksum"], "sha256=ab");
        assert_eq!(create[0]["request"]["saveDir"], "/dl");
    }

    #[tokio::test]
    async fn bt_only_actions_skip_http_tasks_and_reannounce_needs_a_running_task() {
        let mut seeding = task("seed", MAGNET, 3);
        seeding["seedingStatus"] = json!(1);
        let mock = rpc(vec![
            task("http", "https://a.example/x", 1),
            task("idle", MAGNET, 2),
            seeding,
        ]);
        let result = manage_tasks_versioned(
            &mock,
            json!({ "taskIds": ["http", "idle", "seed"], "action": "reannounce" }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(result["applied"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["skipped"].as_array().map(Vec::len), Some(2));
        assert_eq!(mock.calls_of(method::DAEMON_TASK_REANNOUNCE).len(), 1);
        let recheck = manage_tasks_versioned(
            &mock,
            json!({ "taskIds": ["http", "idle"], "action": "recheck" }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(recheck["applied"][0]["taskId"], "idle");
        assert_eq!(mock.calls_of(method::DAEMON_TASK_RECHECK).len(), 1);
    }

    #[tokio::test]
    async fn file_priorities_map_names_and_reject_conflicts() {
        let mock = rpc(vec![task("bt", MAGNET, 1)]);
        let result = manage_tasks_versioned(
            &mock,
            json!({
                "taskIds": ["bt"], "action": "setFilePriorities",
                "filePriorities": [
                    { "index": 0, "priority": "skip" },
                    { "index": 2, "priority": "high" },
                    { "index": 2, "priority": "high" },
                    { "index": 3, "priority": "maximum" },
                ],
            }),
            true,
        )
        .await
        .expect("ok");
        assert_eq!(result["applied"][0]["files"], 3);
        let calls = mock.calls_of(method::DAEMON_TASK_SET_FILE_PRIORITIES);
        assert_eq!(
            calls[0]["priorities"],
            json!([
                { "index": 0, "priority": 0 },
                { "index": 2, "priority": 6 },
                { "index": 3, "priority": 7 },
            ])
        );
        let conflict = manage_tasks_versioned(
            &mock,
            json!({
                "taskIds": ["bt"], "action": "setFilePriorities",
                "filePriorities": [
                    { "index": 1, "priority": "skip" },
                    { "index": 1, "priority": "normal" },
                ],
            }),
            true,
        )
        .await;
        assert!(matches!(conflict, Err(ToolError::InvalidArguments(_))));
    }
}
