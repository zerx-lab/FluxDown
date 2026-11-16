//! 站点代理规则的逐请求路由：任务出口（沿用发送方自己的 client）与规则出口之间的切换。
//!
//! - [`EgressRouter`] 挂在 `RequestSpec.egress` 上，`http_request` 每一跳（含重定向、
//!   HLS/DASH 分片、探测）都会先问它：命中了与任务出口不同的规则，就换用该规则的 client。
//! - [`apply`] 在插件 resolver 改写最终链接之后重新匹配一次，必要时整体改道。

use std::sync::Arc;

use reqwest::Client;
use url::Url;

use crate::client_cache;
use crate::downloader::{DownloadError, DownloadParams};
use crate::events::EngineEvent;
use crate::logger::log_info;
use crate::proxy_rules::ProxyRuleSet;

/// 逐请求规则路由器。
pub struct EgressRouter {
    rules: Arc<ProxyRuleSet>,
    /// 任务出口已由该规则决定时为其下标；命中同一条规则的请求沿用发送方自己的 client。
    base_rule: Option<usize>,
    user_agent: String,
    ignore_tls_errors: bool,
}

impl std::fmt::Debug for EgressRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressRouter")
            .field("base_rule", &self.base_rule)
            .field("rules", &self.rules.len())
            .finish()
    }
}

impl EgressRouter {
    pub fn new(
        rules: Arc<ProxyRuleSet>,
        base_rule: Option<usize>,
        user_agent: &str,
        ignore_tls_errors: bool,
    ) -> Self {
        Self {
            rules,
            base_rule,
            user_agent: user_agent.to_owned(),
            ignore_tls_errors,
        }
    }

    pub(crate) fn rules(&self) -> &Arc<ProxyRuleSet> {
        &self.rules
    }

    pub(crate) fn base_rule(&self) -> Option<usize> {
        self.base_rule
    }

    pub(crate) fn with_base(&self, index: usize) -> Self {
        Self {
            rules: Arc::clone(&self.rules),
            base_rule: Some(index),
            user_agent: self.user_agent.clone(),
            ignore_tls_errors: self.ignore_tls_errors,
        }
    }

    /// 该请求应使用的规则 client；`None` = 沿用发送方自己的 client（CDN 钉定、网卡、
    /// Auto 路径 client 均保持不变）。构建失败返回 `Err`，**不回退到任务出口**，
    /// 否则规则会被静默绕过。
    pub(crate) async fn client_for(&self, url: &Url) -> Result<Option<Client>, DownloadError> {
        let Some(rule) = self.rules.first_match_url(url) else {
            return Ok(None);
        };
        if Some(rule.index()) == self.base_rule {
            return Ok(None);
        }
        let egress = rule.egress().clone();
        if !egress.needs_detection()
            && let Some(client) =
                client_cache::lookup(&egress.resolve(), &self.user_agent, self.ignore_tls_errors)
        {
            return Ok(Some(client));
        }
        let ua = self.user_agent.clone();
        let tls = self.ignore_tls_errors;
        let client =
            tokio::task::spawn_blocking(move || client_cache::get(&egress.resolve(), &ua, tls))
                .await
                .map_err(|e| {
                    DownloadError::Other(format!("proxy rule client task failed: {e}"))
                })??;
        Ok(Some(client))
    }
}

/// 在 resolver 补丁定下最终链接之后重新匹配规则。返回 `true` 表示本任务出口由规则
/// 决定（调用方据此跳过 PAC 修正）。
pub(crate) async fn apply(params: &mut DownloadParams) -> bool {
    let Some(router) = params.spec.egress.clone() else {
        return false;
    };
    let Some(rule) = router.rules().first_match(&params.url) else {
        return router.base_rule().is_some();
    };
    if Some(rule.index()) == router.base_rule() {
        return true;
    }
    let index = rule.index();
    let egress = rule.egress().clone();
    let ua = params.run_meta.user_agent.clone();
    let tls = params.run_meta.ignore_tls_errors;
    let built = tokio::task::spawn_blocking(move || {
        let proxy = egress.resolve();
        client_cache::get(&proxy, &ua, tls).map(|client| (proxy, client))
    })
    .await;
    let (proxy, client) = match built {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            log_info!(
                "[proxy-rules] task {} 构建规则 #{} 出口 client 失败: {e:#}",
                params.task_id,
                index + 1
            );
            return router.base_rule().is_some();
        }
        Err(e) => {
            log_info!(
                "[proxy-rules] task {} 规则 #{} 出口构建任务异常: {e}",
                params.task_id,
                index + 1
            );
            return router.base_rule().is_some();
        }
    };
    let Some(rule) = router.rules().first_match(&params.url) else {
        return router.base_rule().is_some();
    };
    let label = rule.route_label(&proxy);
    let goes_through_proxy = proxy.mode != crate::proxy_config::ProxyMode::None;
    params.client = client;
    params.proxy_config = proxy;
    params.auto_proxy = None;
    if goes_through_proxy {
        crate::pac_route::route_through_proxy(params);
    }
    params.spec.egress = Some(Arc::new(router.with_base(index)));
    if let Err(error) = params.db.set_task_auto_route(&params.task_id, label).await {
        log_info!(
            "[proxy-rules] task {} auto_route 落库失败: {error:#}",
            params.task_id
        );
    }
    params.sink.emit(EngineEvent::TaskRouteChanged {
        task_id: params.task_id.clone(),
        route: label.to_string(),
    });
    log_info!(
        "[proxy-rules] task {} 最终链接命中规则 #{}，出口 {} endpoint={}",
        params.task_id,
        index + 1,
        label,
        params.proxy_config.log_endpoint()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_config::ProxyConfig;

    fn rules(json: &str) -> Arc<ProxyRuleSet> {
        Arc::new(ProxyRuleSet::compile(json, &ProxyConfig::default()))
    }

    const RULES: &str = r#"[
        {"id":"a","match":"domain","pattern":"direct.test","action":"direct"},
        {"id":"b","match":"domain","pattern":"proxied.test","action":"custom","proxy":"http://127.0.0.1:9"}
    ]"#;

    #[tokio::test]
    async fn unmatched_and_base_rule_keep_the_senders_client() {
        let router = EgressRouter::new(rules(RULES), Some(1), "ua", false);
        let miss = Url::parse("https://other.test/x").unwrap();
        assert!(router.client_for(&miss).await.unwrap().is_none());
        let base = Url::parse("https://proxied.test/x").unwrap();
        assert!(router.client_for(&base).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn other_rule_switches_client() {
        let router = EgressRouter::new(rules(RULES), Some(1), "ua", false);
        let hit = Url::parse("https://direct.test/x").unwrap();
        assert!(router.client_for(&hit).await.unwrap().is_some());
        let none_base = EgressRouter::new(rules(RULES), None, "ua", false);
        let hit = Url::parse("https://proxied.test/x").unwrap();
        assert!(none_base.client_for(&hit).await.unwrap().is_some());
    }

    #[test]
    fn with_base_keeps_rules_and_updates_base() {
        let router = EgressRouter::new(rules(RULES), None, "ua", false).with_base(1);
        assert_eq!(router.base_rule(), Some(1));
        assert_eq!(router.rules().len(), 2);
    }
}
