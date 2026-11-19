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
    pub index: usize,
    label: String,
    container: String,
    bandwidth: i64,
    width: i64,
    height: i64,
}

impl VariantChoice {
    pub fn new(index: usize, variant: &ResolveVariant) -> Self {
        Self {
            index,
            label: variant.label.clone(),
            container: variant.container.clone(),
            bandwidth: variant.bandwidth,
            width: variant.width,
            height: variant.height,
        }
    }

    pub fn matches(&self, variant: &ResolveVariant) -> bool {
        self.label == variant.label
            && self.container == variant.container
            && self.bandwidth == variant.bandwidth
            && self.width == variant.width
            && self.height == variant.height
    }
}

/// Accepts only the same variant on the second pass; a result from another
/// protocol could be bound to yet another exit and must not reach the download.
pub(super) fn accept_retry(
    source: &str,
    mut result: Option<ResolveResult>,
    choice: Option<&VariantChoice>,
    protocol: DownloadProtocol,
    overrides: &ProtocolProxies,
) -> Result<Option<ResolveResult>, PluginError> {
    if let Some(resolved) = &mut result {
        if let Some(choice) = choice {
            let variants = std::mem::take(&mut resolved.variants);
            if !variants
                .get(choice.index)
                .is_some_and(|v| choice.matches(v))
            {
                return Err(PluginError::InvalidOutput(
                    "Resolver changed the selected variant after switching protocol proxy"
                        .to_owned(),
                ));
            }
            super::apply_chosen_variant(resolved, variants, choice.index as i32);
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
