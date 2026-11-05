//! 浏览器扩展嗅探规则的 plugin 层聚合。
//!
//! 只在被查询时计算：聚合所有「启用、已加载、已授权」且声明了 `contributes.sniffRules`
//! 的插件规则文件，改写 id / tier 后得到一个 pack。`sequence` 持久化在配置键
//! `plugin.sniff_rules.sequence`，规则内容哈希存 `plugin.sniff_rules.hash`；仅当哈希变化时
//! 写配置并让 sequence 加一（空闲静默、严格递增、永不回退）。
//!
//! `epoch`（配置键 `plugin.sniff_rules.epoch`）是随序号持久化的随机实例标识：数据库被重置 /
//! 重装后序号会从头计数，扩展凭它识别「序号倒退不是回滚攻击」并整体重置 plugin 层。
//!
//! 规则文件按 `(mtime, len)` 缓存：每次能力探测不再重读、重解析；读取失败（瞬时 IO 错误）
//! 视为「未变化」沿用缓存，避免规则被误清空再恢复时序号来回跳。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::manager::PluginInfo;
use super::manifest::validate_sniff_pack;
use super::runtime::PluginError;
use crate::db::Db;

pub const SEQUENCE_KEY: &str = "plugin.sniff_rules.sequence";
pub const HASH_KEY: &str = "plugin.sniff_rules.hash";
pub const EPOCH_KEY: &str = "plugin.sniff_rules.epoch";
/// 规则文件缓存条目数超过此值时清理已卸载插件的条目。
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_RULE_ID_LEN: usize = 64;
/// 所有插件规则聚合后的序列化体积上限；超出时后面的插件整体丢弃（按 identity 排序，确定性）。
/// 与 NMH 1 MiB 帧上限配合：单个 plugin 包必须能独自装进一帧。
pub const MAX_AGGREGATE_BYTES: usize = 512 * 1024;

/// 串行化「读哈希 → 比较 → 写配置」，避免并发查询重复递增。
static QUERY_LOCK: Mutex<()> = Mutex::const_new(());

/// 单个插件规则文件的解析缓存，键为 `(identity, package_dir)`。
static RULES_CACHE: Mutex<BTreeMap<(String, String), CachedRules>> =
    Mutex::const_new(BTreeMap::new());

struct CachedRules {
    file: String,
    modified: Option<SystemTime>,
    len: u64,
    /// 缓存对应的插件包修订号；与当前不同即视为过期（更新后绝不返回旧内容）。
    revision: u64,
    rules: Vec<Value>,
    /// `rules` 序列化后的字节数（聚合上限判定用）。
    bytes: usize,
}

/// plugin 层查询结果。`pack` 为 `None` 仅当从未有过插件规则（`sequence == 0`）。
#[derive(Debug)]
pub struct PluginPack {
    pub sequence: u64,
    pub epoch: String,
    pub pack: Option<Value>,
}

/// 查询当前 plugin 层规则包。
pub async fn query(plugins: &[PluginInfo], db: &Db) -> Result<PluginPack, PluginError> {
    let rules = collect_rules(plugins).await;
    let hash = rules_hash(&rules)?;

    let _guard = QUERY_LOCK.lock().await;
    let stored_seq = read_config(db, SEQUENCE_KEY)
        .await?
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let stored_hash = read_config(db, HASH_KEY).await?;
    let stored_epoch = read_config(db, EPOCH_KEY)
        .await?
        .filter(|epoch| !epoch.trim().is_empty());

    let mut writes = BTreeMap::new();
    let epoch = if let Some(epoch) = stored_epoch {
        epoch
    } else {
        let epoch = uuid::Uuid::new_v4().simple().to_string();
        writes.insert(EPOCH_KEY.to_string(), epoch.clone());
        epoch
    };

    let sequence = if stored_hash.as_deref() == Some(hash.as_str()) && stored_seq > 0 {
        stored_seq
    } else if stored_seq == 0 && stored_hash.is_none() && rules.is_empty() {
        0
    } else {
        let next = stored_seq.saturating_add(1);
        writes.insert(SEQUENCE_KEY.to_string(), next.to_string());
        writes.insert(HASH_KEY.to_string(), hash);
        next
    };
    if !writes.is_empty() {
        db.set_config_batch_atomic(&writes)
            .await
            .map_err(|e| PluginError::Runtime(format!("{e:#}")))?;
    }
    if sequence == 0 {
        return Ok(PluginPack {
            sequence,
            epoch,
            pack: None,
        });
    }

    let pack = json!({
        "format": "fluxdown-sniff-pack",
        "packId": "plugin",
        "sequence": sequence,
        "engine": 1,
        "issuedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "rules": rules,
    });
    Ok(PluginPack {
        sequence,
        epoch,
        pack: Some(pack),
    })
}

async fn read_config(db: &Db, key: &str) -> Result<Option<String>, PluginError> {
    db.get_config(key)
        .await
        .map_err(|e| PluginError::Runtime(format!("{e:#}")))
}

fn rules_hash(rules: &[Value]) -> Result<String, PluginError> {
    let bytes = serde_json::to_vec(rules)
        .map_err(|e| PluginError::Runtime(format!("序列化 plugin 规则失败: {e}")))?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

async fn collect_rules(plugins: &[PluginInfo]) -> Vec<Value> {
    let mut sorted: Vec<&PluginInfo> = plugins.iter().collect();
    sorted.sort_by(|a, b| a.identity.cmp(&b.identity));
    let mut cache = RULES_CACHE.lock().await;
    let mut declared = BTreeSet::new();
    let mut out = Vec::new();
    let mut aggregate = 0_usize;
    for info in sorted {
        let (Some(file), Some(dir)) = (
            info.contributions.sniff_rules.as_deref(),
            info.package_dir.as_deref(),
        ) else {
            continue;
        };
        let key = (info.identity.clone(), dir.to_string());
        declared.insert(key.clone());
        if !info.enabled
            || info.load_status != "Loaded"
            || !info.consent_pending.is_empty()
            || !info.consent_pending_hosts.is_empty()
        {
            continue;
        }
        refresh_cached(&mut cache, &key, file, info.package_revision).await;
        let Some(cached) = cache.get(&key) else {
            continue;
        };
        if aggregate.saturating_add(cached.bytes) > MAX_AGGREGATE_BYTES {
            crate::log_warn!(
                "[plugin-sniff] {} 规则使聚合体积超过 {MAX_AGGREGATE_BYTES} 字节，已丢弃",
                info.identity
            );
            continue;
        }
        aggregate += cached.bytes;
        out.extend(cached.rules.iter().cloned());
    }
    // 插件卸载 / 不再声明规则后的缓存只在条目过多时才清理（条目少时保留，避免抖动重读）。
    if cache.len() > MAX_CACHE_ENTRIES {
        cache.retain(|key, _| declared.contains(key));
    }
    out
}

/// 让 `cache[key]` 反映磁盘上的规则文件：`(mtime, len)` 未变直接沿用；变了才读取解析。
/// 元数据 / 读取的 IO 失败沿用旧缓存（无旧缓存则保持缺席）；内容非法则缓存为空规则，
/// 文件修好后 mtime 变化自然重读。
async fn refresh_cached(
    cache: &mut BTreeMap<(String, String), CachedRules>,
    key: &(String, String),
    file: &str,
    revision: u64,
) {
    let (identity, dir) = key;
    let path = Path::new(dir).join(file);
    let meta = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta,
        Err(error) => {
            crate::log_warn!("[plugin-sniff] {identity} 规则文件状态读取失败: {error}");
            drop_if_stale(cache, key, revision);
            return;
        }
    };
    let modified = meta.modified().ok();
    let len = meta.len();
    if cache.get(key).is_some_and(|c| {
        c.file == file && c.modified == modified && c.len == len && c.revision == revision
    }) {
        return;
    }
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            crate::log_warn!("[plugin-sniff] {identity} 规则读取失败: {error}");
            drop_if_stale(cache, key, revision);
            return;
        }
    };
    let (rules, size) = match parse_plugin_rules(identity, file, &bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            crate::log_warn!("[plugin-sniff] {identity} 规则无效: {error}");
            (Vec::new(), 0)
        }
    };
    cache.insert(
        key.clone(),
        CachedRules {
            file: file.to_string(),
            modified,
            len,
            revision,
            rules,
            bytes: size,
        },
    );
}

/// IO 失败时沿用旧缓存只限同一包修订；修订已变则丢弃，不得回放旧版本内容。
fn drop_if_stale(
    cache: &mut BTreeMap<(String, String), CachedRules>,
    key: &(String, String),
    revision: u64,
) {
    if cache.get(key).is_some_and(|c| c.revision != revision) {
        cache.remove(key);
    }
}

/// 卸载插件时清除其全部规则缓存条目。
pub(crate) async fn purge_identity(identity: &str) {
    RULES_CACHE
        .lock()
        .await
        .retain(|(cached_identity, _), _| cached_identity != identity);
}

/// 校验并改写单个规则文件：返回改写后的规则与其序列化字节数。
fn parse_plugin_rules(
    identity: &str,
    file: &str,
    bytes: &[u8],
) -> Result<(Vec<Value>, usize), PluginError> {
    validate_sniff_pack(bytes)?;
    let pack: Value = serde_json::from_slice(bytes)
        .map_err(|e| PluginError::Runtime(format!("解析 {file} 失败: {e}")))?;
    let Some(rules) = pack.get("rules").and_then(Value::as_array) else {
        return Ok((Vec::new(), 0));
    };
    let prefix = format!("plugin.{}.", identity.replace('@', "."));
    let mut out = Vec::with_capacity(rules.len());
    for rule in rules {
        let Some(mut object) = rule.as_object().cloned() else {
            crate::log_warn!("[plugin-sniff] {identity} 丢弃非对象规则");
            continue;
        };
        let Some(original) = object.get("id").and_then(Value::as_str) else {
            crate::log_warn!("[plugin-sniff] {identity} 丢弃缺少 id 的规则");
            continue;
        };
        let id = format!("{prefix}{original}");
        if id.len() > MAX_RULE_ID_LEN {
            crate::log_warn!(
                "[plugin-sniff] {identity} 规则 id 改写后超过 {MAX_RULE_ID_LEN} 字符，已丢弃: {id}"
            );
            continue;
        }
        object.insert("id".to_string(), Value::String(id));
        object.insert("tier".to_string(), Value::String("plugin".to_string()));
        out.push(Value::Object(object));
    }
    let size = serde_json::to_vec(&out)
        .map_err(|e| PluginError::Runtime(format!("序列化 plugin 规则失败: {e}")))?
        .len();
    Ok((out, size))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::manifest::{ContributesDecl, PluginRisk};

    async fn open_db() -> (Db, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_sniff_pack_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let db = Db::open(&dir).await.expect("open db");
        (db, dir)
    }

    fn info(identity: &str, dir: &Path, rules: Option<&str>) -> PluginInfo {
        PluginInfo {
            identity: identity.into(),
            name: identity.into(),
            version: "1.0.0".into(),
            description: String::new(),
            homepage: String::new(),
            enabled: true,
            dev_mode: false,
            disabled_reason: String::new(),
            settings: Vec::new(),
            settings_values: Vec::new(),
            permissions: vec!["browserRules".into()],
            auth_supported: false,
            subscription_provider_ids: Vec::new(),
            resolver_urls: Vec::new(),
            load_status: "Loaded".into(),
            load_error: String::new(),
            contributions: ContributesDecl {
                sniff_rules: rules.map(str::to_string),
                ..ContributesDecl::default()
            },
            view_icons: Vec::new(),
            has_main: false,
            risk: PluginRisk::Elevated,
            network_hosts: Vec::new(),
            consent_pending: Vec::new(),
            consent_pending_hosts: Vec::new(),
            package_revision: 0,
            package_dir: Some(dir.to_string_lossy().into_owned()),
            source: String::new(),
        }
    }

    fn write_pack(dir: &Path, ids: &[&str]) {
        std::fs::create_dir_all(dir).expect("mkdir");
        let rules: Vec<Value> = ids
            .iter()
            .map(|id| json!({"id": id, "tier": "local", "x": 1}))
            .collect();
        let pack = json!({"format": "fluxdown-sniff-pack", "rules": rules});
        std::fs::write(dir.join("rules.json"), pack.to_string()).expect("write");
    }

    #[tokio::test]
    async fn no_rules_ever_returns_zero_and_null() {
        let (db, dir) = open_db().await;
        let result = query(&[], &db).await.unwrap();
        assert_eq!(result.sequence, 0);
        assert!(result.pack.is_none());
        assert!(db.get_config(SEQUENCE_KEY).await.unwrap().is_none());
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[tokio::test]
    async fn aggregates_rewrites_and_tracks_sequence() {
        let (db, root) = open_db().await;
        let a = root.join("a");
        let b = root.join("b");
        let long_id = "x".repeat(60);
        write_pack(&a, &["one", &long_id]);
        write_pack(&b, &["two"]);
        let mut plugins = vec![
            info("a@acme", &a, Some("rules.json")),
            info("b@acme", &b, Some("rules.json")),
        ];

        let first = query(&plugins, &db).await.unwrap();
        assert_eq!(first.sequence, 1);
        let pack = first.pack.unwrap();
        assert_eq!(pack["packId"], "plugin");
        assert_eq!(pack["sequence"], 1);
        let ids: Vec<&str> = pack["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["plugin.a.acme.one", "plugin.b.acme.two"]);
        assert!(
            pack["rules"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["tier"] == "plugin")
        );

        // 内容不变：序号不变。
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 1);

        // 变化：+1。
        plugins[1].enabled = false;
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 2);

        // 授权撤销到清空：产生空包新序号，随后稳定。
        plugins[0].consent_pending = vec!["browserRules".into()];
        let emptied = query(&plugins, &db).await.unwrap();
        assert_eq!(emptied.sequence, 3);
        assert!(
            emptied.pack.unwrap()["rules"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let stable = query(&plugins, &db).await.unwrap();
        assert_eq!(stable.sequence, 3);
        assert!(stable.pack.is_some());

        // 重新出现规则：继续递增。
        plugins[0].consent_pending.clear();
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 4);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn epoch_is_stable_within_a_database_and_changes_after_reset() {
        let (db, dir) = open_db().await;
        let first = query(&[], &db).await.unwrap();
        assert!(!first.epoch.is_empty());
        assert_eq!(query(&[], &db).await.unwrap().epoch, first.epoch);

        // 数据库重置 / 重装：序号从头计数，但 epoch 必须不同，扩展据此重置 plugin 层。
        let (fresh, fresh_dir) = open_db().await;
        assert_ne!(query(&[], &fresh).await.unwrap().epoch, first.epoch);
        std::fs::remove_dir_all(dir).expect("cleanup");
        std::fs::remove_dir_all(fresh_dir).expect("cleanup");
    }

    #[tokio::test]
    async fn transient_read_failure_keeps_previous_rules_and_sequence() {
        let (db, root) = open_db().await;
        let a = root.join("a");
        write_pack(&a, &["one"]);
        let plugins = vec![info("a@acme", &a, Some("rules.json"))];
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 1);

        // 文件暂时读不到（被替换 / 瞬时 IO 错误）：视为未变化，不能把规则清空再恢复。
        std::fs::remove_file(a.join("rules.json")).expect("remove");
        let during = query(&plugins, &db).await.unwrap();
        assert_eq!(during.sequence, 1);
        assert_eq!(during.pack.unwrap()["rules"].as_array().unwrap().len(), 1);

        write_pack(&a, &["one"]);
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 1);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn aggregate_over_cap_drops_later_plugins() {
        let (db, root) = open_db().await;
        let mut plugins = Vec::new();
        let pad = "x".repeat(200 * 1024);
        for name in ["a", "b", "c"] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).expect("mkdir");
            let pack = json!({
                "format": "fluxdown-sniff-pack",
                "rules": [{"id": "r", "tier": "local", "pad": pad}],
            });
            std::fs::write(dir.join("rules.json"), pack.to_string()).expect("write");
            plugins.push(info(&format!("{name}@acme"), &dir, Some("rules.json")));
        }
        let result = query(&plugins, &db).await.unwrap();
        let rules = result.pack.unwrap()["rules"].as_array().unwrap().clone();
        let ids: Vec<&str> = rules.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["plugin.a.acme.r", "plugin.b.acme.r"]);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
