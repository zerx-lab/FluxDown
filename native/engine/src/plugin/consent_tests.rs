//! 授权与生命周期审计的引擎层测试（zip 安装 → 待授权 → 授权 → 更新 → 卸载）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use crate::db::Db;
use crate::events::{EngineEvent, EventSink};
use crate::plugin::PluginManager;
use crate::plugin::quickjs::QuickJsScriptRuntime;
use crate::plugin::runtime::{
    BridgeHttpRequest, BridgeHttpResponse, PluginBridge, PluginError, PluginLogLevel,
};

pub(super) const ID: &str = "acme@demo";

struct NullBridge;

#[async_trait::async_trait]
impl PluginBridge for NullBridge {
    async fn http_request(
        &self,
        _plugin_id: &str,
        _req: BridgeHttpRequest,
    ) -> Result<BridgeHttpResponse, PluginError> {
        Err(PluginError::Runtime("no network in test".to_string()))
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

struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: EngineEvent) {}
}

pub(super) struct Fixture {
    pub(super) manager: Arc<PluginManager>,
    pub(super) db: Db,
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("consent test dir removal failed: {error}");
        }
    }
}

pub(super) async fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!("fluxdown-consent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = Db::connect("sqlite::memory:").await.unwrap();
    let manager = Arc::new(PluginManager::new(
        Arc::new(QuickJsScriptRuntime::new(1).unwrap()),
        Arc::new(NullBridge),
        db.clone(),
        root.clone(),
        "1.0.0".to_string(),
        Arc::new(NullSink),
    ));
    // 首次加载写迁移标记；此后安装的插件不再自动获得授权。
    manager.load_all().await;
    Fixture { manager, db, root }
}

pub(super) fn fxplug(version: &str, permissions: &[&str]) -> Vec<u8> {
    let manifest = serde_json::json!({
        "identity": ID, "name": "Demo", "version": version, "permissions": permissions,
    });
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("manifest.json", options).unwrap();
    writer.write_all(manifest.to_string().as_bytes()).unwrap();
    writer.finish().unwrap().into_inner()
}

async fn info(f: &Fixture) -> crate::plugin::PluginInfo {
    f.manager
        .list()
        .await
        .into_iter()
        .find(|p| p.identity == ID)
        .expect("plugin listed")
}

async fn actions(f: &Fixture) -> Vec<String> {
    let (rows, _) = f.manager.audit_list(Some(ID), None, 100).await.unwrap();
    // 升序（时间正序）便于断言先后。
    rows.into_iter().rev().map(|r| r.action).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_zip_install_requires_grant_and_audits_lifecycle() {
    let f = fixture().await;
    f.manager
        .install_from_zip(fxplug("1.0.0", &["tasks"]))
        .await
        .unwrap();

    let p = info(&f).await;
    assert_eq!(p.consent_pending, ["tasks"]);
    assert_eq!(p.disabled_reason, "ConsentRequired");
    assert!(!p.enabled);
    // 不写 enabled 键：授权后直接生效。
    assert!(
        f.db.get_config(&format!("plugin.{ID}.enabled"))
            .await
            .unwrap()
            .is_none_or(|v| v == "true")
    );

    match f.manager.set_enabled(ID, true).await {
        Err(PluginError::ManifestInvalid(m)) => assert_eq!(m, "consent required"),
        other => panic!("expected consent required, got {other:?}"),
    }
    match f.manager.grant_consent(ID, &["ytdlp".into()], &[]).await {
        Err(PluginError::ManifestInvalid(m)) => {
            assert_eq!(m, "consent does not match requested capabilities");
        }
        other => panic!("expected mismatch, got {other:?}"),
    }
    f.manager
        .grant_consent(ID, &["tasks".into()], &[])
        .await
        .unwrap();
    let p = info(&f).await;
    assert!(p.consent_pending.is_empty());
    assert!(p.enabled);
    assert_eq!(p.disabled_reason, "None");

    // 升级新增权限：旧授权保留，只有新增项待确认。
    f.manager
        .install_from_zip(fxplug("1.1.0", &["tasks", "ytdlp"]))
        .await
        .unwrap();
    let p = info(&f).await;
    assert_eq!(p.consent_pending, ["ytdlp"]);
    assert_eq!(p.disabled_reason, "ConsentRequired");
    f.manager
        .grant_consent(ID, &["ytdlp".into(), "tasks".into()], &[])
        .await
        .unwrap();

    f.manager.set_enabled(ID, false).await.unwrap();
    f.manager.set_enabled(ID, true).await.unwrap();
    let revoked = HashSet::from([(ID.to_string(), "1.1.0".to_string())]);
    f.manager.apply_market_revocations(&revoked).await.unwrap();
    f.manager.uninstall(ID).await.unwrap();

    assert_eq!(
        actions(&f).await,
        [
            "install",
            "consent.granted",
            "update",
            "consent.granted",
            "disable",
            "enable",
            "revoked",
            "uninstall",
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consent_corrupt_record_is_treated_as_ungranted() {
    let f = fixture().await;
    f.manager
        .install_from_zip(fxplug("1.0.0", &["tasks"]))
        .await
        .unwrap();
    f.db.set_config(&format!("plugin.{ID}.consent"), "{broken")
        .await
        .unwrap();
    f.manager.load_all().await;
    assert_eq!(info(&f).await.consent_pending, ["tasks"]);
}

fn write_dev_plugin(f: &Fixture, identity: &str) -> PathBuf {
    let dir = f.root.join(format!("src-{identity}"));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = serde_json::json!({ "identity": identity, "name": "Dev", "version": "0.1.0" });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
    dir
}

async fn revision_of(f: &Fixture, identity: &str) -> u64 {
    f.manager
        .list()
        .await
        .into_iter()
        .find(|p| p.identity == identity)
        .expect("plugin listed")
        .package_revision
}

/// 包修订号只随「替换包代码」的动作变化，且只变被操作的插件：桌面端据此重挂脚本视图，
/// 启停等其他整表重载不能让无关插件的界面被拆掉重建。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn package_revision_tracks_only_code_replacing_actions() {
    const OTHER: &str = "other@demo";
    let f = fixture().await;
    let dir = write_dev_plugin(&f, ID);
    f.manager.install_dev(&dir).await.unwrap();
    let other_dir = write_dev_plugin(&f, OTHER);
    f.manager.install_dev(&other_dir).await.unwrap();
    let installed = revision_of(&f, ID).await;
    let other = revision_of(&f, OTHER).await;

    f.manager.set_enabled(ID, false).await.unwrap();
    f.manager.set_enabled(ID, true).await.unwrap();
    f.manager.load_all().await;
    assert_eq!(revision_of(&f, ID).await, installed);

    f.manager.reload_dev(ID).await.unwrap();
    let reloaded = revision_of(&f, ID).await;
    assert_ne!(reloaded, installed);
    assert_eq!(revision_of(&f, OTHER).await, other);

    // 校验失败的重载不替换生效代码，修订号不变。
    std::fs::write(dir.join("manifest.json"), "{broken").unwrap();
    assert!(f.manager.reload_dev(ID).await.is_err());
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::json!({ "identity": ID, "name": "Dev", "version": "0.1.0" }).to_string(),
    )
    .unwrap();
    f.manager.load_all().await;
    assert_eq!(revision_of(&f, ID).await, reloaded);
}
