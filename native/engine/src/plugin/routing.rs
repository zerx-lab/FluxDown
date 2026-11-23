//! 多插件 resolver 冲突：候选匹配、站点默认插件与首次起跑时的插件选择。
//!
//! 多个已启用插件的 resolver 同时匹配一个链接时，建任务先绑定回退插件并置
//! `tasks.resolver_choice_pending`；首次 resolve 前（off-actor worker）按以下优先级定案：
//!
//! 1. 用户确认过、且覆盖当前全部候选的站点默认插件 → 直接使用，不询问；
//! 2. 无人值守任务（RSS / 远程派发 / 免打扰跳过二次选择）→ 站点记录的插件 > 已绑定回退
//!    插件 > 首个候选，并把这次自动选择记成未确认的站点记录，供插件页展示冲突；
//! 3. 其余任务经 [`HostSelection::select_resolver_plugin`] 询问用户，可选记为站点默认。
//!
//! 从未起跑（无落盘进度、无 `resolver_item`）的任务若绑定的插件被禁用 / 待授权 / 熔断 /
//! 不再匹配，而其它插件命中，起跑前同样走上述定案并改绑；有进度的任务维持绑定并
//! fail-closed（中途换 resolver 可能写坏文件）。空绑定从不自动改绑——它与「忽略插件
//! 重试」逃生舱的结果无法区分。没有任何候选时不写回，待选标记保持，等插件重新启用后
//! 的下一次起跑再定案。
//!
//! 站点记录存在 config `plugin.site_default.<site>`（JSON）。`site` 是链接主机名（小写、
//! 去掉 `www.`）；记录携带当时的候选集合，出现集合外的新插件时会重新询问。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::PluginManager;
use crate::logger::log_info;
use crate::plugin::manifest::url_glob_match;
use crate::plugin::runtime::PluginError;
use crate::selection::{
    HostSelection, PluginCandidate, PluginChoice, PluginChoiceRequest, SelectionOutcome,
};

/// 站点记录的 config 键前缀（与 `plugin.<identity>.` 不相交：identity 必含 `@`）。
const SITE_DEFAULT_PREFIX: &str = "plugin.site_default.";
/// 多插件选择框的等待上限，到期采用回退插件。
const PLUGIN_CHOICE_TIMEOUT: Duration = Duration::from_secs(60);
/// 站点键长度上限（DNS 主机名 253 + IPv6 方括号余量）。
const MAX_SITE_LEN: usize = 255;
/// 一条站点记录的候选插件上限（客户端预置候选时校验）。
const MAX_SITE_CANDIDATES: usize = 64;

/// 建任务时的 resolver 绑定结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverRoute {
    /// 绑定的插件；待选时为回退插件。
    pub identity: String,
    /// 多个插件同时匹配且站点没有确认过的默认插件：首次起跑时再定案。
    pub choice_pending: bool,
}

/// 起跑前多插件选择的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolverChoice {
    /// 使用该插件（已写回任务绑定）。
    Use(String),
    /// 用户关闭了选择框：任务取消，绑定保持待选，下次起跑重新询问。
    Cancelled,
}

/// 站点记录（`daemon.plugin.siteDefaults` 列表项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteDefault {
    pub site: String,
    pub identity: String,
    pub candidates: Vec<String>,
    /// `true` = 无人值守下载自动选用、未经用户确认。
    pub automatic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSiteDefault {
    identity: String,
    candidates: Vec<String>,
    #[serde(default)]
    automatic: bool,
}

impl StoredSiteDefault {
    /// 记录的插件仍是候选，且当前候选没有记录之外的新插件。
    fn covers(&self, candidates: &[PluginCandidate]) -> bool {
        candidates.iter().any(|c| c.identity == self.identity)
            && candidates
                .iter()
                .all(|c| self.candidates.contains(&c.identity))
    }
}

/// 站点键：链接主机名小写、去掉结尾的点与 `www.` 前缀；无主机名时为 `None`。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::plugin::routing::site_key;
///
/// assert_eq!(site_key("https://WWW.Example.com/a").as_deref(), Some("example.com"));
/// assert_eq!(site_key("magnet:?xt=urn:btih:abc"), None);
/// ```
#[must_use]
pub fn site_key(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let site = host.strip_prefix("www.").unwrap_or(&host);
    (!site.is_empty()).then(|| site.to_owned())
}

fn site_config_key(site: &str) -> String {
    format!("{SITE_DEFAULT_PREFIX}{site}")
}

fn invalid(key: &str, message: impl Into<String>) -> PluginError {
    PluginError::InvalidSetting {
        key: key.to_owned(),
        message: message.into(),
    }
}

fn validate_site(site: &str) -> Result<(), PluginError> {
    if site.is_empty() || site.len() > MAX_SITE_LEN || site.chars().any(char::is_control) {
        return Err(invalid("site", "站点无效"));
    }
    Ok(())
}

fn candidate_ids(candidates: &[PluginCandidate]) -> Vec<String> {
    candidates.iter().map(|c| c.identity.clone()).collect()
}

/// 每站点串行化选择框：同一站点一批任务只弹一个框，用户勾选「记住」后其余任务直接沿用。
#[derive(Default)]
pub(super) struct SitePromptLocks {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SitePromptLocks {
    fn get(&self, site: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = lock_or_recover(&self.locks);
        Arc::clone(locks.entry(site.to_owned()).or_default())
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 无人值守定案：仍是候选的站点记录 > 仍是候选的已绑定回退插件 > 首个候选。
fn unattended_pick(
    stored: Option<&StoredSiteDefault>,
    bound: &str,
    candidates: &[PluginCandidate],
) -> String {
    let is_candidate = |identity: &str| candidates.iter().any(|c| c.identity == identity);
    stored
        .map(|s| s.identity.as_str())
        .filter(|identity| is_candidate(identity))
        .or_else(|| is_candidate(bound).then_some(bound))
        .or_else(|| candidates.first().map(|c| c.identity.as_str()))
        .unwrap_or(bound)
        .to_owned()
}

impl PluginManager {
    /// 链接命中的已启用 resolver 插件（identity 升序）。`multi_only` 只取声明
    /// `"multi": true` 的 resolver（前置预解析用）。
    async fn matching_resolvers(&self, url: &str, multi_only: bool) -> Vec<PluginCandidate> {
        let snapshot = self.plugins.read().await.clone();
        let mut candidates: Vec<PluginCandidate> = snapshot
            .iter()
            .filter(|p| p.enabled)
            .filter(|p| match p.manifest.resolvers.first() {
                Some(resolver) => !multi_only || resolver.multi,
                None => false,
            })
            .filter(|p| {
                p.manifest
                    .resolver_urls()
                    .iter()
                    .any(|pattern| url_glob_match(pattern, url))
            })
            .map(|p| PluginCandidate {
                identity: p.manifest.identity.clone(),
                name: p.manifest.name.clone(),
                version: p.manifest.version.clone(),
                description: p.manifest.description.clone(),
            })
            .collect();
        candidates.sort_by(|a, b| a.identity.cmp(&b.identity));
        candidates
    }

    /// 建任务时的 resolver 绑定：单一命中直接绑定；多个命中时优先用户确认过的站点默认
    /// 插件，否则绑定首个候选并标记待选。无命中返回 `None`。
    pub async fn route_resolver(&self, url: &str) -> Option<ResolverRoute> {
        let candidates = self.matching_resolvers(url, false).await;
        let first = candidates.first()?.identity.clone();
        if candidates.len() == 1 {
            return Some(ResolverRoute {
                identity: first,
                choice_pending: false,
            });
        }
        if let Some(identity) = self.confirmed_default(url, &candidates).await {
            return Some(ResolverRoute {
                identity,
                choice_pending: false,
            });
        }
        Some(ResolverRoute {
            identity: first,
            choice_pending: true,
        })
    }

    /// 插件当前是否可作为 resolver 使用：已启用（待授权 / 熔断 / 被撤销的插件都不算）且
    /// 声明了 resolver。任务组成员的显式绑定据此判定，不可用即 fail-closed。
    pub async fn resolver_available(&self, identity: &str) -> bool {
        self.plugins.read().await.iter().any(|p| {
            p.enabled && p.manifest.identity == identity && !p.manifest.resolvers.is_empty()
        })
    }

    /// 已绑定的插件 `bound` 不再命中链接（被禁用 / 待授权 / 熔断 / 规则不再匹配），但仍有
    /// 其它已启用 resolver 命中——从未起跑的任务据此改按当前路由重新选择。没有任何候选时
    /// 返回 `false`（无处可换，维持 fail-closed）。
    pub async fn bound_resolver_superseded(&self, url: &str, bound: &str) -> bool {
        let candidates = self.matching_resolvers(url, false).await;
        !candidates.is_empty() && !candidates.iter().any(|c| c.identity == bound)
    }

    /// 前置预解析（[`super::super::ResolveManifest`]）选用的插件：只看声明了
    /// `"multi": true` 的 resolver，多个命中时优先确认过的站点默认插件，否则取首个——
    /// 避免解析昂贵的单文件插件在创建下载时白跑一次（A2 契约）。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run(pm: std::sync::Arc<fluxdown_engine::plugin::PluginManager>) {
    /// // 未声明 multi 的插件恒不命中，即使 URL glob 匹配。
    /// let hit = pm.match_multi_resolver("https://pan.example.com/s/abc").await;
    /// assert!(hit.is_none() || hit.is_some());
    /// # }
    /// ```
    pub async fn match_multi_resolver(&self, url: &str) -> Option<String> {
        let candidates = self.matching_resolvers(url, true).await;
        let first = candidates.first()?.identity.clone();
        if candidates.len() > 1
            && let Some(identity) = self.confirmed_default(url, &candidates).await
        {
            return Some(identity);
        }
        Some(first)
    }

    /// 不建任务的宿主自身下载（应用更新、组件、插件包）选用的 resolver：按无人值守任务的
    /// 规则定案（站点记录 > 首个候选），不询问、不写站点记录。无命中返回 `None`。
    pub async fn unattended_resolver(&self, url: &str) -> Option<String> {
        let candidates = self.matching_resolvers(url, false).await;
        match candidates.as_slice() {
            [] => None,
            [only] => Some(only.identity.clone()),
            _ => {
                let stored = match site_key(url) {
                    Some(site) => self.read_site_default(&site).await,
                    None => None,
                };
                Some(unattended_pick(stored.as_ref(), "", &candidates))
            }
        }
    }

    /// 首次 resolve 前为待选任务定案（off-actor worker 调用）并写回任务绑定。
    ///
    /// `bound` 是建任务时绑定的回退插件；`unattended` 任务绝不询问。用户关闭选择框时
    /// 返回 [`ResolverChoice::Cancelled`]，任务绑定保持待选。
    ///
    /// 当前没有任何候选（全部被禁用 / 卸载 / 待授权）时**不写回**：返回 `Use(bound)`，
    /// 本次 resolve 按既有 fail-closed 语义报错，而任务仍保持待选，等插件重新启用后的
    /// 下一次起跑再定案。
    pub async fn choose_task_resolver(
        &self,
        task_id: &str,
        url: &str,
        bound: &str,
        unattended: bool,
        selector: &dyn HostSelection,
    ) -> Result<ResolverChoice, PluginError> {
        let candidates = self.matching_resolvers(url, false).await;
        let chosen = match candidates.as_slice() {
            [] => return Ok(ResolverChoice::Use(bound.to_owned())),
            [only] => only.identity.clone(),
            _ => {
                match self
                    .choose_among(task_id, url, bound, candidates, unattended, selector)
                    .await
                {
                    Some(identity) => identity,
                    None => return Ok(ResolverChoice::Cancelled),
                }
            }
        };
        self.db
            .set_task_resolver(task_id, &chosen)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        Ok(ResolverChoice::Use(chosen))
    }

    /// 两个以上候选时的定案；`None` = 用户取消。
    async fn choose_among(
        &self,
        task_id: &str,
        url: &str,
        bound: &str,
        candidates: Vec<PluginCandidate>,
        unattended: bool,
        selector: &dyn HostSelection,
    ) -> Option<String> {
        let site = site_key(url);
        let lock = site.as_deref().map(|site| self.site_prompts.get(site));
        let _serial = match &lock {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        // 排队期间同站点的上一个选择框可能刚记住了默认插件，持锁后重新读取。
        let stored = match &site {
            Some(site) => self.read_site_default(site).await,
            None => None,
        };
        if let Some(stored) = &stored
            && !stored.automatic
            && stored.covers(&candidates)
        {
            return Some(stored.identity.clone());
        }
        let fallback = unattended_pick(stored.as_ref(), bound, &candidates);

        if unattended {
            // 记录这次自动选择（不覆盖用户确认过的记录），插件页据此提示冲突并让用户确认。
            if let Some(site) = &site
                && stored.as_ref().is_none_or(|s| s.automatic)
            {
                let record = StoredSiteDefault {
                    identity: fallback.clone(),
                    candidates: candidate_ids(&candidates),
                    automatic: true,
                };
                if stored.as_ref() != Some(&record) {
                    self.write_site_default(site, &record).await;
                }
            }
            log_info!(
                "[plugin-route] task {task_id} unattended, {} resolvers match, using {fallback}",
                candidates.len()
            );
            return Some(fallback);
        }

        let request = PluginChoiceRequest {
            site: site.clone().unwrap_or_default(),
            url: url.to_owned(),
            candidates,
            default_identity: fallback.clone(),
        };
        let outcome = selector
            .select_resolver_plugin(task_id, &request, PLUGIN_CHOICE_TIMEOUT)
            .await;
        log_info!("[plugin-route] task {task_id} plugin choice outcome: {outcome:?}");
        match outcome {
            SelectionOutcome::UserChose(PluginChoice::Cancel) => None,
            SelectionOutcome::UserChose(PluginChoice::Use { identity, remember })
                if request.candidates.iter().any(|c| c.identity == identity) =>
            {
                if remember && let Some(site) = &site {
                    let record = StoredSiteDefault {
                        identity: identity.clone(),
                        candidates: candidate_ids(&request.candidates),
                        automatic: false,
                    };
                    self.write_site_default(site, &record).await;
                }
                Some(identity)
            }
            _ => Some(fallback),
        }
    }

    /// 用户确认过、且覆盖当前全部候选的站点默认插件。
    async fn confirmed_default(&self, url: &str, candidates: &[PluginCandidate]) -> Option<String> {
        let stored = self.read_site_default(&site_key(url)?).await?;
        (!stored.automatic && stored.covers(candidates)).then_some(stored.identity)
    }

    async fn read_site_default(&self, site: &str) -> Option<StoredSiteDefault> {
        let raw = match self.db.get_config(&site_config_key(site)).await {
            Ok(raw) => raw?,
            Err(error) => {
                crate::logger::report_error("plugin-route", "read site default plugin", &error);
                return None;
            }
        };
        match serde_json::from_str(&raw) {
            Ok(stored) => Some(stored),
            Err(error) => {
                crate::logger::report_error("plugin-route", "decode site default plugin", &error);
                None
            }
        }
    }

    /// 写站点记录并广播插件状态（客户端据此重拉站点默认）。失败只记日志：本次选择已经
    /// 生效，记录缺失只会让下次重新询问。
    async fn write_site_default(&self, site: &str, record: &StoredSiteDefault) {
        let json = match serde_json::to_string(record) {
            Ok(json) => json,
            Err(error) => {
                crate::logger::report_error("plugin-route", "encode site default plugin", &error);
                return;
            }
        };
        if let Err(error) = self.db.set_config(&site_config_key(site), &json).await {
            crate::logger::report_error("plugin-route", "save site default plugin", &error);
            return;
        }
        self.publish_state().await;
    }

    /// 全部站点记录（按站点排序）。
    pub async fn site_defaults(&self) -> Result<Vec<SiteDefault>, PluginError> {
        let entries = self
            .db
            .list_config_with_prefix(SITE_DEFAULT_PREFIX)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, raw) in entries {
            let Some(site) = key.strip_prefix(SITE_DEFAULT_PREFIX) else {
                continue;
            };
            match serde_json::from_str::<StoredSiteDefault>(&raw) {
                Ok(stored) => out.push(SiteDefault {
                    site: site.to_owned(),
                    identity: stored.identity,
                    candidates: stored.candidates,
                    automatic: stored.automatic,
                }),
                Err(error) => {
                    crate::logger::report_error(
                        "plugin-route",
                        "decode site default plugin",
                        &error,
                    );
                }
            }
        }
        out.sort_by(|a, b| a.site.cmp(&b.site));
        Ok(out)
    }

    /// 确认 / 改选（`Some`）或删除（`None`）站点记录。
    ///
    /// `candidates` 与已有记录的候选合并（保持已有顺序、去重）：插件页可在尚无记录时按
    /// URL 模式冲突预先选定默认插件。`identity` 必须在合并后的候选里。
    pub async fn set_site_default(
        &self,
        site: &str,
        identity: Option<&str>,
        candidates: &[String],
    ) -> Result<(), PluginError> {
        validate_site(site)?;
        let key = site_config_key(site);
        let Some(identity) = identity else {
            self.db
                .delete_config(&key)
                .await
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
            self.publish_state().await;
            return Ok(());
        };
        if candidates.len() > MAX_SITE_CANDIDATES
            || candidates
                .iter()
                .any(|c| c.is_empty() || c.len() > MAX_SITE_LEN || c.chars().any(char::is_control))
        {
            return Err(invalid("candidates", "候选插件无效"));
        }
        let stored = self.read_site_default(site).await;
        if stored.is_none() && candidates.is_empty() {
            return Err(invalid("site", format!("站点 {site} 没有插件选择记录")));
        }
        let mut merged = stored.map(|s| s.candidates).unwrap_or_default();
        for candidate in candidates {
            if !merged.contains(candidate) {
                merged.push(candidate.clone());
            }
        }
        if !merged.iter().any(|known| known == identity) {
            return Err(invalid(
                "identity",
                format!("插件 {identity} 不在站点 {site} 的候选插件中"),
            ));
        }
        let record = StoredSiteDefault {
            identity: identity.to_owned(),
            candidates: merged,
            automatic: false,
        };
        let json =
            serde_json::to_string(&record).map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.db
            .set_config(&key, &json)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        self.publish_state().await;
        Ok(())
    }

    /// 卸载插件后整理站点记录：把它移出候选；选中它或剩余候选不足两个的记录删除。
    pub(super) async fn forget_site_defaults_of(&self, identity: &str) -> Result<(), PluginError> {
        for entry in self.site_defaults().await? {
            if !entry.candidates.iter().any(|c| c == identity) {
                continue;
            }
            let key = site_config_key(&entry.site);
            let candidates: Vec<String> = entry
                .candidates
                .into_iter()
                .filter(|c| c != identity)
                .collect();
            if entry.identity == identity || candidates.len() < 2 {
                self.db
                    .delete_config(&key)
                    .await
                    .map_err(|e| PluginError::Runtime(e.to_string()))?;
                continue;
            }
            let record = StoredSiteDefault {
                identity: entry.identity,
                candidates,
                automatic: entry.automatic,
            };
            let json =
                serde_json::to_string(&record).map_err(|e| PluginError::Runtime(e.to_string()))?;
            self.db
                .set_config(&key, &json)
                .await
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{PluginCandidate, StoredSiteDefault, site_key};

    fn candidate(identity: &str) -> PluginCandidate {
        PluginCandidate {
            identity: identity.to_owned(),
            name: identity.to_owned(),
            version: "1.0.0".to_owned(),
            description: String::new(),
        }
    }

    #[test]
    fn site_key_normalizes_host() {
        assert_eq!(
            site_key("https://www.Bilibili.com./video/1").as_deref(),
            Some("bilibili.com")
        );
        assert_eq!(
            site_key("https://m.bilibili.com/").as_deref(),
            Some("m.bilibili.com")
        );
        assert_eq!(site_key("http://[::1]:8080/x").as_deref(), Some("[::1]"));
        assert_eq!(site_key("not a url"), None);
    }

    #[test]
    fn stored_default_requires_chosen_candidate_and_no_new_competitor() {
        let stored = StoredSiteDefault {
            identity: "a@x".into(),
            candidates: vec!["a@x".into(), "b@x".into()],
            automatic: false,
        };
        assert!(stored.covers(&[candidate("a@x"), candidate("b@x")]));
        // 候选缩小（另一插件被卸载 / 禁用）仍沿用。
        assert!(stored.covers(&[candidate("a@x")]));
        // 选中的插件已不在候选里。
        assert!(!stored.covers(&[candidate("b@x"), candidate("c@x")]));
        // 新装的插件也能处理该站点：重新询问。
        assert!(!stored.covers(&[candidate("a@x"), candidate("b@x"), candidate("c@x")]));
    }
}
