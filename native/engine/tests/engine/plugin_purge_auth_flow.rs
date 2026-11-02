//! 插件卸载/认证回归测试：覆盖 PR #671 评审发现的三个问题。
//!
//! - 671#1：`purge` 用未校验的失败插件 identity 拼 config 键前缀，会清空全部
//!   `plugin.dev.*` 注册（identity 恰为目录名 `"dev"` 时精确撞上该保留前缀）。
//! - M-3：`authenticate("logout")` 在插件被禁用时也必须放行（走宿主兜底删除），
//!   不应该要求先重新启用一个已知有问题的插件。
//! - M-4：安装回滚复用的 `purge` 不得连带删除认证凭据；只有用户主动
//!   `uninstall` 才清凭据。
//! - 671#6：失败插件的 `PluginInfo.enabled` 必须被引擎强制为 `false`。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fluxdown_engine::auth::{self, AuthProfile};
use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::plugin::{AuthRequest, PluginError, ResolveRequest};
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), n)
}

async fn make_engine(work: &std::path::Path) -> Engine {
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

/// 带 auth 入口的最小 dev 插件：auth.js 只用于满足安装期 `check_compile`，
/// 本文件的场景都不会真正驱动它跑 `authenticate()`。
async fn write_auth_plugin(dir: &std::path::Path, identity: &str) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = format!(
        r#"{{
      "identity": "{identity}",
      "name": "Test Auth Plugin",
      "version": "1.0.0",
      "permissions": ["auth"],
      "auth": {{ "entry": "auth.js" }}
    }}"#
    );
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    let auth_js = r#"
      globalThis.authenticate = async (ctx) => {
        return JSON.stringify({ status: "success" });
      };
    "#;
    tokio::fs::write(dir.join("auth.js"), auth_js)
        .await
        .expect("write auth.js");
}

async fn write_resolver_plugin(dir: &std::path::Path, identity: &str, host: &str) {
    tokio::fs::create_dir_all(dir)
        .await
        .expect("mkdir resolver");
    let manifest = format!(
        r#"{{
      "identity": "{identity}",
      "name": "Uninstall Resolver",
      "version": "1.0.0",
      "resolvers": [{{ "match": {{ "urls": ["https://{host}/*"] }}, "entry": "resolve.js" }}]
    }}"#
    );
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write resolver manifest");
    let source = format!(
        r#"globalThis.resolve = async () => ({{ url: "https://cdn.test/{identity}.bin" }});"#
    );
    tokio::fs::write(dir.join("resolve.js"), source)
        .await
        .expect("write resolver source");
}

/// 671#1：`root/dev/` 恰是一个没有 manifest.json 的目录（插件开发者常见的
/// 工作区习惯）→ 被当成 identity="dev" 的失败插件；卸载它不得清空 `plugin.
/// dev.*` 命名空间下其他真实 dev 插件的注册。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn purge_of_directory_named_dev_does_not_wipe_other_dev_registrations() {
    let work = std::env::temp_dir().join(format!("fluxdown-purge-dev-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");

    // root/dev/ 是一个空目录：无 manifest.json，会被 load_all 当作失败插件，
    // identity 落到目录名 "dev"（failure_identity 的目录名兜底分支）。
    let plugins_root = work.join("plugins");
    tokio::fs::create_dir_all(plugins_root.join("dev"))
        .await
        .expect("mkdir root/dev");

    // 另一个真正的 dev 插件，装在 root 之外的任意目录，走 plugin.dev.<id> 注册。
    let real_dev_src = work.join("real-dev-src");
    write_auth_plugin(&real_dev_src, "tester@realdev").await;
    pm.install_dev(&real_dev_src)
        .await
        .expect("install real dev plugin");

    // 触发一次重新扫描，让 root/dev/ 目录被识别为失败插件（identity="dev"）。
    pm.list().await;

    // 卸载失败插件 "dev"：671#1 修复前会把 `plugin.dev.` 当前缀扫描删除，
    // 连带清空 `plugin.dev.tester@realdev` 注册。
    pm.uninstall("dev").await.expect("uninstall dev");

    let plugins = pm.list().await;
    assert!(
        plugins.iter().any(|p| p.identity == "tester@realdev"),
        "unrelated dev plugin registration must survive purging the unrelated \"dev\" directory, got: {:?}",
        plugins.iter().map(|p| &p.identity).collect::<Vec<_>>()
    );
}

/// M-3 + M-4：禁用插件仍可 logout（宿主兜底删除凭据，不驱动 JS）；
/// 用户主动卸载会清凭据，但安装回滚（purge）不会。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_plugin_logout_removes_credential_without_running_js() {
    let work = std::env::temp_dir().join(format!("fluxdown-purge-auth-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");

    let plugin_src = work.join("auth-plugin-src");
    write_auth_plugin(&plugin_src, "tester@authflow").await;
    pm.install_from_dir(&plugin_src)
        .await
        .expect("install auth plugin");

    // 直接写入一份凭据（跳过真实登录流程，聚焦 logout/purge 的清理语义）。
    let db = engine.db.clone();
    let profile = AuthProfile {
        auth_ref: "tester@authflow::https://example.com".to_string(),
        plugin_id: "tester@authflow".to_string(),
        site: "https://example.com".to_string(),
        cookies: "sid=1".to_string(),
        ..Default::default()
    };
    auth::save(&db, &profile).await.expect("seed credential");

    // 禁用插件——M-3 修复前 authenticate() 在这里会直接报「插件未启用」。
    pm.set_enabled("tester@authflow", false)
        .await
        .expect("disable plugin");

    let result = pm
        .authenticate(
            "tester@authflow",
            AuthRequest {
                action: "logout".to_string(),
                site: String::new(),
                auth_ref: profile.auth_ref.clone(),
                session_id: String::new(),
                input: String::new(),
            },
        )
        .await
        .expect("logout on a disabled plugin must succeed via host-side fallback");
    assert_eq!(result.status, "success");

    let loaded = auth::load(&db, &profile.auth_ref)
        .await
        .expect("load after logout");
    assert!(
        loaded.is_none(),
        "logout must remove the credential even while the plugin is disabled"
    );
}

/// M-4：安装回滚（`install_dev` 因 manifest 非法而 `purge`）不得删除同一
/// identity 已经保存的凭据——回滚不是用户主动卸载。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_rollback_purge_preserves_existing_credential() {
    let work = std::env::temp_dir().join(format!("fluxdown-purge-rollback-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");

    let db = engine.db.clone();
    let profile = AuthProfile {
        auth_ref: "tester@rollback::https://example.com".to_string(),
        plugin_id: "tester@rollback".to_string(),
        site: "https://example.com".to_string(),
        cookies: "sid=1".to_string(),
        ..Default::default()
    };
    auth::save(&db, &profile).await.expect("seed credential");

    // 同 identity 的 dev 安装，但 manifest 本身非法（version 不是 semver）→
    // `PluginManifest::parse`/`validate()` 在 `install_dev` 顶部直接失败，
    // 根本不会写 `plugin.dev.<id>` 键、也走不到 `finish_install`/`purge`——
    // 这里改用「manifest 合法但 auth 脚本编译失败」触发 `finish_install`
    // 内部的 `check_compile` 失败，从而命中 `install_dev` 的
    // `purge(&identity)` 回滚分支。
    let bad_src = work.join("bad-dev-src");
    tokio::fs::create_dir_all(&bad_src)
        .await
        .expect("mkdir bad src");
    let manifest = r#"{
      "identity": "tester@rollback",
      "name": "Broken",
      "version": "1.0.0",
      "permissions": ["auth"],
      "auth": { "entry": "auth.js" }
    }"#;
    tokio::fs::write(bad_src.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    // 语法错误：check_compile 必然失败。
    tokio::fs::write(bad_src.join("auth.js"), b"this is not valid javascript {{{")
        .await
        .expect("write broken auth.js");

    let err = pm.install_dev(&bad_src).await;
    assert!(
        err.is_err(),
        "install must fail on a syntactically broken auth entry"
    );

    let loaded = auth::load(&db, &profile.auth_ref)
        .await
        .expect("load after failed install");
    assert!(
        loaded.is_some(),
        "a failed install's rollback purge must not delete a pre-existing credential for the same identity"
    );
}

/// 671#6：失败插件（无 manifest.json 的裸目录）在 `list()` 里必须
/// `enabled == false`，即便从未写过 `plugin.<identity>.enabled` 键
/// （该键缺省会被 `plugin_state()` 当作「新装默认启用」解释成 true）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_plugin_is_reported_as_disabled_even_without_enabled_key() {
    let work = std::env::temp_dir().join(format!("fluxdown-failed-enabled-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");

    let plugins_root = work.join("plugins");
    tokio::fs::create_dir_all(plugins_root.join("broken"))
        .await
        .expect("mkdir root/broken");
    pm.load_all().await;

    let plugins = pm.list().await;
    let broken = plugins
        .iter()
        .find(|p| p.identity == "broken")
        .expect("failed plugin listed");
    assert_eq!(broken.load_status, "Failed");
    assert!(
        !broken.enabled,
        "a failed plugin must never report enabled=true, contradicting its Failed load_status"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninstall_config_failure_invalidates_removed_resolver_and_preserves_other_plugins() {
    let work = std::env::temp_dir().join(format!("fluxdown-uninstall-state-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");
    let removed = "tester@uninstall-state";
    let retained = "tester@retained";
    let removed_url = "https://removed.test/watch";
    let retained_url = "https://retained.test/watch";
    for (identity, host) in [(removed, "removed.test"), (retained, "retained.test")] {
        let source = work.join(identity);
        write_resolver_plugin(&source, identity, host).await;
        pm.install_from_dir(&source)
            .await
            .expect("install resolver");
        pm.set_enabled(identity, true)
            .await
            .expect("enable resolver");
    }
    let request = |url: &str| ResolveRequest {
        task_id: "uninstall-state".to_string(),
        url: url.to_string(),
        auth_ref: String::new(),
        cookies: String::new(),
        referrer: String::new(),
        user_agent: String::new(),
        extra_headers: Default::default(),
        resolver_item: String::new(),
    };
    assert_eq!(
        pm.route_resolver(removed_url)
            .await
            .map(|r| r.identity)
            .as_deref(),
        Some(removed)
    );
    assert_eq!(
        pm.resolve(removed, request(removed_url))
            .await
            .expect("resolve installed plugin")
            .expect("resolver output")
            .url,
        format!("https://cdn.test/{removed}.bin")
    );

    // 在真实 SQLite 删除上失败：目录已经移除，enabled 键仍保留。
    let database_url = format!(
        "sqlite:{}?mode=rw",
        work.join("flux_down.db")
            .to_string_lossy()
            .replace('\\', "/")
    );
    let fault_db = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect fault-injection database");
    sqlx::raw_sql(
        "CREATE TRIGGER reject_plugin_cleanup BEFORE DELETE ON config
         WHEN OLD.key = 'plugin.tester@uninstall-state.enabled'
         BEGIN SELECT RAISE(ABORT, 'injected plugin cleanup failure'); END",
    )
    .execute(&fault_db)
    .await
    .expect("install cleanup failure trigger");

    let error = pm.uninstall(removed).await.expect_err("cleanup must fail");
    assert!(
        matches!(
            &error,
            PluginError::Runtime(message)
                if message.contains("injected plugin cleanup failure")
        ),
        "uninstall must preserve the original cleanup failure: {error:?}"
    );
    assert!(!work.join("plugins").join(removed).exists());
    assert_eq!(
        engine
            .db
            .get_config(&format!("plugin.{removed}.enabled"))
            .await
            .expect("read retained enabled key")
            .as_deref(),
        Some("true")
    );
    assert_eq!(pm.route_resolver(removed_url).await, None);
    assert!(
        matches!(
            pm.resolve(removed, request(removed_url)).await,
            Err(PluginError::Runtime(_))
        ),
        "an uninstalled resolver must not execute its cached script"
    );
    let plugins = pm.list().await;
    assert!(
        plugins.iter().all(|plugin| plugin.identity != removed),
        "the removed plugin must not remain Loaded after a cleanup failure"
    );
    let other = plugins
        .iter()
        .find(|plugin| plugin.identity == retained)
        .expect("unrelated plugin stays listed");
    assert_eq!(other.load_status, "Loaded");
    assert!(other.enabled);
    assert_eq!(
        pm.route_resolver(retained_url)
            .await
            .map(|r| r.identity)
            .as_deref(),
        Some(retained)
    );
    assert_eq!(
        pm.resolve(retained, request(retained_url))
            .await
            .expect("resolve unrelated plugin")
            .expect("unrelated resolver output")
            .url,
        format!("https://cdn.test/{retained}.bin")
    );

    fault_db.close().await;
    drop(pm);
    drop(engine);
    tokio::fs::remove_dir_all(&work)
        .await
        .expect("remove isolated test directory");
}

/// 安装扫描只认 manifest identity、不认目录名：`plugins/<identity>` 之外再有一份
/// 目录名不同的同 identity 副本（手动解压的旧版本），一次卸载必须把两份都删掉，
/// 否则重载后剩下的副本重新出现、卸载却报成功（要点两次才删干净）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninstall_removes_every_installed_copy_of_identity() {
    let work = std::env::temp_dir().join(format!("fluxdown-purge-copies-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");
    let identity = "tester@copies";

    let src = work.join("copies-src");
    write_resolver_plugin(&src, identity, "copies.test").await;
    pm.install_from_dir(&src).await.expect("install package");
    let stray = work.join("plugins").join("Old-Copy");
    write_resolver_plugin(&stray, identity, "copies.test").await;
    pm.load_all().await;
    assert_eq!(
        pm.list()
            .await
            .iter()
            .filter(|p| p.identity == identity)
            .count(),
        2,
        "both copies are listed before uninstall"
    );

    pm.uninstall(identity).await.expect("uninstall");

    assert!(
        pm.list().await.iter().all(|p| p.identity != identity),
        "a single uninstall must remove every copy"
    );
    assert!(!stray.exists());
    assert!(!work.join("plugins").join(identity).exists());

    drop(pm);
    drop(engine);
    tokio::fs::remove_dir_all(&work)
        .await
        .expect("remove isolated test directory");
}

/// 唯一副本的目录名不等于 identity 时，卸载删的必须是它真实所在的目录，而不是
/// 不存在的 `plugins/<identity>`（此前永远删不掉）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninstall_removes_copy_whose_directory_name_differs_from_identity() {
    let work = std::env::temp_dir().join(format!("fluxdown-purge-misnamed-{}", uniq()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir");
    let engine = make_engine(&work).await;
    let pm = engine.manager.plugin_manager().expect("pm installed");
    let identity = "tester@misnamed";

    let stray = work.join("plugins").join("Misnamed-Plugin");
    write_resolver_plugin(&stray, identity, "misnamed.test").await;
    pm.load_all().await;
    assert!(pm.list().await.iter().any(|p| p.identity == identity));

    pm.uninstall(identity).await.expect("uninstall");

    assert!(pm.list().await.iter().all(|p| p.identity != identity));
    assert!(!stray.exists());

    drop(pm);
    drop(engine);
    tokio::fs::remove_dir_all(&work)
        .await
        .expect("remove isolated test directory");
}
