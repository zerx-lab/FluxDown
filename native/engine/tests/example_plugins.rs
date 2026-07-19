//! 校验 `examples/plugins/` 下随仓库分发的示例插件：manifest 必须通过
//! 引擎真实校验器，声明的入口脚本必须存在。防示例与校验规则漂移。
#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::events::{EngineEvent, EventSink};
use fluxdown_engine::plugin::manifest::PluginManifest;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins")
}

#[test]
fn all_example_plugin_manifests_are_valid() {
    let dir = examples_dir();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("读取 {dir:?} 失败: {e}"));
    let mut checked = 0usize;
    for entry in entries {
        let plugin_dir = entry.expect("dir entry").path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let manifest_path = plugin_dir.join("manifest.json");
        let bytes = std::fs::read(&manifest_path)
            .unwrap_or_else(|e| panic!("读取 {manifest_path:?} 失败: {e}"));
        let manifest = PluginManifest::parse(&bytes)
            .unwrap_or_else(|e| panic!("{manifest_path:?} 解析失败: {e}"));
        manifest
            .validate()
            .unwrap_or_else(|e| panic!("{manifest_path:?} 校验失败: {e}"));

        for resolver in &manifest.resolvers {
            let entry_path = plugin_dir.join(&resolver.entry);
            assert!(
                entry_path.is_file(),
                "{:?} 声明的 resolver 入口不存在: {:?}",
                manifest_path,
                entry_path
            );
        }
        if let Some(hooks) = &manifest.hooks {
            let entry_path = plugin_dir.join(&hooks.entry);
            assert!(
                entry_path.is_file(),
                "{:?} 声明的 hooks 入口不存在: {:?}",
                manifest_path,
                entry_path
            );
        }
        checked += 1;
    }
    assert!(checked >= 2, "示例插件目录应至少含 echo-rewriter 与 ytdlp");
}

/// 把 [`EngineEvent`] 转发到无界通道的测试专用 sink（照 `plugin_manifest_flow`
/// 模板），供 `begin_resolve_preview` 只读流程做断言。
struct ChannelSink(tokio::sync::mpsc::UnboundedSender<EngineEvent>);

impl EventSink for ChannelSink {
    fn emit(&self, event: EngineEvent) {
        let _ = self.0.send(event);
    }
}

/// 从事件通道中拿到下一个 `ResolvePreviewReady`，跳过其余事件；超时 panic。
async fn recv_preview_ready(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
) -> (String, usize, String) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), rx.recv())
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

/// `manifest-playground` 示例插件的初段 resolve 产物必须通过引擎真实清单
/// 校验器（路径安全/深度/条目数上限），两个数据集条目数与文档承诺一致：
/// normal=98、stress=1000（契约上限）。防示例脚本与运行时校验规则漂移。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_playground_preview_produces_valid_manifest() {
    let work = std::env::temp_dir().join(format!(
        "fluxdown-example-playground-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = Engine::new(
        EngineConfig {
            max_concurrent: 2,
            speed_limit_bps: 0,
            default_save_dir: work.to_string_lossy().into_owned(),
            app_data_dir: work.to_string_lossy().into_owned(),
            bt_config: BtConfig::default(),
            proxy_config: ProxyConfig::default(),
            user_agent: String::new(),
            data_dir_override: Some(work.clone()),
            database_url: None,
        },
        Arc::new(ChannelSink(tx)),
        Arc::new(NoopSelection),
    )
    .await
    .expect("engine");

    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&examples_dir().join("manifest-playground"))
        .await
        .expect("install manifest-playground");

    // normal 数据集（默认设置）：98 项、无错误。
    engine
        .manager
        .begin_resolve_preview(
            "p-normal".to_string(),
            "https://manifest.test/demo".to_string(),
            String::new(),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
        )
        .await;
    let (name, items, error) = recv_preview_ready(&mut rx).await;
    assert_eq!(error, "", "normal 数据集不得报错");
    assert_eq!(items, 98, "normal 数据集条目数须与示例文档一致");
    assert!(name.contains("沙丘"), "清单名透传: {name}");

    // stress 数据集：1000 项（清单契约上限，恰好放行）。
    pm.update_settings(
        "fluxdown@manifest-playground",
        &[("dataset".to_string(), "stress".to_string())],
    )
    .await
    .expect("set dataset=stress");
    engine
        .manager
        .begin_resolve_preview(
            "p-stress".to_string(),
            "https://manifest.test/demo".to_string(),
            String::new(),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
        )
        .await;
    let (name, items, error) = recv_preview_ready(&mut rx).await;
    assert_eq!(error, "", "stress 数据集不得报错");
    assert_eq!(items, 1000, "stress 数据集须恰好压在 1000 上限");
    assert!(name.contains("压测"), "清单名透传: {name}");

    let _ = tokio::fs::remove_dir_all(&work).await;
}
