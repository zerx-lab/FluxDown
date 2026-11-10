//! 多插件 resolver 冲突：首次起跑询问、记住站点默认插件、无人值守回退与取消。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::model::{BtFileEntry, HlsQualityOption, ResolveVariantOption};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::{
    BtFileChoice, HostSelection, PluginChoice, PluginChoiceRequest, SelectionOutcome,
};
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

const FILE_BODY: &[u8] = b"fluxdown plugin choice payload\n";
const PAGE_URL: &str = "https://www.origin.test/watch?v=1";

fn spawn_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
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
    port
}

async fn write_plugin(dir: &std::path::Path, identity: &str, target: &str) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = format!(
        r#"{{
      "identity": "{identity}",
      "name": "Plugin {identity}",
      "version": "1.0.0",
      "description": "resolves origin.test",
      "resolvers": [{{ "match": {{ "urls": ["*://*.origin.test/*"] }}, "entry": "resolve.js", "timeoutMs": 5000 }}]
    }}"#
    );
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    let resolve_js = format!("globalThis.resolve = async () => ({{ url: '{target}' }});");
    tokio::fs::write(dir.join("resolve.js"), resolve_js)
        .await
        .expect("write resolve.js");
}

/// 记录多插件选择请求并按预设答复。
struct ChoosingSelection {
    answer: PluginChoice,
    calls: AtomicUsize,
    last: Mutex<Option<PluginChoiceRequest>>,
}

impl ChoosingSelection {
    fn new(answer: PluginChoice) -> Arc<Self> {
        Arc::new(Self {
            answer,
            calls: AtomicUsize::new(0),
            last: Mutex::new(None),
        })
    }
}

#[async_trait::async_trait]
impl HostSelection for ChoosingSelection {
    async fn select_hls_quality(
        &self,
        _: &str,
        _: &[HlsQualityOption],
        _: Duration,
    ) -> SelectionOutcome<i32> {
        SelectionOutcome::NoSelectorConfigured(0)
    }
    async fn select_bt_files(
        &self,
        _: &str,
        _: &[BtFileEntry],
        _: &str,
        _: Option<Duration>,
    ) -> SelectionOutcome<BtFileChoice> {
        SelectionOutcome::NoSelectorConfigured(BtFileChoice::default())
    }
    async fn select_resolve_variant(
        &self,
        _: &str,
        _: &[ResolveVariantOption],
        default_index: i32,
        _: Duration,
    ) -> SelectionOutcome<i32> {
        SelectionOutcome::NoSelectorConfigured(default_index)
    }
    fn provide_hls_selection(&self, _: &str, _: i32) {}
    fn provide_bt_selection(&self, _: &str, _: Vec<i32>) {}
    fn provide_variant_selection(&self, _: &str, _: i32) {}
    async fn select_resolver_plugin(
        &self,
        _: &str,
        request: &PluginChoiceRequest,
        _: Duration,
    ) -> SelectionOutcome<PluginChoice> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("lock") = Some(request.clone());
        SelectionOutcome::UserChose(self.answer.clone())
    }
}

struct Fixture {
    work: std::path::PathBuf,
    engine: Engine,
    resolve_rx:
        tokio::sync::mpsc::UnboundedReceiver<fluxdown_engine::download_manager::ResolveOutcome>,
    done_rx: tokio::sync::mpsc::Receiver<fluxdown_engine::download_manager::TaskDone>,
}

impl Fixture {
    async fn new(selector: Arc<dyn HostSelection>) -> Self {
        let work = std::env::temp_dir().join(format!(
            "fluxdown-plugin-choice-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&work).await.expect("mkdir work");
        let port = spawn_server();
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
            selector,
        )
        .await
        .expect("engine");
        let pm = engine.manager.plugin_manager().expect("plugin manager");
        for identity in ["alpha@site", "beta@site"] {
            let src = work.join(format!("src-{identity}"));
            write_plugin(
                &src,
                identity,
                &format!("http://127.0.0.1:{port}/{identity}.bin"),
            )
            .await;
            pm.install_from_dir(&src).await.expect("install");
        }
        let resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
        let done_rx = engine.manager.take_done_rx().expect("done_rx");
        Self {
            work,
            engine,
            resolve_rx,
            done_rx,
        }
    }

    async fn create(&mut self, name: &str, unattended: bool) -> String {
        self.engine
            .manager
            .create_task(fluxdown_engine::download_manager::NewTaskSpec {
                url: PAGE_URL.to_owned(),
                save_dir: self.work.to_string_lossy().into_owned(),
                file_name: name.to_owned(),
                segments: 1,
                unattended_selection: unattended,
                ..Default::default()
            })
            .await
            .expect("create task")
    }

    /// 驱动 resolve 回流直到下载完成（`expect_done`）或任务被取消。
    async fn drive(&mut self, task_id: &str, expect_done: bool) {
        loop {
            tokio::select! {
                Some(out) = self.resolve_rx.recv() => {
                    self.engine.manager.on_resolve_ready(out).await;
                    if !expect_done {
                        break;
                    }
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

    async fn cleanup(self) {
        if let Err(error) = tokio::fs::remove_dir_all(&self.work).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("best-effort test directory cleanup: {error}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asks_once_then_remembers_site_default() {
    let selector = ChoosingSelection::new(PluginChoice::Use {
        identity: "beta@site".into(),
        remember: true,
    });
    let mut fx = Fixture::new(selector.clone()).await;

    let first = fx.create("first.bin", false).await;
    // 建任务即起跑，off-actor worker 与此处并发定案并清除待选标记，这里断言待选必然竞态；
    // 下方选择框恰好被询问一次即证明建任务时已落库待选（起跑按库内标记决定是否询问）。
    fx.drive(&first, true).await;

    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    let request = selector.last.lock().unwrap().clone().expect("request");
    assert_eq!(request.site, "origin.test");
    assert_eq!(request.default_identity, "alpha@site");
    let ids: Vec<_> = request
        .candidates
        .iter()
        .map(|c| c.identity.as_str())
        .collect();
    assert_eq!(ids, ["alpha@site", "beta@site"]);
    assert_eq!(
        fx.engine.db.get_task_resolver(&first).await.unwrap(),
        "beta@site"
    );
    assert!(
        !fx.engine
            .db
            .is_task_resolver_choice_pending(&first)
            .await
            .unwrap()
    );
    assert_eq!(
        tokio::fs::read(fx.work.join("first.bin")).await.unwrap(),
        FILE_BODY
    );

    let pm = fx.engine.manager.plugin_manager().unwrap();
    let defaults = pm.site_defaults().await.unwrap();
    assert_eq!(defaults.len(), 1);
    assert_eq!(defaults[0].site, "origin.test");
    assert_eq!(defaults[0].identity, "beta@site");
    assert!(!defaults[0].automatic);

    // 记住之后同站点直接绑定默认插件，不再询问。
    let second = fx.create("second.bin", false).await;
    assert!(
        !fx.engine
            .db
            .is_task_resolver_choice_pending(&second)
            .await
            .unwrap()
    );
    assert_eq!(
        fx.engine.db.get_task_resolver(&second).await.unwrap(),
        "beta@site"
    );
    fx.drive(&second, true).await;
    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);

    // 删除站点默认后恢复询问。
    pm.set_site_default("origin.test", None).await.unwrap();
    assert!(pm.route_resolver(PAGE_URL).await.unwrap().choice_pending);
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unattended_uses_fallback_and_records_unconfirmed_choice() {
    let mut fx = Fixture::new(Arc::new(NoopSelection)).await;
    let task = fx.create("auto.bin", true).await;
    fx.drive(&task, true).await;

    assert_eq!(
        fx.engine.db.get_task_resolver(&task).await.unwrap(),
        "alpha@site"
    );
    let pm = fx.engine.manager.plugin_manager().unwrap();
    let defaults = pm.site_defaults().await.unwrap();
    assert_eq!(defaults.len(), 1);
    assert_eq!(defaults[0].identity, "alpha@site");
    assert!(defaults[0].automatic, "unattended choice stays unconfirmed");
    // 未确认的记录不能让有界面的下载跳过询问。
    assert!(pm.route_resolver(PAGE_URL).await.unwrap().choice_pending);

    // 用户在插件页确认后成为站点默认插件；只能选记录里的候选。
    assert!(
        pm.set_site_default("origin.test", Some("gamma@site"))
            .await
            .is_err()
    );
    pm.set_site_default("origin.test", Some("beta@site"))
        .await
        .unwrap();
    let route = pm.route_resolver(PAGE_URL).await.unwrap();
    assert_eq!(route.identity, "beta@site");
    assert!(!route.choice_pending);

    // 卸载被选中的插件：记录随之删除，剩余单个插件直接绑定。
    pm.uninstall("beta@site").await.unwrap();
    assert!(pm.site_defaults().await.unwrap().is_empty());
    let route = pm.route_resolver(PAGE_URL).await.unwrap();
    assert_eq!(route.identity, "alpha@site");
    assert!(!route.choice_pending);
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_choice_pauses_task_and_asks_again_on_resume() {
    let selector = ChoosingSelection::new(PluginChoice::Cancel);
    let mut fx = Fixture::new(selector.clone()).await;
    let task = fx.create("cancel.bin", false).await;
    fx.drive(&task, false).await;

    assert_eq!(selector.calls.load(Ordering::SeqCst), 1);
    let info = fx
        .engine
        .db
        .load_task_by_id(&task)
        .await
        .unwrap()
        .expect("task row");
    assert_eq!(info.status, 2, "取消插件选择应暂停任务而不是失败");
    assert!(
        fx.engine
            .db
            .is_task_resolver_choice_pending(&task)
            .await
            .unwrap()
    );
    let pm = fx.engine.manager.plugin_manager().unwrap();
    assert!(pm.site_defaults().await.unwrap().is_empty());

    // 恢复时仍待选择：重新询问。
    fx.engine.manager.resume_task(&task).await;
    fx.drive(&task, false).await;
    assert_eq!(selector.calls.load(Ordering::SeqCst), 2);
    fx.cleanup().await;
}
