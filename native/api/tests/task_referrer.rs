//! Repro for issue #111: `TaskDto` must expose the task referrer on the wire.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fluxdown_api::types::TaskDto;
use fluxdown_engine::model::TaskInfo;

#[test]
fn task_dto_json_carries_referrer() {
    let info = TaskInfo {
        task_id: "t1".to_string(),
        url: "https://example.com/f.zip".to_string(),
        file_name: "f.zip".to_string(),
        save_dir: "/tmp".to_string(),
        status: 2,
        downloaded_bytes: 10,
        total_bytes: 100,
        error_message: String::new(),
        created_at: "1700000000".to_string(),
        proxy_url: String::new(),
        queue_id: String::new(),
        checksum: String::new(),
        ignore_tls_errors: false,
        file_missing: false,
        completed_at: String::new(),
        segments: 0,
        queue_order: 0,
        uploaded_bytes: 0,
        uploaded_at_completion: 0,
        seeding_status: 0,
        seeding_message: String::new(),
        seed_ratio_limit_milli: -2,
        seed_post_ratio_limit_milli: -2,
        seed_time_limit_minutes: -2,
        seed_inactive_time_limit_minutes: -2,
        referrer: "https://example.com/page".to_string(),
        group_id: String::new(),
        rss_source_id: String::new(),
        origin_url: String::new(),
        auto_route: String::new(),
    };
    let dto = TaskDto::from(info);
    assert_eq!(dto.referrer, "https://example.com/page");
    let json = serde_json::to_string(&dto).expect("serialize");
    assert!(json.contains(r#""referrer":"https://example.com/page""#));
}
