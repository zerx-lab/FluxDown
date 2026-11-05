//! 浏览器扩展页面脚本（`contributes.pageScripts`）的聚合与源码读取。
//!
//! 只在被查询时计算：纳入「启用、已加载、已授权（无待授权权限 / 主机）、有 package_dir、
//! 声明了 pageScripts」的插件，按 identity、再按 manifest 顺序排序。`sequence` 持久化在配置键
//! `plugin.page_scripts.sequence`，索引（含每条脚本 sha256）的哈希存 `plugin.page_scripts.hash`；
//! 仅当哈希变化时写配置并让 sequence 加一（空闲静默、严格递增、永不回退）。`epoch`
//! （`plugin.page_scripts.epoch`）是随序号持久化的随机实例标识，数据库重置后必变。
//!
//! 源码不随索引下发：每条脚本的 `js` 文件按声明顺序以 `"\n;\n"` 拼接，`sha256` 为拼接结果
//! UTF-8 字节的小写十六进制摘要。js 文件按 `(mtime, len)` 缓存；瞬时 IO 失败沿用旧缓存，
//! 避免脚本被误清空再恢复时序号来回跳。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::manager::PluginInfo;
use super::manifest::{PageScriptDecl, concat_page_script};
use super::runtime::PluginError;
use crate::db::Db;

pub const SEQUENCE_KEY: &str = "plugin.page_scripts.sequence";
pub const HASH_KEY: &str = "plugin.page_scripts.hash";
pub const EPOCH_KEY: &str = "plugin.page_scripts.epoch";
/// 缓存条目数超过此值时清理已卸载插件的条目。
const MAX_CACHE_ENTRIES: usize = 128;

/// 串行化「读哈希 → 比较 → 写配置」，避免并发查询重复递增。
static QUERY_LOCK: Mutex<()> = Mutex::const_new(());

/// 单条脚本拼接结果缓存，键为 `(identity, package_dir, script id)`。
static SCRIPT_CACHE: Mutex<BTreeMap<(String, String, String), CachedScript>> =
    Mutex::const_new(BTreeMap::new());

/// 一个 js 文件的磁盘戳。
#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    file: String,
    modified: Option<SystemTime>,
    len: u64,
}

struct CachedScript {
    /// 缓存对应的插件包修订号；与当前不同即视为过期（更新后绝不返回旧内容）。
    revision: u64,
    stamps: Vec<FileStamp>,
    /// 内容非法（超限 / 非 UTF-8）时为 `None`：缓存「无效」避免每次重读。
    built: Option<Built>,
}

#[derive(Clone)]
struct Built {
    code: String,
    sha256: String,
}

/// 索引条目（字段与 `PluginPageScriptDto` 一一对应，由 daemon 映射）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageScriptEntry {
    pub identity: String,
    pub plugin_name: String,
    pub plugin_version: String,
    pub id: String,
    pub title: String,
    pub matches: Vec<String>,
    pub exclude_matches: Vec<String>,
    pub run_at: String,
    pub world: String,
    pub all_frames: bool,
    pub sha256: String,
    pub bytes: u64,
}

/// 页面脚本索引。
#[derive(Debug)]
pub struct PageScriptIndex {
    pub sequence: u64,
    pub epoch: String,
    pub scripts: Vec<PageScriptEntry>,
}

/// 单条页面脚本源码。
#[derive(Debug)]
pub struct PageScriptSource {
    pub sha256: String,
    pub code: String,
}

/// 插件是否满足页面脚本门槛。
fn eligible(info: &PluginInfo) -> bool {
    info.enabled
        && info.load_status == "Loaded"
        && info.consent_pending.is_empty()
        && info.consent_pending_hosts.is_empty()
        && info.package_dir.is_some()
        && !info.contributions.page_scripts.is_empty()
}

/// 查询当前页面脚本索引。
pub async fn query(plugins: &[PluginInfo], db: &Db) -> Result<PageScriptIndex, PluginError> {
    let scripts = collect(plugins).await;
    let hash = index_hash(&scripts)?;

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
    } else if stored_seq == 0 && stored_hash.is_none() && scripts.is_empty() {
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
    Ok(PageScriptIndex {
        sequence,
        epoch,
        scripts,
    })
}

/// 读取单条脚本源码；插件不满足门槛或脚本不在索引内则报错。
pub async fn source(
    plugins: &[PluginInfo],
    identity: &str,
    id: &str,
) -> Result<PageScriptSource, PluginError> {
    let not_found = || PluginError::Runtime(format!("page script not found: {identity}/{id}"));
    let info = plugins
        .iter()
        .find(|p| p.identity == identity && eligible(p))
        .ok_or_else(not_found)?;
    let (Some(decl), Some(dir)) = (
        info.contributions.page_scripts.iter().find(|s| s.id == id),
        info.package_dir.as_deref(),
    ) else {
        return Err(not_found());
    };
    let key = (info.identity.clone(), dir.to_string(), decl.id.clone());
    let mut cache = SCRIPT_CACHE.lock().await;
    refresh_cached(&mut cache, &key, decl, info.package_revision).await;
    match cache.get(&key).and_then(|c| c.built.clone()) {
        Some(built) => Ok(PageScriptSource {
            sha256: built.sha256,
            code: built.code,
        }),
        None => Err(not_found()),
    }
}

async fn read_config(db: &Db, key: &str) -> Result<Option<String>, PluginError> {
    db.get_config(key)
        .await
        .map_err(|e| PluginError::Runtime(format!("{e:#}")))
}

fn index_hash(scripts: &[PageScriptEntry]) -> Result<String, PluginError> {
    let bytes = serde_json::to_vec(scripts)
        .map_err(|e| PluginError::Runtime(format!("序列化页面脚本索引失败: {e}")))?;
    Ok(hex(&Sha256::digest(&bytes)))
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

async fn collect(plugins: &[PluginInfo]) -> Vec<PageScriptEntry> {
    let mut sorted: Vec<&PluginInfo> = plugins.iter().collect();
    sorted.sort_by(|a, b| a.identity.cmp(&b.identity));
    let mut cache = SCRIPT_CACHE.lock().await;
    let mut declared = BTreeSet::new();
    let mut out = Vec::new();
    for info in sorted {
        let Some(dir) = info.package_dir.as_deref() else {
            continue;
        };
        for decl in &info.contributions.page_scripts {
            declared.insert((info.identity.clone(), dir.to_string(), decl.id.clone()));
        }
        if !eligible(info) {
            continue;
        }
        for decl in &info.contributions.page_scripts {
            let key = (info.identity.clone(), dir.to_string(), decl.id.clone());
            refresh_cached(&mut cache, &key, decl, info.package_revision).await;
            let Some(built) = cache.get(&key).and_then(|c| c.built.as_ref()) else {
                continue;
            };
            out.push(PageScriptEntry {
                identity: info.identity.clone(),
                plugin_name: info.name.clone(),
                plugin_version: info.version.clone(),
                id: decl.id.clone(),
                title: decl.title.clone(),
                matches: decl.matches.clone(),
                exclude_matches: decl.exclude_matches.clone(),
                run_at: decl.run_at.as_str().to_string(),
                world: decl.world.as_str().to_string(),
                all_frames: decl.all_frames,
                sha256: built.sha256.clone(),
                bytes: built.code.len() as u64,
            });
        }
    }
    // 卸载 / 不再声明后的缓存只在条目过多时才清理（条目少时保留，避免抖动重读）。
    if cache.len() > MAX_CACHE_ENTRIES {
        cache.retain(|key, _| declared.contains(key));
    }
    out
}

/// 读取声明的每个 js 文件当前的 `(mtime, len)`；任一元数据读取失败返回 `None`。
async fn current_stamps(dir: &str, decl: &PageScriptDecl) -> Option<Vec<FileStamp>> {
    let mut stamps = Vec::with_capacity(decl.js.len());
    for file in &decl.js {
        let meta = match tokio::fs::metadata(Path::new(dir).join(file)).await {
            Ok(meta) => meta,
            Err(error) => {
                crate::log_warn!("[plugin-page-scripts] {dir}/{file} 状态读取失败: {error}");
                return None;
            }
        };
        stamps.push(FileStamp {
            file: file.clone(),
            modified: meta.modified().ok(),
            len: meta.len(),
        });
    }
    Some(stamps)
}

/// 让 `cache[key]` 反映磁盘：全部 `(mtime, len)` 未变沿用；变了才重读拼接。
/// IO 失败沿用旧缓存（若旧缓存对应的文件列表与当前声明已不同则丢弃）；内容非法缓存为
/// 「无效」，文件修好后戳变化自然重读。
async fn refresh_cached(
    cache: &mut BTreeMap<(String, String, String), CachedScript>,
    key: &(String, String, String),
    decl: &PageScriptDecl,
    revision: u64,
) {
    let (identity, dir, _) = key;
    let Some(stamps) = current_stamps(dir, decl).await else {
        drop_if_files_differ(cache, key, decl, revision);
        return;
    };
    if cache
        .get(key)
        .is_some_and(|c| c.stamps == stamps && c.revision == revision)
    {
        return;
    }
    let mut contents: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for file in &decl.js {
        match tokio::fs::read(Path::new(dir).join(file)).await {
            Ok(bytes) => {
                contents.insert(file.as_str(), bytes);
            }
            Err(error) => {
                crate::log_warn!("[plugin-page-scripts] {identity} 读取 {file} 失败: {error}");
                drop_if_files_differ(cache, key, decl, revision);
                return;
            }
        }
    }
    let built = match concat_page_script(decl, |p| contents.get(p).cloned()) {
        Ok(code) => {
            let sha256 = hex(&Sha256::digest(code.as_bytes()));
            Some(Built { code, sha256 })
        }
        Err(error) => {
            crate::log_warn!("[plugin-page-scripts] {identity} 脚本无效: {error}");
            None
        }
    };
    cache.insert(
        key.clone(),
        CachedScript {
            revision,
            stamps,
            built,
        },
    );
}

/// IO 失败时沿用旧缓存只限同一包修订且文件列表一致；否则丢弃，不得回放旧版本内容。
fn drop_if_files_differ(
    cache: &mut BTreeMap<(String, String, String), CachedScript>,
    key: &(String, String, String),
    decl: &PageScriptDecl,
    revision: u64,
) {
    let differs = cache.get(key).is_some_and(|c| {
        c.revision != revision
            || c.stamps.len() != decl.js.len()
            || c.stamps.iter().zip(&decl.js).any(|(s, f)| &s.file != f)
    });
    if differs {
        cache.remove(key);
    }
}

/// 卸载插件时清除其全部脚本缓存条目。
pub(crate) async fn purge_identity(identity: &str) {
    SCRIPT_CACHE
        .lock()
        .await
        .retain(|(cached_identity, _, _), _| cached_identity != identity);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::manifest::{ContributesDecl, PageScriptRunAt, PageScriptWorld, PluginRisk};

    async fn open_db() -> (Db, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_page_scripts_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create dir");
        let db = Db::open(&dir).await.expect("open db");
        (db, dir)
    }

    fn decl(id: &str, js: &[&str]) -> PageScriptDecl {
        PageScriptDecl {
            id: id.into(),
            title: format!("T {id}"),
            matches: vec!["https://*.example.com/*".into()],
            exclude_matches: Vec::new(),
            js: js.iter().map(|s| (*s).to_string()).collect(),
            run_at: PageScriptRunAt::DocumentIdle,
            world: PageScriptWorld::Isolated,
            all_frames: false,
        }
    }

    fn info(identity: &str, dir: &Path, scripts: Vec<PageScriptDecl>) -> PluginInfo {
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
            permissions: vec!["pageScripts".into()],
            auth_supported: false,
            subscription_provider_ids: Vec::new(),
            resolver_urls: Vec::new(),
            load_status: "Loaded".into(),
            load_error: String::new(),
            contributions: ContributesDecl {
                page_scripts: scripts,
                ..ContributesDecl::default()
            },
            view_icons: Vec::new(),
            has_main: false,
            risk: PluginRisk::Full,
            network_hosts: Vec::new(),
            consent_pending: Vec::new(),
            consent_pending_hosts: Vec::new(),
            package_revision: 0,
            package_dir: Some(dir.to_string_lossy().into_owned()),
            source: String::new(),
        }
    }

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::create_dir_all(dir).expect("mkdir");
        std::fs::write(dir.join(name), content).expect("write");
    }

    #[tokio::test]
    async fn no_scripts_ever_returns_zero_and_empty() {
        let (db, dir) = open_db().await;
        let result = query(&[], &db).await.unwrap();
        assert_eq!(result.sequence, 0);
        assert!(result.scripts.is_empty());
        assert!(!result.epoch.is_empty());
        assert!(db.get_config(SEQUENCE_KEY).await.unwrap().is_none());
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[tokio::test]
    async fn gate_excludes_disabled_unconsented_and_failed_plugins() {
        let (db, root) = open_db().await;
        let mut plugins = Vec::new();
        for name in ["a", "b", "c", "d", "e"] {
            let dir = root.join(name);
            write(&dir, "x.js", "1;");
            plugins.push(info(
                &format!("{name}@acme"),
                &dir,
                vec![decl("s", &["x.js"])],
            ));
        }
        plugins[1].enabled = false;
        plugins[2].consent_pending = vec!["pageScripts".into()];
        plugins[3].load_status = "Failed".into();
        plugins[4].consent_pending_hosts = vec!["example.com".into()];
        let result = query(&plugins, &db).await.unwrap();
        let ids: Vec<&str> = result.scripts.iter().map(|s| s.identity.as_str()).collect();
        assert_eq!(ids, ["a@acme"]);
        assert_eq!(result.sequence, 1);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn concat_sha_order_and_sequence_tracking() {
        let (db, root) = open_db().await;
        let b = root.join("b");
        let a = root.join("a");
        write(&b, "one.js", "var x = 1;");
        write(&b, "two.js", "var y = 2;");
        write(&a, "main.js", "a();");
        let mut plugins = vec![
            info(
                "b@acme",
                &b,
                vec![decl("s2", &["one.js", "two.js"]), decl("s1", &["two.js"])],
            ),
            info("a@acme", &a, vec![decl("only", &["main.js"])]),
        ];

        let first = query(&plugins, &db).await.unwrap();
        assert_eq!(first.sequence, 1);
        let order: Vec<(&str, &str)> = first
            .scripts
            .iter()
            .map(|s| (s.identity.as_str(), s.id.as_str()))
            .collect();
        assert_eq!(
            order,
            [("a@acme", "only"), ("b@acme", "s2"), ("b@acme", "s1")]
        );

        let joined = "var x = 1;\n;\nvar y = 2;";
        let expected = hex(&Sha256::digest(joined.as_bytes()));
        assert_eq!(first.scripts[1].sha256, expected);
        assert_eq!(first.scripts[1].bytes, joined.len() as u64);
        let src = source(&plugins, "b@acme", "s2").await.unwrap();
        assert_eq!(src.code, joined);
        assert_eq!(src.sha256, expected);

        // 不变：序号不变。
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 1);

        // 代码变化：+1（长度变化保证 (mtime,len) 戳变化）。
        write(&a, "main.js", "a(); // changed");
        let changed = query(&plugins, &db).await.unwrap();
        assert_eq!(changed.sequence, 2);
        assert_ne!(changed.scripts[0].sha256, first.scripts[0].sha256);
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 2);

        // 全部排除：产生空索引新序号，随后稳定。
        plugins[0].enabled = false;
        plugins[1].enabled = false;
        let emptied = query(&plugins, &db).await.unwrap();
        assert_eq!(emptied.sequence, 3);
        assert!(emptied.scripts.is_empty());
        assert_eq!(query(&plugins, &db).await.unwrap().sequence, 3);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn epoch_is_stable_within_a_database_and_changes_after_reset() {
        let (db, dir) = open_db().await;
        let first = query(&[], &db).await.unwrap();
        assert_eq!(query(&[], &db).await.unwrap().epoch, first.epoch);
        let (fresh, fresh_dir) = open_db().await;
        assert_ne!(query(&[], &fresh).await.unwrap().epoch, first.epoch);
        std::fs::remove_dir_all(dir).expect("cleanup");
        std::fs::remove_dir_all(fresh_dir).expect("cleanup");
    }

    #[tokio::test]
    async fn source_rejects_scripts_outside_the_index() {
        let (_db, root) = open_db().await;
        let dir = root.join("a");
        write(&dir, "x.js", "1;");
        let mut plugins = vec![info("a@acme", &dir, vec![decl("s", &["x.js"])])];
        assert!(source(&plugins, "a@acme", "s").await.is_ok());
        assert!(source(&plugins, "a@acme", "missing").await.is_err());
        assert!(source(&plugins, "zzz@acme", "s").await.is_err());
        plugins[0].consent_pending = vec!["pageScripts".into()];
        assert!(source(&plugins, "a@acme", "s").await.is_err());
        plugins[0].consent_pending.clear();
        plugins[0].enabled = false;
        assert!(source(&plugins, "a@acme", "s").await.is_err());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[tokio::test]
    async fn oversized_script_is_excluded() {
        let (db, root) = open_db().await;
        let dir = root.join("a");
        write(&dir, "big.js", &"x".repeat(256 * 1024 + 1));
        let plugins = vec![info("a@acme", &dir, vec![decl("s", &["big.js"])])];
        assert!(query(&plugins, &db).await.unwrap().scripts.is_empty());
        assert!(source(&plugins, "a@acme", "s").await.is_err());
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
