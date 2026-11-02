use crate::logger::log_info;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::super::{
    archive_rules::{MAX_PACK_BYTES, VerifiedArchiveRules},
    bridge::GuardResolver,
    manifest::PERMISSION_ARCHIVE,
    runtime::PluginError,
};
use super::PluginManager;

const MAX_RULE_UPDATE_BYTES: usize = MAX_PACK_BYTES;
const AUTO_UPDATE_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ArchiveRuleCache {
    pub envelope: String,
    pub public_key: String,
    pub update_url: Option<String>,
    pub sequence: u64,
    pub digest: String,
    pub last_checked_at: Option<u64>,
    pub last_updated_at: Option<u64>,
    pub last_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RuleWatermark {
    sequence: u64,
    digest: String,
}

const MAX_CACHE_JSON_BYTES: usize = MAX_PACK_BYTES + 32 * 1024;

impl PluginManager {
    /// Read or refresh this plugin's verified signed archive-rule pack.
    ///
    /// # Errors
    ///
    /// Returns an error if the plugin is not enabled, the seed is invalid, trusted state is
    /// corrupt, or a required database operation fails. Fetch and signature failures are
    /// recorded in the returned status while the last verified pack remains active.
    pub async fn archive_rules_status(
        &self,
        identity: &str,
        refresh: bool,
    ) -> Result<serde_json::Value, PluginError> {
        let (plugin_enabled, plugin_dir, declaration) = {
            let plugins = self.plugins.read().await;
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.manifest.identity == identity)
                .ok_or_else(|| PluginError::Runtime(format!("插件 {identity} 不存在")))?;
            if !plugin.manifest.has_permission(PERMISSION_ARCHIVE) {
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} 未启用 archive 授权"
                )));
            }
            let declaration = plugin
                .manifest
                .contributes
                .as_ref()
                .and_then(|contributes| contributes.archive_rules.clone())
                .ok_or_else(|| {
                    PluginError::Runtime(format!("插件 {identity} 未声明 archiveRules"))
                })?;
            (plugin.enabled, plugin.dir.clone(), declaration)
        };
        if !plugin_enabled {
            self.archive_rule_packs.write().await.remove(identity);
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 未启用，无法读取或更新规则"
            )));
        }
        let key = format!("plugin.{identity}.archive_rules.cache");
        let plugin_lock = {
            let mut locks = self.archive_rule_locks.lock().await;
            Arc::clone(
                locks
                    .entry(identity.to_owned())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = plugin_lock.lock().await;
        let now = now_ms();
        let watermark_key = format!("plugin.{identity}.archive_rules.watermark");
        let watermark_raw = self
            .db
            .get_config(&watermark_key)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        let watermark: Option<RuleWatermark> = match watermark_raw {
            Some(raw) => {
                if raw.len() > 1024 {
                    return Err(PluginError::Runtime(
                        "archive rules watermark exceeds size limit".to_owned(),
                    ));
                }
                let stored: RuleWatermark = serde_json::from_str(&raw).map_err(|error| {
                    PluginError::Runtime(format!("invalid archive rules watermark: {error}"))
                })?;
                if stored.sequence == 0
                    || stored.digest.len() != 64
                    || !stored.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(PluginError::Runtime(
                        "invalid archive rules watermark".to_owned(),
                    ));
                }
                Some(stored)
            }
            None => None,
        };
        let cache_raw = self
            .db
            .get_config(&key)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        let mut cache: Option<ArchiveRuleCache> = match cache_raw {
            Some(raw) if raw.len() <= MAX_CACHE_JSON_BYTES => serde_json::from_str(&raw).ok(),
            _ => None,
        };
        if let Some(current) = cache.as_ref() {
            let verified =
                VerifiedArchiveRules::verify(current.envelope.as_bytes(), &declaration.public_key);
            let accepted = current.public_key == declaration.public_key
                && verified.as_ref().is_ok_and(|rules| {
                    rules.sequence() == current.sequence && rules.digest() == current.digest
                })
                && watermark.as_ref().is_none_or(|stored| {
                    current.sequence > stored.sequence
                        || (current.sequence == stored.sequence && current.digest == stored.digest)
                });
            if !accepted {
                cache = None;
            }
        }
        if cache.is_none() {
            let path = plugin_dir.join(&declaration.entry);
            let bytes = super::super::archive_rules::read_bounded(&path).await?;
            let verified = VerifiedArchiveRules::verify(&bytes, &declaration.public_key)?;
            if watermark.as_ref().is_some_and(|stored| {
                verified.sequence() < stored.sequence
                    || (verified.sequence() == stored.sequence
                        && verified.digest() != stored.digest)
            }) {
                return Err(PluginError::Runtime(
                    "archive rules seed is below or conflicts with trusted sequence watermark"
                        .to_owned(),
                ));
            }
            let seed = ArchiveRuleCache {
                envelope: String::from_utf8(bytes).map_err(|error| {
                    PluginError::Runtime(format!("archiveRules seed is not UTF-8: {error}"))
                })?,
                public_key: declaration.public_key.clone(),
                update_url: declaration.update_url.clone(),
                sequence: verified.sequence(),
                digest: verified.digest().to_owned(),
                last_checked_at: None,
                last_updated_at: Some(now),
                last_error: None,
            };
            let trusted = RuleWatermark {
                sequence: seed.sequence,
                digest: seed.digest.clone(),
            };
            if !self
                .archive_rules_declaration_active(identity, &declaration)
                .await
            {
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} 已停用或规则声明已变更"
                )));
            }
            self.persist_archive_rules(&key, &watermark_key, &seed, &trusted)
                .await?;
            cache = Some(seed);
        } else if let Some(current) = cache.as_ref() {
            let trusted = RuleWatermark {
                sequence: current.sequence,
                digest: current.digest.clone(),
            };
            if watermark.as_ref().is_none_or(|stored| {
                current.sequence > stored.sequence || current.digest != stored.digest
            }) {
                self.persist_archive_rules(&key, &watermark_key, current, &trusted)
                    .await?;
            }
        }
        let mut current = cache
            .ok_or_else(|| PluginError::Runtime("archiveRules cache unavailable".to_owned()))?;
        let due = current
            .last_checked_at
            .is_none_or(|checked| now.saturating_sub(checked) >= AUTO_UPDATE_INTERVAL_MS);
        if (refresh || due)
            && let Some(url) = declaration.update_url.as_deref()
        {
            match self.fetch_archive_rule_update(url).await {
                Ok(bytes) => match VerifiedArchiveRules::verify(&bytes, &declaration.public_key) {
                    Ok(verified) if verified.sequence() > current.sequence => {
                        let envelope = match String::from_utf8(bytes) {
                            Ok(value) => value,
                            Err(_) => {
                                current.last_error = Some("update pack is not UTF-8".to_owned());
                                String::new()
                            }
                        };
                        if !envelope.is_empty() {
                            let promoted = ArchiveRuleCache {
                                envelope,
                                public_key: declaration.public_key.clone(),
                                update_url: Some(url.to_owned()),
                                sequence: verified.sequence(),
                                digest: verified.digest().to_owned(),
                                last_checked_at: Some(now),
                                last_updated_at: Some(now),
                                last_error: None,
                            };
                            let trusted = RuleWatermark {
                                sequence: promoted.sequence,
                                digest: promoted.digest.clone(),
                            };
                            if !self
                                .archive_rules_declaration_active(identity, &declaration)
                                .await
                            {
                                return Err(PluginError::Runtime(format!(
                                    "插件 {identity} 已停用或规则声明已变更"
                                )));
                            }
                            self.persist_archive_rules(&key, &watermark_key, &promoted, &trusted)
                                .await?;
                            current = promoted;
                        }
                    }
                    Ok(verified)
                        if verified.sequence() == current.sequence
                            && verified.digest() == current.digest =>
                    {
                        current.last_error = None;
                        current.last_checked_at = Some(now);
                    }
                    Ok(_) => {
                        current.last_error = Some(
                            "archive rules sequence rollback or equivocation rejected".to_owned(),
                        )
                    }
                    Err(error) => current.last_error = Some(error.to_string()),
                },
                Err(error) => current.last_error = Some(error.to_string()),
            }
            if !self
                .archive_rules_declaration_active(identity, &declaration)
                .await
            {
                self.archive_rule_packs.write().await.remove(identity);
                return Err(PluginError::Runtime(format!(
                    "插件 {identity} 已停用或规则声明已变更"
                )));
            }
            current.last_checked_at = Some(now);
            let json = serde_json::to_string(&current)
                .map_err(|error| PluginError::Runtime(error.to_string()))?;
            self.db
                .set_config(&key, &json)
                .await
                .map_err(|error| PluginError::Runtime(error.to_string()))?;
        }
        let plugins = self.plugins.read().await;
        let active = plugins.iter().any(|plugin| {
            plugin.manifest.identity == identity
                && plugin.enabled
                && plugin.manifest.has_permission(PERMISSION_ARCHIVE)
                && plugin
                    .manifest
                    .contributes
                    .as_ref()
                    .and_then(|contributes| contributes.archive_rules.as_ref())
                    .is_some_and(|current| {
                        current.entry == declaration.entry
                            && current.public_key == declaration.public_key
                            && current.update_url == declaration.update_url
                    })
        });
        if !active {
            drop(plugins);
            self.archive_rule_packs.write().await.remove(identity);
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 已停用或规则声明已变更"
            )));
        }
        let verified =
            VerifiedArchiveRules::verify(current.envelope.as_bytes(), &current.public_key)?;
        self.archive_rule_packs
            .write()
            .await
            .insert(identity.to_owned(), verified);
        drop(plugins);
        Ok(
            serde_json::json!({"sequence":current.sequence,"ruleCount":VerifiedArchiveRules::verify(current.envelope.as_bytes(), &current.public_key)?.rule_count(),"lastCheckedAt":current.last_checked_at,"lastUpdatedAt":current.last_updated_at,"lastError":current.last_error,"publicKey":declaration.public_key,"updateUrl":declaration.update_url,"automaticUpdates":declaration.update_url.is_some()}),
        )
    }
    async fn archive_rules_declaration_active(
        &self,
        identity: &str,
        declaration: &super::super::manifest::ArchiveRulesDecl,
    ) -> bool {
        self.plugins.read().await.iter().any(|plugin| {
            plugin.manifest.identity == identity
                && plugin.enabled
                && plugin.manifest.has_permission(PERMISSION_ARCHIVE)
                && plugin
                    .manifest
                    .contributes
                    .as_ref()
                    .and_then(|contributes| contributes.archive_rules.as_ref())
                    .is_some_and(|current| {
                        current.entry == declaration.entry
                            && current.public_key == declaration.public_key
                            && current.update_url == declaration.update_url
                    })
        })
    }

    pub(crate) async fn initialize_archive_rule_packs(&self) {
        let identities = self
            .plugins
            .read()
            .await
            .iter()
            .filter(|plugin| plugin.enabled)
            .filter_map(|plugin| {
                plugin
                    .manifest
                    .contributes
                    .as_ref()
                    .and_then(|contributes| contributes.archive_rules.as_ref())
                    .map(|_| plugin.manifest.identity.clone())
            })
            .collect::<Vec<_>>();
        for identity in identities {
            if let Err(error) = self.archive_rules_status(&identity, false).await {
                log_info!("[plugin] archive rules load for {identity} failed: {error}");
            }
        }
    }

    pub(super) async fn archive_rule_match(
        &self,
        identity: &str,
        context: &crate::model::ArchiveContext,
    ) -> Option<(String, String, u64)> {
        let packs = self.archive_rule_packs.read().await;
        let pack = packs.get(identity)?;
        let matched = pack.match_context(context)?;
        Some((
            matched.folder_name.to_owned(),
            matched.id.to_owned(),
            pack.sequence(),
        ))
    }

    async fn persist_archive_rules(
        &self,
        cache_key: &str,
        watermark_key: &str,
        cache: &ArchiveRuleCache,
        watermark: &RuleWatermark,
    ) -> Result<(), PluginError> {
        let cache_json = serde_json::to_string(cache)
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        if cache_json.len() > MAX_CACHE_JSON_BYTES {
            return Err(PluginError::Runtime(
                "archive rules cache exceeds size limit".to_owned(),
            ));
        }
        let watermark_json = serde_json::to_string(watermark)
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        let values = BTreeMap::from([
            (cache_key.to_owned(), cache_json),
            (watermark_key.to_owned(), watermark_json),
        ]);
        self.db
            .set_config_batch_atomic(&values)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))
    }

    async fn fetch_archive_rule_update(&self, url: &str) -> Result<Vec<u8>, PluginError> {
        let egress = crate::egress::Egress::new(self.db.clone());
        let clients = egress
            .routed(|builder| {
                builder
                    .timeout(std::time::Duration::from_secs(20))
                    .redirect(reqwest::redirect::Policy::none())
                    .dns_resolver(Arc::new(GuardResolver))
            })
            .await
            .map_err(|error| {
                PluginError::Runtime(format!("archive rules egress failed: {error}"))
            })?;
        let client = clients.client(url).await.map_err(|error| {
            PluginError::Runtime(format!("archive rules client failed: {error}"))
        })?;
        let mut response = client.get(url).send().await.map_err(|error| {
            PluginError::Runtime(format!("archive rules request failed: {error}"))
        })?;
        if !response.status().is_success() {
            return Err(PluginError::Runtime(format!(
                "archive rules HTTP {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            PluginError::Runtime(format!("archive rules response failed: {error}"))
        })? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RULE_UPDATE_BYTES {
                return Err(PluginError::Runtime(
                    "archive rules response exceeds 512 KiB".to_owned(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}
