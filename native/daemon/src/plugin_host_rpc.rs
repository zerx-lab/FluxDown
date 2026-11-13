//! 常驻插件宿主的 RPC：可移植视图会话（open / event / close / snapshot / keepalive）、
//! 命令与 invoke。作为 `service` 的子模块编译，复用其私有辅助函数。

use super::{DaemonService, internal_error, invalid_argument, parse_params, to_value};
use fluxdown_engine::plugin::PluginError;
use fluxdown_protocol::{
    PluginArchiveRulesParams, PluginCommandParams, PluginInvokeParams, PluginPageInvokeParams,
    PluginPageInvokeResultDto, PluginViewEventParams, PluginViewKeepaliveParams,
    PluginViewOpenParams, PluginViewOpenedDto, PluginViewSessionParams, PluginViewSnapshotDto,
    RpcErrorObject, method, normalize_view,
};
use serde_json::{Map, Value, json};

/// 宿主可预期的失败（插件不存在 / 未启用 / 视图未声明 / 脚本抛错）按参数错误返回；
/// 超时、内存超限、并发过载等运行时故障按内部错误返回。
fn host_error(field: &str, error: &PluginError) -> RpcErrorObject {
    match error {
        PluginError::Runtime(message) | PluginError::ManifestInvalid(message) => {
            invalid_argument(field, message)
        }
        other => internal_error(other.to_string()),
    }
}

fn unknown_session() -> RpcErrorObject {
    invalid_argument("session", "unknown session")
}

impl DaemonService {
    pub(super) async fn plugin_host_call(
        &self,
        method_name: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcErrorObject> {
        let manager = self.plugin_manager()?;
        match method_name {
            method::DAEMON_PLUGIN_ARCHIVE_RULES => {
                let params = parse_params::<PluginArchiveRulesParams>(params)?;
                let value = manager
                    .archive_rules_status(&params.identity, params.refresh)
                    .await
                    .map_err(|error| host_error("identity", &error))?;
                let status =
                    serde_json::from_value::<fluxdown_protocol::PluginArchiveRulesDto>(value)
                        .map_err(|error| internal_error(error.to_string()))?;
                to_value(status)
            }
            method::DAEMON_PLUGIN_VIEW_OPEN => {
                let params = parse_params::<PluginViewOpenParams>(params)?;
                let (session, raw) = manager
                    .open_view(
                        &params.identity,
                        &params.view,
                        params.props,
                        params.task_id,
                        params.locale.as_deref().unwrap_or_default(),
                    )
                    .await
                    .map_err(|error| host_error("view", &error))?;
                let tree = match normalize_view(&raw) {
                    Ok(tree) => tree,
                    Err(error) => {
                        // 插件交出了非法的树：会话没有意义，直接结束它。
                        manager.close_view(&session);
                        return Err(internal_error(error.to_string()));
                    }
                };
                self.events.plugin_views().open(&session, tree.clone());
                // 插件可能在 init 里就 `ctx.close()` 了：引擎已结束会话、关闭事件先于登记发出，
                // 这里补清存储，避免留下永不关闭的条目（客户端缓存了关闭动作，登记后自行收尾）。
                if !manager.has_view_session(&session) {
                    self.events.plugin_views().close(&session);
                }
                to_value(PluginViewOpenedDto {
                    session,
                    seq: 0,
                    tree,
                })
            }
            method::DAEMON_PLUGIN_VIEW_EVENT => {
                let params = parse_params::<PluginViewEventParams>(params)?;
                let mut event = Map::new();
                event.insert("node".to_owned(), Value::String(params.node));
                event.insert("type".to_owned(), Value::String(params.event.kind));
                if let Some(value) = params.event.value {
                    event.insert("value".to_owned(), value);
                }
                if let Some(key) = params.event.key {
                    event.insert("key".to_owned(), Value::String(key));
                }
                if let Some(action) = params.event.action {
                    event.insert("action".to_owned(), Value::String(action));
                }
                manager
                    .view_event(&params.session, Value::Object(event))
                    .map_err(|_| unknown_session())?;
                Ok(json!({}))
            }
            method::DAEMON_PLUGIN_VIEW_CLOSE => {
                let params = parse_params::<PluginViewSessionParams>(params)?;
                manager.close_view(&params.session);
                self.events.plugin_views().close(&params.session);
                Ok(json!({}))
            }
            method::DAEMON_PLUGIN_VIEW_SNAPSHOT => {
                let params = parse_params::<PluginViewSessionParams>(params)?;
                let (seq, tree) = self
                    .events
                    .plugin_views()
                    .snapshot(&params.session)
                    .ok_or_else(unknown_session)?;
                to_value(PluginViewSnapshotDto { seq, tree })
            }
            method::DAEMON_PLUGIN_VIEW_KEEPALIVE => {
                let params = parse_params::<PluginViewKeepaliveParams>(params)?;
                manager.keepalive(&params.sessions);
                Ok(json!({}))
            }
            method::DAEMON_PLUGIN_COMMAND => {
                let params = parse_params::<PluginCommandParams>(params)?;
                manager
                    .run_command(&params.identity, &params.command, params.task_ids)
                    .await
                    .map_err(|error| host_error("command", &error))?;
                Ok(json!({}))
            }
            method::DAEMON_PLUGIN_INVOKE => {
                let params = parse_params::<PluginInvokeParams>(params)?;
                manager
                    .invoke(&params.identity, &params.name, params.args)
                    .await
                    .map_err(|error| host_error("name", &error))
            }
            method::DAEMON_PLUGIN_PAGE_INVOKE => {
                let params = parse_params::<PluginPageInvokeParams>(params)?;
                let (value, started) = manager
                    .invoke_from_page(
                        &params.identity,
                        &params.name,
                        params.args,
                        &params.page_url,
                    )
                    .await
                    .map_err(|error| host_error("name", &error))?;
                to_value(PluginPageInvokeResultDto {
                    value,
                    created_task_ids: started.created,
                    resumed_task_ids: started.resumed,
                })
            }
            other => Err(internal_error(format!(
                "unrouted plugin host method: {other}"
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use fluxdown_engine::plugin::PluginManager;
    use fluxdown_engine::plugin::quickjs::QuickJsScriptRuntime;
    use fluxdown_engine::plugin::runtime::{
        BridgeHttpRequest, BridgeHttpResponse, PluginBridge, PluginError, PluginLogLevel,
    };
    use fluxdown_protocol::{
        ApplicationErrorCode, DaemonEvent, PluginViewFrameDto, ServiceEvent, ViewOp,
    };
    use serde_json::{Value, json};
    use tokio::sync::broadcast;

    use crate::event_hub::{DaemonEngineEventSink, DaemonEventHub};
    use crate::service::DaemonService;

    const ID: &str = "tester@views";

    const MAIN_JS: &str = r#"
flux.ui.view("counter", {
  init: () => ({ n: 0, fail: false }),
  render: (st) => {
    if (st.fail) throw new Error("boom");
    return flux.ui.h("stack", {}, flux.ui.h("text", { text: String(st.n) }));
  },
  event: (st, ev) => {
    if (ev.type === "fail") st.fail = true;
    else if (ev.type === "heal") st.fail = false;
    else st.n += 1;
  },
});
flux.commands.register("noop", async () => {});
flux.rpc.handle("sum", async (a) => a.x + a.y);
"#;

    struct NullBridge;

    #[async_trait::async_trait]
    impl PluginBridge for NullBridge {
        async fn http_request(
            &self,
            _plugin_id: &str,
            _req: BridgeHttpRequest,
        ) -> Result<BridgeHttpResponse, PluginError> {
            Err(PluginError::Runtime("no network in test".to_owned()))
        }
        async fn storage_get(&self, _plugin_id: &str, _key: &str) -> Option<String> {
            None
        }
        async fn storage_set(
            &self,
            _plugin_id: &str,
            _key: &str,
            _value: String,
        ) -> Result<(), PluginError> {
            Ok(())
        }
        fn log(&self, _plugin_id: &str, _level: PluginLogLevel, _message: &str) {}
        fn request_retry(&self, _plugin_id: &str, _task_id: &str, _delay_ms: u64) {}
    }

    struct Fixture {
        service: DaemonService,
        frames: broadcast::Receiver<fluxdown_protocol::EventFrame>,
        base: std::path::PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.base) {
                eprintln!("plugin host test dir removal failed: {error}");
            }
        }
    }

    async fn fixture() -> Fixture {
        fixture_with(
            json!({
                "identity": ID, "name": "Views", "version": "1.0.0",
                "main": { "entry": "main.js" },
                "contributes": {
                    "views": [{ "id": "counter", "title": "Counter", "location": "activity" }],
                    "commands": [{ "id": "noop", "title": "Noop" }],
                },
            }),
            MAIN_JS,
            &[],
        )
        .await
    }

    async fn fixture_with(manifest: Value, main_js: &str, files: &[(&str, &str)]) -> Fixture {
        let base =
            std::env::temp_dir().join(format!("fluxdown-plugin-views-{}", uuid::Uuid::new_v4()));
        let root = base.join("plugins");
        let source = base.join("source");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(source.join("main.js"), main_js).unwrap();
        for (name, content) in files {
            let path = source.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let db = fluxdown_engine::db::Db::connect("sqlite::memory:")
            .await
            .unwrap();
        let events = DaemonEventHub::new(Default::default(), 64);
        let frames = events.subscribe();
        let manager = Arc::new(PluginManager::new(
            Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
            Arc::new(NullBridge),
            db.clone(),
            root,
            "1.0.0".to_owned(),
            Arc::new(DaemonEngineEventSink(events.clone())),
        ));
        // 先完成一次加载（含「升级前已装插件视为已授权」的一次性迁移），之后安装的插件才需授权。
        manager.load_all().await;
        manager.install_from_dir(&source).await.unwrap();
        let selections = crate::selection::DaemonSelection::new(events.clone());
        let blobs = Arc::new(
            crate::blob_store::BlobStore::open(base.join("blobs"))
                .await
                .unwrap(),
        );
        let service = DaemonService::new(
            crate::service_hello("plugin-views-test", Vec::new()),
            events,
            selections,
            blobs,
            crate::actor::DaemonActorHandle::disconnected(),
            db,
            base.clone(),
            Some(manager),
        );
        Fixture {
            service,
            frames,
            base,
        }
    }

    async fn call(
        f: &Fixture,
        method: &str,
        params: Value,
    ) -> Result<Value, fluxdown_protocol::RpcErrorObject> {
        f.service
            .call_inner("plugin-views-test", true, method, Some(params))
            .await
    }

    /// 等下一个视图帧（跳过其他事件）。
    async fn next_frame(f: &mut Fixture) -> PluginViewFrameDto {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), f.frames.recv())
                .await
                .expect("a plugin view frame arrives")
                .expect("event channel open");
            if let ServiceEvent::Daemon(DaemonEvent::PluginViewFrame(frame)) = frame.event {
                return frame;
            }
        }
    }

    async fn open(f: &Fixture) -> String {
        let opened = call(
            f,
            "daemon.plugin.view.open",
            json!({ "identity": ID, "view": "counter" }),
        )
        .await
        .unwrap();
        assert_eq!(opened["seq"], 0);
        assert_eq!(opened["tree"]["k"], "stack");
        opened["session"].as_str().unwrap().to_owned()
    }

    async fn press(f: &Fixture, session: &str, kind: &str) {
        call(
            f,
            "daemon.plugin.view.event",
            json!({ "session": session, "node": "root", "event": { "type": kind } }),
        )
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rendered_events_become_sequential_frames_and_snapshot_tracks_them() {
        let mut f = fixture().await;
        let session = open(&f).await;

        press(&f, &session, "press").await;
        let first = next_frame(&mut f).await;
        assert_eq!((first.session.as_str(), first.seq), (session.as_str(), 1));
        press(&f, &session, "press").await;
        let second = next_frame(&mut f).await;
        assert_eq!(second.seq, 2);

        let snapshot = call(
            &f,
            "daemon.plugin.view.snapshot",
            json!({ "session": session }),
        )
        .await
        .unwrap();
        assert_eq!(snapshot["seq"], 2);
        assert_eq!(snapshot["tree"]["c"][0]["p"]["text"], "2");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failure_frame_is_cleared_by_the_next_successful_render() {
        let mut f = fixture().await;
        let session = open(&f).await;

        press(&f, &session, "fail").await;
        let failed = next_frame(&mut f).await;
        assert!(
            matches!(&failed.ops[..], [ViewOp::Error { message }] if message.contains("boom")),
            "unexpected ops: {:?}",
            failed.ops
        );
        let snapshot = call(
            &f,
            "daemon.plugin.view.snapshot",
            json!({ "session": session }),
        )
        .await
        .unwrap();
        assert!(snapshot["tree"]["p"]["__error"].is_string());

        press(&f, &session, "heal").await;
        let healed = next_frame(&mut f).await;
        assert_eq!(healed.seq, failed.seq + 1);
        let snapshot = call(
            &f,
            "daemon.plugin.view.snapshot",
            json!({ "session": session }),
        )
        .await
        .unwrap();
        assert!(snapshot["tree"]["p"].get("__error").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_session_and_closed_session_are_invalid_arguments() {
        let f = fixture().await;
        let error = call(
            &f,
            "daemon.plugin.view.snapshot",
            json!({ "session": "nope" }),
        )
        .await
        .unwrap_err();
        let data = error.data.expect("application error data");
        assert_eq!(data.code, ApplicationErrorCode::InvalidArgument);
        assert_eq!(data.field.as_deref(), Some("session"));

        let session = open(&f).await;
        call(
            &f,
            "daemon.plugin.view.close",
            json!({ "session": session }),
        )
        .await
        .unwrap();
        let error = call(
            &f,
            "daemon.plugin.view.snapshot",
            json!({ "session": session }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.data.expect("application error data").code,
            ApplicationErrorCode::InvalidArgument
        );
        let error = call(
            &f,
            "daemon.plugin.view.event",
            json!({ "session": session, "node": "root", "event": { "type": "press" } }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.data.expect("application error data").code,
            ApplicationErrorCode::InvalidArgument
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_and_invoke_reach_the_plugin_host() {
        let f = fixture().await;
        call(
            &f,
            "daemon.plugin.command",
            json!({ "identity": ID, "command": "noop", "taskIds": ["a"] }),
        )
        .await
        .unwrap();
        let sum = call(
            &f,
            "daemon.plugin.invoke",
            json!({ "identity": ID, "name": "sum", "args": { "x": 2, "y": 3 } }),
        )
        .await
        .unwrap();
        assert_eq!(sum, json!(5));
        let error = call(
            &f,
            "daemon.plugin.command",
            json!({ "identity": ID, "command": "undeclared" }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.data.expect("application error data").code,
            ApplicationErrorCode::InvalidArgument
        );
    }

    const PAGE_MAIN_JS: &str = r#"
flux.rpc.handle("echo", async (a, ctx) => ({ got: a, ctx }), { page: true });
flux.rpc.handle("hidden", async () => "secret");
const optsErrors = [];
for (const bad of [null, 5, [], { page: "yes" }]) {
  try {
    flux.rpc.handle("x", async () => 1, bad);
    optsErrors.push("accepted");
  } catch (e) {
    optsErrors.push(e instanceof TypeError ? "TypeError" : "other");
  }
}
flux.rpc.handle("optsErrors", async () => optsErrors);
"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn page_scripts_index_source_and_page_invoke() {
        let f = fixture_with(
            json!({
                "identity": ID, "name": "Views", "version": "1.0.0",
                "permissions": ["pageScripts"],
                "main": { "entry": "main.js" },
                "contributes": {
                    "pageScripts": [{
                        "id": "demo", "title": "Demo",
                        "matches": ["https://*.example.com/*"],
                        "js": ["page/a.js", "page/b.js"],
                    }],
                },
            }),
            PAGE_MAIN_JS,
            &[("page/a.js", "var a = 1;"), ("page/b.js", "var b = 2;")],
        )
        .await;

        // 未授权：不入索引，源码与页面调用都被拒。
        let index = call(&f, "daemon.plugin.pageScripts", json!({}))
            .await
            .unwrap();
        assert_eq!(index["scripts"], json!([]));
        assert_eq!(index["sequence"], 0);
        assert!(
            call(
                &f,
                "daemon.plugin.pageScriptSource",
                json!({ "identity": ID, "id": "demo" })
            )
            .await
            .is_err()
        );
        let denied = call(
            &f,
            "daemon.plugin.pageInvoke",
            json!({ "identity": ID, "name": "echo", "args": 1, "pageUrl": "https://a.example.com/" }),
        )
        .await;
        assert!(denied.is_err());

        f.service
            .plugin_manager()
            .unwrap()
            .grant_consent(ID, &["pageScripts".to_owned()], &[])
            .await
            .unwrap();

        let index = call(&f, "daemon.plugin.pageScripts", json!({}))
            .await
            .unwrap();
        assert_eq!(index["sequence"], 1);
        let scripts = index["scripts"].as_array().unwrap();
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0]["identity"], ID);
        assert_eq!(scripts[0]["id"], "demo");
        assert_eq!(scripts[0]["runAt"], "document_idle");
        assert_eq!(scripts[0]["world"], "isolated");
        assert_eq!(scripts[0]["bytes"], "var a = 1;\n;\nvar b = 2;".len());

        let source = call(
            &f,
            "daemon.plugin.pageScriptSource",
            json!({ "identity": ID, "id": "demo" }),
        )
        .await
        .unwrap();
        assert_eq!(source["code"], "var a = 1;\n;\nvar b = 2;");
        assert_eq!(source["sha256"], scripts[0]["sha256"]);
        assert!(
            call(
                &f,
                "daemon.plugin.pageScriptSource",
                json!({ "identity": ID, "id": "nope" })
            )
            .await
            .is_err()
        );

        let echoed = call(
            &f,
            "daemon.plugin.pageInvoke",
            json!({ "identity": ID, "name": "echo", "args": { "k": 1 }, "pageUrl": "https://a.example.com/x" }),
        )
        .await
        .unwrap();
        assert_eq!(echoed["value"]["got"], json!({ "k": 1 }));
        assert_eq!(
            echoed["value"]["ctx"],
            json!({ "caller": "page", "pageUrl": "https://a.example.com/x" })
        );

        let hidden = call(
            &f,
            "daemon.plugin.pageInvoke",
            json!({ "identity": ID, "name": "hidden", "args": null, "pageUrl": "https://a.example.com/" }),
        )
        .await
        .unwrap_err();
        assert!(
            hidden.message.contains("rpc not exposed to page: hidden"),
            "{}",
            hidden.message
        );

        // 桌面 invoke 仍可调用任何处理器，ctx 为 desktop。
        let desktop = call(
            &f,
            "daemon.plugin.invoke",
            json!({ "identity": ID, "name": "echo", "args": 7 }),
        )
        .await
        .unwrap();
        assert_eq!(desktop["ctx"], json!({ "caller": "desktop" }));
        assert_eq!(
            call(
                &f,
                "daemon.plugin.invoke",
                json!({ "identity": ID, "name": "hidden" })
            )
            .await
            .unwrap(),
            json!("secret")
        );
        assert_eq!(
            call(
                &f,
                "daemon.plugin.invoke",
                json!({ "identity": ID, "name": "optsErrors" })
            )
            .await
            .unwrap(),
            json!(["TypeError", "TypeError", "TypeError", "TypeError"])
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actions_from_init_reach_clients_and_closed_session_is_not_stored() {
        let mut f = fixture_with(
            json!({
                "identity": ID, "name": "Views", "version": "1.0.0",
                "main": { "entry": "main.js" },
                "contributes": {
                    "views": [{ "id": "gate", "title": "Gate", "location": "activity" }],
                },
            }),
            r#"
flux.ui.view("gate", {
  init: async (ctx) => { await ctx.navigate("downloads"); ctx.close(); return {}; },
  render: () => flux.ui.h("text", { text: "bye" }),
});
"#,
            &[],
        )
        .await;
        let opened = call(
            &f,
            "daemon.plugin.view.open",
            json!({ "identity": ID, "view": "gate" }),
        )
        .await
        .unwrap();
        let session = opened["session"].as_str().unwrap().to_owned();
        // 存储没有留下已在 init 里关闭的会话。
        let error = call(
            &f,
            "daemon.plugin.view.snapshot",
            json!({ "session": session }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.data.as_ref().map(|data| data.code),
            Some(ApplicationErrorCode::InvalidArgument)
        );
        // 动作照样广播（跳转在前、关闭在后），客户端缓存后在登记会话时执行。
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let frame = tokio::time::timeout(Duration::from_secs(10), f.frames.recv())
                .await
                .expect("plugin view events arrive")
                .expect("event channel open");
            match frame.event {
                ServiceEvent::Daemon(DaemonEvent::PluginViewFrame(frame))
                    if frame.session == session =>
                {
                    assert_eq!(frame.seq, 0);
                    seen.push(format!("{:?}", frame.effects));
                }
                ServiceEvent::Daemon(DaemonEvent::PluginViewClosed { session: s, reason })
                    if s == session =>
                {
                    seen.push(reason);
                }
                _ => {}
            }
        }
        assert_eq!(
            seen,
            [
                format!(
                    "{:?}",
                    vec![fluxdown_protocol::ViewEffect::Navigate {
                        target: "downloads".to_owned()
                    }]
                ),
                "requested".to_owned(),
            ]
        );
    }
}
