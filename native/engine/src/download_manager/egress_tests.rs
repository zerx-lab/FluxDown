//! 站点代理规则在任务级出口决策（`task_http_context`）与 Auto 故障转移上的行为。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use super::{
    ActiveTaskEntry, BtConfig, Db, DownloadManager, DownloadManagerConfig, EngineEvent, EventSink,
    MAIN_QUEUE_ID, NewTaskSpec, ProxyConfig, TaskDone, TaskDonePhase,
};
use crate::pac_route::PacRouting;

#[derive(Default)]
struct NullSink(Mutex<Vec<EngineEvent>>);

impl EventSink for NullSink {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().expect("event sink lock").push(event);
    }
}

async fn fixture(config: &[(&str, &str)]) -> (Db, DownloadManager) {
    let db = Db::connect("sqlite::memory:")
        .await
        .expect("memory database");
    db.seed_builtin_queues().await.expect("builtin queues");
    let map: HashMap<String, String> = config
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let manager = DownloadManager::new(
        db.clone(),
        DownloadManagerConfig {
            max_concurrent: 5,
            speed_limit_bps: 0,
            upload_limit_bps: 0,
            default_save_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            app_data_dir: String::new(),
            data_dir: std::env::temp_dir(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::from_config_map(&map),
            user_agent: String::new(),
        },
        Arc::new(NullSink::default()),
        Arc::new(crate::NoopSelection),
    )
    .expect("download manager");
    (db, manager)
}

const RULE: &str = r#"[{"id":"a","match":"regex","pattern":"/dl/","action":"custom","proxy":"http://127.0.0.1:9"}]"#;

#[tokio::test]
async fn matching_rule_decides_the_task_egress_in_mode_none() {
    let (_db, manager) = fixture(&[("proxy_mode", "none"), ("proxy_rules", RULE)]).await;
    let egress =
        manager.task_http_context("https://a.test/dl/x", "", "", MAIN_QUEUE_ID, false, None);
    assert_eq!(egress.route, "rule:custom");
    assert_eq!(egress.proxy.port, 9);
    assert_eq!(egress.proxy.host, "127.0.0.1");
    let router = egress.router.expect("router");
    assert_eq!(router.base_rule(), Some(0));
    assert_eq!(egress.pac, PacRouting::Off);
    assert!(egress.auto_ctx.is_none());
}

#[tokio::test]
async fn unmatched_url_in_auto_mode_keeps_the_auto_decision_and_has_no_base_rule() {
    let (_db, manager) = fixture(&[
        ("proxy_mode", "auto"),
        ("proxy_type", "http"),
        ("proxy_host", "127.0.0.1"),
        ("proxy_port", "9"),
        ("proxy_rules", RULE),
    ])
    .await;
    let egress = manager.task_http_context("https://b.test/x", "", "", MAIN_QUEUE_ID, false, None);
    assert!(!crate::auto_proxy::route::is_rule_label(egress.route));
    let router = egress.router.expect("router");
    assert_eq!(router.base_rule(), None);
    assert_eq!(egress.pac, PacRouting::Auto);
}

#[tokio::test]
async fn task_level_proxy_ignores_rules_entirely() {
    let (_db, manager) = fixture(&[("proxy_mode", "none"), ("proxy_rules", RULE)]).await;
    let egress = manager.task_http_context(
        "https://a.test/dl/x",
        "direct://",
        "",
        MAIN_QUEUE_ID,
        false,
        None,
    );
    assert!(egress.router.is_none());
    assert_eq!(egress.route, crate::auto_proxy::route::TASK_DIRECT);
}

async fn failover_pending_after_transport_error(auto_route: &str) -> bool {
    let (db, mut manager) = fixture(&[
        ("proxy_mode", "auto"),
        ("proxy_type", "http"),
        ("proxy_host", "127.0.0.1"),
        ("proxy_port", "9"),
    ])
    .await;
    let id = manager
        .create_task(NewTaskSpec {
            url: "https://failover.test/file.bin".into(),
            save_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            file_name: "file.bin".into(),
            hint_file_size: 1,
            start_paused: true,
            ..Default::default()
        })
        .await
        .expect("task");
    db.update_task_status(&id, 4, "error sending request: connection refused")
        .await
        .expect("status");
    db.set_task_auto_route(&id, auto_route)
        .await
        .expect("route label");
    manager.active_tasks.insert(
        id.clone(),
        ActiveTaskEntry {
            token: CancellationToken::new(),
            generation: 1,
            handle: None,
            is_bt: false,
            queue_id: MAIN_QUEUE_ID.into(),
        },
    );
    manager
        .handle_task_done(&TaskDone {
            task_id: id.clone(),
            generation: 1,
            reserved_temp_path: None,
            phase: TaskDonePhase::Finished,
        })
        .await;
    manager.auto_failover_pending.contains_key(&id)
}

#[tokio::test]
async fn rule_routed_task_is_not_failed_over_by_auto() {
    assert!(
        !failover_pending_after_transport_error("rule:custom").await,
        "a rule-decided exit must not be retried through another path"
    );
    if crate::route_health::network_reachable() {
        assert!(
            failover_pending_after_transport_error("direct").await,
            "control: a plain Auto direct task is failed over to the proxy"
        );
    }
}
