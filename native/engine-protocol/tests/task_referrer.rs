//! `TaskDto` 必须在 wire 上保留任务来源页与加速来源累计。

use fluxdown_engine::model::{SourceBytes, TaskInfo};
use fluxdown_engine::transfer_activity::TaskRuntime;

#[test]
fn task_dto_json_carries_referrer() -> Result<(), serde_json::Error> {
    let info = TaskInfo {
        task_id: "t1".to_owned(),
        url: "https://example.com/f.zip".to_owned(),
        file_name: "f.zip".to_owned(),
        save_dir: "/tmp".to_owned(),
        status: 2,
        downloaded_bytes: 10,
        total_bytes: 100,
        error_message: String::new(),
        created_at: "1700000000".to_owned(),
        proxy_url: String::new(),
        queue_id: String::new(),
        checksum: String::new(),
        ignore_tls_errors: false,
        file_missing: false,
        completed_at: String::new(),
        file_hash: String::new(),
        segments: 0,
        queue_order: 0,
        uploaded_bytes: 0,
        uploaded_at_completion: 0,
        seeding_status: 0,
        seeding_message: String::new(),
        seeding_time_secs: 0,
        seed_ratio_limit_milli: -2,
        seed_post_ratio_limit_milli: -2,
        seed_time_limit_minutes: -2,
        seed_inactive_time_limit_minutes: -2,
        seed_upload_limit_bps: 2048,
        bt_sequential: true,
        bt_first_last_pieces_first: false,
        bt_download_limit_bps: 4096,
        bt_super_seeding: true,
        referrer: "https://example.com/page".to_owned(),
        group_id: String::new(),
        rss_source_id: String::new(),
        origin_url: String::new(),
        auto_route: String::new(),
        source_bytes: SourceBytes::default(),
    };
    let dto = fluxdown_engine_protocol::task_info_to_dto(info);
    assert_eq!(dto.referrer, "https://example.com/page");
    assert_eq!(dto.seed_upload_limit_bps, 2048);
    assert!(dto.bt_sequential && !dto.bt_first_last_pieces_first);
    let json = serde_json::to_string(&dto)?;
    assert!(json.contains(r#""referrer":"https://example.com/page""#));
    assert!(json.contains(r#""seedUploadLimitBps":2048"#));
    assert!(json.contains(r#""btSequential":true"#));
    assert!(!json.contains("btFirstLastPiecesFirst"));
    assert!(json.contains(r#""btDownloadLimitBps":4096"#));
    assert!(json.contains(r#""btSuperSeeding":true"#));
    Ok(())
}

#[test]
fn task_dto_json_carries_persisted_source_bytes() -> Result<(), serde_json::Error> {
    let info = TaskInfo {
        task_id: "t2".to_owned(),
        url: String::new(),
        file_name: String::new(),
        save_dir: String::new(),
        status: 1,
        downloaded_bytes: 0,
        total_bytes: 0,
        error_message: String::new(),
        created_at: String::new(),
        proxy_url: String::new(),
        queue_id: String::new(),
        checksum: String::new(),
        ignore_tls_errors: false,
        file_missing: false,
        completed_at: String::new(),
        file_hash: String::new(),
        segments: 0,
        queue_order: 0,
        uploaded_bytes: 0,
        uploaded_at_completion: 0,
        seeding_status: 0,
        seeding_message: String::new(),
        seeding_time_secs: 0,
        seed_ratio_limit_milli: -2,
        seed_post_ratio_limit_milli: -2,
        seed_time_limit_minutes: -2,
        seed_inactive_time_limit_minutes: -2,
        seed_upload_limit_bps: 0,
        bt_sequential: false,
        bt_first_last_pieces_first: false,
        bt_download_limit_bps: 0,
        bt_super_seeding: false,
        referrer: String::new(),
        group_id: String::new(),
        rss_source_id: String::new(),
        origin_url: String::new(),
        auto_route: String::new(),
        source_bytes: SourceBytes {
            cdn: 1_000,
            proxy: 200,
            nic: 30,
        },
    };
    let dto = fluxdown_engine_protocol::task_info_to_dto(info);
    assert_eq!(
        (
            dto.source_bytes.cdn_bytes,
            dto.source_bytes.proxy_bytes,
            dto.source_bytes.nic_bytes
        ),
        (1_000, 200, 30)
    );
    let json = serde_json::to_string(&dto)?;
    assert!(json.contains(r#""sourceBytes":{"cdnBytes":1000,"proxyBytes":200,"nicBytes":30}"#));
    Ok(())
}

#[test]
fn runtime_dto_distinguishes_no_attribution_from_zero_attribution() -> Result<(), serde_json::Error>
{
    let unattributed = fluxdown_engine_protocol::task_runtime_to_dto(TaskRuntime::default());
    assert_eq!(unattributed.source_bytes, None);
    assert!(serde_json::to_string(&unattributed)?.contains(r#""sourceBytes":null"#));

    let attributed = fluxdown_engine_protocol::task_runtime_to_dto(TaskRuntime {
        source_bytes: Some(SourceBytes {
            cdn: 5,
            proxy: 0,
            nic: 9,
        }),
        ..TaskRuntime::default()
    });
    assert_eq!(
        attributed.source_bytes,
        Some(fluxdown_protocol::TaskSourceBytesDto {
            cdn_bytes: 5,
            proxy_bytes: 0,
            nic_bytes: 9,
        })
    );
    Ok(())
}
