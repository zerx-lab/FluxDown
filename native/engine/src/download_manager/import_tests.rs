use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    ActiveTaskEntry, BatchItemOutcome, BtConfig, CreateRejection, Db, DownloadManager,
    DownloadManagerConfig, EngineEvent, EventSink, MAIN_QUEUE_ID, META_PROBE_CONCURRENCY,
    NewTaskSpec, ProxyConfig,
};

#[derive(Default)]
struct ImportSink(Mutex<Vec<EngineEvent>>);

impl EventSink for ImportSink {
    fn emit(&self, event: EngineEvent) {
        self.0.lock().expect("event sink lock").push(event);
    }
}

impl ImportSink {
    fn take(&self) -> Vec<EngineEvent> {
        std::mem::take(&mut *self.0.lock().expect("event sink lock"))
    }
}

async fn fixture() -> (Db, DownloadManager, Arc<ImportSink>) {
    let db = Db::connect("sqlite::memory:")
        .await
        .expect("memory database");
    db.seed_builtin_queues().await.expect("builtin queues");
    let sink = Arc::new(ImportSink::default());
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
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
        },
        sink.clone(),
        Arc::new(crate::NoopSelection),
    )
    .expect("download manager");
    (db, manager, sink)
}

fn occupy_five_slots(manager: &mut DownloadManager) {
    for slot in 0..5 {
        manager.active_tasks.insert(
            format!("occupied-{slot}"),
            ActiveTaskEntry {
                hls_discard: None,
                token: CancellationToken::new(),
                generation: 0,
                handle: None,
                is_bt: false,
                queue_id: MAIN_QUEUE_ID.into(),
            },
        );
    }
}

fn spec(index: usize) -> NewTaskSpec {
    NewTaskSpec {
        url: format!("http://127.0.0.1:1/import-{index}.bin"),
        save_dir: std::env::temp_dir().to_string_lossy().into_owned(),
        file_name: format!("import-{index}.bin"),
        // The creation tests must neither spawn a metadata probe nor perform I/O.
        hint_file_size: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn paused_creation_fact_matches_every_persisted_field() {
    let (db, mut manager, sink) = fixture().await;
    let id = manager
        .create_task(NewTaskSpec {
            segments: 3,
            checksum: format!("sha256={}", "0".repeat(64)),
            ignore_tls_errors: true,
            proxy_url: "http://127.0.0.1:8123".into(),
            referrer: "https://example.test/source".into(),
            start_paused: true,
            ..spec(0)
        })
        .await
        .expect("create paused task");
    let persisted = db
        .load_task_by_id(&id)
        .await
        .expect("read task")
        .expect("task");
    let events = sink.take();
    let EngineEvent::TaskChanged(created) = events.first().expect("creation event") else {
        panic!("complete task fact must precede any progress or metadata");
    };
    assert_eq!(created.as_ref(), &persisted);
    assert_eq!(created.status, 2);
    assert_eq!(created.queue_order, 1);
    assert_eq!(created.queue_id, MAIN_QUEUE_ID);
    assert_eq!(created.segments, 3);
    assert!(created.ignore_tls_errors);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EngineEvent::TasksSnapshot(_)))
    );
    assert!(manager.active_tasks.is_empty());
    assert!(manager.pending_queue.is_empty());
}

#[tokio::test]
async fn rejected_resolver_creation_never_publishes_pending_fact() {
    let (_db, mut manager, sink) = fixture().await;
    let id = manager
        .create_task(NewTaskSpec {
            resolver_item: "unavailable-selection".into(),
            ..spec(0)
        })
        .await
        .expect("persist failed task");
    let events = sink.take();
    let EngineEvent::TaskChanged(created) = events.first().expect("creation event") else {
        panic!("missing complete creation fact");
    };
    assert_eq!(created.task_id, id);
    assert_eq!(created.status, 4);
    assert!(events.iter().all(|event| match event {
        EngineEvent::TaskChanged(task) => task.status == 4,
        EngineEvent::TaskProgress { status, .. } => *status == 4,
        EngineEvent::TasksSnapshot(_) => false,
        _ => true,
    }));
    assert!(manager.active_tasks.is_empty());
}

#[tokio::test]
async fn queued_import_coalesces_positions_and_pause_stops_every_queued_task() {
    let (db, mut manager, sink) = fixture().await;
    occupy_five_slots(&mut manager);
    let mut ids = Vec::new();
    for index in 0..128 {
        ids.push(
            manager
                .create_task(spec(index))
                .await
                .expect("create queued task"),
        );
    }
    let events = sink.take();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, EngineEvent::TaskChanged(_)))
            .count(),
        128
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        EngineEvent::TasksSnapshot(_) | EngineEvent::QueuePositionsChanged(_)
    )));
    assert_eq!(manager.active_tasks.len(), 5);
    assert_eq!(manager.max_concurrent, 5);
    assert_eq!(manager.pending_queue.len(), 128);
    let queue_storage = manager.pending_queue.as_slices().0.as_ptr();
    manager.drain_queue().await;
    assert_eq!(
        manager.pending_queue.as_slices().0.as_ptr(),
        queue_storage,
        "a full scheduler must not rebuild the waiting queue"
    );
    assert!(sink.take().is_empty());

    manager.flush_queue_positions();
    let positions = sink.take();
    let [EngineEvent::QueuePositionsChanged(positions)] = positions.as_slice() else {
        panic!("one coalesced complete position snapshot expected");
    };
    assert_eq!(positions.len(), ids.len());
    for (index, position) in positions.iter().enumerate() {
        assert_eq!(position.task_id, ids[index]);
        assert_eq!(position.position, (index + 1) as i32);
    }
    manager.flush_queue_positions();
    assert!(
        sink.take().is_empty(),
        "unchanged positions must not be rebroadcast"
    );

    manager.batch_pause_all(&ids).await;
    assert!(manager.pending_queue.is_empty());
    assert!(
        db.load_all_tasks()
            .await
            .expect("paused rows")
            .iter()
            .all(|task| task.status == 2)
    );
    assert!(
        sink.take().iter().any(|event| matches!(event,
            EngineEvent::QueuePositionsChanged(positions) if positions.is_empty()
        )),
        "pause must immediately clear stale positions"
    );
    manager.flush_queue_positions();
    assert!(sink.take().is_empty());
}

#[tokio::test]
async fn queue_drain_refills_all_five_configured_slots_not_one() {
    let (_db, mut manager, _sink) = fixture().await;
    let directory = std::env::temp_dir().join(format!("fluxdown-import-slots-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("isolated download directory");
    occupy_five_slots(&mut manager);
    let mut ids = Vec::new();
    for index in 0..8 {
        ids.push(
            manager
                .create_task(NewTaskSpec {
                    save_dir: directory.to_string_lossy().into_owned(),
                    ..spec(index)
                })
                .await
                .expect("create queued task"),
        );
    }
    manager.active_tasks.clear();
    manager.drain_queue().await;
    assert_eq!(manager.max_concurrent, 5);
    assert_eq!(manager.active_tasks.len(), 5);
    assert_eq!(manager.pending_queue.len(), 3);
    assert!(
        ids[..5]
            .iter()
            .all(|id| manager.active_tasks.contains_key(id))
    );
    assert_eq!(
        manager.pending_queue.front().expect("pending head").task_id,
        ids[5]
    );
    // The spawned workers are real downloader tasks; join cancellation before cleanup.
    manager.shutdown().await;
    std::fs::remove_dir_all(directory).expect("remove isolated download directory");
}

fn created(outcomes: &[BatchItemOutcome]) -> Vec<String> {
    outcomes
        .iter()
        .map(|outcome| match outcome {
            BatchItemOutcome::Created(id) => id.clone(),
            BatchItemOutcome::Rejected(reason) => panic!("unexpected rejection: {reason}"),
        })
        .collect()
}

/// 覆盖落库行每一类附加列的规格；全部「稍后下载」，不启动、不探测。
fn rich_spec(index: usize) -> NewTaskSpec {
    use base64::Engine as _;
    let base = NewTaskSpec {
        start_paused: true,
        ..spec(index)
    };
    match index {
        0 => base,
        1 => NewTaskSpec {
            user_agent: "Batch-UA/1.0".into(),
            cookies: "sid=1".into(),
            referrer: "https://example.test/ref".into(),
            extra_headers: [("X-Token".to_owned(), "abc".to_owned())].into(),
            checksum: format!("sha256={}", "0".repeat(64)),
            ignore_tls_errors: true,
            proxy_url: "http://127.0.0.1:8123".into(),
            segments: 4,
            ..base
        },
        2 => NewTaskSpec {
            url: format!(
                "thunder://{}",
                base64::engine::general_purpose::STANDARD
                    .encode("AAhttp://127.0.0.1:1/thunder.binZZ")
            ),
            ..base
        },
        3 => NewTaskSpec {
            file_name: "sub/dir/nested.bin".into(),
            group_id: "group-1".into(),
            unattended_selection: true,
            audio_url: Some("http://127.0.0.1:1/audio.m4a".into()),
            ..base
        },
        _ => NewTaskSpec {
            // hint 任务：名字为空，创建行直接携带 URL 推断的占位名。
            file_name: String::new(),
            hint_file_size: 5,
            ..base
        },
    }
}

/// 同一规格走批量与逐条创建，持久化事实逐字段一致（task_id / created_at 除外）。
#[tokio::test]
async fn batch_creation_matches_single_creation_field_for_field() {
    let (batch_db, mut batch_manager, batch_sink) = fixture().await;
    let (single_db, mut single_manager, _single_sink) = fixture().await;

    let outcomes = batch_manager
        .create_tasks_batch("parity", (0..5).map(rich_spec).collect())
        .await
        .expect("batch create");
    let batch_ids = created(&outcomes);
    let mut single_ids = Vec::new();
    for index in 0..5 {
        single_ids.push(
            single_manager
                .create_task(rich_spec(index))
                .await
                .expect("single create"),
        );
    }

    for (batch_id, single_id) in batch_ids.iter().zip(&single_ids) {
        let mut batch_task = batch_db
            .load_task_by_id(batch_id)
            .await
            .expect("load batch task")
            .expect("batch task");
        let mut single_task = single_db
            .load_task_by_id(single_id)
            .await
            .expect("load single task")
            .expect("single task");
        for task in [&mut batch_task, &mut single_task] {
            task.task_id.clear();
            task.created_at.clear();
        }
        assert_eq!(batch_task, single_task, "task {batch_id}");
        // 请求头 JSON 的键序取决于 HashMap 迭代顺序，按解析后的值比较。
        let context = |context: Option<(String, String, String)>| {
            context.map(|(cookies, referrer, headers)| {
                (
                    cookies,
                    referrer,
                    serde_json::from_str::<serde_json::Value>(&headers).ok(),
                )
            })
        };
        assert_eq!(
            context(
                batch_db
                    .load_task_request_context(batch_id)
                    .await
                    .expect("context")
            ),
            context(
                single_db
                    .load_task_request_context(single_id)
                    .await
                    .expect("context")
            ),
        );
        assert_eq!(
            batch_db.load_audio_url(batch_id).await.expect("audio"),
            single_db.load_audio_url(single_id).await.expect("audio"),
        );
    }
    // 抽查关键事实，防止两条路径同时退化而互相一致。
    let rich = batch_db
        .load_task_by_id(&batch_ids[1])
        .await
        .expect("load")
        .expect("task");
    assert_eq!(
        (rich.status, rich.segments, rich.ignore_tls_errors),
        (2, 4, true)
    );
    let thunder = batch_db
        .load_task_by_id(&batch_ids[2])
        .await
        .expect("load")
        .expect("task");
    assert_eq!(thunder.url, "http://127.0.0.1:1/thunder.bin");
    assert!(thunder.origin_url.starts_with("thunder://"));
    let nested = batch_db
        .load_task_by_id(&batch_ids[3])
        .await
        .expect("load")
        .expect("task");
    assert_eq!(nested.file_name, "nested.bin");
    assert!(nested.save_dir.ends_with("dir"));
    assert_eq!(nested.group_id, "group-1");
    let provisional = batch_db
        .load_task_by_id(&batch_ids[4])
        .await
        .expect("load")
        .expect("task");
    assert_eq!(provisional.file_name, "import-4.bin");

    // 一条整批事实，内容 = 落库后读回的行。
    let events = batch_sink.take();
    let [EngineEvent::TasksChanged(facts), ..] = events.as_slice() else {
        panic!("first event must be the whole-batch creation fact");
    };
    assert_eq!(facts.len(), 5);
    for (fact, id) in facts.iter().zip(&batch_ids) {
        let persisted = batch_db
            .load_task_by_id(id)
            .await
            .expect("load")
            .expect("task");
        assert_eq!(fact, &persisted);
    }
}

#[tokio::test]
async fn batch_continues_the_queue_order_tail_per_queue() {
    let (db, mut manager, _sink) = fixture().await;
    manager
        .create_task(NewTaskSpec {
            start_paused: true,
            ..spec(0)
        })
        .await
        .expect("existing tail task");
    let specs: Vec<_> = (1..=5)
        .map(|index| NewTaskSpec {
            start_paused: true,
            queue_id: if index % 2 == 0 {
                "q2".into()
            } else {
                String::new()
            },
            ..spec(index)
        })
        .collect();
    let ids = created(
        &manager
            .create_tasks_batch("order", specs)
            .await
            .expect("batch create"),
    );
    let mut orders = Vec::new();
    for id in &ids {
        let task = db.load_task_by_id(id).await.expect("load").expect("task");
        orders.push((task.queue_id, task.queue_order));
    }
    let main = MAIN_QUEUE_ID.to_owned();
    assert_eq!(
        orders,
        [
            (main.clone(), 2),
            ("q2".to_owned(), 1),
            (main.clone(), 3),
            ("q2".to_owned(), 2),
            (main, 4),
        ]
    );
}

#[tokio::test]
async fn replaying_a_batch_id_returns_the_first_results_and_creates_nothing() {
    let (db, mut manager, sink) = fixture().await;
    let specs = || (0..3).map(rich_spec).collect::<Vec<_>>();
    let first = manager
        .create_tasks_batch("replay", specs())
        .await
        .expect("first call");
    assert_eq!(db.load_all_tasks().await.expect("tasks").len(), 3);
    sink.take();

    let replay = manager
        .create_tasks_batch("replay", specs())
        .await
        .expect("replay");
    assert_eq!(replay, first);
    assert_eq!(db.load_all_tasks().await.expect("tasks").len(), 3);
    assert!(sink.take().is_empty(), "a replay publishes nothing");

    // 不同 batch_id 即使内容相同也是新批次。
    let other = manager
        .create_tasks_batch("replay-2", specs())
        .await
        .expect("other batch");
    assert_ne!(other, first);
    assert_eq!(db.load_all_tasks().await.expect("tasks").len(), 6);
}

#[tokio::test]
async fn rejected_items_keep_their_slot_and_do_not_abort_the_batch() {
    let (db, mut manager, _sink) = fixture().await;
    let specs = vec![
        rich_spec(0),
        NewTaskSpec {
            url: "thunder://not-base64!!".into(),
            ..rich_spec(1)
        },
        NewTaskSpec {
            checksum: "md5=zz".into(),
            ..rich_spec(2)
        },
        rich_spec(3),
    ];
    let outcomes = manager
        .create_tasks_batch("rejections", specs)
        .await
        .expect("batch call");
    assert!(matches!(outcomes[0], BatchItemOutcome::Created(_)));
    assert!(matches!(
        outcomes[1],
        BatchItemOutcome::Rejected(CreateRejection::InvalidThunderLink(_))
    ));
    assert!(matches!(
        outcomes[2],
        BatchItemOutcome::Rejected(CreateRejection::InvalidChecksum(_))
    ));
    assert!(matches!(outcomes[3], BatchItemOutcome::Created(_)));
    assert_eq!(db.load_all_tasks().await.expect("tasks").len(), 2);
    // 拒绝项同样进入幂等记录：重放逐位一致（重放内容不参与比较，只认 batch_id）。
    let replay = manager
        .create_tasks_batch("rejections", vec![rich_spec(0)])
        .await
        .expect("replay");
    assert_eq!(replay, outcomes);
}

#[tokio::test]
async fn dedup_resolves_existing_and_in_batch_duplicates_to_the_first_task() {
    let (db, mut manager, _sink) = fixture().await;
    db.set_config("dedup_same_url", "true")
        .await
        .expect("enable dedup");
    let existing = manager
        .create_task(NewTaskSpec {
            start_paused: true,
            ..spec(0)
        })
        .await
        .expect("existing task");
    let duplicate_of_existing = NewTaskSpec {
        url: format!("{}#fragment", spec(0).url),
        start_paused: true,
        ..spec(0)
    };
    let specs = vec![
        duplicate_of_existing,
        NewTaskSpec {
            start_paused: true,
            ..spec(1)
        },
        NewTaskSpec {
            start_paused: true,
            ..spec(1)
        },
        NewTaskSpec {
            start_paused: true,
            ..spec(2)
        },
    ];
    let ids = created(
        &manager
            .create_tasks_batch("dedup", specs)
            .await
            .expect("batch call"),
    );
    assert_eq!(ids[0], existing);
    assert_eq!(
        ids[1], ids[2],
        "in-batch duplicate resolves to the first item"
    );
    assert_ne!(ids[1], ids[3]);
    assert_eq!(db.load_all_tasks().await.expect("tasks").len(), 3);
}

#[tokio::test]
async fn batch_publishes_one_tasks_changed_before_any_progress_and_coalesces_positions() {
    let (_db, mut manager, sink) = fixture().await;
    occupy_five_slots(&mut manager);
    let mut specs: Vec<_> = (0..6).map(spec).collect();
    for index in 6..8 {
        specs.push(NewTaskSpec {
            resolver_item: "unavailable".into(),
            ..spec(index)
        });
    }
    let ids = created(
        &manager
            .create_tasks_batch("events", specs)
            .await
            .expect("batch call"),
    );
    let events = sink.take();
    let Some(EngineEvent::TasksChanged(facts)) = events.first() else {
        panic!("TasksChanged must be the first event");
    };
    assert_eq!(
        facts
            .iter()
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, EngineEvent::TasksChanged(_)))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        EngineEvent::TaskChanged(_)
            | EngineEvent::TasksSnapshot(_)
            | EngineEvent::QueuePositionsChanged(_)
    )));
    let failed = events
        .iter()
        .filter(|event| matches!(event, EngineEvent::TaskProgress { status: 4, .. }))
        .count();
    assert_eq!(failed, 2, "fail-closed items report their error progress");
    assert_eq!(manager.pending_queue.len(), 6);

    manager.flush_queue_positions();
    let positions = sink.take();
    let [EngineEvent::QueuePositionsChanged(positions)] = positions.as_slice() else {
        panic!("one coalesced position broadcast expected");
    };
    assert_eq!(positions.len(), 6);
}

#[tokio::test]
async fn saved_site_credential_is_visible_to_later_items_in_the_same_batch() {
    let (db, mut manager, _sink) = fixture().await;
    let specs = vec![
        NewTaskSpec {
            http_user: "user".into(),
            http_password: "pass".into(),
            save_site_auth: true,
            ..rich_spec(0)
        },
        rich_spec(1),
    ];
    let ids = created(
        &manager
            .create_tasks_batch("site-auth", specs)
            .await
            .expect("batch call"),
    );
    for id in &ids {
        let (_, _, headers) = db
            .load_task_request_context(id)
            .await
            .expect("context")
            .expect("row");
        assert!(headers.contains("Authorization"), "task {id}: {headers}");
    }
    let store = db
        .get_config(crate::site_auth::SITE_AUTH_CONFIG_KEY)
        .await
        .expect("read store")
        .expect("store written once at the end");
    assert!(store.contains("user"));
}

#[tokio::test]
async fn meta_probe_waits_for_a_gate_permit() {
    let (_db, manager, _sink) = fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe target");
    let port = listener.local_addr().expect("local addr").port();
    let permits = manager
        .probe_gate
        .clone()
        .acquire_many_owned(META_PROBE_CONCURRENCY as u32)
        .await
        .expect("hold every permit");
    manager.spawn_meta_probe(
        "probe-task".into(),
        format!("http://127.0.0.1:{port}/file.bin"),
        String::new(),
        crate::downloader::RequestSpec::from_captured(
            None,
            String::new(),
            String::new(),
            Default::default(),
            None,
        ),
        manager.client.clone(),
        ProxyConfig::default(),
        crate::intercept::noop(),
        crate::intercept::TaskCtx {
            id: "probe-task".into(),
            url: format!("http://127.0.0.1:{port}/file.bin"),
            source_url: format!("http://127.0.0.1:{port}/file.bin"),
            file_name: String::new(),
            save_dir: String::new(),
            protocol: "http",
            track_pair: false,
            is_resume: false,
        },
        crate::intercept::RunMeta::default(),
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), listener.accept())
            .await
            .is_err(),
        "no probe may start while the gate is exhausted"
    );
    drop(permits);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .is_ok(),
        "the probe starts once a permit is free"
    );
}

#[tokio::test]
async fn queue_head_dequeue_publishes_only_removed_ids_after_the_first_full_table() {
    let (_db, mut manager, sink) = fixture().await;
    occupy_five_slots(&mut manager);
    let mut ids = Vec::new();
    for index in 0..6 {
        ids.push(manager.create_task(spec(index)).await.expect("queued task"));
    }
    sink.take();
    manager.flush_queue_positions();
    let first = sink.take();
    let [EngineEvent::QueuePositionsChanged(full)] = first.as_slice() else {
        panic!("the first publish is one full table");
    };
    assert!(full.windows(2).all(|pair| pair[0].order < pair[1].order));
    let tail_order = full[5].order;

    manager.pending_queue.pop_front();
    manager.queue_positions_dirty = true;
    manager.flush_queue_positions();
    let events = sink.take();
    let [EngineEvent::QueueOrderChanged { removed, added }] = events.as_slice() else {
        panic!("a head dequeue publishes one incremental event");
    };
    assert_eq!(removed, &[ids[0].clone()]);
    assert!(added.is_empty());

    let extra = manager.create_task(spec(6)).await.expect("tail append");
    sink.take();
    manager.flush_queue_positions();
    let events = sink.take();
    let [EngineEvent::QueueOrderChanged { removed, added }] = events.as_slice() else {
        panic!("a tail append publishes one incremental event");
    };
    assert!(removed.is_empty());
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].task_id, extra);
    assert!(added[0].order > tail_order);
}

#[tokio::test]
async fn bulk_queued_import_probes_at_most_the_window_and_small_imports_probe_every_task() {
    let (_db, mut manager, _sink) = fixture().await;
    occupy_five_slots(&mut manager);
    // 占满闸门：已发起的探测全部停在排队等许可，在途数即为发起数。
    let permits = manager
        .probe_gate
        .clone()
        .acquire_many_owned(META_PROBE_CONCURRENCY as u32)
        .await
        .expect("hold every permit");
    for index in 0..3 {
        manager
            .create_task(NewTaskSpec {
                hint_file_size: 0,
                ..spec(index)
            })
            .await
            .expect("small import");
    }
    assert_eq!(
        manager
            .probes_outstanding
            .load(std::sync::atomic::Ordering::Relaxed),
        3,
        "a handful of tasks keeps the probe-per-task behaviour"
    );
    for index in 3..(super::META_PROBE_WINDOW + 40) {
        manager
            .create_task(NewTaskSpec {
                hint_file_size: 0,
                ..spec(index)
            })
            .await
            .expect("bulk import");
    }
    assert_eq!(
        manager
            .probes_outstanding
            .load(std::sync::atomic::Ordering::Relaxed),
        super::META_PROBE_WINDOW,
        "only the head of the queue is probed"
    );
    drop(permits);
}

#[tokio::test]
async fn bulk_paused_import_keeps_in_flight_probes_within_the_window() {
    let (_db, mut manager, _sink) = fixture().await;
    let permits = manager
        .probe_gate
        .clone()
        .acquire_many_owned(META_PROBE_CONCURRENCY as u32)
        .await
        .expect("hold every permit");
    for index in 0..(super::META_PROBE_WINDOW + 40) {
        manager
            .create_task(NewTaskSpec {
                hint_file_size: 0,
                start_paused: true,
                ..spec(index)
            })
            .await
            .expect("paused import");
    }
    assert_eq!(
        manager
            .probes_outstanding
            .load(std::sync::atomic::Ordering::Relaxed),
        super::META_PROBE_WINDOW
    );
    drop(permits);
}
