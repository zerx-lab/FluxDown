//! Opt-in desktop installation while the UI and download engine are idle.

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{AgentSnapshot, UpdateInstallKind, UpdatePhase};
use tokio_util::sync::CancellationToken;

use super::{PREF_AUTO_CHECK, UpdateService, restart};
use crate::shell::ShellState;

const PREF_SILENT_UPDATE: &str = "general.silent_auto_update";
const IDLE_INTERVAL: Duration = Duration::from_secs(30);

pub(super) struct AutomaticInstall {
    pub shell: Arc<ShellState>,
    pub cancel: CancellationToken,
    pub restart: restart::RestartPlan,
}

impl UpdateService {
    /// Desktop-only, memory-only idle checks; never probes the installation or data directory.
    pub(crate) fn start_silent_updates(
        self: &Arc<Self>,
        shell: Arc<ShellState>,
        cancel: CancellationToken,
    ) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            service
                .automatic_updates(shell, cancel, IDLE_INTERVAL)
                .await;
        });
    }

    async fn automatic_updates(
        self: Arc<Self>,
        shell: Arc<ShellState>,
        cancel: CancellationToken,
        interval: Duration,
    ) {
        let mut idle = false;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            let ready = self.lock().status.phase == UpdatePhase::Ready;
            let eligible = ready && self.silent_update_allowed(&shell);
            if eligible && idle {
                // Resolve the original executable BEFORE replacing it (Linux /proc/self/exe
                // otherwise points at the renamed old binary).
                match restart::background_agent() {
                    Ok(restart) => {
                        let mut core = self.lock();
                        if core.status.phase == UpdatePhase::Ready
                            && let Some(candidate) = core.candidate.clone()
                        {
                            core.status.phase = UpdatePhase::Installing;
                            self.commit(&mut core);
                            let service = Arc::clone(&self);
                            let automatic = AutomaticInstall {
                                shell: Arc::clone(&shell),
                                cancel: cancel.clone(),
                                restart,
                            };
                            tokio::spawn(service.install_task(candidate, Some(automatic)));
                        }
                    }
                    Err(error) => tracing::warn!(%error, "cannot plan background update restart"),
                }
                idle = false;
            } else {
                idle = eligible;
            }
        }
    }

    pub(super) fn silent_update_allowed(&self, shell: &ShellState) -> bool {
        let core = self.lock();
        let status = &core.status;
        if !matches!(status.phase, UpdatePhase::Ready | UpdatePhase::Installing)
            || status.manual_reason.is_some()
            || !silent_install_kind(status.install_kind)
            || core.declined_auto.as_deref() == Some(status.latest_version.as_str())
        {
            return false;
        }
        let super::Host::Agent { events, .. } = &self.host else {
            return false;
        };
        shell.ui_clients() == 0
            && events.inspect(|snapshot| {
                snapshot.preferences.values.get(PREF_SILENT_UPDATE)
                    == Some(&serde_json::Value::Bool(true))
                    && snapshot.preferences.values.get(PREF_AUTO_CHECK)
                        != Some(&serde_json::Value::Bool(false))
                    && snapshot
                        .preferences
                        .values
                        .get(super::PREF_CHANNEL)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("stable")
                        == status.channel
                    && snapshot
                        .preferences
                        .values
                        .get(fluxdown_protocol::update_prompt::SKIPPED_VERSION_KEY)
                        .and_then(serde_json::Value::as_str)
                        != Some(status.latest_version.as_str())
                    && engine_idle(snapshot)
            })
    }

    pub(super) fn background_idle(&self, shell: &ShellState) -> bool {
        let super::Host::Agent { events, .. } = &self.host else {
            return false;
        };
        shell.ui_clients() == 0 && events.inspect(engine_idle)
    }
}

fn silent_install_kind(kind: UpdateInstallKind) -> bool {
    matches!(
        kind,
        UpdateInstallKind::WindowsSetup
            | UpdateInstallKind::WindowsPortable
            | UpdateInstallKind::MacosApp
            | UpdateInstallKind::LinuxAppImage
            | UpdateInstallKind::LinuxPortable
    )
}

fn engine_idle(snapshot: &AgentSnapshot) -> bool {
    let daemon = &snapshot.daemon;
    let stats = &daemon.runtime_stats;
    snapshot.daemon_connected
        && snapshot.shell.resident
        && daemon.task_total.is_none()
        && snapshot.pending_captures.is_empty()
        && daemon.pending_selections.is_empty()
        && stats.active_tasks == 0
        && stats.pending_tasks == 0
        && stats.retry_pending_tasks == 0
        && !daemon
            .tasks
            .iter()
            .any(|task| !matches!(task.status, 2..=4) || matches!(task.seeding_status, 1 | 8))
}

impl AutomaticInstall {
    // A capture or remote task can arrive while files are being staged. Recheck before
    // replacement; after replacement, defer shutdown again instead of interrupting it.
    pub(super) async fn wait_for_idle(&self, service: &UpdateService) -> bool {
        while !service.background_idle(&self.shell) {
            tokio::select! {
                () = self.cancel.cancelled() => return false,
                () = tokio::time::sleep(IDLE_INTERVAL) => {}
            }
        }
        !self.cancel.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{
        AgentEvent, AgentSnapshot, ServiceEvent, TaskDto, TrayUnavailableReason,
    };

    use super::*;
    use crate::daemon_client::DaemonClient;
    use crate::egress::Egress;
    use crate::event_hub::AgentEventHub;
    use crate::shell::TrayAvailability;
    use crate::update::{Candidate, InstallTarget, UpdateParts, install, release};

    fn idle_snapshot() -> AgentSnapshot {
        let mut snapshot = AgentSnapshot {
            daemon_connected: true,
            ..AgentSnapshot::default()
        };
        snapshot.shell.resident = true;
        snapshot
    }

    fn task(status: i32, seeding_status: i32) -> TaskDto {
        serde_json::from_value(serde_json::json!({
            "taskId": "task", "url": "magnet:?xt=urn:btih:example",
            "fileName": "example", "saveDir": "/downloads", "status": status,
            "downloadedBytes": 0, "totalBytes": 1, "progress": 0.0,
            "errorMessage": "", "createdAt": "", "proxyUrl": "",
            "queueId": "main", "checksum": "",
            "seedingStatus": seeding_status,
        }))
        .unwrap()
    }

    #[test]
    fn queued_retrying_and_seeding_work_is_not_idle_even_without_traffic() {
        let idle = idle_snapshot();
        assert!(engine_idle(&idle));
        for (status, seeding) in [(0, 0), (1, 0), (5, 0), (3, 1), (3, 8), (99, 0)] {
            let mut snapshot = idle.clone();
            snapshot.daemon.tasks.push(task(status, seeding));
            assert!(!engine_idle(&snapshot), "{status}/{seeding}");
        }
        for status in [2, 3, 4] {
            let mut snapshot = idle.clone();
            snapshot.daemon.tasks.push(task(status, 0));
            assert!(engine_idle(&snapshot), "{status}");
        }
        let mut retry = idle.clone();
        retry.daemon.runtime_stats.retry_pending_tasks = 1;
        assert!(!engine_idle(&retry));
        let mut disconnected = idle.clone();
        disconnected.daemon_connected = false;
        assert!(!engine_idle(&disconnected));
        let mut loading = idle;
        loading.daemon.task_total = Some(1);
        assert!(!engine_idle(&loading));
    }

    fn fixture(dir: &std::path::Path) -> (Arc<UpdateService>, Arc<ShellState>, AgentEventHub) {
        let events = AgentEventHub::new(idle_snapshot());
        let shell = ShellState::new(
            TrayAvailability::Unavailable(TrayUnavailableReason::NotBuilt),
            Arc::new(DaemonClient::disconnected()),
            events.clone(),
        );
        let service = Arc::new(UpdateService::with_version(
            UpdateParts {
                events: events.clone(),
                data_dir: dir.to_path_buf(),
                target: InstallTarget {
                    kind: UpdateInstallKind::WindowsPortable,
                    component: install::ReleaseComponent::Desktop,
                    asset_keys: Vec::new(),
                    manual_reason: None,
                },
                egress: Egress::system_default(),
                request_restart: Box::new(|| panic!("invalid package must not restart")),
            },
            "1.0.0",
            release::Endpoint::with_bases(&["http://127.0.0.1:9"], true, Duration::ZERO),
        ));
        service.mutate(|core| {
            core.status.phase = UpdatePhase::Ready;
            core.status.has_update = true;
            core.status.latest_version = "2.0.0".to_owned();
            core.status.channel = "stable".to_owned();
            core.candidate = Some(Candidate {
                version: "2.0.0".to_owned(),
                tag: "v2.0.0".to_owned(),
                asset_name: "missing.zip".to_owned(),
                size: 1,
                sha256: "0".repeat(64),
            });
        });
        (service, shell, events)
    }

    fn preferences(events: &AgentEventHub, values: serde_json::Value) {
        let mut prefs = events.inspect(|snapshot| snapshot.preferences.clone());
        prefs.values = serde_json::from_value(values).unwrap();
        events.publish(AgentEvent::PreferencesChanged(prefs));
    }

    #[tokio::test]
    async fn automatic_install_requires_local_consent_and_closed_ui() {
        let dir = std::env::temp_dir().join(format!("fluxdown-auto-{}", uuid::Uuid::new_v4()));
        let (service, shell, events) = fixture(&dir);
        assert!(!service.silent_update_allowed(&shell), "default is opt-out");
        preferences(&events, serde_json::json!({PREF_SILENT_UPDATE: true}));
        assert!(service.silent_update_allowed(&shell));
        shell.ui_connected().await;
        assert!(
            !service.silent_update_allowed(&shell),
            "visible UI prevents install"
        );
        shell.ui_disconnected(false).await;
        assert!(service.silent_update_allowed(&shell));
        for values in [
            serde_json::json!({PREF_SILENT_UPDATE: true, PREF_AUTO_CHECK: false}),
            serde_json::json!({PREF_SILENT_UPDATE: true, (super::super::PREF_CHANNEL): "frontier"}),
            serde_json::json!({PREF_SILENT_UPDATE: true, (fluxdown_protocol::update_prompt::SKIPPED_VERSION_KEY): "2.0.0"}),
        ] {
            preferences(&events, values);
            assert!(!service.silent_update_allowed(&shell));
        }
        preferences(&events, serde_json::json!({PREF_SILENT_UPDATE: true}));
        for kind in [
            UpdateInstallKind::LinuxDeb,
            UpdateInstallKind::LinuxArch,
            UpdateInstallKind::AndroidApk,
            UpdateInstallKind::ServerBinary,
        ] {
            service.mutate(|core| core.status.install_kind = kind);
            assert!(!service.silent_update_allowed(&shell), "{kind:?}");
        }
        assert!(!dir.exists(), "eligibility must not touch disk");
    }

    #[tokio::test]
    async fn automatic_install_failure_is_reported_and_keeps_the_service_running() {
        let dir = std::env::temp_dir().join(format!("fluxdown-auto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("updates/2.0.0")).unwrap();
        let (service, shell, events) = fixture(&dir);
        preferences(&events, serde_json::json!({PREF_SILENT_UPDATE: true}));
        let cancel = CancellationToken::new();
        let worker = tokio::spawn(Arc::clone(&service).automatic_updates(
            shell,
            cancel.clone(),
            Duration::from_millis(1),
        ));
        let mut receiver = events.subscribe();
        let mut installing = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = receiver.recv().await.unwrap();
                if let ServiceEvent::Agent(AgentEvent::UpdateChanged(status)) = &frame.event {
                    installing |= status.phase == UpdatePhase::Installing;
                    if status.phase == UpdatePhase::Failed {
                        assert!(status.failure.is_some());
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(installing, "idle opted-in package must be attempted");
        assert!(!dir.join("updates/pending.json").exists());
        cancel.cancel();
        worker.await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
