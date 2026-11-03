//! 插件系统集成测试：多文件任务组（清单裂变 / 前置预解析 / 建组 / 超阈值）端到端。
//!
//! 覆盖契约 Phase A+B 的四个验收场景（照 `plugin_lazy_resolve.rs` 模板）：
//! - 裂变端到端：初段返回 manifest(2 items) → 引擎自动裂变为组 + 2 成员 →
//!   两文件落盘于 `组根/path/`，组行存在，母任务 task_id 不变。
//! - preview 只读：`begin_resolve_preview` → `ResolvePreviewReady(items=2)`
//!   且不建任何任务行；resolver 未声明 `multi` → 立即无清单（不跑 resolve）。
//! - `create_task_group` + `delete_group`：2 items 建组 → 断言行；删组后
//!   任务+组行清空（gc_empty_groups）。
//! - 超阈值：清单 sizes 合计 > 10GiB → 全成员 status=2（paused），
//!   不触发任何二段 resolve、不发生下载。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::Write as _;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::{CreateGroupSpec, GroupItemSpec, NewTaskSpec};
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

const FILE_BODY: &[u8] = b"fluxdown plugin manifest flow integration payload!!\n";

/// 本地 HTTP/1.1 服务器：支持 HEAD（Content-Length + Accept-Ranges）与 GET（全量），
/// 忽略路径/查询串——二段解析改写出的不同 `?item=` URL 均返回同一固定内容。
fn spawn_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 8192];
            let mut header = String::new();
            loop {
                let n = match std::io::Read::read(&mut stream, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                header.push_str(&String::from_utf8_lossy(&buf[..n]));
                if header.contains("\r\n\r\n") {
                    break;
                }
            }
            let is_head = header.starts_with("HEAD ");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                FILE_BODY.len()
            );

            stream.write_all(resp.as_bytes()).unwrap_or_else(|error| {
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
            if !is_head {
                stream.write_all(FILE_BODY).unwrap_or_else(|error| {
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
            }

            stream.flush().unwrap_or_else(|error| {
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
        }
    });
    (port, handle)
}

/// 写一个两段式清单插件（声明 `resolvers[0].multi=true`）：`ctx.resolverItem`
/// 空 → 从 `manifestJson` 设置解析出清单（校验器 fail-closed 强制其余字段
/// 互斥）；非空 → 返回 `target + "?item=" + resolverItem` 的真实直链
/// （二段解析，`?item=` 查询串被 [`spawn_server`] 忽略）。
async fn write_manifest_plugin(dir: &std::path::Path) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = r#"{
      "identity": "test@multiresolver",
      "name": "Test Multi Resolver",
      "version": "1.0.0",
      "resolvers": [{ "match": { "urls": ["*://share.test/*"] }, "entry": "resolve.js", "timeoutMs": 5000, "multi": true }],
      "settings": [
        { "key": "target", "title": "目标直链前缀", "type": "string", "widget": "text", "default": "" },
        { "key": "manifestJson", "title": "清单 JSON", "type": "string", "widget": "text", "default": "" }
      ]
    }"#;
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    let resolve_js = r#"
      globalThis.resolve = async (ctx) => {
        if (ctx.resolverItem) {
          return { url: flux.settings.target + "?item=" + ctx.resolverItem };
        }
        const m = flux.settings.manifestJson;
        if (!m) return null;
        return { manifest: JSON.parse(m) };
      };
    "#;
    tokio::fs::write(dir.join("resolve.js"), resolve_js)
        .await
        .expect("write resolve.js");
}

/// 写一个**未声明** `multi` 的单文件 resolver 插件：resolve.js 无条件返回一个
/// 非空清单——若前置预解析的 multi 门未生效（bug），preview 会观察到
/// `items.len() == 1`；门生效时 preview 根本不会调用它，直接得到空清单。
async fn write_single_only_plugin(dir: &std::path::Path) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = r#"{
      "identity": "test@singleonly",
      "name": "Test Single Only",
      "version": "1.0.0",
      "resolvers": [{ "match": { "urls": ["*://single.test/*"] }, "entry": "resolve.js", "timeoutMs": 5000 }]
    }"#;
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    let resolve_js = r#"
      globalThis.resolve = async (ctx) => {
        return { manifest: { name: "should-not-be-called", items: [{ id: "x", name: "x.bin", path: "" }] } };
      };
    "#;
    tokio::fs::write(dir.join("resolve.js"), resolve_js)
        .await
        .expect("write resolve.js");
}

fn engine_config(work: &std::path::Path) -> EngineConfig {
    EngineConfig {
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
    }
}

async fn create(engine: &mut Engine, url: &str, save_dir: &str, name: &str) -> String {
    engine
        .manager
        .create_task(NewTaskSpec {
            url: url.to_string(),
            save_dir: save_dir.to_string(),
            file_name: name.to_string(),
            segments: 1,
            ..Default::default()
        })
        .await
        .expect("create_task returns id")
}

/// 简易唯一后缀（避免引入 uuid 到测试；进程 id + 纳秒时间戳）。
fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

/// 把 [`EngineEvent`] 转发到无界通道的测试专用 sink（照 scout §7 模板：
/// 「ChannelSink 捕获事件」），供 `begin_resolve_preview` 这类不建任务/不写库
/// 的只读流程做断言——`NoopSink` 只打日志，无法在测试里观察结果。
struct ChannelSink(tokio::sync::mpsc::UnboundedSender<EngineEvent>);

impl EventSink for ChannelSink {
    fn emit(&self, event: EngineEvent) {
        if self.0.send(event).is_err() {
            tracing::debug!("test event receiver was dropped after observation");
        }
    }
}

/// 从事件通道中拿到下一个 `ResolvePreviewReady`，跳过其余事件；超时 panic。
async fn recv_preview_ready(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
) -> (String, usize, String) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("preview event timeout")
            .expect("event channel closed");
        if let EngineEvent::ResolvePreviewReady {
            name, items, error, ..
        } = event
        {
            return (name, items.len(), error);
        }
    }
}

/// 场景 1：裂变端到端。单任务命中 resolver → 初段返回 2-item 清单 → 引擎
/// 自动裂变为组（母任务原地改写 + 1 个兄弟任务），二段 resolve 各自取得
/// 真实直链并完成下载；两文件落盘于 `组根/vids/`，组行存在，母任务 ID 不变。
async fn run_fission_download(manifest_name: &str, expected_group_name: &str) {
    let work = std::env::temp_dir().join(format!("fluxdown-manifest-it-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real");

    let plugin_src = work.join("plugin_src");
    write_manifest_plugin(&plugin_src).await;

    let mut engine = Engine::new(
        engine_config(&work),
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");

    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&plugin_src).await.expect("install");
    let manifest_json = serde_json::json!({
        "name": manifest_name,
        "items": [
            {"id":"f1","name":"a.mp4","path":"vids","size":100},
            {"id":"f2","name":"b.mp4","path":"vids","size":200}
        ]
    });
    pm.update_settings(
        "test@multiresolver",
        &[
            ("target".to_string(), real_url.clone()),
            ("manifestJson".to_string(), manifest_json.to_string()),
        ],
    )
    .await
    .expect("set settings");

    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let save = work.to_string_lossy().into_owned();
    let mother_tid = create(&mut engine, "http://share.test/album/xyz", &save, "").await;

    // 驱动：初段 resolve(manifest) → on_resolve_ready 触发裂变（内部同步
    // spawn 母任务 + 兄弟任务各自的二段 resolve worker）→ 继续泵 resolve/done
    // 直到两个任务都完成下载。
    let mut done_count = 0u32;
    while done_count < 2 {
        tokio::select! {
            Some(out) = resolve_rx.recv() => {
                engine.manager.on_resolve_ready(out).await;
            }
            Some(done) = done_rx.recv() => {
                engine.manager.on_task_done(&done).await;
                done_count += 1;
            }
            _ = tokio::time::sleep(Duration::from_secs(20)) => {
                panic!("timeout waiting for fission downloads (done_count={done_count})");
            }
        }
    }

    // 组行存在，母任务 task_id 不变，成员数=2。
    let groups = engine.db.load_all_groups().await.expect("load groups");
    assert_eq!(groups.len(), 1, "exactly one group must be created");
    let group = &groups[0];
    assert_eq!(group.name, expected_group_name);
    assert_eq!(group.source_url, "http://share.test/album/xyz");

    let member_ids = engine
        .db
        .group_member_ids(&group.group_id)
        .await
        .expect("member ids");
    assert_eq!(member_ids.len(), 2, "group must have exactly 2 members");
    assert!(
        member_ids.contains(&mother_tid),
        "mother task_id must be preserved (in-place rewrite, not a new row)"
    );

    // 两文件落盘于 组根/vids/，内容正确。
    let a_path = work.join(expected_group_name).join("vids").join("a.mp4");
    let b_path = work.join(expected_group_name).join("vids").join("b.mp4");
    assert_eq!(
        tokio::fs::read(&a_path).await.expect("read a.mp4"),
        FILE_BODY
    );
    assert_eq!(
        tokio::fs::read(&b_path).await.expect("read b.mp4"),
        FILE_BODY
    );

    // 母任务行本身也确实完成了改写（group_id/文件名/落盘目标）。
    let mother = engine
        .db
        .load_task_by_id(&mother_tid)
        .await
        .expect("load mother")
        .expect("mother exists");
    assert_eq!(mother.group_id, group.group_id);
    assert!(mother.file_name == "a.mp4" || mother.file_name == "b.mp4");

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fission_end_to_end_creates_group_and_downloads_members() {
    run_fission_download("share_bundle", "share_bundle").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unnamed_manifest_group_uses_first_member_name_as_its_root() {
    run_fission_download("", "a.mp4").await;
}

/// 场景 2：`begin_resolve_preview` 只读——命中声明 `multi=true` 的 resolver
/// 时返回清单但不建任务/不写库；未声明 `multi` 的 resolver 即使 URL glob
/// 命中也不会被调用（立即无清单）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_is_read_only_and_gated_by_multi_declaration() {
    let work = std::env::temp_dir().join(format!("fluxdown-manifest-it-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real");

    let multi_plugin_src = work.join("plugin_multi");
    write_manifest_plugin(&multi_plugin_src).await;
    let single_plugin_src = work.join("plugin_single");
    write_single_only_plugin(&single_plugin_src).await;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = Engine::new(
        engine_config(&work),
        Arc::new(ChannelSink(tx)),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");

    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&multi_plugin_src)
        .await
        .expect("install multi");
    pm.install_from_dir(&single_plugin_src)
        .await
        .expect("install single");
    let manifest_json = r#"{"name":"预览清单","items":[
        {"id":"p1","name":"x.bin","path":"","size":10},
        {"id":"p2","name":"y.bin","path":"","size":20}
    ]}"#;
    pm.update_settings(
        "test@multiresolver",
        &[
            ("target".to_string(), real_url.clone()),
            ("manifestJson".to_string(), manifest_json.to_string()),
        ],
    )
    .await
    .expect("set settings");

    // 命中 multi resolver：返回真实清单。
    engine
        .manager
        .begin_resolve_preview(
            "preview-1".to_string(),
            "http://share.test/album/preview".to_string(),
            String::new(),
            String::new(),
            String::new(),
            HashMap::new(),
        )
        .await;
    let (name, item_count, error) = recv_preview_ready(&mut rx).await;
    assert_eq!(name, "预览清单");
    assert_eq!(item_count, 2);
    assert!(error.is_empty());

    // 未声明 multi 的 resolver：即使 URL glob 命中，也不得被调用——立即无清单。
    engine
        .manager
        .begin_resolve_preview(
            "preview-2".to_string(),
            "http://single.test/x".to_string(),
            String::new(),
            String::new(),
            String::new(),
            HashMap::new(),
        )
        .await;
    let (_, item_count2, error2) = recv_preview_ready(&mut rx).await;
    assert_eq!(
        item_count2, 0,
        "non-multi resolver must not be invoked even on URL match"
    );
    assert!(error2.is_empty());

    // 预解析纯只读：不建任务、不写库。
    assert!(
        engine
            .db
            .load_all_tasks()
            .await
            .expect("load tasks")
            .is_empty(),
        "preview must not create any task row"
    );
    assert!(
        engine
            .db
            .load_all_groups()
            .await
            .expect("load groups")
            .is_empty(),
        "preview must not create any group row"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 场景 3：`create_task_group` 建组 + `delete_group` 删组（GC 回收组行）。
/// 用 `start_paused` 避免真实网络下载，聚焦组/任务行的建立与清理。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_task_group_then_delete_group_cleans_up_rows() {
    let work = std::env::temp_dir().join(format!("fluxdown-manifest-it-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real");

    let mut engine = Engine::new(
        engine_config(&work),
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");

    let save = work.to_string_lossy().into_owned();
    let group_id = engine
        .manager
        .create_task_group(CreateGroupSpec {
            source_url: real_url.clone(),
            group_name: "manual_group".to_string(),
            base_save_dir: save,
            segments: 1,
            start_paused: true,
            items: vec![
                GroupItemSpec {
                    resolver_item: String::new(),
                    file_name: "one.bin".to_string(),
                    rel_path: String::new(),
                    size: 0,
                },
                GroupItemSpec {
                    resolver_item: String::new(),
                    file_name: "two.bin".to_string(),
                    rel_path: String::new(),
                    size: 0,
                },
            ],
            ..Default::default()
        })
        .await
        .expect("create_task_group returns id");

    // 断言：组行 + 2 成员任务存在，且都归属本组。
    let groups = engine.db.load_all_groups().await.expect("load groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].group_id, group_id);
    assert_eq!(groups[0].name, "manual_group");

    let member_ids = engine
        .db
        .group_member_ids(&group_id)
        .await
        .expect("member ids");
    assert_eq!(member_ids.len(), 2);

    let all_tasks = engine.db.load_all_tasks().await.expect("load tasks");
    assert_eq!(all_tasks.len(), 2);
    for t in &all_tasks {
        assert_eq!(t.group_id, group_id);
        assert_eq!(t.status, 2, "start_paused members must land as paused(2)");
    }

    // 删组：批量删成员 + gc_empty_groups 回收组行。
    let outcome = engine
        .manager
        .delete_group(&group_id, false)
        .await
        .expect("delete group");
    assert_eq!(outcome.failed, 0);

    assert!(
        engine
            .db
            .load_all_tasks()
            .await
            .expect("load tasks")
            .is_empty(),
        "delete_group must remove every member task row"
    );
    assert!(
        engine
            .db
            .load_all_groups()
            .await
            .expect("load groups")
            .is_empty(),
        "delete_group must GC the now-empty group row"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 场景 4：清单总大小超过 `FISSION_AUTO_START_MAX_TOTAL_BYTES`（10GiB）阈值
/// → 全体成员（含母任务）静默转 paused(2)，不触发任何二段 resolve、不发生
/// 下载。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fission_over_threshold_pauses_all_members_without_downloading() {
    let work = std::env::temp_dir().join(format!("fluxdown-manifest-it-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real");

    let plugin_src = work.join("plugin_src");
    write_manifest_plugin(&plugin_src).await;

    let mut engine = Engine::new(
        engine_config(&work),
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");

    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&plugin_src).await.expect("install");

    const HUGE_BYTES: i64 = 6 * 1024 * 1024 * 1024; // 6 GiB per item, 12 GiB total > 10 GiB threshold.
    let manifest_json = format!(
        r#"{{"name":"大清单","items":[
            {{"id":"h1","name":"big1.bin","path":"","size":{HUGE_BYTES}}},
            {{"id":"h2","name":"big2.bin","path":"","size":{HUGE_BYTES}}}
        ]}}"#
    );
    pm.update_settings(
        "test@multiresolver",
        &[
            ("target".to_string(), real_url.clone()),
            ("manifestJson".to_string(), manifest_json),
        ],
    )
    .await
    .expect("set settings");

    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let save = work.to_string_lossy().into_owned();
    let mother_tid = create(&mut engine, "http://share.test/album/huge", &save, "").await;

    // 仅需一次 resolve（初段清单）；超阈值裂变检测到后不会再发起二段 resolve。
    let out = tokio::time::timeout(Duration::from_secs(10), resolve_rx.recv())
        .await
        .expect("resolve timeout")
        .expect("resolve channel closed");
    engine.manager.on_resolve_ready(out).await;

    // 短暂等待确认没有后续 resolve / 下载在途（channel 应保持空）。
    let extra_resolve = tokio::time::timeout(Duration::from_millis(300), resolve_rx.recv()).await;
    assert!(
        extra_resolve.is_err(),
        "over-threshold fission must not trigger any second-stage resolve"
    );
    let extra_done = tokio::time::timeout(Duration::from_millis(300), done_rx.recv()).await;
    assert!(
        extra_done.is_err(),
        "over-threshold fission must not start any download"
    );

    let groups = engine.db.load_all_groups().await.expect("load groups");
    assert_eq!(groups.len(), 1);
    let member_ids = engine
        .db
        .group_member_ids(&groups[0].group_id)
        .await
        .expect("member ids");
    assert_eq!(member_ids.len(), 2);
    assert!(member_ids.contains(&mother_tid));

    for id in &member_ids {
        let t = engine
            .db
            .load_task_by_id(id)
            .await
            .expect("load member")
            .expect("member exists");
        assert_eq!(t.status, 2, "every member (incl. mother) must be paused");
        assert_eq!(t.downloaded_bytes, 0, "no download must have started");
    }

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

/// 设置校验失败携带失败字段的 key（含未知键），宿主据此把错误归因到具体字段。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_settings_rejects_with_field_key() {
    use fluxdown_engine::plugin::PluginError;

    let work = std::env::temp_dir().join(format!("fluxdown-setting-key-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let plugin_src = work.join("plugin_src");
    write_manifest_plugin(&plugin_src).await;
    let engine = Engine::new(
        engine_config(&work),
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");
    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&plugin_src).await.expect("install");

    let err = pm
        .update_settings(
            "test@multiresolver",
            &[
                ("target".to_string(), "http://x/".to_string()),
                ("noSuchKey".to_string(), "1".to_string()),
            ],
        )
        .await
        .expect_err("unknown key must be rejected");
    assert!(
        matches!(&err, PluginError::InvalidSetting { key, .. } if key == "noSuchKey"),
        "got {err:?}"
    );

    if let Err(error) = tokio::fs::remove_dir_all(&work).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("best-effort test directory cleanup: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_archive_groups_preserve_paths_and_explicit_directory_choice() -> std::io::Result<()>
{
    use fluxdown_engine::category_route::{CategoryRoute, CategoryRule};
    use fluxdown_engine::model::ArchiveMetadata;

    let work = std::env::temp_dir().join(format!("fluxdown-source-archive-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("create work");
    let mut engine = Engine::new(
        engine_config(&work),
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");
    let archive_dir = work.join("archive-plugin");
    tokio::fs::create_dir_all(&archive_dir)
        .await
        .expect("archive directory");
    tokio::fs::write(
        archive_dir.join("manifest.json"),
        r#"{"identity":"test@source-archive","name":"Source archive","version":"1.0.0","permissions":["archive"],"contributes":{"archive":"archive.js"}}"#,
    )
    .await
    .expect("archive manifest");
    tokio::fs::write(
        archive_dir.join("archive.js"),
        "globalThis.archive = ctx => ctx.isGroup && ctx.metadata?.creatorName ? {folderName:ctx.metadata.creatorName} : null;",
    )
    .await
    .expect("archive source");
    let pm = engine.manager.plugin_manager().expect("plugin manager");
    let identity = pm
        .install_from_dir(&archive_dir)
        .await
        .expect("archive install");
    pm.grant_consent(&identity, &["archive".to_owned()], &[])
        .await
        .expect("archive consent");
    pm.set_enabled(&identity, true)
        .await
        .expect("archive enable");
    let video_dir = work.join("videos");
    let route = CategoryRoute {
        base_dir: work.to_string_lossy().into_owned(),
        rules: vec![CategoryRule {
            extensions: vec!["mp4".to_owned()],
            save_dir: video_dir.to_string_lossy().into_owned(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let metadata = ArchiveMetadata {
        platform: "source".to_owned(),
        media_kind: "post".to_owned(),
        creator_id: "42".to_owned(),
        creator_name: "Creator".to_owned(),
        ..Default::default()
    };
    let make_spec = |name: &str, authorized: bool| CreateGroupSpec {
        source_url: "http://share.test/album/source".to_owned(),
        group_name: name.to_owned(),
        base_save_dir: work.join("manual").to_string_lossy().into_owned(),
        archive_metadata: Some(metadata.clone()),
        category_route: authorized.then(|| route.clone()),
        start_paused: true,
        items: vec![GroupItemSpec {
            file_name: "one.mp4".to_owned(),
            rel_path: "extras".to_owned(),
            ..Default::default()
        }],
        ..Default::default()
    };
    for (name, authorized, expected) in [
        ("Playlist", true, video_dir.join("Creator").join("Playlist")),
        ("Creator", true, video_dir.join("Creator")),
        ("Custom", false, work.join("manual").join("Custom")),
    ] {
        let id = engine
            .manager
            .create_task_group(make_spec(name, authorized))
            .await
            .expect("create group");
        let groups = engine.db.load_all_groups().await.expect("groups");
        let group = groups
            .iter()
            .find(|group| group.group_id == id)
            .expect("group");
        assert_eq!(std::path::Path::new(&group.save_dir), expected);
        let tasks = engine.db.load_all_tasks().await.expect("tasks");
        let member = tasks
            .iter()
            .find(|task| task.group_id == id)
            .expect("member");
        assert_eq!(
            std::path::Path::new(&member.save_dir),
            expected.join("extras")
        );
        assert_eq!(member.status, 2);
    }

    let resolver_dir = work.join("resolver-plugin");
    write_manifest_plugin(&resolver_dir).await;
    pm.install_from_dir(&resolver_dir)
        .await
        .expect("resolver install");
    let manifest = serde_json::json!({
        "name":"Automatic",
        "archiveMetadata": metadata,
        "items":[
            {"id":"a","name":"a.mp4","path":"parts","size":11000000000_i64},
            {"id":"b","name":"b.mp4","path":"parts","size":11000000000_i64}
        ]
    });
    pm.update_settings(
        "test@multiresolver",
        &[("manifestJson".to_owned(), manifest.to_string())],
    )
    .await
    .expect("resolver settings");
    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve channel");
    let mother = engine
        .manager
        .create_task(NewTaskSpec {
            url: "http://share.test/album/source".to_owned(),
            save_dir: work.to_string_lossy().into_owned(),
            category_route: Some(route),
            ..Default::default()
        })
        .await
        .expect("create source task");
    let outcome = tokio::time::timeout(Duration::from_secs(10), resolve_rx.recv())
        .await
        .expect("resolve timeout")
        .expect("resolve outcome");
    engine.manager.on_resolve_ready(outcome).await;
    let automatic = video_dir.join("Creator").join("Automatic");
    let persisted = engine
        .db
        .load_task_by_id(&mother)
        .await
        .expect("load mother")
        .expect("mother");
    assert_eq!(
        std::path::Path::new(&persisted.save_dir),
        automatic.join("parts")
    );
    assert_eq!(persisted.status, 2);
    drop(pm);
    drop(engine);

    let restarted = Engine::new(
        engine_config(&work),
        Arc::new(NoopSink),
        Arc::new(NoopSelection),
    )
    .await
    .expect("restart engine");
    let persisted = restarted
        .db
        .load_task_by_id(&mother)
        .await
        .expect("reload mother")
        .expect("mother after restart");
    assert_eq!(
        std::path::Path::new(&persisted.save_dir),
        automatic.join("parts")
    );
    let groups = restarted.db.load_all_groups().await.expect("reload groups");
    assert!(
        groups
            .iter()
            .any(|group| std::path::Path::new(&group.save_dir) == automatic)
    );
    drop(restarted);
    // QuickJS shuts its worker runtime down in the background; its audit writer may
    // briefly retain SQLite handles on Windows after the last engine is dropped.
    let mut cleanup_attempt = 0;
    while let Err(error) = tokio::fs::remove_dir_all(&work).await {
        if !cfg!(windows) || !matches!(error.raw_os_error(), Some(32 | 33)) || cleanup_attempt == 2
        {
            return Err(error);
        }
        cleanup_attempt += 1;
        tokio::time::sleep(Duration::from_secs(1 << cleanup_attempt)).await;
    }
    Ok(())
}
