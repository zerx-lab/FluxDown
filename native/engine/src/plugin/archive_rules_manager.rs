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
    #[serde(with = "arc_string")]
    pub envelope: Arc<String>,
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
    /// Publisher key that produced this sequence lineage. Empty for pre-key-binding
    /// watermarks; a non-empty value that disagrees with the current declaration ends
    /// the lineage so a rotated key can bootstrap from its seed again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    public_key: String,
}

impl RuleWatermark {
    fn for_pack(sequence: u64, digest: impl Into<String>, public_key: &str) -> Self {
        Self {
            sequence,
            digest: digest.into(),
            public_key: public_key.to_owned(),
        }
    }

    fn matches_key(&self, public_key: &str) -> bool {
        self.public_key.is_empty() || self.public_key == public_key
    }
}

const MAX_CACHE_JSON_BYTES: usize = MAX_PACK_BYTES + 32 * 1024;

mod arc_string {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(value: &Arc<String>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<String>, D::Error> {
        String::deserialize(deserializer).map(Arc::new)
    }
}

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
        let (plugin_enabled, plugin_dir, declaration) = self.archive_rule_target(identity).await?;
        if !plugin_enabled {
            self.archive_rule_packs.write().await.remove(identity);
            self.archive_rule_status.write().await.remove(identity);
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 未启用，无法读取或更新规则"
            )));
        }
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
        let key = format!("plugin.{identity}.archive_rules.cache");
        let watermark_key = format!("plugin.{identity}.archive_rules.watermark");
        let cached = self.archive_rule_status.read().await.get(identity).cloned();
        let from_memory = cached.is_some();
        let (mut cache, mut watermark, mut active_verified) = self
            .validated_archive_rule_cache(
                identity,
                &key,
                &watermark_key,
                &declaration.public_key,
                cached,
            )
            .await?;
        if !from_memory
            && let Some(current) = cache.as_ref()
            && let Some((seed, verified)) = self
                .promote_bundled_seed_if_newer(
                    identity,
                    &plugin_dir,
                    &declaration,
                    (&key, &watermark_key),
                    current,
                    now,
                )
                .await?
        {
            watermark = Some(RuleWatermark::for_pack(
                seed.sequence,
                seed.digest.clone(),
                &declaration.public_key,
            ));
            cache = Some(seed);
            active_verified = Some(Arc::new(verified));
        }
        if cache.is_none() {
            let (seed, verified) = self
                .bootstrap_archive_rule_seed(
                    identity,
                    &plugin_dir,
                    &declaration,
                    (&key, &watermark_key),
                    watermark.as_ref(),
                    now,
                )
                .await?;
            active_verified = Some(Arc::new(verified));
            cache = Some(seed);
        } else if let Some(current) = cache.as_ref() {
            self.persist_trust_watermark_if_needed(
                &key,
                &watermark_key,
                current,
                watermark.as_ref(),
                from_memory,
            )
            .await?;
        }
        let mut current = cache
            .ok_or_else(|| PluginError::Runtime("archiveRules cache unavailable".to_owned()))?;
        active_verified = self
            .refresh_archive_rules_if_due(identity, &declaration, &mut current, refresh, now)
            .await?
            .or(active_verified);
        let verified = active_verified
            .ok_or_else(|| PluginError::Runtime("verified archive rules unavailable".to_owned()))?;
        self.publish_archive_rule_status(identity, &declaration, &current, verified)
            .await
    }
    async fn refresh_archive_rules_if_due(
        &self,
        identity: &str,
        declaration: &super::super::manifest::ArchiveRulesDecl,
        current: &mut ArchiveRuleCache,
        force: bool,
        now: u64,
    ) -> Result<Option<Arc<VerifiedArchiveRules>>, PluginError> {
        let due = current
            .last_checked_at
            .is_none_or(|checked| now.saturating_sub(checked) >= AUTO_UPDATE_INTERVAL_MS);
        let Some(url) = declaration.update_url.as_deref().filter(|_| force || due) else {
            return Ok(None);
        };
        let update = self.fetch_archive_rule_update(url).await;
        let verified = self
            .process_archive_rule_update(identity, declaration, url, current, update, now)
            .await?;
        let verified = verified.map(Arc::new);
        if !self
            .archive_rules_declaration_active(identity, declaration)
            .await
        {
            self.archive_rule_packs.write().await.remove(identity);
            self.archive_rule_status.write().await.remove(identity);
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 已停用或规则声明已变更"
            )));
        }
        Ok(verified)
    }
    async fn archive_rule_target(
        &self,
        identity: &str,
    ) -> Result<
        (
            bool,
            std::path::PathBuf,
            super::super::manifest::ArchiveRulesDecl,
        ),
        PluginError,
    > {
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
            .ok_or_else(|| PluginError::Runtime(format!("插件 {identity} 未声明 archiveRules")))?;
        Ok((plugin.enabled, plugin.dir.clone(), declaration))
    }
    async fn publish_archive_rule_status(
        &self,
        identity: &str,
        declaration: &super::super::manifest::ArchiveRulesDecl,
        current: &ArchiveRuleCache,
        verified: Arc<VerifiedArchiveRules>,
    ) -> Result<serde_json::Value, PluginError> {
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
            self.archive_rule_status.write().await.remove(identity);
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 已停用或规则声明已变更"
            )));
        }
        let rule_count = verified.rule_count();
        self.archive_rule_packs
            .write()
            .await
            .insert(identity.to_owned(), verified);
        self.archive_rule_status
            .write()
            .await
            .insert(identity.to_owned(), current.clone());
        drop(plugins);
        Ok(serde_json::json!({
            "sequence": current.sequence,
            "ruleCount": rule_count,
            "lastCheckedAt": current.last_checked_at,
            "lastUpdatedAt": current.last_updated_at,
            "lastError": current.last_error,
            "publicKey": declaration.public_key,
            "updateUrl": declaration.update_url,
            "automaticUpdates": declaration.update_url.is_some()
        }))
    }
    async fn bootstrap_archive_rule_seed(
        &self,
        identity: &str,
        plugin_dir: &std::path::Path,
        declaration: &super::super::manifest::ArchiveRulesDecl,
        keys: (&str, &str),
        watermark: Option<&RuleWatermark>,
        now: u64,
    ) -> Result<(ArchiveRuleCache, VerifiedArchiveRules), PluginError> {
        let bytes =
            super::super::archive_rules::read_bounded(&plugin_dir.join(&declaration.entry)).await?;
        let verified = VerifiedArchiveRules::verify(&bytes, &declaration.public_key)?;
        if watermark.is_some_and(|stored| {
            verified.sequence() < stored.sequence
                || (verified.sequence() == stored.sequence && verified.digest() != stored.digest)
        }) {
            return Err(PluginError::Runtime(
                "archive rules seed is below or conflicts with trusted sequence watermark"
                    .to_owned(),
            ));
        }
        let seed = ArchiveRuleCache {
            envelope: Arc::new(String::from_utf8(bytes).map_err(|error| {
                PluginError::Runtime(format!("archiveRules seed is not UTF-8: {error}"))
            })?),
            public_key: declaration.public_key.clone(),
            update_url: declaration.update_url.clone(),
            sequence: verified.sequence(),
            digest: verified.digest().to_owned(),
            last_checked_at: None,
            last_updated_at: Some(now),
            last_error: None,
        };
        if !self
            .archive_rules_declaration_active(identity, declaration)
            .await
        {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 已停用或规则声明已变更"
            )));
        }
        let trusted =
            RuleWatermark::for_pack(seed.sequence, seed.digest.clone(), &declaration.public_key);
        self.persist_archive_rules(keys.0, keys.1, &seed, &trusted)
            .await?;
        Ok((seed, verified))
    }

    async fn promote_bundled_seed_if_newer(
        &self,
        identity: &str,
        plugin_dir: &std::path::Path,
        declaration: &super::super::manifest::ArchiveRulesDecl,
        keys: (&str, &str),
        current: &ArchiveRuleCache,
        now: u64,
    ) -> Result<Option<(ArchiveRuleCache, VerifiedArchiveRules)>, PluginError> {
        let bytes =
            super::super::archive_rules::read_bounded(&plugin_dir.join(&declaration.entry)).await?;
        if bytes.as_slice() == current.envelope.as_bytes() {
            return Ok(None);
        }
        let verified = VerifiedArchiveRules::verify(&bytes, &declaration.public_key)?;
        if verified.sequence() == current.sequence && verified.digest() != current.digest {
            return Err(PluginError::Runtime(
                "archive rules seed conflicts with cached sequence".to_owned(),
            ));
        }
        if verified.sequence() <= current.sequence {
            return Ok(None);
        }
        let seed = ArchiveRuleCache {
            envelope: Arc::new(String::from_utf8(bytes).map_err(|error| {
                PluginError::Runtime(format!("archiveRules seed is not UTF-8: {error}"))
            })?),
            public_key: declaration.public_key.clone(),
            update_url: declaration.update_url.clone(),
            sequence: verified.sequence(),
            digest: verified.digest().to_owned(),
            last_checked_at: None,
            last_updated_at: Some(now),
            last_error: None,
        };
        if !self
            .archive_rules_declaration_active(identity, declaration)
            .await
        {
            return Err(PluginError::Runtime(format!(
                "插件 {identity} 已停用或规则声明已变更"
            )));
        }
        let trusted =
            RuleWatermark::for_pack(seed.sequence, seed.digest.clone(), &declaration.public_key);
        self.persist_archive_rules(keys.0, keys.1, &seed, &trusted)
            .await?;
        Ok(Some((seed, verified)))
    }

    async fn persist_trust_watermark_if_needed(
        &self,
        cache_key: &str,
        watermark_key: &str,
        cache: &ArchiveRuleCache,
        watermark: Option<&RuleWatermark>,
        from_memory: bool,
    ) -> Result<(), PluginError> {
        if !from_memory
            && watermark.is_none_or(|stored| {
                cache.sequence > stored.sequence
                    || (cache.sequence == stored.sequence && cache.digest != stored.digest)
                    || (!stored.public_key.is_empty() && stored.public_key != cache.public_key)
            })
        {
            let trusted =
                RuleWatermark::for_pack(cache.sequence, cache.digest.clone(), &cache.public_key);
            self.persist_archive_rules(cache_key, watermark_key, cache, &trusted)
                .await?;
        }
        Ok(())
    }

    async fn clear_archive_rule_trust(
        &self,
        cache_key: &str,
        watermark_key: &str,
    ) -> Result<(), PluginError> {
        self.db
            .delete_config(cache_key)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        self.db
            .delete_config(watermark_key)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))
    }

    async fn validated_archive_rule_cache(
        &self,
        identity: &str,
        cache_key: &str,
        watermark_key: &str,
        public_key: &str,
        mut cache: Option<ArchiveRuleCache>,
    ) -> Result<
        (
            Option<ArchiveRuleCache>,
            Option<RuleWatermark>,
            Option<Arc<VerifiedArchiveRules>>,
        ),
        PluginError,
    > {
        let from_memory = cache.is_some();
        let mut watermark = None;
        if cache.is_none() {
            (cache, watermark) = self
                .load_archive_rule_cache(identity, cache_key, watermark_key)
                .await?;
        }
        let cache_key_mismatch = cache
            .as_ref()
            .is_some_and(|current| current.public_key != public_key);
        let watermark_key_mismatch = watermark
            .as_ref()
            .is_some_and(|stored| !stored.matches_key(public_key));
        if cache_key_mismatch || watermark_key_mismatch {
            // Rotating the publisher key ends the previous sequence lineage.
            self.clear_archive_rule_trust(cache_key, watermark_key)
                .await?;
            if cache_key_mismatch {
                cache = None;
            }
            watermark = None;
        }
        let mut verified_cache = None;
        if let Some(current) = cache.as_ref() {
            let already_verified = if from_memory {
                self.archive_rule_packs
                    .read()
                    .await
                    .get(identity)
                    .filter(|rules| {
                        rules.sequence() == current.sequence && rules.digest() == current.digest
                    })
                    .cloned()
            } else {
                None
            };
            let verified = match already_verified {
                Some(rules) => Ok(rules),
                None => VerifiedArchiveRules::verify(current.envelope.as_bytes(), public_key)
                    .map(Arc::new),
            };
            let accepted = current.public_key == public_key
                && verified.as_ref().is_ok_and(|rules| {
                    rules.sequence() == current.sequence && rules.digest() == current.digest
                })
                && (from_memory
                    || watermark.as_ref().is_none_or(|stored| {
                        current.sequence > stored.sequence
                            || (current.sequence == stored.sequence
                                && current.digest == stored.digest)
                    }));
            if accepted {
                verified_cache = verified.ok();
            } else {
                if from_memory {
                    let (_, trusted) = self
                        .load_archive_rule_cache(identity, cache_key, watermark_key)
                        .await?;
                    watermark = trusted.filter(|stored| stored.matches_key(public_key));
                }
                cache = None;
            }
        }
        Ok((cache, watermark, verified_cache))
    }
    async fn load_archive_rule_cache(
        &self,
        identity: &str,
        cache_key: &str,
        watermark_key: &str,
    ) -> Result<(Option<ArchiveRuleCache>, Option<RuleWatermark>), PluginError> {
        let watermark_raw = self
            .db
            .get_config(watermark_key)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        let watermark = match watermark_raw {
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
        let raw = self
            .db
            .get_config(cache_key)
            .await
            .map_err(|error| PluginError::Runtime(error.to_string()))?;
        let cache = match raw {
            Some(raw) if raw.len() <= MAX_CACHE_JSON_BYTES => {
                match serde_json::from_str::<ArchiveRuleCache>(&raw) {
                    Ok(mut cache) => {
                        cache.last_checked_at = None;
                        cache.last_error = None;
                        Some(cache)
                    }
                    Err(error) => {
                        log_info!("[plugin] invalid archive rules cache for {identity}: {error:#}");
                        None
                    }
                }
            }
            Some(raw) => {
                log_info!(
                    "[plugin] archive rules cache for {identity} exceeds size limit ({} bytes)",
                    raw.len()
                );
                None
            }
            None => None,
        };
        Ok((cache, watermark))
    }
    pub(super) async fn process_archive_rule_update(
        &self,
        identity: &str,
        declaration: &super::super::manifest::ArchiveRulesDecl,
        url: &str,
        current: &mut ArchiveRuleCache,
        result: Result<Vec<u8>, PluginError>,
        now: u64,
    ) -> Result<Option<VerifiedArchiveRules>, PluginError> {
        let mut promoted_rules = None;
        match result {
            Ok(bytes) if bytes.as_slice() == current.envelope.as_bytes() => {
                current.last_error = None;
            }
            Ok(bytes) => match VerifiedArchiveRules::verify(&bytes, &declaration.public_key) {
                Ok(verified) if verified.sequence() > current.sequence => {
                    let envelope = match String::from_utf8(bytes) {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            current.last_error = Some(format!("update pack is not UTF-8: {error}"));
                            String::new()
                        }
                    };
                    if !envelope.is_empty() {
                        let promoted = ArchiveRuleCache {
                            envelope: Arc::new(envelope),
                            public_key: declaration.public_key.clone(),
                            update_url: Some(url.to_owned()),
                            sequence: verified.sequence(),
                            digest: verified.digest().to_owned(),
                            last_checked_at: Some(now),
                            last_updated_at: Some(now),
                            last_error: None,
                        };
                        if !self
                            .archive_rules_declaration_active(identity, declaration)
                            .await
                        {
                            return Err(PluginError::Runtime(format!(
                                "插件 {identity} 已停用或规则声明已变更"
                            )));
                        }
                        let trusted = RuleWatermark::for_pack(
                            promoted.sequence,
                            promoted.digest.clone(),
                            &declaration.public_key,
                        );
                        self.persist_archive_rules(
                            &format!("plugin.{identity}.archive_rules.cache"),
                            &format!("plugin.{identity}.archive_rules.watermark"),
                            &promoted,
                            &trusted,
                        )
                        .await?;
                        *current = promoted;
                        promoted_rules = Some(verified);
                    }
                }
                Ok(verified)
                    if verified.sequence() == current.sequence
                        && verified.digest() == current.digest =>
                {
                    current.last_error = None;
                }
                Ok(_) => {
                    current.last_error =
                        Some("archive rules sequence rollback or equivocation rejected".to_owned());
                }
                Err(error) => current.last_error = Some(error.to_string()),
            },
            Err(error) => current.last_error = Some(error.to_string()),
        }
        current.last_checked_at = Some(now);
        Ok(promoted_rules)
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
                self.archive_rule_packs.write().await.remove(&identity);
                self.archive_rule_status.write().await.remove(&identity);
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
        let mut persistent = cache.clone();
        persistent.last_checked_at = None;
        persistent.last_error = None;
        let cache_json = serde_json::to_string(&persistent)
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
