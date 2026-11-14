//! 真实 gpui-shell 运行时加载插件界面：入口解析、`fluxdown` 模块注入与失败兜底。

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use fluxdown_protocol::PluginDesktopEntryDto;
use fluxdown_ui_i18n::{I18nCatalog, Translator};
use gpui::{
    AnyView, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, TestApp, Window, div,
};
use gpui_shell::{ScriptView, ShellRoot, ToastRequest};
use serde_json::{Value, json};

use super::{PluginUiPort, PluginWindowSpec, PortFuture};

#[derive(Default)]
struct RecordingPort {
    calls: Mutex<Vec<(String, Value)>>,
}

impl PluginUiPort for RecordingPort {
    fn call(&self, method: String, params: Value) -> PortFuture<Value> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((method, params));
        }
        Box::pin(async { Ok(json!({ "tasks": [] })) })
    }
}

struct Host {
    content: AnyView,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.content.clone())
    }
}

/// 临时插件包（测试结束删除）。
struct Package(PathBuf);

impl Package {
    fn new(name: &str, entry: &str, source: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("fluxdown-plugin-ui-{name}-{}", std::process::id()));
        let path = dir.join(entry);
        let parent = path.parent().unwrap_or(&dir);
        std::fs::create_dir_all(parent).expect("create package dir");
        std::fs::write(&path, source).expect("write entry");
        Self(dir)
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("cleanup {}: {error}", self.0.display());
        }
    }
}

fn spec(package: &Package, entry: &str) -> PluginWindowSpec {
    PluginWindowSpec {
        identity: "tester@window".to_owned(),
        name: "Window".to_owned(),
        view_id: "main".to_owned(),
        view_title: "Window".to_owned(),
        package_dir: package.0.clone(),
        entry: PluginDesktopEntryDto {
            entry: entry.to_owned(),
            width: None,
            height: None,
            network_hosts: Vec::new(),
        },
    }
}

fn app(port: Arc<RecordingPort>, data_root: PathBuf) -> (TestApp, Entity<Translator>) {
    let mut app = TestApp::new();
    let translator = app.update(|cx| {
        super::init(cx, port, data_root).expect("plugin UI init");
        let catalog = I18nCatalog::load_embedded().expect("embedded translations");
        cx.new(|_| catalog.translator("en"))
    });
    (app, translator)
}

fn load(app: &mut TestApp, spec: PluginWindowSpec) -> Result<(), String> {
    load_in(app, spec, super::ViewContainer::default())
}

/// 同 [`load`]，由给定容器承载。
fn load_in(
    app: &mut TestApp,
    spec: PluginWindowSpec,
    container: super::ViewContainer,
) -> Result<(), String> {
    let failure = Arc::new(Mutex::new(None));
    let seen = failure.clone();
    let window = app.open_window(move |window, cx| {
        match super::load_desktop_view(&spec, container, window, cx) {
            Ok(content) => Host {
                content: content.into(),
            },
            Err(error) => {
                if let Ok(mut seen) = seen.lock() {
                    *seen = Some(error.to_string());
                }
                Host {
                    content: super::load_failure_view(&error, cx),
                }
            }
        }
    });
    app.run_until_parked();
    let script = window.read(|host, _| host.content.clone().downcast::<ScriptView>().is_ok());
    match failure.lock().map_err(|_| "poisoned".to_owned())?.clone() {
        Some(error) => Err(error),
        None if script => Ok(()),
        None => Err("loaded a non-script view without an error".to_owned()),
    }
}

#[test]
fn window_entry_loads_and_reaches_agent_through_fluxdown_module() {
    let package = Package::new(
        "ok",
        "ui/window.js",
        r#"
import { View, div } from "gpui-kit";
import { identity, call } from "fluxdown";

export default class Window extends View {
  init() {
    // 非白名单方法在调用处同步抛错（与 gpui-shell 的能力拒绝一致），不会到达 agent。
    try {
      call("system.shutdown");
      this.denied = false;
    } catch {
      this.denied = true;
    }
    this.id = identity();
    call("daemon.task.list", { limit: 5, who: this.id, denied: this.denied });
  }
  render(cx) {
    return div().child(this.id);
  }
}
"#,
    );
    let port = Arc::new(RecordingPort::default());
    let (mut app, _) = app(port.clone(), package.0.join("data"));

    assert_eq!(load(&mut app, spec(&package, "ui/window.js")), Ok(()));
    let calls = port.calls.lock().expect("calls").clone();
    // 只有白名单方法到达 agent；整数参数保持 JSON 整数。
    assert_eq!(
        calls,
        vec![(
            "daemon.task.list".to_owned(),
            json!({ "limit": 5, "who": "tester@window", "denied": true })
        )]
    );
}

/// `fluxdown.close()` / `fluxdown.navigate()`：容器可关闭时返回 `true`；跳转交给进程级跳转器，
/// 形状非法或跳转器拒绝时同步抛错。
#[test]
fn fluxdown_module_closes_the_container_and_navigates() {
    let package = Package::new(
        "host-actions",
        "ui/window.js",
        r#"
import { View, div } from "gpui-kit";
import { call, close, navigate } from "fluxdown";

export default class Window extends View {
  init() {
    const closed = close();
    navigate("rss");
    navigate({ view: "dash" });
    const rejected = [];
    for (const target of ["settings", { view: "" }, 5, { view: "ghost" }]) {
      try {
        navigate(target);
        rejected.push(false);
      } catch {
        rejected.push(true);
      }
    }
    call("daemon.task.list", { closed, rejected });
  }
  render(cx) { return div(); }
}
"#,
    );
    let port = Arc::new(RecordingPort::default());
    let (mut app, _) = app(port.clone(), package.0.join("data"));
    let navigated = Rc::new(RefCell::new(Vec::new()));
    let recorder = navigated.clone();
    app.update(|cx| {
        super::set_navigator(
            cx,
            Rc::new(move |identity, route, _| {
                recorder
                    .borrow_mut()
                    .push((identity.to_owned(), route.to_owned()));
                if route.ends_with(":ghost") {
                    Err("view 'ghost' is not an activity view of this plugin".to_owned())
                } else {
                    Ok(())
                }
            }),
        );
    });
    let closes = Rc::new(Cell::new(0_u32));
    let counter = closes.clone();
    let container = super::ViewContainer::closable(move |_| counter.set(counter.get() + 1));

    assert_eq!(
        load_in(&mut app, spec(&package, "ui/window.js"), container),
        Ok(())
    );
    assert_eq!(closes.get(), 1);
    assert_eq!(
        *navigated.borrow(),
        [
            ("tester@window".to_owned(), "rss".to_owned()),
            (
                "tester@window".to_owned(),
                "plugin:tester@window:dash".to_owned()
            ),
            (
                "tester@window".to_owned(),
                "plugin:tester@window:ghost".to_owned()
            ),
        ]
    );
    let calls = port.calls.lock().expect("calls").clone();
    assert_eq!(
        calls,
        vec![(
            "daemon.task.list".to_owned(),
            json!({ "closed": true, "rejected": [true, true, true, true] })
        )]
    );
}

#[test]
fn embedded_containers_cannot_be_closed() {
    let package = Package::new(
        "embedded-close",
        "ui/window.js",
        r#"
import { View, div } from "gpui-kit";
import { call, close } from "fluxdown";

export default class Window extends View {
  init() { call("daemon.task.list", { closed: close() }); }
  render(cx) { return div(); }
}
"#,
    );
    let port = Arc::new(RecordingPort::default());
    let (mut app, _) = app(port.clone(), package.0.join("data"));
    assert_eq!(load(&mut app, spec(&package, "ui/window.js")), Ok(()));
    let calls = port.calls.lock().expect("calls").clone();
    assert_eq!(
        calls,
        vec![("daemon.task.list".to_owned(), json!({ "closed": false }))]
    );
}

#[test]
fn broken_or_missing_entry_is_a_load_error_not_a_crash() {
    let package = Package::new(
        "broken",
        "window.js",
        "export default class extends NotDefinedAnywhere {}",
    );
    let port = Arc::new(RecordingPort::default());
    let (mut app, translator) = app(port, package.0.join("data"));

    let broken = load(&mut app, spec(&package, "window.js"));
    assert!(
        broken
            .as_ref()
            .is_err_and(|error| error.contains("NotDefinedAnywhere")),
        "{broken:?}"
    );
    // 远端主机的包目录在本机不存在时同样是加载错误。
    assert!(load(&mut app, spec(&package, "missing.js")).is_err());

    // 加载失败时 app 以失败面板代替脚本视图（不是脚本视图）。
    let missing = spec(&package, "missing.js");
    let window = app.open_window(move |window, cx| {
        let content =
            match super::load_desktop_view(&missing, super::ViewContainer::default(), window, cx) {
                Ok(view) => view.into(),
                Err(error) => super::load_failure_view(&error, cx),
            };
        Host { content }
    });
    app.run_until_parked();
    let script = window.read(|host, _| host.content.clone().downcast::<ScriptView>().is_ok());
    assert!(!script);
    drop(translator);
}

/// 窗口层级 `Root(ShellRoot(view))`（app 的组装方式）：`Root` 的直接内层是 `ShellRoot`
/// （gpui-shell `ShellRoot::find` 认的层级），脚本浮层的 toast 落在它上面。
#[test]
fn desktop_view_under_root_shell_root_hosts_toasts() {
    let package = Package::new(
        "toast",
        "ui/window.js",
        r#"
import { View, div } from "gpui-kit";
export default class Window extends View {
  render(cx) { return div().child("x"); }
}
"#,
    );
    let port = Arc::new(RecordingPort::default());
    let (mut app, _) = app(port, package.0.join("data"));
    let spec = spec(&package, "ui/window.js");
    let shell_root: Arc<Mutex<Option<Entity<ShellRoot>>>> = Arc::new(Mutex::new(None));
    let slot = shell_root.clone();
    let mut window = app.open_window(move |window, cx| {
        let view = super::load_desktop_view(&spec, super::ViewContainer::default(), window, cx)
            .expect("load desktop view");
        let root = cx.new(|cx| ShellRoot::new(view.into(), window, cx));
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(root.clone());
        }
        gpui_component::Root::new(root, window, cx)
    });
    app.run_until_parked();
    let root = shell_root
        .lock()
        .expect("slot")
        .clone()
        .expect("shell root created");
    let nested = window.read(|root_view, _| {
        root_view
            .view()
            .clone()
            .downcast::<ShellRoot>()
            .is_ok_and(|inner| inner == root)
    });
    assert!(nested, "Root's direct inner view must be the ShellRoot");
    window.update(|_, window, cx| {
        root.update(cx, |shell, cx| {
            shell.push_toast(ToastRequest::new("hello"), window, cx);
        });
    });
    app.run_until_parked();
    assert_eq!(app.read_entity(&root, |shell, _| shell.toast_count()), 1);
}
