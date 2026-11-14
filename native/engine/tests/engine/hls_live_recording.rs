//! HLS 直播录制端到端回归(本地 mock HTTP,经 `run_hls_download` 全链路):
//!   1. 播放列表无 EXT-X-ENDLIST 时不再报错,按轮询追加新分片,出现 ENDLIST 后以
//!      已录内容定稿并完成;
//!   2. 直播中途用户暂停/取消(任务令牌取消)不丢弃已录内容,而是定稿为完成文件。
//!
//! 分片载荷不是合法 TS,remux 必然失败并按既有降级保留 `.ts`,因此输出文件内容
//! 应恰好等于各分片按序拼接。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fluxdown_engine::db::Db;
use fluxdown_engine::downloader::{DownloadParams, ProgressUpdate, RequestSpec, build_client};
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::speed_limiter::SpeedLimiter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct NoopTestSink;
impl EventSink for NoopTestSink {
    fn emit(&self, _event: EngineEvent) {}
}

fn segment_body(sequence: u64) -> Vec<u8> {
    format!("S{sequence}-{}", "x".repeat(sequence as usize + 1)).into_bytes()
}

fn concat_segments(sequences: &[u64]) -> Vec<u8> {
    sequences.iter().flat_map(|n| segment_body(*n)).collect()
}

fn playlist(media_sequence: u64, count: u64, end: bool) -> Vec<u8> {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"
    );
    for n in media_sequence..media_sequence + count {
        text.push_str(&format!("#EXTINF:1.0,\nseg{n}.ts\n"));
    }
    if end {
        text.push_str("#EXT-X-ENDLIST\n");
    }
    text.into_bytes()
}

/// 播放列表按请求次序依次返回 `playlists`(用尽后沿用最后一份);`/segN.ts` 返回固定载荷。
async fn start_live_server(playlists: Vec<Vec<u8>>) -> String {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let playlists = Arc::new(playlists);
    let playlist_hits = Arc::new(Mutex::new(0usize));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let playlists = Arc::clone(&playlists);
            let playlist_hits = Arc::clone(&playlist_hits);
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let Ok(n) = socket.read(&mut buf).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                let body = if path == "/live/index.m3u8" {
                    let mut hits = playlist_hits.lock().unwrap();
                    let body = playlists[(*hits).min(playlists.len() - 1)].clone();
                    *hits += 1;
                    Some(body)
                } else {
                    path.strip_prefix("/live/seg")
                        .and_then(|rest| rest.strip_suffix(".ts"))
                        .and_then(|n| n.parse::<u64>().ok())
                        .map(segment_body)
                };
                let response = match body {
                    Some(body) => {
                        let mut response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        response.extend(body);
                        response
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                socket.write_all(&response).await.unwrap_or_else(|error| {
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ),
                        "test server response failed: {error}"
                    );
                });
            });
        }
    });
    format!("http://{address}/live/index.m3u8")
}

fn make_params(
    url: &str,
    work_dir: &std::path::Path,
    db: &Db,
    tx: mpsc::Sender<ProgressUpdate>,
    cancel: CancellationToken,
) -> DownloadParams {
    DownloadParams {
        spawn_gen: 1,
        unattended: true,
        preprobed: None,
        interceptor: fluxdown_engine::intercept::noop(),
        connection_cap: 0,
        run_meta: fluxdown_engine::intercept::RunMeta::default(),
        auto_proxy: None,
        multi_nic: None,
        task_id: "tlive".to_string(),
        url: url.to_string(),
        save_dir: work_dir.to_string_lossy().to_string(),
        file_name: "live.ts".to_string(),
        segment_count: 0,
        is_resume: false,
        range_verified: true,
        db: db.clone(),
        client: build_client(&ProxyConfig::default(), "FluxDownRealTest/1.0").expect("client"),
        progress_tx: tx,
        cancel_token: cancel,
        sink: Arc::new(NoopTestSink),
        cookies: String::new(),
        referrer: String::new(),
        speed_limiter: {
            let limiter = SpeedLimiter::new(0);
            limiter.spawn_refill_task();
            limiter
        },
        hint_file_size: 0,
        proxy_config: ProxyConfig::default(),
        selector: Arc::new(fluxdown_engine::NoopSelection),
        checksum: String::new(),
        extra_headers: HashMap::new(),
        spec: RequestSpec::empty_get(),
        audio_url: None,
        auto_max_connections: 0,
        use_server_time: false,
        overwrite: fluxdown_engine::file_exists::OverwritePolicy::Never,
        ffmpeg_path: None,
        cdn: fluxdown_engine::cdn::CdnTaskInput::default(),
    }
}

async fn setup(tag: &str, url: &str) -> (std::path::PathBuf, Db) {
    let work_dir =
        std::env::temp_dir().join(format!("fluxdown-hls-live-{tag}-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&work_dir).await.unwrap();
    let db = Db::connect("sqlite::memory:").await.expect("db");
    db.insert_task(
        "tlive",
        url,
        "live.ts",
        &work_dir.to_string_lossy(),
        0,
        0,
        "",
        "",
        "",
        0,
    )
    .await
    .expect("insert_task");
    (work_dir, db)
}

/// 断言任务以"完成"收尾,返回输出文件字节。
async fn completed_output(db: &Db, work_dir: &std::path::Path) -> Vec<u8> {
    let task = db.load_task_by_id("tlive").await.unwrap().unwrap();
    assert_eq!(task.status, 3, "task must complete, got {task:?}");
    assert!(task.file_name.ends_with(".ts"), "{}", task.file_name);
    assert!(
        db.get_config("hls_resume_tlive").await.unwrap().is_none(),
        "completed recording must clear its resume checkpoint"
    );
    tokio::fs::read(work_dir.join(&task.file_name))
        .await
        .unwrap()
}

#[tokio::test]
async fn hls_live_recording_runs_until_endlist() {
    // 首个快照 0,1;随后窗口滑到 1,2、再到 2,3 并出现 ENDLIST。
    let url = start_live_server(vec![
        playlist(0, 2, false),
        playlist(1, 2, false),
        playlist(2, 2, true),
    ])
    .await;
    let (work_dir, db) = setup("endlist", &url).await;
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(1024);
    let statuses = Arc::new(Mutex::new(Vec::<(i32, i64)>::new()));
    let seen = Arc::clone(&statuses);
    let collector = tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            seen.lock()
                .unwrap()
                .push((update.status, update.total_bytes));
        }
    });

    let fallback = fluxdown_engine::hls_downloader::run_hls_download(make_params(
        &url,
        &work_dir,
        &db,
        tx,
        CancellationToken::new(),
    ))
    .await;
    collector.await.unwrap();

    assert!(
        fallback.is_none(),
        "live playlist is not a plain-HTTP fallback"
    );
    assert_eq!(
        completed_output(&db, &work_dir).await,
        concat_segments(&[0, 1, 2, 3])
    );
    {
        let statuses = statuses.lock().unwrap();
        assert!(
            statuses.iter().any(|(status, _)| *status == 3),
            "a completed frame must be reported: {statuses:?}"
        );
        assert!(
            statuses
                .iter()
                .filter(|(status, _)| *status == 1)
                .all(|(_, total)| *total == 0),
            "live progress must report an unknown total: {statuses:?}"
        );
    }
    tokio::fs::remove_dir_all(&work_dir).await.unwrap();
}

#[tokio::test]
async fn hls_live_user_stop_finalizes_recorded_content() {
    // 播放列表永不结束;录到两个分片后用户暂停/取消(令牌取消)。
    let url = start_live_server(vec![playlist(0, 2, false)]).await;
    let (work_dir, db) = setup("stop", &url).await;
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(1024);
    let cancel = CancellationToken::new();
    let watcher_cancel = cancel.clone();
    let recorded = concat_segments(&[0, 1]).len() as i64;
    let collector = tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            if update.status == 1 && update.downloaded_bytes >= recorded {
                watcher_cancel.cancel();
            }
        }
    });

    let fallback = fluxdown_engine::hls_downloader::run_hls_download(make_params(
        &url, &work_dir, &db, tx, cancel,
    ))
    .await;
    collector.await.unwrap();

    assert!(fallback.is_none());
    assert_eq!(
        completed_output(&db, &work_dir).await,
        concat_segments(&[0, 1])
    );
    tokio::fs::remove_dir_all(&work_dir).await.unwrap();
}

#[tokio::test]
async fn hls_live_delete_discards_recording_without_output() {
    // 与"停止"相同的令牌取消,但 manager 先声明了删除:不得定稿,temp 与检查点清掉。
    const TASK: &str = "tdel";
    let url = start_live_server(vec![playlist(0, 2, false)]).await;
    let (work_dir, db) = setup("delete", &url).await;
    db.insert_task(
        TASK,
        &url,
        "live.ts",
        &work_dir.to_string_lossy(),
        0,
        0,
        "",
        "",
        "",
        0,
    )
    .await
    .expect("insert_task");
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(1024);
    let cancel = CancellationToken::new();
    let watcher_cancel = cancel.clone();
    let recorded = concat_segments(&[0, 1]).len() as i64;
    let collector = tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            if update.status == 1 && update.downloaded_bytes >= recorded {
                fluxdown_engine::hls_downloader::request_discard(TASK);
                watcher_cancel.cancel();
            }
        }
    });
    let mut params = make_params(&url, &work_dir, &db, tx, cancel);
    params.task_id = TASK.to_string();

    let fallback = fluxdown_engine::hls_downloader::run_hls_download(params).await;
    collector.await.unwrap();
    fluxdown_engine::hls_downloader::clear_discard(TASK);

    assert!(fallback.is_none());
    let task = db.load_task_by_id(TASK).await.unwrap().unwrap();
    assert_ne!(task.status, 3, "a deleted recording must not complete");
    for name in ["live.ts", "live.mp4", "live.ts.fdownloading"] {
        assert!(
            !work_dir.join(name).exists(),
            "{name} must not survive a delete"
        );
    }
    assert!(
        db.get_config(&format!("hls_resume_{TASK}"))
            .await
            .unwrap()
            .is_none(),
        "checkpoint must be cleared"
    );
    tokio::fs::remove_dir_all(&work_dir).await.unwrap();
}
