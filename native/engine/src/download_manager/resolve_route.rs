use std::sync::Arc;

use futures_util::FutureExt;

use super::{DownloadProtocol, ProtocolProxies, ProxyConfig, is_bt_url, is_ed2k_url};
use crate::plugin::call_egress::{EgressOutcome, EgressPlan, PlanTask, plan_call};
use crate::plugin::{PluginError, PluginManager, ResolveRequest, ResolveResult, ResolveVariant};

pub(super) struct ResolverRoutes {
    pub overrides: Arc<ProtocolProxies>,
    replan: Option<(ProxyConfig, DownloadProtocol)>,
}

impl ResolverRoutes {
    pub fn new(
        global: &ProxyConfig,
        overrides: Arc<ProtocolProxies>,
        url: &str,
        task: PlanTask<'_>,
        has_audio: bool,
    ) -> (Result<EgressPlan, PluginError>, Self) {
        let protocol = DownloadProtocol::for_http_family(url, has_audio);
        let use_protocol = task.proxy_url.is_empty()
            && !global
                .rules
                .as_ref()
                .is_some_and(|rules| rules.first_match(url).is_some())
            && overrides.has_http_family_override()
            && !is_bt_url(url)
            && !is_ed2k_url(url);
        let initial = if use_protocol {
            protocol_plan(global, &overrides, url, task, protocol)
        } else {
            Ok(plan_call(global, url, task))
        };
        (
            initial,
            Self {
                overrides,
                replan: use_protocol.then(|| (global.clone(), protocol)),
            },
        )
    }

    pub fn retry_protocol(&self, url: &str, result: &ResolveResult) -> Option<DownloadProtocol> {
        let (_, initial) = self.replan.as_ref()?;
        let actual = result_protocol(url, result)?;
        (self.overrides.raw(*initial) != self.overrides.raw(actual)).then_some(actual)
    }

    pub async fn retry_plan(
        &self,
        url: &str,
        protocol: DownloadProtocol,
    ) -> Result<EgressPlan, PluginError> {
        let (global, _) = self.replan.as_ref().ok_or_else(|| {
            PluginError::Runtime("Missing protocol proxy context for resolver".to_owned())
        })?;
        let global = global.clone();
        let overrides = self.overrides.clone();
        let url = url.to_owned();
        tokio::task::spawn_blocking(move || {
            protocol_plan(&global, &overrides, &url, PlanTask::default(), protocol)
        })
        .await
        .map_err(|error| PluginError::Runtime(format!("Plan protocol proxy: {error}")))?
    }

    pub fn may_retry(&self) -> bool {
        self.replan.is_some()
    }
}

fn protocol_plan(
    global: &ProxyConfig,
    overrides: &ProtocolProxies,
    subject: &str,
    task: PlanTask<'_>,
    protocol: DownloadProtocol,
) -> Result<EgressPlan, PluginError> {
    let proxy = overrides
        .route(protocol)
        .map_err(|error| PluginError::Runtime(error.to_string()))?;
    Ok(match proxy {
        Some(proxy) => EgressPlan::fixed(proxy, true, global.rules.clone()),
        None => plan_call(global, subject, task),
    })
}

pub(super) async fn attempt(
    manager: &PluginManager,
    identity: &str,
    req: ResolveRequest,
    plan: Result<EgressPlan, PluginError>,
) -> (Result<Option<ResolveResult>, PluginError>, EgressOutcome) {
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => return (Err(error), EgressOutcome::default()),
    };
    match std::panic::AssertUnwindSafe(manager.resolve_planned(identity, req, Some(plan)))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("resolver panicked");
            (
                Err(PluginError::Runtime(message.to_owned())),
                EgressOutcome::default(),
            )
        }
    }
}

fn result_protocol(source: &str, result: &ResolveResult) -> Option<DownloadProtocol> {
    let url = if result.url.is_empty() {
        source
    } else {
        &result.url
    };
    (result.manifest.is_none() && !is_bt_url(url) && !is_ed2k_url(url))
        .then(|| DownloadProtocol::for_http_family(url, result.audio_url.is_some()))
}

pub(super) struct VariantChoice {
    label: String,
    container: String,
    width: i64,
    height: i64,
    required_bandwidth: Option<i64>,
}

impl VariantChoice {
    pub fn new(variants: &[ResolveVariant], index: usize) -> Option<Self> {
        let variant = variants.get(index)?;
        let mut choice = Self {
            label: variant.label.clone(),
            container: variant.container.clone(),
            width: variant.width,
            height: variant.height,
            required_bandwidth: None,
        };
        if variants
            .iter()
            .filter(|variant| choice.matches(variant))
            .nth(1)
            .is_some()
        {
            choice.required_bandwidth = Some(variant.bandwidth);
        }
        Some(choice)
    }

    fn matches(&self, variant: &ResolveVariant) -> bool {
        self.label == variant.label
            && self.container == variant.container
            && self.width == variant.width
            && self.height == variant.height
            && self
                .required_bandwidth
                .is_none_or(|bandwidth| bandwidth == variant.bandwidth)
    }

    fn matching_index(&self, variants: &[ResolveVariant]) -> Option<usize> {
        let mut matches = variants
            .iter()
            .enumerate()
            .filter(|(_, variant)| self.matches(variant));
        let index = matches.next()?.0;
        matches.next().is_none().then_some(index)
    }
}

/// Accepts only the same uniquely identifiable variant on the second pass; a
/// result from another protocol could be bound to yet another exit and must not
/// reach the download.
pub(super) fn accept_retry(
    source: &str,
    mut result: Option<ResolveResult>,
    choice: Option<&VariantChoice>,
    protocol: DownloadProtocol,
    overrides: &ProtocolProxies,
) -> Result<Option<ResolveResult>, PluginError> {
    if let Some(resolved) = &mut result {
        if let Some(choice) = choice {
            let Some(index) = choice
                .matching_index(&resolved.variants)
                .and_then(|index| i32::try_from(index).ok())
            else {
                return Err(PluginError::InvalidOutput(
                    "Resolver changed or ambiguously matched the selected variant after switching protocol proxy"
                        .to_owned(),
                ));
            };
            let variants = std::mem::take(&mut resolved.variants);
            super::apply_chosen_variant(resolved, variants, index);
        } else if !resolved.variants.is_empty() {
            return Err(PluginError::InvalidOutput(
                "Resolver returned unexpected variants after switching protocol proxy".to_owned(),
            ));
        }
        if result_protocol(source, resolved)
            .is_some_and(|actual| overrides.raw(actual) == overrides.raw(protocol))
        {
            return Ok(result);
        }
    }
    Err(PluginError::InvalidOutput(
        "Resolver changed the download route after switching protocol proxy".to_owned(),
    ))
}
#[cfg(test)]
mod tests {
    use super::{
        ProxyConfig, ResolveResult, ResolveVariant, ResolverRoutes, VariantChoice, accept_retry,
    };
    use crate::protocol_proxy::{DownloadProtocol, ProtocolProxies};
    use std::sync::Arc;

    fn overrides(http: &str, hls: &str) -> ProtocolProxies {
        ProtocolProxies::from_config(
            &[
                ("proxy_protocol_http".to_owned(), http.to_owned()),
                ("proxy_protocol_hls".to_owned(), hls.to_owned()),
            ]
            .into(),
        )
    }

    fn resolved(url: &str) -> ResolveResult {
        ResolveResult {
            url: url.to_owned(),
            ..ResolveResult::default()
        }
    }

    fn variant(url: &str, bandwidth: i64) -> ResolveVariant {
        ResolveVariant {
            label: "1080p".to_owned(),
            url: url.to_owned(),
            bandwidth,
            width: 1920,
            height: 1080,
            container: "mp4".to_owned(),
            ..ResolveVariant::default()
        }
    }

    #[test]
    fn does_not_retry_when_protocols_share_exit() {
        let overrides = overrides("socks5://proxy.test:1080", "socks5://proxy.test:1080");
        let routes = ResolverRoutes {
            overrides: Arc::new(overrides),
            replan: Some((ProxyConfig::default(), DownloadProtocol::Http)),
        };

        assert_eq!(
            routes.retry_protocol(
                "https://source.test/video",
                &resolved("https://cdn.test/live.m3u8")
            ),
            None
        );
    }

    #[test]
    fn retry_accepts_same_variant_after_protocol_switch() {
        let overrides = overrides("socks5://http.test:1080", "socks5://hls.test:1080");
        let mut first = resolved("");
        let lower_quality = |url| ResolveVariant {
            label: "720p".to_owned(),
            ..variant(url, 500)
        };
        first.variants = vec![
            lower_quality("https://cdn.test/old-720.m3u8"),
            variant("https://cdn.test/old.m3u8", 1_000),
        ];
        let choice = VariantChoice::new(&first.variants, 1).expect("selected variant");
        let mut replacement = resolved("");
        replacement.variants = vec![
            variant("https://cdn.test/new.m3u8", 1_500),
            lower_quality("https://cdn.test/new-720.m3u8"),
        ];

        let accepted = accept_retry(
            "https://source.test/video",
            Some(replacement),
            Some(&choice),
            DownloadProtocol::Hls,
            &overrides,
        )
        .unwrap()
        .unwrap();

        assert_eq!(accepted.url, "https://cdn.test/new.m3u8");
        assert!(accepted.variants.is_empty());
    }

    #[test]
    fn retry_rejects_variant_mismatch() {
        let overrides = overrides("socks5://http.test:1080", "socks5://hls.test:1080");
        let choice = VariantChoice::new(&[variant("https://cdn.test/old.m3u8", 1_000)], 0)
            .expect("selected variant");
        let mut replacement = resolved("");
        replacement.variants = vec![ResolveVariant {
            label: "720p".to_owned(),
            ..variant("https://cdn.test/new.m3u8", 1_000)
        }];

        assert!(
            accept_retry(
                "https://source.test/video",
                Some(replacement),
                Some(&choice),
                DownloadProtocol::Hls,
                &overrides,
            )
            .is_err()
        );
    }

    #[test]
    fn retry_rejects_a_different_exit() {
        let overrides = overrides("socks5://http.test:1080", "socks5://hls.test:1080");

        assert!(
            accept_retry(
                "https://source.test/video",
                Some(resolved("https://cdn.test/video.mp4")),
                None,
                DownloadProtocol::Hls,
                &overrides,
            )
            .is_err()
        );
    }

    #[test]
    fn protocol_switch_is_selected_for_the_replacement_plan() {
        let overrides = overrides("socks5://http.test:1080", "socks5://hls.test:1080");
        let routes = ResolverRoutes {
            overrides: Arc::new(overrides),
            replan: Some((ProxyConfig::default(), DownloadProtocol::Http)),
        };

        assert_eq!(
            routes.retry_protocol(
                "https://source.test/video",
                &resolved("https://cdn.test/live.m3u8")
            ),
            Some(DownloadProtocol::Hls)
        );
    }

    #[tokio::test]
    async fn retry_plan_uses_resolved_protocol_override() {
        let routes = ResolverRoutes {
            overrides: Arc::new(overrides(
                "socks5://http.test:1080",
                "socks5://hls.test:1080",
            )),
            replan: Some((ProxyConfig::default(), DownloadProtocol::Http)),
        };

        let plan = routes
            .retry_plan("https://source.test/video", DownloadProtocol::Hls)
            .await
            .unwrap();

        assert_eq!(plan.steps[0].proxy.host, "hls.test");
    }
}
