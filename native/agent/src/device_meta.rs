//! 设备元数据上报：把本机默认下载目录 / 路径风格 / 版本经 `PATCH /devices/current` 告知 FluxCloud，
//! 让其他设备下发任务时知道目标目录该怎么写。
//!
//! 触发：启动时已登录、登录成功、daemon 的 `default_save_dir` 变化（含 daemon 重连）。
//! 旧版云端没有该端点（404）：视为已上报并忽略；其他失败按固定间隔重试。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{AgentEvent, DaemonEvent, PathStyle, ServiceEvent};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::cloud::CloudApi;
use crate::event_hub::AgentEventHub;

const RETRY_DELAY: Duration = Duration::from_secs(30);
/// 合并配置连续变化（例如用户正在输入目录）。
const DEBOUNCE: Duration = Duration::from_millis(500);
const DEFAULT_SAVE_DIR_KEY: &str = "default_save_dir";

/// 上报内容；任何字段变化都需要重新上报。
#[derive(Clone, Debug, Eq, PartialEq)]
struct DeviceMeta {
    default_save_dir: Option<String>,
    path_style: PathStyle,
    app_version: &'static str,
}

impl DeviceMeta {
    fn body(&self) -> Value {
        json!({
            "defaultSaveDir": self.default_save_dir,
            "pathStyle": self.path_style,
            "appVersion": self.app_version,
        })
    }
}

pub struct DeviceMetaService {
    cloud: CloudApi,
    events: AgentEventHub,
}

impl DeviceMetaService {
    #[must_use]
    pub fn new(cloud: CloudApi, events: AgentEventHub) -> Self {
        Self { cloud, events }
    }

    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let (mut events, _) = self.events.subscribe_and_snapshot();
        // (账号, 已上报内容)：换号 / 登出后重新上报。
        let mut reported: Option<(String, DeviceMeta)> = None;
        loop {
            let mut retry = false;
            match self.cloud.current_user_id().await {
                Some(uid) => {
                    let meta = self.current_meta();
                    if reported.as_ref() != Some(&(uid.clone(), meta.clone())) {
                        match self.report(&meta).await {
                            Ok(()) => reported = Some((uid, meta)),
                            Err(error) => {
                                tracing::warn!(error = %error, "device metadata report failed; will retry");
                                retry = true;
                            }
                        }
                    }
                }
                None => reported = None,
            }
            let delay = retry.then_some(RETRY_DELAY);
            tokio::select! {
                _ = cancel.cancelled() => return,
                () = retry_sleep(delay) => {}
                () = wait_relevant_event(&mut events) => {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(DEBOUNCE) => {}
                    }
                }
            }
        }
    }

    fn current_meta(&self) -> DeviceMeta {
        let configured = self.events.inspect(|snapshot| {
            snapshot
                .daemon
                .config
                .values
                .get(DEFAULT_SAVE_DIR_KEY)
                .cloned()
        });
        DeviceMeta {
            default_save_dir: effective_default_dir(configured.as_deref(), os_download_dir),
            path_style: PathStyle::current(),
            app_version: fluxdown_protocol::APP_VERSION,
        }
    }

    async fn report(&self, meta: &DeviceMeta) -> Result<(), crate::cloud::CloudError> {
        match self.cloud.patch_current_device(&meta.body()).await {
            Ok(_) => Ok(()),
            // 旧版云端没有该端点。
            Err(error) if error.status == Some(404) => {
                tracing::debug!(
                    "FluxCloud has no PATCH /devices/current; skipping device metadata"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

/// 本机默认下载目录：daemon 配置优先；未配置时用系统「下载」目录（与 daemon 建任务时的兜底一致）。
fn effective_default_dir(
    configured: Option<&str>,
    os_default: impl FnOnce() -> Option<String>,
) -> Option<String> {
    configured
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(str::to_owned)
        .or_else(os_default)
}

fn os_download_dir() -> Option<String> {
    directories::UserDirs::new().and_then(|dirs| {
        dirs.download_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
    })
}

/// 等到下一个可能改变上报内容的事件；接收端 lag 时也返回（重新计算即可）。
async fn wait_relevant_event(events: &mut broadcast::Receiver<Arc<fluxdown_protocol::EventFrame>>) {
    loop {
        match events.recv().await {
            Ok(frame) => {
                if let ServiceEvent::Agent(event) = &frame.event
                    && matches!(
                        event,
                        AgentEvent::SessionChanged(_)
                            | AgentEvent::DaemonSnapshotReplaced(_)
                            | AgentEvent::DaemonConnectionChanged(true)
                            | AgentEvent::Daemon(DaemonEvent::ConfigChanged(_))
                    )
                {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => return,
            Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
        }
    }
}

async fn retry_sleep(delay: Option<Duration>) {
    match delay {
        Some(delay) => tokio::time::sleep(delay).await,
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::patch;
    use fluxdown_protocol::{AgentSnapshot, DaemonConfigSnapshot, DaemonEvent, DaemonSnapshot};
    use serde_json::{Value, json};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::{DeviceMetaService, effective_default_dir};
    use crate::state::{AgentState, CloudCredentials, StateStore};

    #[test]
    fn configured_directory_wins_and_blank_falls_back_to_os_default() {
        assert_eq!(
            effective_default_dir(Some(" /data/dl "), || Some("/home/u/Downloads".into())),
            Some("/data/dl".to_owned())
        );
        assert_eq!(
            effective_default_dir(Some("  "), || Some("/home/u/Downloads".into())),
            Some("/home/u/Downloads".to_owned())
        );
        assert_eq!(effective_default_dir(None, || None), None);
    }

    type Reports = Arc<Mutex<Vec<Value>>>;

    async fn record(
        State(reports): State<Reports>,
        axum::Json(body): axum::Json<Value>,
    ) -> StatusCode {
        reports.lock().await.push(body);
        StatusCode::OK
    }

    async fn reports_reach(reports: &Reports, count: usize) -> Vec<Value> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = reports.lock().await.clone();
                if current.len() >= count {
                    return current;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("device metadata reports arrive")
    }

    #[tokio::test]
    async fn reports_on_login_and_again_when_default_save_dir_changes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock cloud");
        let address = listener.local_addr().expect("mock address");
        let reports: Reports = Arc::default();
        let app = Router::new()
            .route("/api/v1/devices/current", patch(record))
            .with_state(reports.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_device_meta_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let session = serde_json::from_value(json!({
            "user": { "id": "u1", "email": "user@example.com" },
            "device": { "id": "row1", "deviceId": "device1" }
        }))
        .expect("session dto");
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            credentials: Some(CloudCredentials {
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                expires_at_unix: i64::MAX,
                session: Some(session),
            }),
            ..AgentState::default()
        }));
        let cloud = crate::cloud::CloudApi::new(
            crate::cloud::CloudClient::new(format!("http://{address}"), state, store.clone())
                .expect("cloud client"),
        );
        let events = crate::event_hub::AgentEventHub::new(AgentSnapshot {
            daemon: DaemonSnapshot {
                config: DaemonConfigSnapshot {
                    revision: 1,
                    values: [("default_save_dir".to_owned(), "/srv/first".to_owned())].into(),
                },
                ..DaemonSnapshot::default()
            },
            ..AgentSnapshot::default()
        });
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(
            Arc::new(DeviceMetaService::new(cloud, events.clone())).run(cancel.clone()),
        );

        let first = reports_reach(&reports, 1).await;
        assert_eq!(first[0]["defaultSaveDir"], "/srv/first");
        assert_eq!(first[0]["appVersion"], fluxdown_protocol::APP_VERSION);
        assert!(matches!(
            first[0]["pathStyle"].as_str(),
            Some("windows" | "posix")
        ));

        events.publish(fluxdown_protocol::AgentEvent::Daemon(
            DaemonEvent::ConfigChanged(DaemonConfigSnapshot {
                revision: 2,
                values: [("default_save_dir".to_owned(), "/srv/second".to_owned())].into(),
            }),
        ));
        let second = reports_reach(&reports, 2).await;
        assert_eq!(second[1]["defaultSaveDir"], "/srv/second");

        // 内容没变的无关事件不会重复上报。
        events.publish(fluxdown_protocol::AgentEvent::DaemonConnectionChanged(true));
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        assert_eq!(reports.lock().await.len(), 2);

        cancel.cancel();
        worker.await.expect("join worker");
        server.abort();
        match server.await {
            Ok(result) => result.expect("device metadata mock server completes successfully"),
            Err(error) if error.is_cancelled() => {
                tracing::debug!("device metadata mock server stopped")
            }
            Err(error) => panic!("device metadata mock server panicked: {error}"),
        }
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), %error, "device metadata test cleanup failed");
        }
    }
}
