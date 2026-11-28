//! resolver 的 QuickJS 堆上限：manifest `resolvers[0].memoryLimitMb` 替换宿主默认
//! 64 MiB（供要在脚本内解析大体量数据的插件声明），缺省时超出默认上限即
//! `MemoryLimitExceeded`。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::plugin::{PluginError, ResolveRequest};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), n)
}

async fn make_engine(work: &Path) -> Engine {
    let cfg = EngineConfig {
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
    };
    Engine::new(cfg, Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine")
}

/// resolver 在调用期持有约 96 MiB 的 ArrayBuffer（超过默认 64 MiB 上限）。
const HEAVY_RESOLVER: &str = r#"
globalThis.resolve = async (ctx) => {
  const chunks = [];
  for (let i = 0; i < 96; i++) chunks.push(new Uint8Array(1024 * 1024));
  return { url: "https://cdn.test/heavy.bin?n=" + chunks.length };
};
"#;

async fn write_plugin(dir: &Path, identity: &str, host: &str, memory: Option<u64>) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let memory = memory
        .map(|mb| format!(r#", "memoryLimitMb": {mb}"#))
        .unwrap_or_default();
    let manifest = format!(
        r#"{{
      "identity": "{identity}",
      "name": "Memory Budget",
      "version": "1.0.0",
      "resolvers": [{{ "match": {{ "urls": ["https://{host}/*"] }}, "entry": "resolve.js"{memory} }}]
    }}"#
    );
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    tokio::fs::write(dir.join("resolve.js"), HEAVY_RESOLVER)
        .await
        .expect("write resolve.js");
}

fn request(url: &str) -> ResolveRequest {
    ResolveRequest {
        task_id: "memory-budget".to_string(),
        url: url.to_string(),
        browser_original_url: None,
        auth_ref: String::new(),
        cookies: String::new(),
        referrer: String::new(),
        user_agent: String::new(),
        extra_headers: Default::default(),
        resolver_item: String::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_memory_limit_replaces_default_resolve_heap_cap() {
    let work = std::env::temp_dir().join(format!("fluxdown-resolve-memory-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");

    let declared = "tester@memory-declared";
    let default = "tester@memory-default";
    write_plugin(&work.join(declared), declared, "declared.test", Some(160)).await;
    write_plugin(&work.join(default), default, "default.test", None).await;
    for identity in [declared, default] {
        pm.install_from_dir(&work.join(identity))
            .await
            .expect("install resolver");
        pm.set_enabled(identity, true).await.expect("enable");
    }

    let out = pm
        .resolve(declared, request("https://declared.test/v"))
        .await
        .expect("declared budget fits the allocation")
        .expect("resolver output");
    assert_eq!(out.url, "https://cdn.test/heavy.bin?n=96");

    let err = pm
        .resolve(default, request("https://default.test/v"))
        .await
        .expect_err("default 64 MiB budget must reject the allocation");
    assert!(
        matches!(err, PluginError::MemoryLimitExceeded),
        "got {err:?}"
    );
}
