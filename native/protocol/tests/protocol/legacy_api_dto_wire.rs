#![allow(clippy::unwrap_used, clippy::expect_used)]

use fluxdown_protocol::daemon::{
    ComponentInstallParams, ComponentKind, CreateTaskRequest, DownloadRequest, QueueDto,
    RequestBody, TaskDto,
};

struct DeserCase {
    name: &'static str,
    json: &'static str,
    check: fn(&DownloadRequest),
}

/// Native Messaging 下载请求的反序列化兼容测试：
/// 浏览器扩展 / 油猴脚本发来的 wire JSON 必须精确映射到字段。
#[test]
fn download_request_deserializes_wire_fields() {
    let cases = [
        DeserCase {
            name: "full payload with headers",
            json: r#"{
                    "url": "https://example.com/file.zip",
                    "filename": "file.zip",
                    "referrer": "https://example.com/",
                    "cookies": "session=abc123",
                    "headers": {"Authorization": "Bearer token123", "X-Custom": "value"},
                    "fileSize": 1024,
                    "mimeType": "application/zip"
                }"#,
            check: |req| {
                assert_eq!(req.url, "https://example.com/file.zip");
                assert_eq!(req.filename, "file.zip");
                assert_eq!(req.referrer, "https://example.com/");
                assert_eq!(req.cookies, "session=abc123");
                let headers = req.headers.as_ref().unwrap();
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer token123");
                assert_eq!(headers.get("X-Custom").unwrap(), "value");
                assert_eq!(req.file_size, Some(1024));
                assert_eq!(req.mime_type.as_deref(), Some("application/zip"));
            },
        },
        DeserCase {
            name: "minimal payload omits optional fields",
            json: r#"{"url": "https://example.com/file.zip"}"#,
            check: |req| {
                assert!(req.headers.is_none());
                assert_eq!(req.cookies, "");
                assert_eq!(req.referrer, "");
                assert_eq!(req.file_size, None);
            },
        },
        DeserCase {
            name: "empty headers object deserializes to Some(empty map)",
            json: r#"{"url": "https://example.com/file.zip", "headers": {}}"#,
            check: |req| {
                assert!(req.headers.as_ref().unwrap().is_empty());
            },
        },
        DeserCase {
            name: "fileSize -1 marks skip-probe hint",
            json: r#"{"url": "https://x/y", "cookies": "session=abc", "fileSize": -1}"#,
            check: |req| {
                assert_eq!(req.file_size, Some(-1));
                assert_eq!(req.cookies, "session=abc");
            },
        },
        DeserCase {
            name: "embedded newline in url survives round trip (batch join format)",
            json: r#"{"url": "https://a.com/1.zip\nhttps://b.com/2.zip"}"#,
            check: |req| {
                let urls: Vec<&str> = req.url.split('\n').collect();
                assert_eq!(urls, ["https://a.com/1.zip", "https://b.com/2.zip"]);
            },
        },
    ];

    for case in cases {
        let req: DownloadRequest = serde_json::from_str(case.json)
            .unwrap_or_else(|e| panic!("case `{}` failed to parse: {e}", case.name));
        (case.check)(&req);
    }
}

/// 扩展/接管入口透传的浏览器请求事务字段：`method`/`body`/`audioUrl`
/// 必须按 camelCase wire 名精确落到 [`CreateTaskRequest`]，且缺省安全。
#[test]
fn create_task_request_deserializes_browser_transaction_fields() {
    let req: CreateTaskRequest = serde_json::from_str(
        r#"{
                "url": "https://example.com/dl",
                "method": "POST",
                "body": {"kind": "raw", "bytesB64": "aGk=", "contentType": "text/plain"},
                "audioUrl": "https://example.com/audio.m4s"
            }"#,
    )
    .unwrap();
    assert_eq!(req.method.as_deref(), Some("POST"));
    assert_eq!(
        req.audio_url.as_deref(),
        Some("https://example.com/audio.m4s")
    );
    match req.body.as_ref().unwrap() {
        RequestBody::Raw {
            bytes_b64,
            content_type,
        } => {
            assert_eq!(bytes_b64, "aGk=");
            assert_eq!(content_type.as_deref(), Some("text/plain"));
        }
        other => panic!("expected Raw body, got {other:?}"),
    }

    // 缺省：旧客户端（CLI / aria2 shim）不带这三个字段，必须解析为 None。
    let minimal: CreateTaskRequest =
        serde_json::from_str(r#"{"url": "https://example.com/f.zip"}"#).unwrap();
    assert!(minimal.method.is_none());
    assert!(minimal.body.is_none());
    assert!(minimal.audio_url.is_none());
}

#[test]
fn task_dto_serializes_camel_case_with_correct_values() {
    let dto = TaskDto {
        task_id: "t1".to_string(),
        url: "https://example.com/f.zip".to_string(),
        file_name: "f.zip".to_string(),
        save_dir: "/tmp".to_string(),
        status: 1,
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
        file_hash: String::new(),
        referrer: String::new(),
        group_id: "g1".to_string(),
        rss_source_id: String::new(),
        origin_url: String::new(),
        auto_route: String::new(),
        source_bytes: Default::default(),
        queue_order: 7,
        uploaded_bytes: 42,
        uploaded_at_completion: 7,
        seeding_status: 1,
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
    };
    let v = serde_json::to_value(&dto).unwrap();
    assert_eq!(v["taskId"], "t1");
    assert_eq!(v["url"], "https://example.com/f.zip");
    assert_eq!(v["fileName"], "f.zip");
    assert_eq!(v["saveDir"], "/tmp");
    assert_eq!(v["status"], 1);
    assert_eq!(v["queueOrder"], 7);
    assert_eq!(v["downloadedBytes"], 10);
    assert_eq!(v["totalBytes"], 100);
    assert_eq!(v["createdAt"], "1700000000");
    assert_eq!(v["groupId"], "g1");
    // 默认值字段不上线；缺省反序列化回同一个 DTO。
    for absent in [
        "errorMessage",
        "proxyUrl",
        "queueId",
        "checksum",
        "ignoreTlsErrors",
        "fileMissing",
        "completedAt",
        "referrer",
        "rssSourceId",
        "originUrl",
        "autoRoute",
        "sourceBytes",
        "seedingMessage",
        "seedingTimeSecs",
        "seedRatioLimitMilli",
        "seedPostRatioLimitMilli",
        "seedTimeLimitMinutes",
        "seedInactiveTimeLimitMinutes",
        "seedUploadLimitBps",
        "btSequential",
        "btFirstLastPiecesFirst",
    ] {
        assert!(v.get(absent).is_none(), "{absent} must be omitted");
    }
    let back: TaskDto = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(back, dto);
    // 自定义做种上限与非空字段仍然上线。
    let custom = TaskDto {
        seed_ratio_limit_milli: 0,
        error_message: "boom".to_string(),
        ..dto
    };
    let v = serde_json::to_value(&custom).unwrap();
    assert_eq!(v["seedRatioLimitMilli"], 0);
    assert_eq!(v["errorMessage"], "boom");
    // 蛇形字段名不应残留（防止漏掉 rename_all）。
    assert!(v.get("task_id").is_none());
    assert!(v.get("file_name").is_none());
}

#[test]
fn queue_dto_serializes_camel_case_with_correct_values() {
    let dto = QueueDto {
        queue_id: "q1".to_string(),
        name: "工作".to_string(),
        speed_limit_kbps: 512,
        upload_limit_kbps: 128,
        max_concurrent: 3,
        default_save_dir: "/tmp".to_string(),
        position: 0,
        default_segments: 4,
        default_user_agent: "UA/1".to_string(),
        is_running: true,
        schedule_enabled: false,
        schedule_start: String::new(),
        schedule_stop: String::new(),
        schedule_days: 127,
    };
    let v = serde_json::to_value(&dto).unwrap();
    assert_eq!(v["queueId"], "q1");
    assert_eq!(v["name"], "工作");
    assert_eq!(v["speedLimitKbps"], 512);
    assert_eq!(v["uploadLimitKbps"], 128);
    assert_eq!(v["maxConcurrent"], 3);
    assert_eq!(v["defaultSaveDir"], "/tmp");
    assert_eq!(v["position"], 0);
    assert_eq!(v["defaultSegments"], 4);
    assert_eq!(v["defaultUserAgent"], "UA/1");
    assert!(v.get("queue_id").is_none());
    assert!(v.get("speed_limit_kbps").is_none());
}

#[test]
fn component_install_params_use_stable_camel_case_kind() {
    let params: ComponentInstallParams =
        serde_json::from_str(r#"{"component":"ytdlp","version":"2026.08.01"}"#).unwrap();
    assert_eq!(params.component, ComponentKind::Ytdlp);
    assert_eq!(params.version.as_deref(), Some("2026.08.01"));
    let wire = serde_json::to_value(params).unwrap();
    assert_eq!(wire["component"], "ytdlp");
    assert_eq!(wire["version"], "2026.08.01");
}
