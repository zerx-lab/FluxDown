//! FluxCloud 私有商店安装的引擎层测试：claim 核对、来源标记、公开市场隔离。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use super::consent_tests::{Fixture, ID, fixture, fxplug};
use super::market::{MarketClient, MarketEntry, MarketError, install_private_package, sha256_hex};
use super::{PluginError, PluginInfo, SOURCE_PRIVATE};
use crate::proxy_config::ProxyConfig;

fn entry(version: &str, permissions: &[&str], bytes: &[u8]) -> MarketEntry {
    serde_json::from_value(serde_json::json!({
        "pluginId": ID,
        "version": version,
        "sequence": 1,
        "contentHash": format!("sha256:{}", sha256_hex(bytes)),
        "permissions": permissions,
        "yanked": "none",
    }))
    .unwrap()
}

async fn listed(f: &Fixture) -> Option<PluginInfo> {
    f.manager
        .list()
        .await
        .into_iter()
        .find(|p| p.identity == ID)
}

async fn source_key(f: &Fixture) -> Option<String> {
    f.db.get_config(&format!("plugin.{ID}.source"))
        .await
        .unwrap()
}

fn revoked(version: &str) -> HashSet<(String, String)> {
    HashSet::from([(ID.to_string(), version.to_string())])
}

/// 联网前的快速拒绝之外，安装锁内的重查是原子兜底：直接以 `forbid_private` 走公开安装路径，
/// 即使上层快速检查被绕过（或来源在检查后才变成 private），也回滚且不改已装插件与来源。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_install_path_rechecks_the_source_under_the_install_lock() {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);
    install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], &bytes))
        .await
        .unwrap();

    let error = f
        .manager
        .install_from_zip_checked(fxplug("1.1.0", &[]), true, |_| Ok(()))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, PluginError::PrivateSource(id) if id == ID),
        "{error:?}"
    );
    let info = listed(&f).await.expect("still installed");
    assert_eq!(info.version, "1.0.0");
    assert_eq!(info.source, SOURCE_PRIVATE);
    assert!(info.enabled);

    // 非私有来源不受影响；声明 forbid_private 的公开安装对它照常成功。
    f.manager
        .install_from_zip(fxplug("1.2.0", &[]))
        .await
        .unwrap();
    f.manager
        .install_from_zip_checked(fxplug("1.3.0", &[]), true, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(listed(&f).await.expect("listed").version, "1.3.0");
    assert_eq!(source_key(&f).await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matching_claim_installs_and_marks_private_source() {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);
    let identity = install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], &bytes))
        .await
        .unwrap();
    assert_eq!(identity, ID);
    assert_eq!(source_key(&f).await.as_deref(), Some(SOURCE_PRIVATE));
    let info = listed(&f).await.expect("listed");
    assert_eq!(info.source, SOURCE_PRIVATE);
    assert_eq!(info.version, "1.0.0");
    assert!(f.manager.is_private_source(ID).await.unwrap());

    // 私有升级保持来源。
    let next = fxplug("1.1.0", &[]);
    install_private_package(&f.manager, next.clone(), &entry("1.1.0", &[], &next))
        .await
        .unwrap();
    assert_eq!(listed(&f).await.expect("listed").version, "1.1.0");
    assert_eq!(source_key(&f).await.as_deref(), Some(SOURCE_PRIVATE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_claims_are_rejected_without_touching_installed_state() {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);

    // identity：条目声明另一个 id。
    let mut other = entry("1.0.0", &[], &bytes);
    other.plugin_id = "acme@other".to_string();
    let error = install_private_package(&f.manager, bytes.clone(), &other)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, MarketError::Plugin(PluginError::ManifestInvalid(m)) if m.contains("identity")),
        "{error:?}"
    );

    // version：条目与包内版本不同。
    let error = install_private_package(&f.manager, bytes.clone(), &entry("1.0.1", &[], &bytes))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, MarketError::Plugin(PluginError::ManifestInvalid(m)) if m.contains("version")),
        "{error:?}"
    );

    // contentHash：条目钉住的是另一份字节。
    let error = install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], b"other"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, MarketError::HashMismatch { .. }),
        "{error:?}"
    );

    // 权限：包内声明了条目未展示的权限。
    let greedy = fxplug("1.0.0", &["tasks"]);
    let error = install_private_package(&f.manager, greedy.clone(), &entry("1.0.0", &[], &greedy))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, MarketError::Plugin(PluginError::ManifestInvalid(m)) if m.contains("tasks")),
        "{error:?}"
    );

    // 被撤回 / 弃用的条目不可装。
    let mut yanked = entry("1.0.0", &[], &bytes);
    yanked.yanked = "deprecated".to_string();
    let error = install_private_package(&f.manager, bytes.clone(), &yanked)
        .await
        .unwrap_err();
    assert!(matches!(error, MarketError::Yanked(_)), "{error:?}");

    assert!(listed(&f).await.is_none());
    assert_eq!(source_key(&f).await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_private_upgrade_keeps_previous_source() {
    let f = fixture().await;
    // 先以本机包装 1.0.0（无来源标记）；失败的私有升级不能留下 private 标记。
    f.manager
        .install_from_zip(fxplug("1.0.0", &[]))
        .await
        .unwrap();
    let bad = fxplug("1.1.0", &[]);
    let error = install_private_package(&f.manager, bad.clone(), &entry("1.2.0", &[], &bad))
        .await
        .unwrap_err();
    assert!(matches!(error, MarketError::Plugin(_)), "{error:?}");
    assert_eq!(source_key(&f).await, None);
    assert_eq!(listed(&f).await.expect("listed").source, "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_market_install_refuses_private_source_but_local_zip_clears_it() {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);
    install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], &bytes))
        .await
        .unwrap();

    // 公开 marketInstall 的唯一入口 `install_entry` 在联网前就拒绝。
    let client = MarketClient::new(
        f.manager.clone(),
        f.db.clone(),
        vec!["http://127.0.0.1:9/index.json".to_string()],
        &ProxyConfig::default(),
    )
    .unwrap();
    let public = entry("9.9.9", &[], b"public");
    let error = client.install_entry(&public, false).await.unwrap_err();
    assert!(
        matches!(&error, MarketError::PrivateSource(id) if id == ID),
        "{error:?}"
    );
    assert_eq!(listed(&f).await.expect("listed").version, "1.0.0");

    // 用户主动装本机包是显式动作：来源回到非私有。
    f.manager
        .install_from_zip(fxplug("1.0.1", &[]))
        .await
        .unwrap();
    assert_eq!(source_key(&f).await, None);
    assert_eq!(listed(&f).await.expect("listed").source, "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocations_are_scoped_by_install_source() {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);
    install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], &bytes))
        .await
        .unwrap();

    // 公开撤回轮询不处理私有包（命中的 id/version 也不禁用）。
    assert!(
        !f.manager
            .apply_market_revocations(&revoked("1.0.0"))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(info.enabled);
    assert_eq!(info.disabled_reason, "None");

    // 私有撤回：不同版本不命中；命中版本被 Revoked 禁用且不可再启用。
    assert!(
        !f.manager
            .apply_private_revocations(&revoked("0.9.0"), None)
            .await
            .unwrap()
    );
    assert!(
        f.manager
            .apply_private_revocations(&revoked("1.0.0"), None)
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Revoked");
    assert!(f.manager.set_enabled(ID, true).await.is_err());
    // 幂等；清单不再包含时只增不清，仍保持封锁。
    assert!(
        !f.manager
            .apply_private_revocations(&revoked("1.0.0"), None)
            .await
            .unwrap()
    );
    assert!(
        !f.manager
            .apply_private_revocations(&HashSet::new(), None)
            .await
            .unwrap()
    );
    assert!(f.manager.set_enabled(ID, true).await.is_err());

    // 升级到未撤回版本自动解封（沿用安装启停规则）。
    let next = fxplug("1.1.0", &[]);
    install_private_package(&f.manager, next.clone(), &entry("1.1.0", &[], &next))
        .await
        .unwrap();
    assert!(listed(&f).await.expect("listed").enabled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_revocation_ignores_non_private_plugins() {
    let f = fixture().await;
    f.manager
        .install_from_zip(fxplug("1.0.0", &[]))
        .await
        .unwrap();
    assert!(
        !f.manager
            .apply_private_revocations(&revoked("1.0.0"), None)
            .await
            .unwrap()
    );
    assert!(listed(&f).await.expect("listed").enabled);
    // 公开路径仍照常撤回非私有插件。
    assert!(
        f.manager
            .apply_market_revocations(&revoked("1.0.0"))
            .await
            .unwrap()
    );
    assert_eq!(listed(&f).await.expect("listed").disabled_reason, "Revoked");
}

fn scope(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

async fn revoked_marker(f: &Fixture) -> Option<String> {
    f.db.get_config(&format!("plugin.{ID}.revoked_version"))
        .await
        .unwrap()
}

async fn audit_actions(f: &Fixture) -> Vec<String> {
    let (rows, _) = f.manager.audit_list(Some(ID), None, 100).await.unwrap();
    rows.into_iter().rev().map(|row| row.action).collect()
}

/// 安装 1.0.0 私有包并按清单撤回它（`scope` 内），返回已处于 Revoked 的夹具。
async fn revoked_private_fixture() -> Fixture {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);
    install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], &bytes))
        .await
        .unwrap();
    assert!(
        f.manager
            .apply_private_revocations(&revoked("1.0.0"), Some(&scope(&[ID])))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Revoked");
    f
}

/// scope 内插件的撤回已不在权威清单里：清除 `revoked_version`，保守地保持禁用（原因降为 Manual），
/// 用户可手动启用；幂等、不重复审计；再次撤回照常生效。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifted_private_revocation_clears_the_block_but_keeps_the_plugin_disabled() {
    let f = revoked_private_fixture().await;
    assert!(f.manager.set_enabled(ID, true).await.is_err());

    assert!(
        f.manager
            .apply_private_revocations(&HashSet::new(), Some(&scope(&[ID])))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled, "lifting never switches a plugin back on");
    assert_eq!(info.disabled_reason, "Manual");
    assert_eq!(info.source, SOURCE_PRIVATE);
    assert_eq!(revoked_marker(&f).await, None);
    let actions = audit_actions(&f).await;
    assert_eq!(actions.last().map(String::as_str), Some("unrevoked"));

    // 幂等：再来一遍无变化，也不新增审计。
    assert!(
        !f.manager
            .apply_private_revocations(&HashSet::new(), Some(&scope(&[ID])))
            .await
            .unwrap()
    );
    assert_eq!(audit_actions(&f).await, actions);

    // 解封后可手动启用；之后再次被撤回仍然生效。
    f.manager.set_enabled(ID, true).await.unwrap();
    assert!(listed(&f).await.expect("listed").enabled);
    assert!(
        f.manager
            .apply_private_revocations(&revoked("1.0.0"), Some(&scope(&[ID])))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Revoked");
}

/// 仍在 revoked 里的版本不解封；清单里只有无关条目（别的版本 / 别的插件）不会替它续命，
/// 因为判定的是被记录的 `(identity, revoked_version)`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_revocation_still_listed_is_not_lifted() {
    let f = revoked_private_fixture().await;
    let in_scope = scope(&[ID]);

    assert!(
        !f.manager
            .apply_private_revocations(&revoked("1.0.0"), Some(&in_scope))
            .await
            .unwrap()
    );
    let mut also_other = revoked("1.0.0");
    also_other.insert(("other@private".to_string(), "9.9.9".to_string()));
    assert!(
        !f.manager
            .apply_private_revocations(&also_other, Some(&scope(&[ID, "other@private"])))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Revoked");
    assert_eq!(revoked_marker(&f).await.as_deref(), Some("1.0.0"));
    assert!(f.manager.set_enabled(ID, true).await.is_err());

    // 清单里只剩别的版本：被记录的 1.0.0 已不被撤回，解封。
    assert!(
        f.manager
            .apply_private_revocations(&revoked("0.9.0"), Some(&in_scope))
            .await
            .unwrap()
    );
    assert_eq!(revoked_marker(&f).await, None);
}

/// scope 外的插件（换了一个没有该授权的账号登录）与 scope=None（旧版云端）一律不解封。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_revocation_is_not_lifted_outside_scope_or_without_scope() {
    let f = revoked_private_fixture().await;
    let empty = HashSet::new();
    for outside in [scope(&[]), scope(&["other@private"])] {
        assert!(
            !f.manager
                .apply_private_revocations(&empty, Some(&outside))
                .await
                .unwrap()
        );
    }
    assert!(
        !f.manager
            .apply_private_revocations(&empty, None)
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Revoked");
    assert_eq!(revoked_marker(&f).await.as_deref(), Some("1.0.0"));
    assert!(f.manager.set_enabled(ID, true).await.is_err());
    assert_eq!(
        audit_actions(&f).await.last().map(String::as_str),
        Some("revoked")
    );
}

/// 撤回前用户手动禁用：撤回会覆盖启停原因，无法区分「撤回前是否启用」；解封后无论如何都保持禁用，
/// 绝不替用户重新打开一个曾被标记恶意的插件。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifting_after_a_manual_disable_never_switches_the_plugin_on() {
    let f = fixture().await;
    let bytes = fxplug("1.0.0", &[]);
    install_private_package(&f.manager, bytes.clone(), &entry("1.0.0", &[], &bytes))
        .await
        .unwrap();
    f.manager.set_enabled(ID, false).await.unwrap();
    let in_scope = scope(&[ID]);
    assert!(
        f.manager
            .apply_private_revocations(&revoked("1.0.0"), Some(&in_scope))
            .await
            .unwrap()
    );
    assert!(
        f.manager
            .apply_private_revocations(&HashSet::new(), Some(&in_scope))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Manual");
    assert_eq!(revoked_marker(&f).await, None);
}

/// 上一轮解封在清标记前中断（原因已是 Manual、标记仍在）：下一轮继续清标记，不改启停状态。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_lift_resumes_without_touching_the_enabled_state() {
    let f = revoked_private_fixture().await;
    f.db.set_config(&format!("plugin.{ID}.disabled_reason"), "Manual")
        .await
        .unwrap();
    assert!(
        f.manager
            .apply_private_revocations(&HashSet::new(), Some(&scope(&[ID])))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Manual");
    assert_eq!(revoked_marker(&f).await, None);
}

/// 解封只作用于来源为私有商店的插件：公开 / 本机来源被公开撤回封锁后，私有同步（哪怕 scope 含该 id）不碰它。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifting_ignores_non_private_plugins() {
    let f = fixture().await;
    f.manager
        .install_from_zip(fxplug("1.0.0", &[]))
        .await
        .unwrap();
    assert!(
        f.manager
            .apply_market_revocations(&revoked("1.0.0"))
            .await
            .unwrap()
    );
    assert!(
        !f.manager
            .apply_private_revocations(&HashSet::new(), Some(&scope(&[ID])))
            .await
            .unwrap()
    );
    let info = listed(&f).await.expect("listed");
    assert!(!info.enabled);
    assert_eq!(info.disabled_reason, "Revoked");
    assert_eq!(revoked_marker(&f).await.as_deref(), Some("1.0.0"));
}
