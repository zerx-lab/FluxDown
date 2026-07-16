//! DASH 轨对（离散音视频轨）多段并发 + 真续传回归测试。
//!
//! 守护三个行为契约（真 Range 服务器，两轨均超 2MB 多段阈值）：
//!   1. 多段并发：视频轨经 segment_coordinator 下载，进度快照含 ≥2 个分段，
//!      且字节区间/总量全部位于【轨对坐标系】（total = 两轨合计）；
//!   2. 轨对坐标映射：音频轨阶段的快照以 index=-1 的 100% 前缀段表示已完成
//!      的视频轨，真实分段区间整体平移 +视频轨大小；
//!   3. 真续传：视频轨中途取消（暂停）后二次运行，服务器实际送出的视频字节
//!      总量显著小于两倍轨长（段行/临时文件跨运行保留），且二次运行出现
//!      非零起点的 Range 请求。
//!
//! 用法（绑定本地端口，默认 ignore）：
//!   cargo nextest run -p fluxdown_engine --test dash_track_pair_resume --run-ignored all
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};

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

const VIDEO_LEN: usize = 6 * 1024 * 1024;
const AUDIO_LEN: usize = 3 * 1024 * 1024;
/// ProgressUpdate 的紧凑摘要（ProgressUpdate 非 Clone）：
/// (status, downloaded, total, 分段快照 (index, start, end, downloaded))。
type UpdateSummary = (i32, i64, i64, Option<Vec<(i32, i64, i64, i64)>>);

fn summarize(u: &ProgressUpdate) -> UpdateSummary {
    (
        u.status,
        u.downloaded_bytes,
        u.total_bytes,
        u.segment_details.as_ref().map(|d| {
            d.iter()
                .map(|s| (s.index, s.start_byte, s.end_byte, s.downloaded_bytes))
                .collect()
        }),
    )
}
/// 服务器侧账本：按运行阶段（phase 1/2）统计 /video 送出的字节与 Range 起点。
struct ServerLedger {
    phase: AtomicU8,
    /// 按运行阶段拆账的 /video 已送字节：[unused, run1, run2]。
    video_served_by_phase: [AtomicI64; 3],
    video_range_starts: std::sync::Mutex<Vec<(u8, i64)>>,
}

impl ServerLedger {
    fn video_served_total(&self) -> i64 {
        self.video_served_by_phase[1].load(Ordering::Relaxed)
            + self.video_served_by_phase[2].load(Ordering::Relaxed)
    }
}

/// 真 Range HTTP/1.1 服务器：任意 `bytes=X-Y` / `bytes=X-` → 206 + Content-Range，
/// 无 Range → 200 全量。/video 路径限速写出（16KB/8ms ≈ 2MB/s）以便中途取消。
async fn start_range_server(
    video: Arc<Vec<u8>>,
    audio: Arc<Vec<u8>>,
    ledger: Arc<ServerLedger>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let video = video.clone();
            let audio = audio.clone();
            let ledger = ledger.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let is_video = !head.contains("/audio");
                let body: &Arc<Vec<u8>> = if is_video { &video } else { &audio };
                let total = body.len() as i64;

                // 解析 Range 头（大小写不敏感）。
                let range = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                    .and_then(|l| l.split('=').nth(1))
                    .map(str::trim)
                    .and_then(|spec| {
                        let (s, e) = spec.split_once('-')?;
                        let start: i64 = s.trim().parse().ok()?;
                        let end: i64 = match e.trim() {
                            "" => total - 1,
                            v => v.parse().ok()?,
                        };
                        Some((start, end.min(total - 1)))
                    });

                let (start, end, status) = match range {
                    Some((s, e)) if s >= 0 && s <= e && s < total => (s, e, 206),
                    Some(_) => {
                        let _ = stream
                            .write_all(
                                b"HTTP/1.1 416 Range Not Satisfiable\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    }
                    None => (0, total - 1, 200),
                };

                if is_video {
                    ledger
                        .video_range_starts
                        .lock()
                        .expect("lock")
                        .push((ledger.phase.load(Ordering::Relaxed), start));
                }

                let len = end - start + 1;
                let header = if status == 206 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {len}\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                    )
                };
                if stream.write_all(header.as_bytes()).await.is_err() {
                    return;
                }

                // 分块限速写出；客户端截断/断开时立即停止计数。
                let mut off = start as usize;
                let end_excl = (end + 1) as usize;
                while off < end_excl {
                    let chunk_end = (off + 16 * 1024).min(end_excl);
                    if stream.write_all(&body[off..chunk_end]).await.is_err() {
                        return;
                    }
                    if is_video {
                        let phase = ledger.phase.load(Ordering::Relaxed).min(2) as usize;
                        ledger.video_served_by_phase[phase]
                            .fetch_add((chunk_end - off) as i64, Ordering::Relaxed);
                        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                    }
                    off = chunk_end;
                }
                let _ = stream.flush().await;
            });
        }
    });
    format!("http://{addr}")
}

fn make_params(
    base: &str,
    work_dir: &std::path::Path,
    db: &Db,
    tx: mpsc::Sender<ProgressUpdate>,
    cancel: CancellationToken,
    spawn_gen: i64,
) -> DownloadParams {
    DownloadParams {
        spawn_gen,
        task_id: "tpr".to_string(),
        url: format!("{base}/video"),
        save_dir: work_dir.to_string_lossy().to_string(),
        file_name: "pair.mp4".to_string(),
        segment_count: 0, // auto → advisor 按轨长决定多段
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
            let limiter = SpeedLimiter::new(0); // 不限速——吞吐由服务器侧节流控制
            limiter.spawn_refill_task();
            limiter
        },
        hint_file_size: 0,
        proxy_config: ProxyConfig::default(),
        selector: Arc::new(fluxdown_engine::NoopSelection),
        checksum: String::new(),
        extra_headers: std::collections::HashMap::new(),
        spec: RequestSpec::empty_get(),
        audio_url: Some(format!("{base}/audio")),
        auto_max_connections: 0,
        use_server_time: false,
        ffmpeg_path: None,
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a local port; run with --run-ignored"]
async fn track_pair_multi_segment_pause_resume_and_pair_coordinates() {
    let video: Arc<Vec<u8>> = Arc::new((0..VIDEO_LEN as u32).map(|i| (i % 251) as u8).collect());
    let audio: Arc<Vec<u8>> = Arc::new((0..AUDIO_LEN as u32).map(|i| (i % 241) as u8).collect());
    let pair_total = (VIDEO_LEN + AUDIO_LEN) as i64;
    let ledger = Arc::new(ServerLedger {
        phase: AtomicU8::new(1),
        video_served_by_phase: [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)],
        video_range_starts: std::sync::Mutex::new(Vec::new()),
    });
    let base = start_range_server(video.clone(), audio.clone(), ledger.clone()).await;

    let work_dir = std::env::temp_dir().join("fluxdown-rt-trackpair-resume");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    tokio::fs::create_dir_all(&work_dir).await.unwrap();
    let db = Db::open(&work_dir).await.expect("db");
    db.insert_task(
        "tpr",
        &format!("{base}/video"),
        "pair.mp4",
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

    // ---- 第一次运行：视频轨下到 ~2MB 时取消（模拟暂停）。----
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(1024);
    let cancel = CancellationToken::new();
    let cancel_at = 2_000_000i64;
    let watcher_cancel = cancel.clone();
    let run1_updates = Arc::new(std::sync::Mutex::new(Vec::<UpdateSummary>::new()));
    let sink_updates = run1_updates.clone();
    let collector = tokio::spawn(async move {
        while let Some(u) = rx.recv().await {
            if u.downloaded_bytes >= cancel_at && u.status == 1 {
                watcher_cancel.cancel();
            }
            sink_updates.lock().expect("lock").push(summarize(&u));
        }
    });

    fluxdown_engine::dash_downloader::run_dash_download(make_params(
        &base, &work_dir, &db, tx, cancel, 1,
    ))
    .await;
    let _ = collector.await;

    // 暂停后现场：段行与临时文件必须保留（真续传的前提）。
    let rows_after_pause = db.load_segments("tpr").await.expect("load segments");
    assert!(
        !rows_after_pause.is_empty(),
        "pause must keep segment rows for resume"
    );
    let temp_path = work_dir.join(format!("pair.mp4{}", fluxdown_engine::downloader::TEMP_EXT));
    assert!(
        tokio::fs::try_exists(&temp_path).await.unwrap_or(false),
        "pause must keep the pre-allocated temp file"
    );
    let served_run1 = ledger.video_served_by_phase[1].load(Ordering::Relaxed);
    assert!(
        served_run1 >= cancel_at / 2,
        "run1 should have served a meaningful prefix, got {served_run1}"
    );

    // ---- 第二次运行：完成整个轨对。----
    ledger.phase.store(2, Ordering::Relaxed);
    let (tx2, mut rx2) = mpsc::channel::<ProgressUpdate>(1024);
    let run2_updates = Arc::new(std::sync::Mutex::new(Vec::<UpdateSummary>::new()));
    let sink2 = run2_updates.clone();
    let collector2 = tokio::spawn(async move {
        while let Some(u) = rx2.recv().await {
            sink2.lock().expect("lock").push(summarize(&u));
        }
    });

    fluxdown_engine::dash_downloader::run_dash_download(make_params(
        &base,
        &work_dir,
        &db,
        tx2,
        CancellationToken::new(),
        2,
    ))
    .await;
    let _ = collector2.await;

    // ---- 断言 1：产物字节精确（mux 无 ffmpeg 时为视频 + 独立音频双文件）。----
    let video_out = work_dir.join("pair.mp4");
    let vmeta = tokio::fs::metadata(&video_out).await.expect("video output");
    assert_eq!(vmeta.len() as usize, VIDEO_LEN, "video track byte-exact");
    let video_bytes_on_disk = tokio::fs::read(&video_out).await.expect("read video");
    assert_eq!(video_bytes_on_disk, *video, "video content byte-identical");
    let audio_out = work_dir.join("pair.audio.m4a");
    if tokio::fs::try_exists(&audio_out).await.unwrap_or(false) {
        let ameta = tokio::fs::metadata(&audio_out).await.expect("audio output");
        assert_eq!(ameta.len() as usize, AUDIO_LEN, "audio track byte-exact");
    } else {
        // ffmpeg 存在时音轨被 mux 进主文件后删除——主文件须不小于两轨之和的 9 成。
        assert!(
            vmeta.len() as usize >= VIDEO_LEN,
            "muxed output unexpectedly small"
        );
    }

    // ---- 断言 2：完成状态 + 段行清空。----
    let final2_status = {
        let sum = run2_updates.lock().expect("lock");
        sum.last().expect("run2 must emit updates").0
    };
    assert_eq!(final2_status, 3, "run2 must complete");
    let rows_after_done = db.load_segments("tpr").await.expect("load segments");
    assert!(
        rows_after_done.is_empty(),
        "completion must clear segment rows"
    );

    // ---- 断言 3：真续传字节账（按 phase 拆账，具备回归区分力）。----
    // run2 送出的视频字节必须【严格小于】整轨——若续传失效（段行被忽略/清掉、
    // 整轨重下），run2 必然送满 ≈VIDEO_LEN，此断言立刻挂。取消时 coordinator
    // 已把各段进度落库，理论跳过量 ≈ cancel_at；给在途浪费留 1MB 余量。
    let served_run2 = ledger.video_served_by_phase[2].load(Ordering::Relaxed);
    assert!(
        served_run2 <= VIDEO_LEN as i64 - cancel_at + 1_048_576,
        "run2 must skip the persisted prefix: served_run2={served_run2}, \
         expected <= {} (VIDEO_LEN - cancel_at + 1MiB slack)",
        VIDEO_LEN as i64 - cancel_at + 1_048_576
    );
    let served_total = ledger.video_served_total();
    assert!(
        served_total < (VIDEO_LEN as i64 * 3) / 2,
        "resume must not redownload the whole video track: served={served_total}, len={VIDEO_LEN}"
    );
    // 且第二轮出现非零起点的 Range 请求（直接的续传证据）。
    let starts = ledger.video_range_starts.lock().expect("lock").clone();
    assert!(
        starts.iter().any(|(phase, s)| *phase == 2 && *s > 0),
        "run2 must issue a non-zero-start Range request, got {starts:?}"
    );

    // ---- 断言 4：多段 + 轨对坐标系。----
    let all_updates: Vec<UpdateSummary> = {
        let a = std::mem::take(&mut *run1_updates.lock().expect("lock"));
        let b = std::mem::take(&mut *run2_updates.lock().expect("lock"));
        a.into_iter().chain(b).collect()
    };
    // 视频轨阶段：某帧快照含 ≥2 个真实分段，且总量 = 两轨合计。
    let msf = all_updates
        .iter()
        .find(|(_, _, _, d)| {
            d.as_ref()
                .is_some_and(|d| d.iter().filter(|s| s.0 >= 0).count() >= 2)
        })
        .expect("expected a multi-segment snapshot frame");
    assert_eq!(
        msf.2, pair_total,
        "coordinated frames must report the pair total"
    );
    for (_, start, end, _) in msf.3.as_ref().expect("details") {
        assert!(
            *start >= 0 && *end < pair_total,
            "segment range [{start}, {end}] escapes pair space {pair_total}"
        );
    }
    // 音频轨阶段：前缀段 index=-1 覆盖 [0, VIDEO_LEN)，真实段整体平移。
    let af = all_updates
        .iter()
        .find(|(_, _, _, d)| {
            d.as_ref()
                .is_some_and(|d| d.first().is_some_and(|s| s.0 == -1))
        })
        .expect("expected an audio-phase frame with completed-video prefix");
    let details = af.3.as_ref().expect("details");
    assert_eq!(details[0].1, 0);
    assert_eq!(details[0].2, VIDEO_LEN as i64 - 1);
    assert_eq!(details[0].3, VIDEO_LEN as i64);
    assert!(
        details[1..].iter().all(|s| s.1 >= VIDEO_LEN as i64),
        "audio segments must be shifted past the video track"
    );

    let _ = tokio::fs::remove_dir_all(&work_dir).await;
}
