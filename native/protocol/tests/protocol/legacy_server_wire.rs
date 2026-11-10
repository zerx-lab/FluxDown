#![allow(clippy::unwrap_used, clippy::expect_used)]

use fluxdown_protocol::daemon::{
    BtFileDto, HlsQualityOptionDto, QueueDto, QueuePositionDto, ResolveVariantOptionDto,
    SegmentDetailDto, TaskDto, WsClientMsg, WsServerMsg,
};

#[test]
fn ws_server_msg_uses_type_tag_and_camel_case() {
    let msg = WsServerMsg::TaskProgress {
        task_id: "t1".into(),
        status: 1,
        downloaded_bytes: 10,
        total_bytes: 100,
        speed: 5,
        file_name: "f.bin".into(),
        save_dir: "/tmp".into(),
        upload_speed: 7,
        url: "http://x".into(),
        error_message: String::new(),
        uploaded_bytes: 42,
        seeding_status: 1,
        seeding_message: String::new(),
        seeding_time_secs: 0,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"taskProgress\""));
    assert!(json.contains("\"taskId\":\"t1\""));
    assert!(json.contains("\"downloadedBytes\":10"));
    assert!(json.contains("\"uploadedBytes\":42"));
    assert!(json.contains("\"uploadSpeed\":7"));
    assert!(json.contains("\"seedingStatus\":1"));
}

#[test]
fn ws_server_msg_plugin_hook_activity_uses_camel_case() {
    let msg = WsServerMsg::PluginHookActivity {
        task_id: "t1".into(),
        plugin_id: "p1".into(),
        running: true,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"pluginHookActivity\""));
    assert!(json.contains("\"taskId\":\"t1\""));
    assert!(json.contains("\"pluginId\":\"p1\""));
    assert!(json.contains("\"running\":true"));
}

#[test]
fn ws_client_msg_roundtrip() {
    let msg: WsClientMsg =
        serde_json::from_str(r#"{"type":"hlsSelection","taskId":"t1","selectedIndex":2}"#).unwrap();
    match msg {
        WsClientMsg::HlsSelection {
            task_id,
            selected_index,
        } => {
            assert_eq!(task_id, "t1");
            assert_eq!(selected_index, 2);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
    let ping: WsClientMsg = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
    assert!(matches!(ping, WsClientMsg::Ping {}));
}

#[test]
fn ws_client_msg_select_variant_roundtrip() {
    let msg: WsClientMsg =
        serde_json::from_str(r#"{"type":"selectVariant","taskId":"t4","selectedIndex":1}"#)
            .unwrap();
    match msg {
        WsClientMsg::SelectVariant {
            task_id,
            selected_index,
        } => {
            assert_eq!(task_id, "t4");
            assert_eq!(selected_index, 1);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn ws_client_msg_set_task_seed_limits_roundtrip() {
    let msg: WsClientMsg = serde_json::from_str(
            r#"{"type":"setTaskSeedLimits","taskId":"t5","ratioLimitMilli":1500,"postRatioLimitMilli":-1,"seedTimeLimitMinutes":-2,"inactiveTimeLimitMinutes":30}"#,
        )
        .unwrap();
    match msg {
        WsClientMsg::SetTaskSeedLimits {
            task_id,
            ratio_limit_milli,
            post_ratio_limit_milli,
            seed_time_limit_minutes,
            inactive_time_limit_minutes,
            upload_limit_bps,
        } => {
            assert_eq!(task_id, "t5");
            assert_eq!(ratio_limit_milli, 1500);
            assert_eq!(post_ratio_limit_milli, -1);
            assert_eq!(seed_time_limit_minutes, -2);
            assert_eq!(inactive_time_limit_minutes, 30);
            // 旧客户端不带 uploadLimitBps → serde default 0。
            assert_eq!(upload_limit_bps, 0);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn ws_client_msg_bt_selection_roundtrip_with_indices() {
    let msg: WsClientMsg =
        serde_json::from_str(r#"{"type":"btSelection","taskId":"t2","selectedIndices":[0,2,5]}"#)
            .unwrap();
    match msg {
        WsClientMsg::BtSelection {
            task_id,
            selected_indices,
        } => {
            assert_eq!(task_id, "t2");
            assert_eq!(selected_indices, vec![0, 2, 5]);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn ws_client_msg_bt_selection_empty_array_means_download_all() {
    // Per the `BtSelection` doc comment, an empty array is the wire
    // encoding for "download all files" -- it must deserialize to an
    // empty (not missing/defaulted) `Vec`, distinct from the field
    // being absent from the payload entirely.
    let msg: WsClientMsg =
        serde_json::from_str(r#"{"type":"btSelection","taskId":"t3","selectedIndices":[]}"#)
            .unwrap();
    match msg {
        WsClientMsg::BtSelection {
            task_id,
            selected_indices,
        } => {
            assert_eq!(task_id, "t3");
            assert!(selected_indices.is_empty());
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

fn sample_task_dto(id: &str) -> TaskDto {
    TaskDto {
        task_id: id.to_string(),
        url: "http://example.com/file".into(),
        file_name: "video.mp4".into(),
        save_dir: "/downloads".into(),
        status: 1,
        downloaded_bytes: 10,
        total_bytes: 100,
        error_message: String::new(),
        created_at: "1700000000".into(),
        proxy_url: String::new(),
        queue_id: "q1".into(),
        checksum: String::new(),
        ignore_tls_errors: false,
        file_missing: false,
        completed_at: String::new(),
        file_hash: String::new(),
        referrer: String::new(),
        group_id: String::new(),
        rss_source_id: String::new(),
        origin_url: String::new(),
        auto_route: String::new(),
        source_bytes: Default::default(),
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
    }
}

fn sample_queue_dto(id: &str) -> QueueDto {
    QueueDto {
        queue_id: id.to_string(),
        name: "工作队列".into(),
        speed_limit_kbps: 512,
        upload_limit_kbps: 128,
        max_concurrent: 3,
        default_save_dir: "/downloads/work".into(),
        position: 1,
        default_segments: 4,
        default_user_agent: "FluxDown/1.0".into(),
        is_running: true,
        schedule_enabled: false,
        schedule_start: String::new(),
        schedule_stop: String::new(),
        schedule_days: 127,
    }
}

#[test]
fn ws_server_msg_tasks_snapshot_variant() {
    let msg = WsServerMsg::TasksSnapshot {
        tasks: vec![sample_task_dto("task-1")],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "tasksSnapshot");
    assert_eq!(v["tasks"][0]["taskId"], "task-1");
    assert_eq!(v["tasks"][0]["fileName"], "video.mp4");
    assert_eq!(v["tasks"][0]["downloadedBytes"], 10);
    assert_eq!(v["tasks"][0]["queueId"], "q1");
}

#[test]
fn ws_server_msg_segment_progress_variant() {
    let msg = WsServerMsg::SegmentProgress {
        task_id: "t1".into(),
        total_bytes: 1000,
        segment_count: 2,
        segments: vec![
            SegmentDetailDto {
                index: 0,
                start_byte: 0,
                end_byte: 500,
                downloaded_bytes: 250,
            },
            SegmentDetailDto {
                index: 1,
                start_byte: 500,
                end_byte: 1000,
                downloaded_bytes: 100,
            },
        ],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "segmentProgress");
    assert_eq!(v["taskId"], "t1");
    assert_eq!(v["totalBytes"], 1000);
    assert_eq!(v["segmentCount"], 2);
    assert_eq!(v["segments"][0]["startByte"], 0);
    assert_eq!(v["segments"][0]["endByte"], 500);
    assert_eq!(v["segments"][1]["downloadedBytes"], 100);
}

#[test]
fn ws_server_msg_segment_split_variant() {
    let msg = WsServerMsg::SegmentSplit {
        task_id: "t1".into(),
        parent_index: 0,
        parent_new_end: 400,
        child_index: 2,
        child_start: 400,
        child_end: 800,
        is_proactive: true,
        total_segments: 3,
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "segmentSplit");
    assert_eq!(v["parentIndex"], 0);
    assert_eq!(v["parentNewEnd"], 400);
    assert_eq!(v["childIndex"], 2);
    assert_eq!(v["childStart"], 400);
    assert_eq!(v["childEnd"], 800);
    assert_eq!(v["isProactive"], true);
    assert_eq!(v["totalSegments"], 3);
}

#[test]
fn ws_server_msg_task_meta_probed_variant() {
    let msg = WsServerMsg::TaskMetaProbed {
        task_id: "t1".into(),
        file_name: "movie.mkv".into(),
        total_bytes: 123_456,
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "taskMetaProbed");
    assert_eq!(v["fileName"], "movie.mkv");
    assert_eq!(v["totalBytes"], 123_456);
}

#[test]
fn ws_server_msg_queues_changed_variant() {
    let msg = WsServerMsg::QueuesChanged {
        queues: vec![sample_queue_dto("q1")],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "queuesChanged");
    assert_eq!(v["queues"][0]["queueId"], "q1");
    assert_eq!(v["queues"][0]["speedLimitKbps"], 512);
    assert_eq!(v["queues"][0]["defaultSaveDir"], "/downloads/work");
}

#[test]
fn ws_server_msg_task_queue_changed_variant() {
    let msg = WsServerMsg::TaskQueueChanged {
        task_id: "t1".into(),
        queue_id: "later".into(),
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "taskQueueChanged");
    assert_eq!(v["taskId"], "t1");
    assert_eq!(v["queueId"], "later");
}

#[test]
fn ws_server_msg_queue_positions_changed_variant() {
    let msg = WsServerMsg::QueuePositionsChanged {
        positions: vec![QueuePositionDto {
            task_id: "t1".into(),
            position: 3,
            order: 7,
        }],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "queuePositionsChanged");
    assert_eq!(v["positions"][0]["taskId"], "t1");
    assert_eq!(v["positions"][0]["position"], 3);
    assert_eq!(v["positions"][0]["order"], 7);
}

#[test]
fn ws_server_msg_priority_task_changed_variant() {
    let msg = WsServerMsg::PriorityTaskChanged {
        priority_task_id: "t9".into(),
        auto_paused_count: 4,
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "priorityTaskChanged");
    assert_eq!(v["priorityTaskId"], "t9");
    assert_eq!(v["autoPausedCount"], 4);
}

#[test]
fn ws_server_msg_hls_selection_request_variant() {
    let msg = WsServerMsg::HlsSelectionRequest {
        task_id: "t1".into(),
        options: vec![HlsQualityOptionDto {
            index: 0,
            bandwidth: 5_000_000,
            width: 1920,
            height: 1080,
        }],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "hlsSelectionRequest");
    assert_eq!(v["taskId"], "t1");
    assert_eq!(v["options"][0]["bandwidth"], 5_000_000);
    assert_eq!(v["options"][0]["height"], 1080);
}

#[test]
fn ws_server_msg_bt_selection_request_variant() {
    let msg = WsServerMsg::BtSelectionRequest {
        task_id: "t1".into(),
        files: vec![BtFileDto {
            index: 1,
            path: "folder/video.mp4".into(),
            size: 999,
        }],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "btSelectionRequest");
    assert_eq!(v["files"][0]["path"], "folder/video.mp4");
    assert_eq!(v["files"][0]["size"], 999);
}

#[test]
fn ws_server_msg_resolve_variant_request_variant() {
    let msg = WsServerMsg::ResolveVariantRequest {
        task_id: "t1".into(),
        default_index: 0,
        options: vec![ResolveVariantOptionDto {
            index: 0,
            label: "1080p MP4".into(),
            container: "mp4".into(),
            bandwidth: 5_000_000,
            width: 1920,
            height: 1080,
            total_bytes: 123_456,
        }],
    };
    let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
    assert_eq!(v["type"], "resolveVariantRequest");
    assert_eq!(v["taskId"], "t1");
    assert_eq!(v["defaultIndex"], 0);
    assert_eq!(v["options"][0]["label"], "1080p MP4");
    assert_eq!(v["options"][0]["container"], "mp4");
    assert_eq!(v["options"][0]["totalBytes"], 123_456);
}

#[test]
fn ws_server_msg_pong_variant_has_no_extra_fields() {
    let v: serde_json::Value = serde_json::to_value(&WsServerMsg::Pong {}).unwrap();
    assert_eq!(v, serde_json::json!({ "type": "pong" }));
}
