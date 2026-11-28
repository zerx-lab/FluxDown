//! 插件状态新鲜度：预览→任务组的插件绑定、从未起跑任务跟随当前路由、零候选保持待选。
//!
//! - 预览结果携带产出清单的插件 ID，建组时按它绑定成员（即使字典序更靠前的非 `multi`
//!   插件也命中同一链接）；该插件已不可用 → 成员 fail-closed（status 4）；旧客户端
//!   （ID 为空）按预览同款的 multi-only 规则选插件。
//! - 从未起跑的任务：绑定的插件被禁用而其它插件命中 → 按当前路由改绑并下载；已有
//!   进度的任务维持 fail-closed。
//! - 没有任何候选时 `resolver_choice_pending` 保持，插件重新启用后的下一次起跑再定案。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::Write as _;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::{CreateGroupSpec, GroupItemSpec, NewTaskSpec};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

const FILE_BODY: &[u8] = b"fluxdown plugin state freshness payload\n";
const SHARE_URL: &str = "http://share.test/album/xyz";

type Requests = Arc<Mutex<Vec<String>>>;

fn spawn_server() -> (u16, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    std::thread::spawn(move || {
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
            if let Some(path) = header.split_whitespace().nth(1) {
                log.lock().expect("request log").push(path.to_owned());
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                FILE_BODY.len()
            );
            // 测试服务器：客户端提前断开属正常，写失败不影响断言。
            if stream.write_all(resp.as_bytes()).is_ok() && !header.starts_with("HEAD ") {
                stream.write_all(FILE_BODY).unwrap_or_default();
            }
        }
    });
    (port, requests)
}

/// 写一个 resolver 插件：`urls` 为匹配 glob，`multi` 决定是否声明 `"multi": true`，
/// `js` 为 resolve.js 全文。
async fn write_plugin(dir: &std::path::Path, identity: &str, urls: &str, multi: bool, js: &str) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let multi = if multi { r#", "multi": true"# } else { "" };
    let manifest = format!(
        r#"{{
      "identity": "{identity}",
      "name": "Plugin {identity}",
      "version": "1.0.0",
      "resolvers": [{{ "match": {{ "urls": ["{urls}"] }}, "entry": "resolve.js", "timeoutMs": 5000{multi} }}]
    }}"#
    );
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    tokio::fs::write(dir.join("resolve.js"), js)
        .await
        .expect("write resolve.js");
}

struct Fixture {
    work: std::path::PathBuf,
    port: u16,
    /// 测试服务器收到的请求路径。
    requests: Requests,
    engine: Engine,
    resolve_rx:
        tokio::sync::mpsc::UnboundedReceiver<fluxdown_engine::download_manager::ResolveOutcome>,
    done_rx: tokio::sync::mpsc::Receiver<fluxdown_engine::download_manager::TaskDone>,
}

impl Fixture {
    async fn new() -> Self {
        let work = std::env::temp_dir().join(format!(
            "fluxdown-plugin-freshness-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&work).await.expect("mkdir work");
        let (port, requests) = spawn_server();
        let mut engine = Engine::new(
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
            },
            Arc::new(NoopSink),
            Arc::new(NoopSelection),
        )
        .await
        .expect("engine");
        let resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
        let done_rx = engine.manager.take_done_rx().expect("done_rx");
        Self {
            work,
            port,
            requests,
            engine,
            resolve_rx,
            done_rx,
        }
    }

    fn page_url(&self) -> String {
        format!("http://127.0.0.1:{}/page", self.port)
    }

    /// 命中本机 `/page*` 的单文件 resolver，解析为 `/<identity>.bin`。
    async fn install_page_plugin(&self, identity: &str) {
        let src = self.work.join(format!("src-{identity}"));
        let js = format!(
            "globalThis.resolve = async () => ({{ url: 'http://127.0.0.1:{}/{identity}.bin' }});",
            self.port
        );
        write_plugin(
            &src,
            identity,
            &format!("http://127.0.0.1:{}/page*", self.port),
            false,
            &js,
        )
        .await;
        self.engine
            .manager
            .plugin_manager()
            .expect("plugin manager")
            .install_from_dir(&src)
            .await
            .expect("install");
    }

    /// 命中 `share.test` 的清单插件；`multi=false` 的版本若被调用会返回清单（用于证明
    /// 预览 / 建组确实绕过了它）。二段解析指向本机测试服务器。
    async fn install_share_plugin(&self, identity: &str, multi: bool) {
        let src = self.work.join(format!("src-{identity}"));
        let js = r#"
          globalThis.resolve = async (ctx) => {
            if (ctx.resolverItem) return { url: 'http://127.0.0.1:PORT/resolved?item=' + ctx.resolverItem };
            return { manifest: { name: 'bundle', items: [
              { id: 'f1', name: 'a.bin', path: '', size: 1 },
              { id: 'f2', name: 'b.bin', path: '', size: 1 }
            ] } };
          };
        "#
        .replace("PORT", &self.port.to_string());
        write_plugin(&src, identity, "*://share.test/*", multi, &js).await;
        self.engine
            .manager
            .plugin_manager()
            .expect("plugin manager")
            .install_from_dir(&src)
            .await
            .expect("install");
    }

    async fn create_paused(&mut self, name: &str) -> String {
        self.engine
            .manager
            .create_task(NewTaskSpec {
                url: self.page_url(),
                save_dir: self.work.to_string_lossy().into_owned(),
                file_name: name.to_owned(),
                segments: 1,
                start_paused: true,
                ..Default::default()
            })
            .await
            .expect("create task")
    }

    /// 处理恰好一个 resolve 回流。
    async fn pump_one_resolve(&mut self) {
        let out = tokio::time::timeout(Duration::from_secs(15), self.resolve_rx.recv())
            .await
            .expect("resolve outcome timeout")
            .expect("resolve channel closed");
        self.engine.manager.on_resolve_ready(out).await;
    }

    /// 驱动 resolve 回流直到 `task_id` 下载完成。
    async fn pump_until_done(&mut self, task_id: &str) {
        loop {
            tokio::select! {
                Some(out) = self.resolve_rx.recv() => {
                    self.engine.manager.on_resolve_ready(out).await;
                }
                Some(finished) = self.done_rx.recv() => {
                    self.engine.manager.on_task_done(&finished).await;
                    if finished.task_id == task_id {
                        break;
                    }
                }
                () = tokio::time::sleep(Duration::from_secs(15)) => panic!("timeout driving {task_id}"),
            }
        }
    }

    async fn status_and_error(&self, task_id: &str) -> (i32, String) {
        let info = self
            .engine
            .db
            .load_task_by_id(task_id)
            .await
            .unwrap()
            .expect("task row");
        (info.status, info.error_message)
    }

    async fn group_members(&self, group_id: &str) -> Vec<String> {
        self.engine.db.group_member_ids(group_id).await.unwrap()
    }

    async fn member_named(&self, members: &[String], file_name: &str) -> String {
        for member in members {
            let info = self
                .engine
                .db
                .load_task_by_id(member)
                .await
                .unwrap()
                .unwrap();
            if info.file_name == file_name {
                return member.clone();
            }
        }
        panic!("no group member named {file_name}");
    }

    async fn create_group(&mut self, name: &str, plugin_id: &str) -> String {
        self.create_group_at(SHARE_URL, name, plugin_id).await
    }

    async fn create_group_at(&mut self, source_url: &str, name: &str, plugin_id: &str) -> String {
        let item = |id: &str, file: &str| GroupItemSpec {
            resolver_item: id.to_owned(),
            file_name: file.to_owned(),
            rel_path: String::new(),
            size: 1,
        };
        self.engine
            .manager
            .create_task_group(CreateGroupSpec {
                source_url: source_url.to_owned(),
                group_name: name.to_owned(),
                base_save_dir: self.work.to_string_lossy().into_owned(),
                segments: 1,
                start_paused: true,
                items: vec![item("f1", "a.bin"), item("f2", "b.bin")],
                resolver_plugin_id: plugin_id.to_owned(),
                ..Default::default()
            })
            .await
            .expect("create group")
            .group_id()
            .expect("two items form a group")
            .to_owned()
    }

    async fn cleanup(self) {
        if let Err(error) = tokio::fs::remove_dir_all(&self.work).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_with_explicit_plugin_id_binds_it_even_when_earlier_non_multi_plugin_matches() {
    let mut fx = Fixture::new().await;
    fx.install_share_plugin("aaa@single", false).await;
    fx.install_share_plugin("zzz@multi", true).await;
    let pm = fx.engine.manager.plugin_manager().unwrap();
    // 普通路由会选字典序靠前的 aaa@single：这正是预览与实际下载不同源的来源。
    assert_eq!(
        pm.route_resolver(SHARE_URL).await.unwrap().identity,
        "aaa@single"
    );

    let group = fx.create_group("explicit", "zzz@multi").await;
    let members = fx.group_members(&group).await;
    assert_eq!(members.len(), 2);
    for member in &members {
        assert_eq!(
            fx.engine.db.get_task_resolver(member).await.unwrap(),
            "zzz@multi"
        );
        assert_eq!(fx.status_and_error(member).await.0, 2, "paused, not failed");
    }
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_with_unavailable_explicit_plugin_is_bound_failed_and_recovers_on_reenable() {
    let mut fx = Fixture::new().await;
    fx.install_share_plugin("aaa@single", false).await;
    fx.install_share_plugin("zzz@multi", true).await;
    let pm = fx.engine.manager.plugin_manager().unwrap();
    pm.set_enabled("zzz@multi", false).await.unwrap();

    // 显式绑定的插件已禁用：绝不回退到仍然启用的 aaa@single，但绑定要保留。
    let group = fx.create_group("disabled", "zzz@multi").await;
    let members = fx.group_members(&group).await;
    assert_eq!(members.len(), 2);
    for member in &members {
        let (status, message) = fx.status_and_error(member).await;
        assert_eq!(status, 4);
        assert_eq!(message, "解析插件不可用");
        assert_eq!(
            fx.engine.db.get_task_resolver(member).await.unwrap(),
            "zzz@multi"
        );
    }
    let member = fx.member_named(&members, "a.bin").await;

    // 插件仍不可用时恢复：由 resolve 的常规错误 fail-closed，不碰原始链接。
    fx.engine.manager.resume_task(&member).await;
    fx.pump_one_resolve().await;
    let (status, message) = fx.status_and_error(&member).await;
    assert_eq!(status, 4);
    assert!(message.contains("zzz@multi"), "{message}");

    // 重新启用后手动恢复：经绑定的插件正常解析并下载。
    pm.set_enabled("zzz@multi", true).await.unwrap();
    fx.engine.manager.resume_task(&member).await;
    fx.pump_until_done(&member).await;
    assert_eq!(fx.status_and_error(&member).await.0, 3);
    assert_eq!(
        tokio::fs::read(fx.work.join("a.bin")).await.unwrap(),
        FILE_BODY
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_second_stage_row_without_plugin_never_fetches_raw_url() {
    let mut fx = Fixture::new().await;
    // 没有任何插件：旧版建档的二段任务绑定为空（同时也是卸载 / 忽略重试后的形态）。
    let source = fx.page_url();
    let group = fx.create_group_at(&source, "legacy-empty", "").await;
    let members = fx.group_members(&group).await;
    assert_eq!(members.len(), 2);
    for member in &members {
        assert!(
            fx.engine
                .db
                .get_task_resolver(member)
                .await
                .unwrap()
                .is_empty()
        );
        fx.engine.manager.resume_task(member).await;
        let (status, message) = fx.status_and_error(member).await;
        assert_eq!(status, 4);
        assert_eq!(message, "解析插件不可用");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        fx.requests.lock().unwrap().is_empty(),
        "the share page must never be fetched as a file: {:?}",
        fx.requests.lock().unwrap()
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_second_stage_row_rebinds_when_a_plugin_matches_again() {
    let mut fx = Fixture::new().await;
    let group = fx.create_group("legacy-rebind", "").await;
    let members = fx.group_members(&group).await;
    let member = fx.member_named(&members, "a.bin").await;
    assert!(
        fx.engine
            .db
            .get_task_resolver(&member)
            .await
            .unwrap()
            .is_empty()
    );

    fx.install_share_plugin("zzz@multi", true).await;
    fx.engine.manager.resume_task(&member).await;
    fx.pump_until_done(&member).await;
    assert_eq!(
        fx.engine.db.get_task_resolver(&member).await.unwrap(),
        "zzz@multi"
    );
    assert_eq!(
        tokio::fs::read(fx.work.join("a.bin")).await.unwrap(),
        FILE_BODY
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninstall_leaves_second_stage_unbound_and_resume_fails_closed() {
    let mut fx = Fixture::new().await;
    fx.install_share_plugin("zzz@multi", true).await;
    let group = fx.create_group("uninstalled", "zzz@multi").await;
    let members = fx.group_members(&group).await;
    let pm = fx.engine.manager.plugin_manager().unwrap();
    pm.uninstall("zzz@multi").await.unwrap();

    for member in &members {
        assert!(
            fx.engine
                .db
                .get_task_resolver(member)
                .await
                .unwrap()
                .is_empty()
        );
        fx.engine.manager.resume_task(member).await;
        let (status, message) = fx.status_and_error(member).await;
        assert_eq!(status, 4);
        assert_eq!(message, "解析插件不可用", "never falls back to the raw url");
    }
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignore_plugin_retry_is_refused_for_second_stage_tasks_only() {
    let mut fx = Fixture::new().await;
    fx.install_share_plugin("zzz@multi", true).await;
    fx.install_page_plugin("alpha@site").await;
    let pm = fx.engine.manager.plugin_manager().unwrap();

    let group = fx.create_group("ignore", "zzz@multi").await;
    let member = fx.group_members(&group).await.remove(0);
    let error = pm.clear_task_resolver(&member).await.unwrap_err();
    assert!(
        matches!(
            error,
            fluxdown_engine::plugin::PluginError::InvalidSetting { .. }
        ),
        "{error:?}"
    );
    assert_eq!(
        fx.engine.db.get_task_resolver(&member).await.unwrap(),
        "zzz@multi"
    );

    // 普通任务保持原有逃生舱语义。
    let plain = fx.create_paused("plain.bin").await;
    pm.clear_task_resolver(&plain).await.unwrap();
    assert!(
        fx.engine
            .db
            .get_task_resolver(&plain)
            .await
            .unwrap()
            .is_empty()
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_without_plugin_id_uses_preview_rule() {
    let mut fx = Fixture::new().await;
    fx.install_share_plugin("aaa@single", false).await;
    fx.install_share_plugin("zzz@multi", true).await;

    let group = fx.create_group("legacy", "").await;
    let members = fx.group_members(&group).await;
    assert_eq!(members.len(), 2);
    for member in &members {
        assert_eq!(
            fx.engine.db.get_task_resolver(member).await.unwrap(),
            "zzz@multi",
            "legacy clients get the multi-only selection the preview used"
        );
    }
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_outcome_carries_producing_plugin_id() {
    let fx = Fixture::new().await;
    fx.install_share_plugin("aaa@single", false).await;
    fx.install_share_plugin("zzz@multi", true).await;

    let outcome = fx
        .engine
        .manager
        .spawn_resolve_preview(
            SHARE_URL.to_owned(),
            String::new(),
            String::new(),
            String::new(),
            HashMap::new(),
        )
        .await
        .expect("preview outcome");
    assert!(outcome.error.is_empty(), "{}", outcome.error);
    assert_eq!(outcome.items.len(), 2);
    assert_eq!(outcome.plugin_id, "zzz@multi");

    let unmatched = fx
        .engine
        .manager
        .spawn_resolve_preview(
            "http://nothing.test/x".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            HashMap::new(),
        )
        .await
        .expect("preview outcome");
    assert!(unmatched.plugin_id.is_empty());
    assert!(unmatched.items.is_empty());
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_started_task_rebinds_when_bound_plugin_is_disabled() {
    let mut fx = Fixture::new().await;
    fx.install_page_plugin("alpha@site").await;
    let task = fx.create_paused("rebind.bin").await;
    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "alpha@site"
    );

    fx.install_page_plugin("beta@site").await;
    let pm = fx.engine.manager.plugin_manager().unwrap();
    pm.set_enabled("alpha@site", false).await.unwrap();

    fx.engine.manager.resume_task(&task).await;
    fx.pump_until_done(&task).await;

    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "beta@site"
    );
    assert!(
        !fx.engine
            .db
            .is_task_resolver_choice_pending(&task)
            .await
            .unwrap()
    );
    assert_eq!(
        tokio::fs::read(fx.work.join("rebind.bin")).await.unwrap(),
        FILE_BODY
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_with_progress_stays_bound_and_fails_closed() {
    let mut fx = Fixture::new().await;
    fx.install_page_plugin("alpha@site").await;
    let task = fx.create_paused("progress.bin").await;
    fx.install_page_plugin("beta@site").await;
    let pm = fx.engine.manager.plugin_manager().unwrap();
    pm.set_enabled("alpha@site", false).await.unwrap();
    // 已有落盘进度：中途换 resolver 可能写坏文件，维持绑定并 fail-closed。
    fx.engine.db.update_task_progress(&task, 5).await.unwrap();

    fx.engine.manager.resume_task(&task).await;
    fx.pump_one_resolve().await;

    let (status, message) = fx.status_and_error(&task).await;
    assert_eq!(status, 4);
    assert!(message.contains("alpha@site"), "{message}");
    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "alpha@site"
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_candidates_keep_choice_pending_until_plugin_returns() {
    let mut fx = Fixture::new().await;
    fx.install_page_plugin("alpha@site").await;
    fx.install_page_plugin("beta@site").await;
    let task = fx.create_paused("pending.bin").await;
    assert!(
        fx.engine
            .db
            .is_task_resolver_choice_pending(&task)
            .await
            .unwrap()
    );
    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "alpha@site"
    );

    let pm = fx.engine.manager.plugin_manager().unwrap();
    pm.set_enabled("alpha@site", false).await.unwrap();
    pm.set_enabled("beta@site", false).await.unwrap();
    fx.engine.manager.resume_task(&task).await;
    fx.pump_one_resolve().await;

    // 零候选：本次 fail-closed，但待选标记与绑定都保持，等插件回来再定案。
    assert_eq!(fx.status_and_error(&task).await.0, 4);
    assert!(
        fx.engine
            .db
            .is_task_resolver_choice_pending(&task)
            .await
            .unwrap()
    );
    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "alpha@site"
    );

    pm.set_enabled("beta@site", true).await.unwrap();
    fx.engine.manager.resume_task(&task).await;
    fx.pump_until_done(&task).await;
    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "beta@site"
    );
    assert!(
        !fx.engine
            .db
            .is_task_resolver_choice_pending(&task)
            .await
            .unwrap()
    );
    fx.cleanup().await;
}
