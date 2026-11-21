//! 桌面模式 agent 的启动失败记录（[`AGENT_STARTUP_FAILURE_FILE`]，与 `agent.token` 同目录）。
//!
//! UI Gateway 开始服务之前任一步失败，都把失败环节与完整错误链写入记录后再退出；开始服务时
//! 删除上一次的记录。拉起 agent 的桌面界面发现 agent 反复退出时读取它，向用户展示具体原因，
//! 而不只是「正在连接」。server 模式（容器 / NAS）的失败由服务管理器与日志承担，不写记录。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxdown_protocol::local_service::{
    AGENT_STARTUP_FAILURE_FILE, AgentStartupFailure, AgentStartupFailureKind,
};
use uuid::Uuid;

/// 临时文件前缀；`StateStore` 打开时清理崩溃遗留的同名临时文件。
pub(crate) const TEMP_PREFIX: &str = ".agent-startup-failure.";

/// 某个启动环节的失败：Display 与 `source` 都透传原错误，只附加环节分类。
#[derive(Debug)]
pub(crate) struct StartupError {
    kind: AgentStartupFailureKind,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl StartupError {
    pub(crate) fn new(
        kind: AgentStartupFailureKind,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            kind,
            source: source.into(),
        }
    }
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

/// 启动失败记录的读写位置与「已开始服务」标记。
pub(crate) struct StartupReport {
    path: PathBuf,
    /// 端点目录本身写不进去时（Windows 上目录 ACL 属于旧账户 SID 等）改写的回退位置
    /// （`local_service::local_fallback_dir`），桌面读不到主记录时读它。
    fallback: Option<PathBuf>,
    serving: AtomicBool,
}

impl StartupReport {
    pub(crate) fn new(directory: &Path) -> Self {
        Self {
            path: directory.join(AGENT_STARTUP_FAILURE_FILE),
            fallback: fluxdown_protocol::local_service::local_fallback_dir(directory)
                .map(|dir| dir.join(AGENT_STARTUP_FAILURE_FILE)),
            serving: AtomicBool::new(false),
        }
    }

    /// Gateway 即将开始服务：此后的失败不再是启动失败；删除上一次留下的记录（含回退位置）。
    pub(crate) async fn mark_serving(&self) {
        self.serving.store(true, Ordering::Release);
        for path in std::iter::once(&self.path).chain(self.fallback.as_ref()) {
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "could not remove the previous agent startup failure record"
                ),
            }
        }
    }

    /// 运行结束：开始服务之前就失败时记录环节与完整错误链。尽力而为——数据目录本身可能就是
    /// 故障点：主位置写不进去时改写回退位置，都写不进去只记日志，不覆盖原错误。
    pub(crate) async fn finish(&self, result: &crate::runtime::AgentResult) {
        let Err(error) = result else {
            return;
        };
        if self.serving.load(Ordering::Acquire) {
            return;
        }
        let kind = error
            .downcast_ref::<StartupError>()
            .map_or(AgentStartupFailureKind::Other, |startup| startup.kind);
        let record = AgentStartupFailure {
            kind,
            detail: crate::logging::error_chain(error.as_ref()),
            at_ms: unix_millis(),
            version: fluxdown_protocol::APP_VERSION.to_owned(),
        };
        let Err(error) = write_record(&self.path, &record).await else {
            return;
        };
        tracing::warn!(
            path = %self.path.display(),
            error = %error,
            "could not write the agent startup failure record"
        );
        if let Some(fallback) = &self.fallback {
            match write_record(fallback, &record).await {
                Ok(()) => tracing::info!(
                    path = %fallback.display(),
                    "agent startup failure record written to the fallback directory"
                ),
                Err(error) => tracing::warn!(
                    path = %fallback.display(),
                    error = %error,
                    "could not write the agent startup failure record to the fallback directory"
                ),
            }
        }
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// 临时文件 + rename 原子替换，读取方不会看到半截 JSON。
async fn write_record(path: &Path, record: &AgentStartupFailure) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(directory).await?;
    let temp = directory.join(format!("{TEMP_PREFIX}{}.tmp", Uuid::new_v4()));
    let result = async {
        tokio::fs::write(&temp, &bytes).await?;
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err()
        && let Err(error) = tokio::fs::remove_file(&temp).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %temp.display(), error = %error, "could not remove the failed startup record temp file");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{StartupError, StartupReport};
    use fluxdown_protocol::local_service::{
        AGENT_STARTUP_FAILURE_FILE, AgentStartupFailure, AgentStartupFailureKind,
    };

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown-startup-failure-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn read_record(dir: &std::path::Path) -> Option<AgentStartupFailure> {
        let bytes = std::fs::read(dir.join(AGENT_STARTUP_FAILURE_FILE)).ok()?;
        Some(serde_json::from_slice(&bytes).expect("valid record"))
    }

    /// 开始服务之前的失败按环节记录完整错误链；未分类的错误记为「其它」。
    #[tokio::test]
    async fn failure_before_serving_records_stage_and_error_chain() {
        let dir = temp_dir("record");
        let report = StartupReport::new(&dir);
        let bind = std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "could not bind the UI gateway on 127.0.0.1:17800",
        );
        let result: crate::runtime::AgentResult = Err(Box::new(StartupError::new(
            AgentStartupFailureKind::GatewayBind,
            bind,
        )));
        report.finish(&result).await;
        let record = read_record(&dir).expect("record written");
        assert_eq!(record.kind, AgentStartupFailureKind::GatewayBind);
        assert!(
            record.detail.contains("127.0.0.1:17800"),
            "{}",
            record.detail
        );
        assert_eq!(record.version, fluxdown_protocol::APP_VERSION);

        let result: crate::runtime::AgentResult = Err("unclassified".into());
        report.finish(&result).await;
        assert_eq!(
            read_record(&dir).expect("record replaced").kind,
            AgentStartupFailureKind::Other
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// 开始服务即清掉旧记录；之后的运行期错误不是启动失败，不再写记录。
    #[tokio::test]
    async fn serving_clears_previous_record_and_suppresses_later_failures() {
        let dir = temp_dir("serving");
        let stale = StartupReport::new(&dir);
        stale.finish(&Err("previous run".into())).await;
        assert!(read_record(&dir).is_some());

        let report = StartupReport::new(&dir);
        report.mark_serving().await;
        assert!(
            read_record(&dir).is_none(),
            "serving removes the stale record"
        );
        report.finish(&Err("runtime failure".into())).await;
        assert!(
            read_record(&dir).is_none(),
            "runtime failures are not recorded"
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
