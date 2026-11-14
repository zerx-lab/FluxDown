use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use fluxdown_protocol::{PluginViewFrameDto, VIEW_CLOSE_REASON_REQUESTED, ViewEffect, ViewOp};
use fluxdown_ui_i18n::I18nCatalog;
use gpui::{AppContext as _, Entity, TestApp};
use serde_json::{Value, json};

use super::{PluginViewHost, dispatch_closed, dispatch_frame, install, render, resync_all};
use crate::{PluginUiPort, PortFuture, ViewContainer, set_navigator};

fn tree(text: &str) -> Value {
    json!({
        "id": "root", "k": "stack", "p": {}, "c": [
            {"id": "root/0", "k": "text", "p": {"text": text}, "c": []}
        ]
    })
}
const PNG_1X1: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\x0bIDATx\x9cc\x60\x00\x02\x00\x00\x05\x00\x01\xa5\xf6\x45\x40\x00\x00\x00\x00IEND\xaeB\x60\x82";
const IMAGE_ID: &str = "12345678-1234-1234-1234-123456789abc";

fn image_tree() -> Value {
    json!({
        "id": "root", "k": "image",
        "p": {"src": format!("flux-image:{IMAGE_ID}"), "alt": "preview"},
        "c": []
    })
}

/// 记录全部调用；`view.open` 返回会话 `s1`，`view.snapshot` 返回 seq 4 的树。
#[derive(Default)]
struct MockPort {
    calls: Mutex<Vec<(String, Value)>>,
    session_exists: Mutex<bool>,
    /// `view.open` 的应答晚一拍（先让出一次执行权），用来模拟事件先于应答到达。
    slow_open: Mutex<bool>,
    image_calls: Mutex<usize>,
    image_failures_before_success: Mutex<usize>,
    image_tree: Mutex<bool>,
    image_ids: Mutex<Vec<String>>,
}

/// 第一次 poll 让出（并立即请求重新调度），第二次就绪。
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

impl MockPort {
    fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|calls| calls.iter().map(|(method, _)| method.clone()).collect())
            .unwrap_or_default()
    }

    fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    fn image_count(&self) -> usize {
        self.image_calls
            .lock()
            .map(|calls| *calls)
            .unwrap_or_default()
    }
    fn image_ids(&self) -> Vec<String> {
        self.image_ids
            .lock()
            .map(|ids| ids.clone())
            .unwrap_or_default()
    }
}

impl PluginUiPort for MockPort {
    fn call(&self, method: String, params: Value) -> PortFuture<Value> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method.clone(), params));
        }
        let exists = self.session_exists.lock().map(|e| *e).unwrap_or(true);
        let slow = method == "daemon.plugin.view.open"
            && self.slow_open.lock().map(|slow| *slow).unwrap_or(false);
        let reply = match method.as_str() {
            "daemon.plugin.view.open" => Ok(json!({
                "session": "s1",
                "seq": 0,
                "tree": if self.image_tree.lock().map(|has_image| *has_image).unwrap_or(false) {
                    image_tree()
                } else {
                    tree("0")
                }
            })),
            "daemon.plugin.view.snapshot" if exists => Ok(json!({"seq": 4, "tree": tree("4")})),
            "daemon.plugin.view.snapshot" => Err(fluxdown_protocol::RpcErrorData::new(
                fluxdown_protocol::ApplicationErrorCode::InvalidArgument,
                false,
            )),
            _ => Ok(json!({})),
        };
        Box::pin(async move {
            if slow {
                YieldOnce(false).await;
            }
            reply
        })
    }
    fn image(&self, _session: String, id: String) -> PortFuture<Vec<u8>> {
        if let Ok(mut ids) = self.image_ids.lock() {
            ids.push(id);
        }
        let mut failures = self
            .image_failures_before_success
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(mut calls) = self.image_calls.lock() {
            *calls += 1;
        }
        if *failures > 0 {
            *failures -= 1;
            Box::pin(async {
                Err(fluxdown_protocol::RpcErrorData::new(
                    fluxdown_protocol::ApplicationErrorCode::Unavailable,
                    true,
                ))
            })
        } else {
            Box::pin(async { Ok(PNG_1X1.to_vec()) })
        }
    }
}
fn frame(seq: u64, text: &str) -> PluginViewFrameDto {
    let mut set = serde_json::Map::new();
    set.insert("text".to_owned(), json!(text));
    PluginViewFrameDto {
        session: "s1".to_owned(),
        seq,
        ops: vec![ViewOp::Props {
            id: "root/0".to_owned(),
            set,
            unset: Vec::new(),
        }],
        effects: Vec::new(),
    }
}

fn text_of(host: &PluginViewHost) -> String {
    host.tree()
        .and_then(|tree| tree.c.first())
        .and_then(|node| node.p.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn setup() -> (TestApp, Arc<MockPort>, Entity<PluginViewHost>) {
    setup_with(ViewContainer::default())
}

fn setup_with(container: ViewContainer) -> (TestApp, Arc<MockPort>, Entity<PluginViewHost>) {
    let (mut app, port, host) = unopened(container);
    app.update(|cx| host.update(cx, |host, cx| host.activate(cx)));
    app.run_until_parked();
    (app, port, host)
}

/// 只创建宿主，不 `activate`（测试自行控制 open 与事件的先后）。
fn unopened(container: ViewContainer) -> (TestApp, Arc<MockPort>, Entity<PluginViewHost>) {
    let port = Arc::new(MockPort {
        session_exists: Mutex::new(true),
        ..MockPort::default()
    });
    unopened_with_port(container, port)
}

fn unopened_with_port(
    container: ViewContainer,
    port: Arc<MockPort>,
) -> (TestApp, Arc<MockPort>, Entity<PluginViewHost>) {
    let mut app = TestApp::new();
    let host = app.update(|cx| {
        let catalog = Arc::new(I18nCatalog::load_embedded().expect("embedded translations"));
        let translator = cx.new(|_| catalog.translator("en"));
        install(cx, port.clone(), translator.clone());
        let port: Arc<dyn PluginUiPort> = port.clone();
        cx.new(|cx| {
            PluginViewHost::new(
                "t@p".to_owned(),
                "main".to_owned(),
                None,
                container,
                port,
                translator,
                cx,
            )
        })
    });
    (app, port, host)
}

#[test]
fn transient_image_failure_retries_the_same_reference_until_it_loads() {
    let port = Arc::new(MockPort {
        session_exists: Mutex::new(true),
        image_failures_before_success: Mutex::new(1),
        image_tree: Mutex::new(true),
        ..MockPort::default()
    });
    let (mut app, port, host) = unopened_with_port(ViewContainer::default(), port);
    app.update(|cx| host.update(cx, |host, cx| host.activate(cx)));
    app.run_until_parked();
    assert_eq!(port.image_count(), 1);
    app.update(|cx| assert!(!host.read(cx).has_loaded_image(IMAGE_ID)));

    app.advance_clock(Duration::from_secs(2));
    app.run_until_parked();
    assert_eq!(port.image_count(), 2);
    assert_eq!(
        port.image_ids(),
        vec![IMAGE_ID.to_owned(), IMAGE_ID.to_owned()]
    );
    app.update(|cx| assert!(host.read(cx).has_loaded_image(IMAGE_ID)));
}

#[test]
fn removing_an_image_cancels_its_pending_retry() {
    let port = Arc::new(MockPort {
        session_exists: Mutex::new(true),
        image_failures_before_success: Mutex::new(usize::MAX),
        image_tree: Mutex::new(true),
        ..MockPort::default()
    });
    let (mut app, port, host) = unopened_with_port(ViewContainer::default(), port);
    app.update(|cx| host.update(cx, |host, cx| host.activate(cx)));
    app.run_until_parked();
    assert_eq!(port.image_count(), 1);

    let mut set = serde_json::Map::new();
    set.insert("src".to_owned(), json!("data:image/png;base64,AA=="));
    app.update(|cx| {
        dispatch_frame(
            PluginViewFrameDto {
                session: "s1".to_owned(),
                seq: 1,
                ops: vec![ViewOp::Props {
                    id: "root".to_owned(),
                    set,
                    unset: Vec::new(),
                }],
                effects: Vec::new(),
            },
            cx,
        )
    });
    app.advance_clock(Duration::from_secs(2));
    app.run_until_parked();
    assert_eq!(port.image_count(), 1);
    app.update(|cx| assert!(!host.read(cx).has_loaded_image(IMAGE_ID)));
}

#[test]
fn image_retries_stop_after_three_attempts() {
    let port = Arc::new(MockPort {
        session_exists: Mutex::new(true),
        image_failures_before_success: Mutex::new(usize::MAX),
        image_tree: Mutex::new(true),
        ..MockPort::default()
    });
    let (mut app, port, host) = unopened_with_port(ViewContainer::default(), port);
    app.update(|cx| host.update(cx, |host, cx| host.activate(cx)));
    app.run_until_parked();
    for delay in [2, 4, 8] {
        app.advance_clock(Duration::from_secs(delay));
        app.run_until_parked();
    }
    assert_eq!(port.image_count(), 4);
    app.advance_clock(Duration::from_secs(30));
    app.run_until_parked();
    assert_eq!(port.image_count(), 4);
    app.update(|cx| assert!(!host.read(cx).has_loaded_image(IMAGE_ID)));
}

#[test]
fn frames_apply_in_order_and_a_gap_fetches_a_snapshot() {
    let (mut app, port, host) = setup();
    app.update(|cx| {
        let host = host.read(cx);
        assert_eq!(host.session(), Some("s1"));
        assert_eq!(host.seq(), 0);
        assert_eq!(text_of(host), "0");
    });

    app.update(|cx| dispatch_frame(frame(1, "1"), cx));
    app.update(|cx| dispatch_frame(frame(2, "2"), cx));
    app.run_until_parked();
    app.update(|cx| {
        assert_eq!(host.read(cx).seq(), 2);
        assert_eq!(text_of(host.read(cx)), "2");
    });
    assert_eq!(port.count("daemon.plugin.view.snapshot"), 0);

    // 重复帧被忽略。
    app.update(|cx| dispatch_frame(frame(2, "dup"), cx));
    app.update(|cx| assert_eq!(text_of(host.read(cx)), "2"));

    // seq 跳到 4：重取 snapshot，树与序号追平。
    app.update(|cx| dispatch_frame(frame(4, "gap"), cx));
    app.run_until_parked();
    assert_eq!(port.count("daemon.plugin.view.snapshot"), 1);
    app.update(|cx| {
        assert_eq!(host.read(cx).seq(), 4);
        assert_eq!(text_of(host.read(cx)), "4");
    });
}

#[test]
fn frames_for_other_sessions_are_ignored() {
    let (mut app, _port, host) = setup();
    let mut other = frame(1, "x");
    other.session = "other".to_owned();
    app.update(|cx| dispatch_frame(other, cx));
    app.update(|cx| assert_eq!(host.read(cx).seq(), 0));
}

#[test]
fn closed_session_is_marked_and_reopen_opens_a_new_one() {
    let (mut app, port, host) = setup();
    app.update(|cx| dispatch_closed("s1", "pluginUnloaded", cx));
    app.update(|cx| {
        assert!(host.read(cx).is_closed());
        assert_eq!(host.read(cx).session(), None);
    });
    app.update(|cx| host.update(cx, |host, cx| host.activate(cx)));
    app.run_until_parked();
    assert_eq!(port.count("daemon.plugin.view.open"), 2);
    app.update(|cx| assert!(!host.read(cx).is_closed()));
}

/// 记录跳转请求 `(identity, route)`；`fail` 为真时模拟目标不存在。
fn install_navigator(app: &mut TestApp, fail: bool) -> Rc<RefCell<Vec<(String, String)>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let recorder = seen.clone();
    app.update(|cx| {
        set_navigator(
            cx,
            Rc::new(move |identity, route, _| {
                recorder
                    .borrow_mut()
                    .push((identity.to_owned(), route.to_owned()));
                if fail {
                    Err("missing".to_owned())
                } else {
                    Ok(())
                }
            }),
        );
    });
    seen
}

fn navigate_frame(seq: u64, target: &str) -> PluginViewFrameDto {
    PluginViewFrameDto {
        session: "s1".to_owned(),
        seq,
        ops: Vec::new(),
        effects: vec![
            ViewEffect::Unknown,
            ViewEffect::Navigate {
                target: target.to_owned(),
            },
        ],
    }
}

#[test]
fn effects_only_frame_navigates_without_touching_tree_or_seq() {
    let (mut app, port, host) = setup();
    let seen = install_navigator(&mut app, false);

    // 动作帧沿用会话当前序号（0），不改树、不触发 snapshot。
    app.update(|cx| dispatch_frame(navigate_frame(0, "downloads"), cx));
    app.run_until_parked();
    assert_eq!(*seen.borrow(), [("t@p".to_owned(), "downloads".to_owned())]);
    app.update(|cx| {
        assert_eq!(host.read(cx).seq(), 0);
        assert_eq!(text_of(host.read(cx)), "0");
    });
    assert_eq!(port.count("daemon.plugin.view.snapshot"), 0);

    // 带 ops 的下一帧：动作先执行，ops 仍按序号规则应用。
    let mut next = frame(1, "1");
    next.effects = vec![ViewEffect::Navigate {
        target: "plugin:t@p:dash".to_owned(),
    }];
    app.update(|cx| dispatch_frame(next, cx));
    assert_eq!(seen.borrow().len(), 2);
    assert_eq!(seen.borrow()[1].1, "plugin:t@p:dash");
    app.update(|cx| {
        assert_eq!(host.read(cx).seq(), 1);
        assert_eq!(text_of(host.read(cx)), "1");
    });
}

#[test]
fn failed_navigation_and_foreign_sessions_are_ignored() {
    let (mut app, _port, host) = setup();
    let seen = install_navigator(&mut app, true);
    app.update(|cx| dispatch_frame(navigate_frame(0, "rss"), cx));
    assert_eq!(seen.borrow().len(), 1);
    app.update(|cx| assert_eq!(host.read(cx).seq(), 0));

    let mut other = navigate_frame(0, "rss");
    other.session = "other".to_owned();
    app.update(|cx| dispatch_frame(other, cx));
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn requested_close_closes_a_closable_container() {
    let closes = Rc::new(Cell::new(0_u32));
    let counter = closes.clone();
    let container = ViewContainer::closable(move |_| counter.set(counter.get() + 1));
    let (mut app, _port, host) = setup_with(container);

    app.update(|cx| dispatch_closed("s1", VIEW_CLOSE_REASON_REQUESTED, cx));
    assert_eq!(closes.get(), 1);
    app.update(|cx| {
        // 容器会随之关闭，不显示「已结束」。
        assert!(!host.read(cx).is_closed());
        assert_eq!(host.read(cx).session(), None);
    });

    // 其他原因（插件卸载等）仍显示已结束态，不动容器。
    let (mut app, _port, host) = setup_with(ViewContainer::closable({
        let counter = closes.clone();
        move |_| counter.set(counter.get() + 1)
    }));
    app.update(|cx| dispatch_closed("s1", "pluginUnloaded", cx));
    assert_eq!(closes.get(), 1);
    app.update(|cx| assert!(host.read(cx).is_closed()));
}

#[test]
fn requested_close_without_a_container_shows_the_closed_state() {
    let (mut app, _port, host) = setup();
    app.update(|cx| dispatch_closed("s1", VIEW_CLOSE_REASON_REQUESTED, cx));
    app.update(|cx| {
        assert!(host.read(cx).is_closed());
        assert_eq!(host.read(cx).session(), None);
    });
}

#[test]
fn actions_arriving_before_the_open_reply_run_once_the_session_registers() {
    let closes = Rc::new(Cell::new(0_u32));
    let counter = closes.clone();
    let (mut app, port, host) = unopened(ViewContainer::closable(move |_| {
        counter.set(counter.get() + 1);
    }));
    if let Ok(mut slow) = port.slow_open.lock() {
        *slow = true;
    }
    let seen = install_navigator(&mut app, false);
    // 插件在 init 里 navigate + close：事件先于 open 应答到达（同一轮里 open 尚未完成）。
    app.update(|cx| {
        host.update(cx, |host, cx| host.activate(cx));
        dispatch_frame(navigate_frame(0, "downloads"), cx);
        dispatch_closed("s1", VIEW_CLOSE_REASON_REQUESTED, cx);
        // 应答尚未到达：会话未登记，动作只被缓存。
        assert_eq!(host.read(cx).session(), None);
        assert!(seen.borrow().is_empty());
        assert_eq!(closes.get(), 0);
    });
    app.run_until_parked();
    assert_eq!(*seen.borrow(), [("t@p".to_owned(), "downloads".to_owned())]);
    assert_eq!(closes.get(), 1);
    app.update(|cx| assert_eq!(host.read(cx).session(), None));
}

#[test]
fn actions_for_unknown_sessions_are_not_kept_without_an_open_in_flight() {
    let closes = Rc::new(Cell::new(0_u32));
    let counter = closes.clone();
    let (mut app, _port, host) = unopened(ViewContainer::closable(move |_| {
        counter.set(counter.get() + 1);
    }));
    // 没有进行中的 open：别的客户端会话的关闭事件不缓存，之后同名会话登记也不受影响。
    app.update(|cx| dispatch_closed("s1", VIEW_CLOSE_REASON_REQUESTED, cx));
    app.update(|cx| host.update(cx, |host, cx| host.activate(cx)));
    app.run_until_parked();
    assert_eq!(closes.get(), 0);
    app.update(|cx| assert_eq!(host.read(cx).session(), Some("s1")));
}

#[test]
fn resync_all_reopens_sessions_the_daemon_no_longer_has() {
    let (mut app, port, host) = setup();
    app.update(resync_all);
    app.run_until_parked();
    assert_eq!(port.count("daemon.plugin.view.snapshot"), 1);
    app.update(|cx| assert_eq!(host.read(cx).seq(), 4));

    if let Ok(mut exists) = port.session_exists.lock() {
        *exists = false;
    }
    app.update(resync_all);
    app.run_until_parked();
    assert_eq!(port.count("daemon.plugin.view.open"), 2);
    app.update(|cx| {
        assert_eq!(host.read(cx).seq(), 0);
        assert_eq!(text_of(host.read(cx)), "0");
    });
}

#[test]
fn releasing_the_host_closes_the_session() {
    let (mut app, port, host) = setup();
    drop(host);
    // 释放在下一次 effect flush 时执行。
    app.update(|_| ());
    app.run_until_parked();
    assert_eq!(port.count("daemon.plugin.view.close"), 1);
}

#[test]
fn tone_table_matches_the_shared_fixture() {
    let fixture: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "../../../../native/protocol/src/plugin_view_tones.json"
    ))
    .expect("tones fixture");
    let table: BTreeMap<String, String> = render::TONES
        .iter()
        .map(|(tone, path)| ((*tone).to_owned(), (*path).to_owned()))
        .collect();
    assert_eq!(table, fixture);
    assert_eq!(render::tone_path("danger"), Some("colors.destructive"));
    assert_eq!(render::tone_path("nope"), None);
}
