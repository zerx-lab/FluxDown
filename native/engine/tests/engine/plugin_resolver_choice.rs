//! 多插件 resolver 冲突：首次起跑询问、记住站点默认插件、无人值守回退与取消。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::download_manager::{NewTaskSpec, TaskDonePhase};
use fluxdown_engine::model::{BtFileEntry, HlsQualityOption, ResolveVariantOption};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::selection::{
    BtFileChoice, HostSelection, PluginChoice, PluginChoiceRequest, SelectionOutcome,
};
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

const FILE_BODY: &[u8] = b"fluxdown plugin choice payload\n";
const PAGE_URL: &str = "https://www.origin.test/watch?v=1";

const OK: &str = "200 OK";
const NOT_FOUND: &str = "404 Not Found";

#[derive(Default)]
struct Requests(Mutex<Vec<String>>);

impl Requests {
    fn for_path(&self, path: &str) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|req| req.split_whitespace().nth(1) == Some(path))
            .cloned()
            .collect()
    }
}

async fn serve_request(
    stream: &mut TcpStream,
    requests: &Requests,
    reply: fn(&str) -> (&'static str, &'static [u8]),
) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while !request.ends_with(b"\r\n\r\n") {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok(()); // A competing probe can be cancelled before sending headers.
        }
        request.extend_from_slice(&chunk[..count]);
    }
    let request = String::from_utf8(request).expect("HTTP request headers");
    let mut line = request.split_whitespace();
    let head = line.next() == Some("HEAD");
    let (status, body) = reply(line.next().expect("HTTP request target"));
    requests
        .0
        .lock()
        .unwrap()
        .push(request.to_ascii_lowercase());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    if !head {
        stream.write_all(body).await?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Resolver<'a> {
    version: &'a str,
    pattern: &'a str,
    browser_original_url: bool,
    multi: bool,
}

impl<'a> Resolver<'a> {
    fn on(pattern: &'a str) -> Self {
        Self {
            version: "1.0.0",
            pattern,
            browser_original_url: false,
            multi: false,
        }
    }

    fn first_hop() -> Self {
        Self {
            browser_original_url: true,
            ..Self::on("https://www.origin.test/*")
        }
    }
}

struct TestDir(PathBuf);

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "could not remove test directory {}: {error}",
                self.0.display()
            );
        }
    }
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
    servers: JoinSet<()>,
    engine: Engine,
    resolve_rx:
        tokio::sync::mpsc::UnboundedReceiver<fluxdown_engine::download_manager::ResolveOutcome>,
    done_rx: tokio::sync::mpsc::Receiver<fluxdown_engine::download_manager::TaskDone>,
    work: TestDir,
}

impl Fixture {
    async fn new(selector: Arc<dyn HostSelection>) -> Self {
        let mut fx = Self::empty(selector).await;
        let (origin, _) = fx.server(|_| (OK, FILE_BODY)).await;
        for identity in ["alpha@site", "beta@site"] {
            fx.install(
                identity,
                Resolver::on("*://*.origin.test/*"),
                &format!(
                    "globalThis.resolve = async () => ({{ url: '{origin}/{identity}.bin' }});"
                ),
            )
            .await;
        }
        fx
    }

    async fn empty(selector: Arc<dyn HostSelection>) -> Self {
        let work = TestDir(std::env::temp_dir().join(format!(
            "fluxdown-plugin-choice-{}",
            uuid::Uuid::new_v4().simple()
        )));
        let dir = work.0.to_string_lossy().into_owned();
        tokio::fs::create_dir_all(&work.0)
            .await
            .expect("mkdir work");
        let mut engine = Engine::new(
            EngineConfig {
                max_concurrent: 4,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: dir.clone(),
                app_data_dir: dir,
                bt_config: BtConfig::default(),
                proxy_config: ProxyConfig::default(),
                user_agent: String::new(),
                data_dir_override: Some(work.0.clone()),
                database_url: None,
            },
            Arc::new(NoopSink),
            selector,
        )
        .await
        .expect("engine");
        engine.manager.set_max_auto_retries(0);
        let resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
        let done_rx = engine.manager.take_done_rx().expect("done_rx");
        Self {
            servers: JoinSet::new(),
            engine,
            resolve_rx,
            done_rx,
            work,
        }
    }

    async fn server(
        &mut self,
        reply: fn(&str) -> (&'static str, &'static [u8]),
    ) -> (String, Arc<Requests>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let origin = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(Requests::default());
        let recorded = requests.clone();
        // Owned by the fixture: always aborted on drop, and joined during normal cleanup.
        self.servers.spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept HTTP connection");
                if let Err(error) = tokio::time::timeout(
                    Duration::from_secs(3),
                    serve_request(&mut stream, &recorded, reply),
                )
                .await
                .expect("serve local HTTP request")
                {
                    // The downloader may cancel a competing probe.
                    assert!(
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                        ),
                        "serve local HTTP request: {error}"
                    );
                }
            }
        });
        (origin, requests)
    }

    async fn install(&self, identity: &str, resolver: Resolver<'_>, script: &str) {
        let src = self.work.0.join(format!("src-{identity}"));
        tokio::fs::create_dir_all(&src).await.expect("mkdir plugin");
        let mut manifest = serde_json::json!({
            "identity": identity,
            "name": identity,
            "version": resolver.version,
            "description": "test resolver",
            "resolvers": [{
                "match": {
                    "urls": [resolver.pattern],
                },
                "entry": "resolve.js",
                "timeoutMs": 5000,
                "multi": resolver.multi,
            }],
        });
        if resolver.browser_original_url {
            manifest["resolvers"][0]["match"]["browserOriginalUrl"] = true.into();
        }
        tokio::fs::write(
            src.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .await
        .expect("write manifest");
        tokio::fs::write(src.join("resolve.js"), script)
            .await
            .expect("write resolve.js");
        self.engine
            .manager
            .plugin_manager()
            .expect("plugin manager")
            .install_from_dir(&src)
            .await
            .expect("install resolver");
    }

    async fn create(&mut self, name: &str, unattended: bool) -> String {
        self.create_task(NewTaskSpec {
            url: PAGE_URL.to_owned(),
            file_name: name.to_owned(),
            unattended_selection: unattended,
            ..Default::default()
        })
        .await
    }

    async fn capture(
        &mut self,
        url: &str,
        name: &str,
        browser_original_url: Option<&str>,
        start_paused: bool,
    ) -> String {
        self.create_task(NewTaskSpec {
            url: url.to_owned(),
            browser_original_url: browser_original_url.map(str::to_owned),
            file_name: name.to_owned(),
            cookies: "cdn=private".to_owned(),
            referrer: url.to_owned(),
            extra_headers: [
                ("Authorization".to_owned(), "Bearer private".to_owned()),
                ("X-Captured".to_owned(), "opaque".to_owned()),
            ]
            .into(),
            unattended_selection: true,
            start_paused,
            ..Default::default()
        })
        .await
    }

    async fn create_task(&mut self, mut spec: NewTaskSpec) -> String {
        spec.save_dir = self.work.0.to_string_lossy().into_owned();
        spec.segments = 1;
        self.engine
            .manager
            .create_task(spec)
            .await
            .expect("create task")
    }

    /// Process actor callbacks until the task finishes or a resolver outcome is applied.
    async fn drive(&mut self, task_id: &str, expect_done: bool) {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                tokio::select! {
                    Some(out) = self.resolve_rx.recv() => {
                        let matched = out.task_id == task_id;
                        self.engine.manager.on_resolve_ready(out).await;
                        if matched && !expect_done {
                            break;
                        }
                    }
                    Some(finished) = self.done_rx.recv() => {
                        self.engine.manager.on_task_done(&finished).await;
                        if finished.task_id == task_id && finished.phase == TaskDonePhase::Finished {
                            assert!(expect_done, "task finished before resolver outcome");
                            break;
                        }
                    }
                }
            }
        })
        .await;
        assert!(result.is_ok(), "timeout driving {task_id}");
    }

    async fn assert_downloaded(&self, task_id: &str, expected: &[u8]) {
        let row = self
            .engine
            .db
            .load_task_by_id(task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, 3, "{}", row.error_message);
        let path = Path::new(&row.save_dir).join(row.file_name);
        assert_eq!(tokio::fs::read(path).await.unwrap(), expected);
    }

    async fn assert_resolver(&self, task_id: &str, expected: &str) {
        assert_eq!(
            self.engine.db.get_task_resolver(task_id).await.unwrap(),
            expected
        );
    }

    async fn cleanup(mut self) {
        self.engine.manager.shutdown().await;
        self.servers.abort_all();
        while let Some(result) = self.servers.join_next().await {
            if let Err(error) = result {
                assert!(error.is_cancelled(), "HTTP test server failed: {error}");
            }
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
        tokio::fs::read(fx.work.0.join("first.bin")).await.unwrap(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_original_url_requires_opt_in_and_yields_to_a_final_url_match() {
    let mut fx = Fixture::empty(Arc::new(NoopSelection)).await;
    let (origin, _) = fx
        .server(|path| match path {
            "/captured.bin" => (OK, b"original transfer"),
            "/resolved.bin" => (OK, b"final URL resolver"),
            _ => (NOT_FOUND, b""),
        })
        .await;
    let final_url = format!("{origin}/captured.bin");
    let unexpected =
        "globalThis.resolve = async () => { throw new Error('unexpected resolver'); };";
    fx.install("legacy@site", Resolver::on(PAGE_URL), unexpected)
        .await;
    let no_opt_in = fx
        .capture(&final_url, "legacy.bin", Some(PAGE_URL), false)
        .await;
    fx.drive(&no_opt_in, true).await;
    fx.assert_downloaded(&no_opt_in, b"original transfer").await;
    fx.assert_resolver(&no_opt_in, "").await;

    fx.install("aaa-first-hop@site", Resolver::first_hop(), unexpected)
        .await;
    let omitted = fx.capture(&final_url, "omitted.bin", None, false).await;
    fx.drive(&omitted, true).await;
    fx.assert_downloaded(&omitted, b"original transfer").await;
    fx.assert_resolver(&omitted, "").await;

    fx.install(
        "zzz-final@site",
        Resolver::on(&final_url),
        &format!(
            r#"globalThis.resolve = async (ctx) => {{
                if (ctx.url !== "{final_url}" || "browserOriginalUrl" in ctx ||
                    ctx.cookies !== "cdn=private") throw new Error("incorrect final URL context");
                return {{ url: "{origin}/resolved.bin" }};
            }};"#
        ),
    )
    .await;
    let direct = fx
        .capture(&final_url, "direct.bin", Some(PAGE_URL), false)
        .await;
    fx.drive(&direct, true).await;
    fx.assert_downloaded(&direct, b"final URL resolver").await;
    fx.assert_resolver(&direct, "zzz-final@site").await;
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_original_url_survives_fission_resume_and_resolver_reselection() {
    let mut fx = Fixture::empty(Arc::new(NoopSelection)).await;
    let (origin, requests) = fx
        .server(|path| match path {
            "/part-a.bin" => (OK, b"fresh-a"),
            "/part-b.bin" => (OK, b"fresh-b"),
            _ => (NOT_FOUND, b""),
        })
        .await;
    let final_url = format!("{origin}/captured.bin");
    let multi = Resolver {
        multi: true,
        ..Resolver::first_hop()
    };
    let script = format!(
        r#"globalThis.resolve = async (ctx) => {{
            if (ctx.url !== "{final_url}" || ctx.browserOriginalUrl !== "{PAGE_URL}" ||
                ctx.cookies || ctx.referrer || Object.keys(ctx.extraHeaders).length) {{
                throw new Error("incorrect redirect context");
            }}
            if (!ctx.resolverItem) {{
                // Exceed the auto-start threshold so both members resume from storage.
                return {{ manifest: {{ name: "parts", items: [
                    {{ id: "part-a", name: "part-a.bin", size: 6 * 1024 ** 3 }},
                    {{ id: "part-b", name: "part-b.bin", size: 6 * 1024 ** 3 }}
                ] }} }};
            }}
            return {{ url: "{origin}/" + ctx.resolverItem + ".bin" }};
        }};"#
    );
    fx.install("original@site", multi, &script).await;
    let id = fx
        .capture(&final_url, "redirect.bin", Some(PAGE_URL), true)
        .await;
    fx.assert_resolver(&id, "original@site").await;
    fx.engine.manager.resume_task(&id).await;
    fx.drive(&id, false).await;
    let mut groups = fx.engine.db.load_all_groups().await.unwrap();
    assert_eq!(groups.len(), 1);
    let group = groups.remove(0);
    let members = fx
        .engine
        .db
        .group_member_ids(&group.group_id)
        .await
        .unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.contains(&id));
    for member in &members {
        let row = fx.engine.db.load_task_by_id(member).await.unwrap().unwrap();
        assert_eq!(row.status, 2);
        assert_eq!(row.url, final_url);
        fx.assert_resolver(member, "original@site").await;
    }

    fx.engine.manager.resume_task(&id).await;
    fx.drive(&id, true).await;
    fx.assert_downloaded(&id, b"fresh-a").await;

    let sibling = members.into_iter().find(|member| member != &id).unwrap();
    fx.engine
        .manager
        .plugin_manager()
        .unwrap()
        .uninstall("original@site")
        .await
        .unwrap();
    fx.assert_resolver(&sibling, "").await;
    fx.install(
        "aaa-single@site",
        Resolver::first_hop(),
        "globalThis.resolve = async () => { throw new Error('not a multi resolver'); };",
    )
    .await;
    fx.install("replacement@site", multi, &script).await;
    fx.engine.manager.resume_task(&sibling).await;
    fx.drive(&sibling, true).await;
    fx.assert_downloaded(&sibling, b"fresh-b").await;
    fx.assert_resolver(&sibling, "replacement@site").await;
    assert!(
        requests.for_path("/captured.bin").is_empty(),
        "must not fetch the old transfer"
    );
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_browser_resolver_that_no_longer_matches_fails_before_running_script() {
    let mut fx = Fixture::empty(Arc::new(NoopSelection)).await;
    let (origin, requests) = fx.server(|_| (NOT_FOUND, b"")).await;
    let final_url = format!("{origin}/captured.bin");
    let script = "globalThis.resolve = async () => { throw new Error('ran stale resolver'); };";
    let resolver = Resolver::first_hop();
    fx.install("redirect@site", resolver, script).await;
    let id = fx
        .capture(&final_url, "redirect.bin", Some(PAGE_URL), true)
        .await;
    fx.assert_resolver(&id, "redirect@site").await;
    fx.install(
        "redirect@site",
        Resolver {
            version: "1.0.1",
            pattern: "https://unrelated.test/*",
            ..resolver
        },
        script,
    )
    .await;
    fx.engine.manager.resume_task(&id).await;
    fx.drive(&id, false).await;
    let row = fx.engine.db.load_task_by_id(&id).await.unwrap().unwrap();
    assert_eq!(row.status, 4);
    assert!(
        row.error_message.contains("已不再匹配"),
        "{}",
        row.error_message
    );
    assert!(requests.for_path("/captured.bin").is_empty());
    fx.cleanup().await;
}

fn assert_no_capture_headers(request: &str) {
    for header in ["cookie", "authorization", "referer", "x-captured"] {
        assert!(!request.contains(&format!("\r\n{header}:")), "{request}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_browser_capture_downloads_new_resource_without_stale_context() {
    let mut fx = Fixture::empty(Arc::new(NoopSelection)).await;
    let (old_origin, old_requests) = fx.server(|_| (NOT_FOUND, b"")).await;
    let (new_origin, new_requests) = fx
        .server(|path| match path {
            "/replacement.bin" => (OK, b"replacement payload"),
            _ => (NOT_FOUND, b""),
        })
        .await;
    fx.install(
        "redirect@site",
        Resolver::first_hop(),
        "globalThis.resolve = async () => { throw new Error('ran old resolver'); };",
    )
    .await;
    let id = fx
        .capture(
            &format!("{old_origin}/captured.bin"),
            "redirect.bin",
            Some(PAGE_URL),
            true,
        )
        .await;
    fx.assert_resolver(&id, "redirect@site").await;

    let replacement = format!("{new_origin}/replacement.bin");
    fx.engine
        .manager
        .change_task_url(&id, &replacement)
        .await
        .unwrap();
    fx.assert_resolver(&id, "").await;
    fx.engine.manager.resume_task(&id).await;
    fx.drive(&id, true).await;
    fx.assert_downloaded(&id, b"replacement payload").await;
    assert!(old_requests.for_path("/captured.bin").is_empty());
    let requests = new_requests.for_path("/replacement.bin");
    assert!(requests.iter().any(|request| request.starts_with("get ")));
    for request in requests {
        assert_no_capture_headers(&request);
    }
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_redirect_cross_origin_media_uses_clean_base_request() {
    let mut fx = Fixture::empty(Arc::new(NoopSelection)).await;
    let (captured, _) = fx.server(|_| (NOT_FOUND, b"")).await;
    let (media, requests) = fx.server(|_| (NOT_FOUND, b"")).await;
    fx.install(
        "redirect@site",
        Resolver::first_hop(),
        &format!(
            r#"globalThis.resolve = async (ctx) => ({{
                url: "{media}/stream." + ctx.url.split("media=")[1],
                extraHeaders: {{ "X-Resolver": "explicit" }}
            }});"#
        ),
    )
    .await;
    for (start_paused, extension) in [(false, "m3u8"), (true, "mpd")] {
        let url = format!("{captured}/captured.bin?media={extension}");
        let id = fx
            .capture(&url, extension, Some(PAGE_URL), start_paused)
            .await;
        if start_paused {
            fx.engine.manager.resume_task(&id).await;
        }
        fx.drive(&id, true).await;
        let requests = requests.for_path(&format!("/stream.{extension}"));
        assert!(requests.iter().any(|request| request.starts_with("get ")));
        for request in requests {
            assert!(
                request.contains("\r\nx-resolver: explicit\r\n"),
                "{request}"
            );
            assert_no_capture_headers(&request);
        }
    }
    fx.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_redirect_keeps_primary_credentials_without_sending_them_to_audio() {
    let mut fx = Fixture::empty(Arc::new(NoopSelection)).await;
    let (primary, video_requests) = fx
        .server(|path| match path {
            "/video.mp4" => (OK, b"video payload"),
            _ => (NOT_FOUND, b""),
        })
        .await;
    // A zero-byte audio response exercises the data request without invoking a muxer.
    let (audio, audio_requests) = fx
        .server(|path| match path {
            "/audio.m4a" => (OK, b""),
            _ => (NOT_FOUND, b""),
        })
        .await;
    fx.install(
        "redirect@site",
        Resolver::first_hop(),
        &format!(
            r#"globalThis.resolve = async () => ({{
                url: "{primary}/video.mp4",
                audioUrl: "{audio}/audio.m4a"
            }});"#
        ),
    )
    .await;
    let final_url = format!("{primary}/captured.bin");
    let id = fx
        .capture(&final_url, "video.mp4", Some(PAGE_URL), false)
        .await;
    fx.drive(&id, true).await;
    fx.assert_downloaded(&id, b"video payload").await;
    let video = video_requests.for_path("/video.mp4");
    let audio = audio_requests.for_path("/audio.m4a");
    for requests in [&video, &audio] {
        assert!(
            requests
                .iter()
                .any(|request| request.contains("\r\nrange: bytes=0-0\r\n"))
        );
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with("get ") && !request.contains("\r\nrange:"))
        );
    }
    for request in video {
        assert!(request.contains("\r\ncookie: cdn=private\r\n"), "{request}");
        assert!(
            request.contains("\r\nauthorization: bearer private\r\n"),
            "{request}"
        );
        assert!(request.contains("\r\nx-captured: opaque\r\n"), "{request}");
        assert!(
            request.contains(&format!("\r\nreferer: {final_url}\r\n")),
            "{request}"
        );
    }
    for request in audio {
        assert_no_capture_headers(&request);
    }
    fx.cleanup().await;
}
