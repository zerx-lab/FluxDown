//! 下载拦截平面的插件侧：按任务挑选匹配插件，并把各阶段调用转给脚本运行时。

use crate::logger::log_warn;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{
    EXTERNAL_TOOL_HOOK_BUDGET, LoadedPlugin, PluginManager, build_typed_settings_json, value_of,
};
use crate::intercept::{InterceptStage, InterceptorEntry, TaskEgressView, TaskInterceptor};
use crate::plugin::audit::PluginAuditRow;
use crate::plugin::call_egress::CallEgress;
use crate::plugin::manifest::{
    InterceptFailure, PERMISSION_ARCHIVE, PERMISSION_AUTH, PERMISSION_FFMPEG, PERMISSION_INTERCEPT,
    PERMISSION_YTDLP, url_glob_match,
};
use crate::plugin::quickjs::HARD_TIMEOUT_CEILING;
use crate::plugin::runtime::{
    ArchiveRequest, ExecutionBudget, HostContext, PluginEntryKind, PluginError, PluginScript,
};

/// 未声明 `timeoutMs` 时的单次调用超时。
const DEFAULT_INTERCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// 直播分片阶段每轮轮询都会调用，超时额外压低。
const LIVE_INTERCEPT_TIMEOUT: Duration = Duration::from_secs(2);

impl PluginManager {
    /// 为一个任务挑选参与拦截的插件；无匹配返回 noop 拦截器。
    pub async fn task_interceptor(
        self: &Arc<Self>,
        task_id: &str,
        source_url: &str,
        current_url: &str,
    ) -> Arc<dyn TaskInterceptor> {
        let snapshot = self.plugins.read().await.clone();
        let mut entries: Vec<InterceptorEntry> = snapshot
            .iter()
            .filter(|p| p.enabled && p.manifest.has_permission(PERMISSION_INTERCEPT))
            .filter_map(|p| {
                let decl = p.manifest.interceptors.as_ref()?;
                let hit =
                    decl.match_decl.urls.iter().any(|pat| {
                        url_glob_match(pat, source_url) || url_glob_match(pat, current_url)
                    });
                if !hit {
                    return None;
                }
                let stages: Vec<InterceptStage> = decl
                    .stages
                    .iter()
                    .filter_map(|s| InterceptStage::parse(s))
                    .collect();
                Some(InterceptorEntry {
                    identity: p.manifest.identity.clone(),
                    stages,
                    fail_task: decl.on_failure == InterceptFailure::Fail,
                })
            })
            .collect();
        entries.sort_by(|a, b| a.identity.cmp(&b.identity));
        let has_archivers = snapshot.iter().any(|p| {
            p.enabled
                && p.manifest.has_permission(PERMISSION_ARCHIVE)
                && p.manifest
                    .contributes
                    .as_ref()
                    .and_then(|c| c.archive.as_ref())
                    .is_some()
        });
        if entries.is_empty() && !has_archivers {
            return crate::intercept::noop();
        }
        Arc::new(PluginTaskInterceptor {
            pm: self.clone(),
            task_id: task_id.to_string(),
            source_url: source_url.to_string(),
            entries,
            has_archivers,
            egress: std::sync::Mutex::new(None),
        })
    }

    /// 调用某插件的 `intercept(ctx)`。
    pub(super) async fn intercept(
        &self,
        identity: &str,
        task_id: &str,
        stage: InterceptStage,
        arg_json: String,
        file_root: Option<PathBuf>,
        egress: Option<Arc<CallEgress>>,
    ) -> Result<Option<String>, PluginError> {
        let (manifest, streak, source, dev_mode) = {
            let snapshot = self.plugins.read().await.clone();
            let Some(p) = snapshot.iter().find(|p| p.manifest.identity == identity) else {
                return Err(PluginError::Runtime(format!("插件 {identity} 已卸载")));
            };
            if !p.enabled {
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} {}",
                    p.disabled_reason.describe()
                )));
            }
            let source = p.intercept_source().await;
            (p.manifest.clone(), p.timeout_streak.clone(), source, p.dev)
        };
        let Some(source) = source else {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 未提供 interceptors 入口"
            )));
        };

        let values = self.load_setting_values(identity).await;
        for f in &manifest.settings {
            if f.required && value_of(&values, f).is_none() {
                return Err(PluginError::MissingRequiredSetting(format!(
                    "插件 {identity} 需先配置「{}」",
                    f.title
                )));
            }
        }
        let settings_json = build_typed_settings_json(&manifest, &values);

        let finalize = stage == InterceptStage::Finalize;
        let ffmpeg_permitted = manifest.has_permission(PERMISSION_FFMPEG);
        let ytdlp_permitted = manifest.has_permission(PERMISSION_YTDLP);
        let long_running = finalize && (ffmpeg_permitted || ytdlp_permitted);
        let budget = if long_running {
            EXTERNAL_TOOL_HOOK_BUDGET
        } else {
            let mut timeout = manifest
                .interceptors
                .as_ref()
                .and_then(|d| d.timeout_ms)
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_INTERCEPT_TIMEOUT)
                .min(HARD_TIMEOUT_CEILING);
            if stage == InterceptStage::LiveSegments {
                timeout = timeout.min(LIVE_INTERCEPT_TIMEOUT);
            }
            if stage == InterceptStage::Refresh {
                timeout = timeout.min(DEFAULT_INTERCEPT_TIMEOUT);
            }
            ExecutionBudget {
                timeout,
                memory_limit_bytes: self.resolve_budget.memory_limit_bytes,
            }
        };

        let script = PluginScript {
            identity: identity.to_string(),
            source,
            entry_fn_hint: PluginEntryKind::Intercept,
            version: manifest.version.clone(),
            app_version: self.app_version.clone(),
        };
        let host = HostContext {
            ffmpeg_permitted,
            ffmpeg_root: if finalize { file_root } else { None },
            ytdlp_permitted,
            auth_permitted: manifest.has_permission(PERMISSION_AUTH),
            private_network_allowed: dev_mode,
            egress: egress.clone(),
        };
        let result = self
            .runtime
            .invoke_intercept(
                &script,
                arg_json,
                settings_json,
                self.bridge.clone(),
                budget,
                host,
                long_running,
                finalize.then(|| task_id.to_string()),
            )
            .await;
        self.note_execution(identity, &streak, result.as_ref().err(), egress.as_deref())
            .await;

        let (outcome, detail) = match &result {
            Ok(_) => ("ok", String::new()),
            Err(e) => ("error", e.to_string()),
        };
        self.audit.record(PluginAuditRow::new(
            identity,
            "daemon",
            &format!("intercept.{}", stage.as_str()),
            task_id,
            outcome,
            &detail,
        ));
        result
    }
    /// 按 identity 升序询问已启用的归档插件，返回首个合法目录名。没有插件给出合法目录名
    /// 时，若有插件失败（缺必填设置、抛错、超时、过载、返回非法值或不安全的目录名），
    /// 以 `Err` 返回逐条原因供任务活动展示；全部明确放弃（返回空）时为 `Ok(None)`。
    pub(super) async fn archive_folder(
        &self,
        file_name: &str,
        task_id: &str,
    ) -> Result<Option<String>, String> {
        let snapshot = self.plugins.read().await.clone();
        let mut candidates: Vec<_> = snapshot
            .iter()
            .filter(|plugin| {
                plugin.enabled
                    && plugin.manifest.has_permission(PERMISSION_ARCHIVE)
                    && plugin
                        .manifest
                        .contributes
                        .as_ref()
                        .and_then(|contributes| contributes.archive.as_ref())
                        .is_some()
            })
            .collect();
        candidates.sort_by(|a, b| a.manifest.identity.cmp(&b.manifest.identity));
        let mut problems = Vec::new();
        for plugin in candidates {
            let identity = &plugin.manifest.identity;
            let problem = match self.archive_one(plugin, file_name, task_id).await {
                Ok(Some(folder_name))
                    if crate::category_route::valid_archive_folder(&folder_name) =>
                {
                    return Ok(Some(folder_name));
                }
                Ok(Some(folder_name)) => format!("返回的目录名 {folder_name:?} 不合法"),
                Ok(None) => continue,
                Err(problem) => problem,
            };
            log_warn!("[plugin] archive {identity} skipped: {problem}");
            problems.push(format!("{identity}：{problem}"));
        }
        if problems.is_empty() {
            Ok(None)
        } else {
            Err(problems.join("；"))
        }
    }

    /// 调用单个归档插件并记审计与熔断；`Err` 为可展示的失败原因。
    async fn archive_one(
        &self,
        plugin: &LoadedPlugin,
        file_name: &str,
        task_id: &str,
    ) -> Result<Option<String>, String> {
        let identity = &plugin.manifest.identity;
        let audit = |outcome: &str, detail: &str| {
            self.audit.record(PluginAuditRow::new(
                identity, "daemon", "archive", task_id, outcome, detail,
            ));
        };
        let Some(source) = plugin.archive_source().await else {
            audit("error", "archive entry unavailable");
            return Err("归档脚本不可读".to_string());
        };
        let values = self.load_setting_values(identity).await;
        if let Some(field) = plugin
            .manifest
            .settings
            .iter()
            .find(|field| field.required && value_of(&values, field).is_none())
        {
            audit("error", "required setting missing");
            return Err(format!("需先配置「{}」", field.title));
        }
        let settings_json = build_typed_settings_json(&plugin.manifest, &values);
        let script = PluginScript {
            identity: identity.clone(),
            source,
            entry_fn_hint: PluginEntryKind::Archive,
            version: plugin.manifest.version.clone(),
            app_version: self.app_version.clone(),
        };
        let result = self
            .runtime
            .invoke_archive(
                &script,
                ArchiveRequest {
                    file_name: file_name.to_string(),
                },
                settings_json,
                self.bridge.clone(),
                ExecutionBudget {
                    timeout: DEFAULT_INTERCEPT_TIMEOUT,
                    memory_limit_bytes: self.resolve_budget.memory_limit_bytes,
                },
            )
            .await;
        self.note_execution(
            identity,
            &plugin.timeout_streak,
            result.as_ref().err(),
            None,
        )
        .await;
        match result {
            Ok(Some(value)) => {
                if crate::category_route::valid_archive_folder(&value.folder_name) {
                    audit("ok", "");
                } else {
                    audit("error", "invalid folderName");
                }
                Ok(Some(value.folder_name))
            }
            Ok(None) => {
                audit("ok", "");
                Ok(None)
            }
            Err(error) => {
                let detail = error.to_string();
                audit("error", &detail);
                Err(detail)
            }
        }
    }
}

/// `Drop` 时中止底层任务：调用方丢弃 future 即终止 JS 与外部子进程。
struct AbortGuard<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortGuard<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 绑定一个任务的插件拦截器。
struct PluginTaskInterceptor {
    pm: Arc<PluginManager>,
    task_id: String,
    source_url: String,
    entries: Vec<InterceptorEntry>,
    has_archivers: bool,
    /// 任务当前出口（见 [`crate::intercept::sync_egress`]）；`None` = 全局出口。
    egress: std::sync::Mutex<Option<TaskEgressView>>,
}

#[async_trait::async_trait]
impl TaskInterceptor for PluginTaskInterceptor {
    fn entries(&self) -> &[InterceptorEntry] {
        &self.entries
    }

    fn source_url(&self) -> &str {
        &self.source_url
    }
    async fn archive_folder(&self, file_name: &str) -> Result<Option<String>, String> {
        if !self.has_archivers {
            return Ok(None);
        }
        let pm = self.pm.clone();
        let task_id = self.task_id.clone();
        let file_name = file_name.to_string();
        let handle = self
            .pm
            .runtime_handle()
            .spawn(async move { pm.archive_folder(&file_name, &task_id).await });
        let mut guard = AbortGuard(handle);
        match (&mut guard.0).await {
            Ok(result) => result,
            Err(error) => Err(format!("插件运行时任务异常: {error}")),
        }
    }

    fn set_egress(&self, egress: TaskEgressView) {
        *self
            .egress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(egress);
    }

    async fn call(
        &self,
        identity: &str,
        stage: InterceptStage,
        arg_json: String,
        file_root: Option<PathBuf>,
    ) -> Result<Option<String>, String> {
        let pm = self.pm.clone();
        let task_id = self.task_id.clone();
        let identity = identity.to_string();
        // 每次调用一个新的观测器：拦截阶段不换路，只用于熔断豁免。
        let egress = self
            .egress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .map(|view| Arc::new(CallEgress::new(view.proxy, view.explicit, view.rules)));
        let handle = self.pm.runtime_handle().spawn(async move {
            pm.intercept(&identity, &task_id, stage, arg_json, file_root, egress)
                .await
        });
        let mut guard = AbortGuard(handle);
        match (&mut guard.0).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(format!("插件运行时任务异常: {e}")),
        }
    }
}
