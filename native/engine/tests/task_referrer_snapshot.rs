//! Repro for issue #111: the outbound task snapshot (DB load path used at
//! startup) must carry the persisted referrer so hosts can show the source page.

use fluxdown_engine::db::Db;

#[tokio::test]
async fn loaded_task_snapshot_carries_referrer() {
    let db = Db::connect("sqlite::memory:").await.expect("open db");
    db.insert_task(
        "t1",
        "https://example.com/f.zip",
        "f.zip",
        "/tmp",
        0,
        0,
        "",
        "main",
        "",
        0,
    )
    .await
    .expect("insert task");
    db.set_task_request_context("t1", "", "https://example.com/page", "")
        .await
        .expect("set request context");

    let tasks = db.load_all_tasks().await.expect("load all");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].referrer, "https://example.com/page");

    let one = db
        .load_task_by_id("t1")
        .await
        .expect("load by id")
        .expect("task exists");
    assert_eq!(one.referrer, "https://example.com/page");
}
