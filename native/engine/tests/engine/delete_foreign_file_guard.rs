//! 「删除任务并删除文件」/「重新下载」的最终产物认领守卫集成测试。
//!
//! 契约：`file_name` 的 dedup 改名只在启动序幕（非 BT）或完成期（BT）落库，
//! **从未启动**的任务（稍后下载/排队中）`file_name` 仍是建任务时的原始名，
//! 可能与磁盘上早前同名任务留下的成品相撞。因此：
//!
//! - `delete_task` / `delete_tasks_batch` 带 `delete_files=true` 对未完成
//!   （status≠3）任务**不得**删除 `save_dir/file_name`；
//! - 对已完成（status=3）任务必须照常删除其产物；
//! - `restart_task` 对从未启动的任务同样不得删除同名既有文件。
//!
//! 复现场景：下载完成 A → 删任务保留文件 → 同链接重新添加为稍后下载 →
//! 「删除任务和文件」曾把早前的成品 A 一并删掉。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::NewTaskSpec;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), n)
}

async fn make_engine(work: &std::path::Path) -> Engine {
    let cfg = EngineConfig {
        max_concurrent: 4,
        speed_limit_bps: 0,
        upload_limit_bps: 0,
        default_save_dir: work.to_string_lossy().into_owned(),
        app_data_dir: work.to_string_lossy().into_owned(),
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: String::new(),
        data_dir_override: Some(work.to_path_buf()),
        database_url: None,
    };
    let mut engine = Engine::new(cfg, Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine");
    // 排空进度/完成通道，防止删除路径的合成 TaskProgress 因通道满而阻塞。
    if let Some(mut rx) = engine.manager.take_progress_rx() {
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
    }
    if let Some(mut done_rx) = engine.manager.take_done_rx() {
        tokio::spawn(async move { while done_rx.recv().await.is_some() {} });
    }
    engine
}

/// 建一个「稍后下载」任务（paused 落库，从未启动，file_name 未 dedup）。
async fn create_later_task(engine: &mut Engine, save_dir: &str, file_name: &str) -> String {
    engine
        .manager
        .create_task(NewTaskSpec {
            // 端口 1 连接立即被拒：后台 meta probe 静默失败，不改 file_name。
            url: format!("http://127.0.0.1:1/{file_name}"),
            save_dir: save_dir.to_string(),
            file_name: file_name.to_string(),
            start_paused: true,
            ..Default::default()
        })
        .await
        .expect("create later task")
}

const FOREIGN_CONTENT: &[u8] = b"earlier completed download - must survive";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_files_on_never_started_task_keeps_foreign_file() {
    let work = std::env::temp_dir().join(format!("fluxdown-delguard-single-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let save_dir = work.to_string_lossy().into_owned();
    // 磁盘上已有早前任务留下的同名成品。
    let existing = work.join("a.bin");
    tokio::fs::write(&existing, FOREIGN_CONTENT)
        .await
        .expect("seed file");

    let mut engine = make_engine(&work).await;
    let id = create_later_task(&mut engine, &save_dir, "a.bin").await;

    engine
        .manager
        .delete_task(&id, true)
        .await
        .expect("delete task");

    // 任务记录已删，但别人的同名成品必须原封不动。
    assert!(engine.db.load_task_by_id(&id).await.expect("db").is_none());
    let content = tokio::fs::read(&existing)
        .await
        .expect("foreign file must survive");
    assert_eq!(content, FOREIGN_CONTENT);

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_files_on_completed_task_removes_its_product() {
    let work = std::env::temp_dir().join(format!("fluxdown-delguard-done-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let save_dir = work.to_string_lossy().into_owned();
    let product = work.join("c.bin");
    tokio::fs::write(&product, b"product of this task")
        .await
        .expect("seed file");

    let mut engine = make_engine(&work).await;
    let id = create_later_task(&mut engine, &save_dir, "c.bin").await;
    // 模拟完成态（产物已 rename 到最终路径的时点）。
    engine
        .db
        .update_task_status(&id, 3, "")
        .await
        .expect("mark completed");

    engine
        .manager
        .delete_task(&id, true)
        .await
        .expect("delete task");

    assert!(
        !product.exists(),
        "completed task's own product must be removed with delete_files=true"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_files_on_never_started_tasks_keeps_foreign_files() {
    let work = std::env::temp_dir().join(format!("fluxdown-delguard-batch-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let save_dir = work.to_string_lossy().into_owned();
    let foreign = work.join("b.bin");
    tokio::fs::write(&foreign, FOREIGN_CONTENT)
        .await
        .expect("seed file");
    let owned = work.join("d.bin");
    tokio::fs::write(&owned, b"completed product")
        .await
        .expect("seed file");

    let mut engine = make_engine(&work).await;
    let never_started = create_later_task(&mut engine, &save_dir, "b.bin").await;
    let completed = create_later_task(&mut engine, &save_dir, "d.bin").await;
    engine
        .db
        .update_task_status(&completed, 3, "")
        .await
        .expect("mark completed");

    let outcome = engine
        .manager
        .delete_tasks_batch(&[never_started, completed], true)
        .await
        .expect("batch delete");
    assert_eq!((outcome.deleted, outcome.failed), (2, 0));

    let content = tokio::fs::read(&foreign)
        .await
        .expect("foreign file must survive");
    assert_eq!(content, FOREIGN_CONTENT);
    assert!(!owned.exists(), "completed task's product must be removed");

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_never_started_task_keeps_foreign_file() {
    let work = std::env::temp_dir().join(format!("fluxdown-delguard-restart-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let save_dir = work.to_string_lossy().into_owned();
    let existing = work.join("r.bin");
    tokio::fs::write(&existing, FOREIGN_CONTENT)
        .await
        .expect("seed file");

    let mut engine = make_engine(&work).await;
    let id = create_later_task(&mut engine, &save_dir, "r.bin").await;

    // 「重新下载」的磁盘复位同步完成于函数返回前；随后的下载会因端口拒绝
    // 而失败，但启动期 dedup 会另起新名，绝不触碰既有同名文件。
    engine.manager.restart_task(&id).await;

    let content = tokio::fs::read(&existing)
        .await
        .expect("foreign file must survive");
    assert_eq!(content, FOREIGN_CONTENT);

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 两条任务的 `file_name` 指向同一磁盘名：删除其中已完成的一条（含文件）时，
/// 不得带走另一条任务登记的同名文件；另一条删除后该文件才归它自己处置。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_files_keeps_file_named_by_another_task() {
    let work = std::env::temp_dir().join(format!("fluxdown-delguard-shared-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let save_dir = work.to_string_lossy().into_owned();
    let shared = work.join("e.bin");
    tokio::fs::write(&shared, b"claimed by two tasks")
        .await
        .expect("seed file");

    let mut engine = make_engine(&work).await;
    let completed = create_later_task(&mut engine, &save_dir, "e.bin").await;
    let other = create_later_task(&mut engine, &save_dir, "e.bin").await;
    for id in [&completed, &other] {
        let row = engine
            .db
            .load_task_by_id(id)
            .await
            .expect("db")
            .expect("row");
        assert_eq!(row.file_name, "e.bin", "precondition: names must collide");
    }
    engine
        .db
        .update_task_status(&completed, 3, "")
        .await
        .expect("mark completed");

    engine
        .manager
        .delete_task(&completed, true)
        .await
        .expect("delete task");
    assert!(
        shared.exists(),
        "file still named by another task must survive"
    );

    // 批量删除同一批内的两条同名任务：都在本批内，文件归它们，照常清除。
    let third = create_later_task(&mut engine, &save_dir, "e.bin").await;
    engine
        .db
        .update_task_status(&third, 3, "")
        .await
        .expect("mark completed");
    let outcome = engine
        .manager
        .delete_tasks_batch(&[other, third], true)
        .await
        .expect("batch delete");
    assert_eq!((outcome.deleted, outcome.failed), (2, 0));
    assert!(
        !shared.exists(),
        "file named only by deleted tasks is removed with delete_files=true"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// HLS 任务的独立音轨 sidecar（`<stem>.audio.m4a`）及其 `.fdownloading` 临时文件，
/// 与断点 config 行一起在「删除任务并删除文件」时被清理。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_files_removes_hls_audio_sidecar_and_checkpoints() {
    let work = std::env::temp_dir().join(format!("fluxdown-delguard-hls-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let save_dir = work.to_string_lossy().into_owned();
    let video = work.join("show.ts");
    let audio = work.join("show.audio.m4a");
    let audio_temp = work.join("show.audio.m4a.fdownloading");
    for path in [&video, &audio, &audio_temp] {
        tokio::fs::write(path, b"x").await.expect("seed file");
    }

    let mut engine = make_engine(&work).await;
    let id = engine
        .manager
        .create_task(NewTaskSpec {
            url: "http://127.0.0.1:1/show.m3u8".to_string(),
            save_dir: save_dir.clone(),
            file_name: "show.ts".to_string(),
            start_paused: true,
            ..Default::default()
        })
        .await
        .expect("create hls task");
    engine
        .db
        .update_task_status(&id, 3, "")
        .await
        .expect("mark completed");
    engine
        .db
        .set_config(&format!("hls_resume_{id}"), "{}")
        .await
        .expect("video checkpoint");
    engine
        .db
        .set_config(&format!("hls_audio_resume_{id}"), "{}")
        .await
        .expect("audio checkpoint");

    engine
        .manager
        .delete_task(&id, true)
        .await
        .expect("delete task");

    assert!(!video.exists(), "video product must be removed");
    assert!(!audio.exists(), "hls audio sidecar must be removed");
    assert!(!audio_temp.exists(), "hls audio temp must be removed");
    for key in [format!("hls_resume_{id}"), format!("hls_audio_resume_{id}")] {
        assert!(
            engine.db.get_config(&key).await.expect("config").is_none(),
            "{key} must be cleaned with the task"
        );
    }

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}
