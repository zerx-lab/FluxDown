//! 本机服务（`fluxdown-agent`）拉起失败的可展示描述，以及 agent 启动失败记录的读取。

use std::path::Path;

use fluxdown_protocol::local_service::{AGENT_STARTUP_FAILURE_FILE, AgentStartupFailure};

/// 启动失败记录与本机拉起时刻之间允许的时钟误差：记录由 agent 用墙钟写入、拉起时刻由桌面
/// 用墙钟取得，同机但可能被对时微调。
const STARTUP_FAILURE_CLOCK_SKEW_MS: u64 = 2_000;

/// agent 无法被拉起 / 反复启动失败；界面据此展示原因与处理建议。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceFailure {
    /// 无法定位或拉起 agent 可执行文件（缺失、被安全软件隔离、无权限）。
    Launch { program: String, detail: String },
    /// agent 反复快速异常退出（崩溃循环）。
    Exited {
        /// 最近一次退出的可读状态（含退出码）。
        status: String,
        /// agent 自己写下的启动失败记录；读不到或早于本轮拉起时为 `None`。
        startup: Option<AgentStartupFailure>,
    },
}

/// 记录是否属于本轮拉起：不早于触发崩溃判定的那次拉起（允许时钟误差）。
fn startup_failure_is_current(record_at_ms: u64, spawned_at_ms: u64) -> bool {
    record_at_ms.saturating_add(STARTUP_FAILURE_CLOCK_SKEW_MS) >= spawned_at_ms
}

/// 读取 `bearer_path` 同目录下的 agent 启动失败记录；只采用不早于 `spawned_at_ms` 的记录。
/// 该目录本身读不了时（Windows 上目录 ACL 属于旧账户 SID 等），agent 把记录写在
/// `local_service::local_fallback_dir`，这里随后读那一份。
/// 文件不存在属正常（agent 从未走到写记录的环节，如被系统直接杀死）；读不到或解析失败只记日志。
pub(crate) async fn read_startup_failure(
    bearer_path: &Path,
    spawned_at_ms: u64,
) -> Option<AgentStartupFailure> {
    let directory = bearer_path.parent().unwrap_or_else(|| Path::new("."));
    let primary = directory.join(AGENT_STARTUP_FAILURE_FILE);
    let fallback = fluxdown_protocol::local_service::local_fallback_dir(directory)
        .map(|dir| dir.join(AGENT_STARTUP_FAILURE_FILE));
    let mut found = None;
    for path in std::iter::once(primary).chain(fallback) {
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                found = Some((path, bytes));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "could not read agent startup failure record {}: {error:#}",
                path.display()
            ),
        }
    }
    let (path, bytes) = found?;
    let record = match serde_json::from_slice::<AgentStartupFailure>(&bytes) {
        Ok(record) => record,
        Err(error) => {
            log::warn!(
                "could not parse agent startup failure record {}: {error:#}",
                path.display()
            );
            return None;
        }
    };
    if startup_failure_is_current(record.at_ms, spawned_at_ms) {
        Some(record)
    } else {
        log::debug!(
            "ignoring stale agent startup failure record (at {} ms, spawn at {spawned_at_ms} ms)",
            record.at_ms
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fluxdown_protocol::local_service::{
        AGENT_STARTUP_FAILURE_FILE, AgentStartupFailure, AgentStartupFailureKind,
    };

    use super::{read_startup_failure, startup_failure_is_current};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "fluxdown-startup-failure-{label}-{}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create startup failure fixture");
            Self(path)
        }

        fn bearer(&self) -> PathBuf {
            self.0.join("agent.token")
        }

        fn record(&self, at_ms: u64) {
            let record = AgentStartupFailure {
                kind: AgentStartupFailureKind::GatewayBind,
                detail: "address in use".to_owned(),
                at_ms,
                version: "0.0.0".to_owned(),
            };
            std::fs::write(
                self.0.join(AGENT_STARTUP_FAILURE_FILE),
                serde_json::to_vec(&record).expect("serialize record"),
            )
            .expect("write record");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("could not remove startup failure fixture: {error}");
            }
        }
    }

    #[test]
    fn records_older_than_the_spawn_beyond_clock_skew_are_stale() {
        assert!(startup_failure_is_current(10_000, 10_000));
        assert!(startup_failure_is_current(20_000, 10_000));
        assert!(startup_failure_is_current(8_000, 10_000));
        assert!(!startup_failure_is_current(7_999, 10_000));
        assert!(startup_failure_is_current(0, 0));
    }

    #[tokio::test]
    async fn only_a_record_from_this_spawn_is_adopted() {
        let fixture = Fixture::new("freshness");
        assert_eq!(read_startup_failure(&fixture.bearer(), 50_000).await, None);

        fixture.record(10_000);
        assert_eq!(
            read_startup_failure(&fixture.bearer(), 50_000).await,
            None,
            "a leftover record from an earlier run must not be blamed"
        );
        let adopted = read_startup_failure(&fixture.bearer(), 9_000)
            .await
            .expect("record newer than the spawn");
        assert_eq!(adopted.kind, AgentStartupFailureKind::GatewayBind);
        assert_eq!(adopted.detail, "address in use");
    }

    #[tokio::test]
    async fn unparsable_record_is_ignored() {
        let fixture = Fixture::new("corrupt");
        std::fs::write(fixture.0.join(AGENT_STARTUP_FAILURE_FILE), "{broken")
            .expect("write corrupt record");
        assert_eq!(read_startup_failure(&fixture.bearer(), 0).await, None);
    }
}
